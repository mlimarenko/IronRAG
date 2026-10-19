use super::*;

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
) -> Result<Option<McpSearchEmbeddingContext>, ApiError> {
    let _vector_guard = state
        .application_services
        .search
        .vector_plane_read_guard(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let inventory_version = load_embedding_profile_inventory_version(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let binding = state
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
        })?;

    let embedding_profile_key = binding.embedding_execution_profile_key();
    let index_state = ensure_library_embedding_profile_indexed(
        state,
        library_id,
        &embedding_profile_key,
        inventory_version,
    )
    .await
    .map_err(map_runtime_lifecycle_error)?;
    let dimensions = match index_state {
        EmbeddingProfileIndexState::Empty => {
            ensure_embedding_profile_inventory_version_current(
                state,
                library_id,
                inventory_version,
            )
            .await
            .map_err(map_runtime_lifecycle_error)?;
            return Ok(None);
        }
        EmbeddingProfileIndexState::Ready { dimensions } => dimensions,
    };
    drop(_vector_guard);

    let embedding = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: query_text.to_string(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url.clone(),
            extra_parameters_json: binding.extra_parameters_json.clone(),
        })
        .await
        .map_err(|error| {
            ApiError::ProviderFailure(format!("failed to embed MCP memory search query: {error}"))
        })?;

    ensure_active_embedding_profile_key(state, library_id, &embedding_profile_key)
        .await
        .map_err(map_runtime_lifecycle_error)?;
    ensure_embedding_profile_inventory_version_current(state, library_id, inventory_version)
        .await
        .map_err(map_runtime_lifecycle_error)?;
    validate_embedding_vector_dimensions(
        dimensions,
        &embedding.embedding,
        "MCP document search query",
    )
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    Ok(Some(McpSearchEmbeddingContext {
        model_catalog_id: binding.model_catalog_id,
        embedding_profile_key,
        inventory_version,
        dimensions,
        query_vector: embedding.embedding,
    }))
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
