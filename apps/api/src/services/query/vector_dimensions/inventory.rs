use super::*;

/// Return a typed empty/ready preflight for the exact active profile and fail
/// closed for every partial or mixed inventory. An actually empty library is
/// healthy and skips provider embedding plus ANN. A same-model or
/// same-dimension lane is intentionally not a fallback: operators must rebuild
/// before semantic lookup resumes.
pub(crate) async fn ensure_library_embedding_profile_indexed(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    version: EmbeddingProfileInventoryVersion,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    let cell = profile_inventory_cell(library_id, version, embedding_profile_key);
    let cached = cell
        .get_or_init(|| async {
            cached_profile_inventory_resolution(
                resolve_embedding_profile_index_state(
                    state,
                    library_id,
                    embedding_profile_key,
                    version,
                )
                .await,
            )
        })
        .await;
    cached_profile_inventory_result(cached)
}

pub(crate) async fn load_embedding_profile_inventory_version(
    state: &AppState,
    library_id: Uuid,
) -> AnyhowResult<EmbeddingProfileInventoryVersion> {
    let source_truth_version = async {
        catalog_repository::get_library_source_truth_version(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(anyhow::Error::from)
    };
    let (signals, source_truth_version) = tokio::try_join!(
        state.document_store.aggregate_library_generation_signals(library_id),
        source_truth_version,
    )
    .with_context(|| {
        format!("failed to load exact vector inventory version for library {library_id}")
    })?;
    Ok(EmbeddingProfileInventoryVersion {
        active_vector_generation: signals.active_vector_generation,
        source_truth_version,
        has_ready_vector: signals.has_ready_vector,
    })
}

pub(crate) fn validate_embedding_profile_inventory_version(
    library_id: Uuid,
    expected: EmbeddingProfileInventoryVersion,
    observed: EmbeddingProfileInventoryVersion,
) -> AnyhowResult<()> {
    if expected == observed {
        return Ok(());
    }
    Err(anyhow::Error::new(QueryServiceError::StateConflict {
        message: format!(
            "active vector generation changed while preparing a query for library {library_id}; retry the query against the current exact-profile inventory"
        ),
    }))
}

pub(crate) async fn ensure_embedding_profile_inventory_version_current(
    state: &AppState,
    library_id: Uuid,
    expected: EmbeddingProfileInventoryVersion,
) -> AnyhowResult<()> {
    let observed = load_embedding_profile_inventory_version(state, library_id).await?;
    validate_embedding_profile_inventory_version(library_id, expected, observed)
}

async fn resolve_embedding_profile_index_state(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    version: EmbeddingProfileInventoryVersion,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    let index_state = inspect_library_embedding_profile_indexed_uncached(
        state,
        library_id,
        embedding_profile_key,
    )
    .await?;
    validate_index_state_against_generation(library_id, version, index_state)
}

/// Maintenance-only exact proof before revision readiness is promoted. Query
/// paths must use the generation-fenced cached wrapper above.
pub(crate) async fn inspect_library_embedding_profile_indexed_uncached(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    let (inventory, expected_chunk_count) = tokio::try_join!(
        state.search_store.inspect_chunk_vector_profile(
            library_id,
            embedding_profile_key,
            KNOWLEDGE_CHUNK_VECTOR_KIND,
        ),
        state.document_store.count_active_chunks_by_library(library_id),
    )
    .with_context(|| {
        format!(
            "failed to inspect exact embedding-profile vector inventory for library {library_id}"
        )
    })?;
    validate_embedding_profile_inventory(
        library_id,
        &inventory.dimensions,
        expected_chunk_count,
        inventory.active_vector_count,
    )
}

pub(super) fn validate_index_state_against_generation(
    library_id: Uuid,
    version: EmbeddingProfileInventoryVersion,
    index_state: EmbeddingProfileIndexState,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    match index_state {
        EmbeddingProfileIndexState::Empty => Ok(EmbeddingProfileIndexState::Empty),
        EmbeddingProfileIndexState::Ready { .. }
            if version.has_ready_vector && version.active_vector_generation > 0 =>
        {
            Ok(index_state)
        }
        EmbeddingProfileIndexState::Ready { .. } => {
            Err(anyhow::Error::new(QueryServiceError::StateConflict {
                message: format!(
                    "library {library_id} has active vectors for the active embedding profile but no active vector generation; rebuild the vector plane before semantic lookup"
                ),
            }))
        }
    }
}

fn cached_profile_inventory_resolution(
    result: AnyhowResult<EmbeddingProfileIndexState>,
) -> CachedProfileInventoryResolution {
    let result = result.map_err(CachedQueryFailureSnapshot::from_error);
    let ttl =
        if result.is_ok() { PROFILE_INVENTORY_SUCCESS_TTL } else { PROFILE_INVENTORY_ERROR_TTL };
    CachedProfileInventoryResolution { result, expires_at: Instant::now() + ttl }
}

fn cached_profile_inventory_result(
    cached: &CachedProfileInventoryResolution,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    match &cached.result {
        Ok(index_state) => Ok(*index_state),
        Err(error) => Err(error.to_error()),
    }
}

pub(crate) fn validate_active_embedding_profile_key(
    library_id: Uuid,
    expected_embedding_profile_key: &str,
    active_embedding_profile_key: &str,
) -> AnyhowResult<()> {
    if expected_embedding_profile_key == active_embedding_profile_key {
        return Ok(());
    }
    Err(anyhow::Error::new(QueryServiceError::StateConflict {
        message: format!(
            "active embedding execution profile changed while preparing a query for library {library_id}; retry the query after the vector index is rebuilt for the active profile"
        ),
    }))
}

/// Re-resolve the active `EmbedChunk` binding and fence a long-running
/// operation against a profile change. This is intentionally uncached: the
/// caller uses it at a readiness or ANN boundary where stale identity would
/// make vectors from two execution profiles appear interchangeable.
pub(crate) async fn ensure_active_embedding_profile_key(
    state: &AppState,
    library_id: Uuid,
    expected_embedding_profile_key: &str,
) -> AnyhowResult<()> {
    let active_embedding_profile_key = load_active_embedding_profile_key(state, library_id).await?;
    validate_active_embedding_profile_key(
        library_id,
        expected_embedding_profile_key,
        &active_embedding_profile_key,
    )
}

async fn load_active_embedding_profile_key(
    state: &AppState,
    library_id: Uuid,
) -> AnyhowResult<String> {
    let binding = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .with_context(|| {
            format!("failed to re-resolve active embed_chunk binding for library {library_id}")
        })?
        .ok_or_else(|| QueryServiceError::StateConflict {
            message: format!(
                "active embed_chunk binding disappeared while processing library {library_id}; retry after configuring and rebuilding the vector profile"
            ),
        })?;
    Ok(binding.embedding_execution_profile_key())
}

#[async_trait]
impl ActiveEmbeddingProfileSource for AppState {
    async fn active_embedding_profile_key(&self, library_id: Uuid) -> AnyhowResult<String> {
        load_active_embedding_profile_key(self, library_id).await
    }
}

pub(super) fn validate_embedding_profile_inventory(
    library_id: Uuid,
    dimensions: &[u64],
    expected_chunk_count: u64,
    vector_chunk_count: u64,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    let mut nonzero_dimensions =
        dimensions.iter().copied().filter(|dimensions| *dimensions > 0).collect::<Vec<_>>();
    nonzero_dimensions.sort_unstable();
    nonzero_dimensions.dedup();
    if expected_chunk_count == 0 && vector_chunk_count == 0 && nonzero_dimensions.is_empty() {
        return Ok(EmbeddingProfileIndexState::Empty);
    }
    if let [dimensions] = nonzero_dimensions.as_slice()
        && expected_chunk_count > 0
        && vector_chunk_count == expected_chunk_count
    {
        return Ok(EmbeddingProfileIndexState::Ready { dimensions: *dimensions });
    }

    Err(anyhow::Error::new(QueryServiceError::StateConflict {
        message: if nonzero_dimensions.is_empty() || vector_chunk_count == 0 {
            format!(
                "library {library_id} has an active vector generation but no vectors for the active embedding execution profile; run `ironrag-maintenance rebuild vector-plane --source-library {library_id}` before vector search"
            )
        } else if vector_chunk_count != expected_chunk_count {
            format!(
                "library {library_id} has an incomplete vector inventory for the active embedding execution profile ({vector_chunk_count}/{expected_chunk_count} active chunks); run `ironrag-maintenance rebuild vector-plane --source-library {library_id}` before vector search"
            )
        } else {
            format!(
                "library {library_id} has multiple vector dimensions for one embedding execution profile; run `ironrag-maintenance rebuild vector-plane --source-library {library_id}` before vector search"
            )
        },
    }))
}

#[cfg(test)]
pub(super) async fn resolve_profile_inventory_once_for_test<F, Fut>(
    library_id: Uuid,
    version: EmbeddingProfileInventoryVersion,
    embedding_profile_key: &str,
    resolver: F,
) -> AnyhowResult<EmbeddingProfileIndexState>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = AnyhowResult<EmbeddingProfileIndexState>>,
{
    let cell = profile_inventory_cell(library_id, version, embedding_profile_key);
    let cached =
        cell.get_or_init(|| async { cached_profile_inventory_resolution(resolver().await) }).await;
    cached_profile_inventory_result(cached)
}
