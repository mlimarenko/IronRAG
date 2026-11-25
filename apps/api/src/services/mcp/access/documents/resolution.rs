use super::*;
use crate::services::ai_catalog_service::ResolvedRuntimeBinding;
use crate::services::query::execution::operational_provider_failure as operational_embedding_failure;
use crate::services::query::vector_dimensions::EmbeddingProfileInventoryVersion;

pub(crate) enum SearchEmbeddingResolution {
    Ready(McpSearchEmbeddingContext),
    EmptyInventory,
    ProviderUnavailable,
}

pub(crate) async fn resolve_search_libraries(
    auth: &AuthContext,
    state: &AppState,
    requested_library_refs: Option<&[String]>,
) -> Result<Vec<VisibleLibraryContext>, ApiError> {
    if let Some(library_refs) = requested_library_refs {
        if library_refs.is_empty() {
            return Err(ApiError::invalid_mcp_tool_call(
                "libraries must not be empty when provided",
            ));
        }
        let mut rows = Vec::with_capacity(library_refs.len());
        for library_ref in library_refs {
            let library =
                load_library_by_catalog_ref(auth, state, library_ref, POLICY_MCP_MEMORY_READ)
                    .await?;
            rows.push(library);
        }
        return describe_libraries(auth, state, rows).await;
    }

    let libraries = load_visible_library_contexts(auth, state, None).await?;
    Ok(libraries
        .into_iter()
        .filter(|item| {
            auth.has_library_permission(
                item.library.workspace_id,
                item.library.id,
                POLICY_MCP_MEMORY_READ,
            )
        })
        .collect())
}

pub(crate) async fn resolve_search_embedding_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<SearchEmbeddingResolution, ApiError> {
    let _vector_guard = state
        .application_services
        .search
        .vector_plane_read_guard(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let inventory_version = load_embedding_profile_inventory_version(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let binding = resolve_search_embedding_binding(state, library_id).await?;
    let embedding_profile_key = binding.embedding_execution_profile_key();
    let Some(dimensions) = resolve_indexed_embedding_dimensions(
        state,
        library_id,
        &embedding_profile_key,
        inventory_version,
    )
    .await?
    else {
        return Ok(SearchEmbeddingResolution::EmptyInventory);
    };
    drop(_vector_guard);

    let embedding_result = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: query_text.to_string(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url.clone(),
            extra_parameters_json: binding.extra_parameters_json.clone(),
            dispatch: Some(binding.dispatch_authority()),
        })
        .await;
    let embedding = match embedding_result {
        Ok(embedding) => embedding,
        Err(error) if operational_embedding_failure(&error) => {
            ensure_active_embedding_profile_key(state, library_id, &embedding_profile_key)
                .await
                .map_err(map_runtime_lifecycle_error)?;
            ensure_embedding_profile_inventory_version_current(
                state,
                library_id,
                inventory_version,
            )
            .await
            .map_err(map_runtime_lifecycle_error)?;
            warn!(%library_id, "MCP search embedding provider unavailable");
            return Ok(SearchEmbeddingResolution::ProviderUnavailable);
        }
        Err(error) => return Err(map_search_embedding_failure(error)),
    };

    validate_resolved_search_embedding(
        state,
        library_id,
        &embedding_profile_key,
        inventory_version,
        dimensions,
        &embedding.embedding,
    )
    .await?;

    Ok(SearchEmbeddingResolution::Ready(McpSearchEmbeddingContext {
        model_catalog_id: binding.model_catalog_id,
        embedding_profile_key,
        inventory_version,
        dimensions,
        query_vector: embedding.embedding,
    }))
}

/// Re-proves the profile and inventory after the paid call, then bounds the
/// vector it returned against the proven dimensions.
async fn validate_resolved_search_embedding(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    inventory_version: EmbeddingProfileInventoryVersion,
    dimensions: u64,
    vector: &[f32],
) -> Result<(), ApiError> {
    ensure_active_embedding_profile_key(state, library_id, embedding_profile_key)
        .await
        .map_err(map_runtime_lifecycle_error)?;
    ensure_embedding_profile_inventory_version_current(state, library_id, inventory_version)
        .await
        .map_err(map_runtime_lifecycle_error)?;
    validate_embedding_vector_dimensions(dimensions, vector, "MCP document search query")
        .map(|_| ())
        .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

/// Resolves the active embed_chunk binding that owns this library's query
/// embeddings; a missing binding refuses the search instead of guessing one.
async fn resolve_search_embedding_binding(
    state: &AppState,
    library_id: Uuid,
) -> Result<ResolvedRuntimeBinding, ApiError> {
    state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            map_runtime_lifecycle_error(anyhow::Error::new(QueryServiceError::StateConflict {
                message: format!(
                    "active embed_chunk binding is unavailable while proving the exact vector inventory for library {library_id}; configure the binding and rebuild before querying"
                ),
            }))
        })
}

/// Proves the exact vector inventory for the active profile. `None` means the
/// library has no indexed inventory for it yet, which empties the search rather
/// than querying an unproven plane.
async fn resolve_indexed_embedding_dimensions(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    inventory_version: EmbeddingProfileInventoryVersion,
) -> Result<Option<u64>, ApiError> {
    let index_state = ensure_library_embedding_profile_indexed(
        state,
        library_id,
        embedding_profile_key,
        inventory_version,
    )
    .await
    .map_err(map_runtime_lifecycle_error)?;
    match index_state {
        EmbeddingProfileIndexState::Empty => {
            ensure_embedding_profile_inventory_version_current(
                state,
                library_id,
                inventory_version,
            )
            .await
            .map_err(map_runtime_lifecycle_error)?;
            Ok(None)
        }
        EmbeddingProfileIndexState::Ready { dimensions } => Ok(Some(dimensions)),
    }
}

/// Maps an embedding provider failure to the caller-visible API failure,
/// keeping admission refusals distinguishable from transport failures.
fn map_search_embedding_failure(error: anyhow::Error) -> ApiError {
    if let Some(provider_error) =
        error.downcast_ref::<crate::integrations::retry::ProviderCallError>()
    {
        let failure = crate::shared::provider_failure::ProviderFailure::from(provider_error);
        if failure.is_refusal() {
            return ApiError::ProviderFailure(
                failure.refusal_code().unwrap_or("provider_admission_refused").to_owned(),
            );
        }
    }
    ApiError::ProviderFailure(format!("failed to embed MCP memory search query: {error}"))
}

pub(crate) async fn load_knowledge_chunks_by_ids(
    state: &AppState,
    chunk_ids: &[Uuid],
) -> Result<Vec<crate::infra::knowledge_rows::KnowledgeChunkRow>, ApiError> {
    if chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    state
        .document_store
        .list_chunks_by_ids(chunk_ids)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

#[cfg(test)]
mod degradation_tests {
    use super::*;
    use crate::integrations::retry::ProviderCallError;

    #[test]
    fn only_typed_operational_embedding_errors_are_degradable() {
        assert!(operational_embedding_failure(&anyhow::Error::new(ProviderCallError::timeout(
            "embedding"
        ))));
        assert!(!operational_embedding_failure(&anyhow::Error::new(
            ProviderCallError::local_request_rejected()
        )));
        assert!(!operational_embedding_failure(&anyhow::Error::new(ProviderCallError::admission(
            crate::integrations::provider_budget::ProviderBudgetError::Dispatch(
                crate::shared::provider_http::ProviderDispatchRefusal::Authority,
            ),
        ))));
        assert!(!operational_embedding_failure(&anyhow::anyhow!("provider timeout")));
    }
}
