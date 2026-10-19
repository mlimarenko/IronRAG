use super::{
    persistence::{load_vector_write_source_fence, resolve_active_embedding_binding},
    *,
};

use crate::infra::knowledge_plane::RevisionReadinessUpdate;

impl SearchService {
    /// Rebuild the vector plane for a single library against its active
    /// `EmbedChunk` binding dimension. Per-library now: dropping a library's
    /// rows from any wrong-dim shard, ensuring the target-dim shard exists,
    /// then re-embedding chunks and entities into it.
    ///
    /// Other libraries' material is untouched — different libraries on
    /// different embed bindings (and therefore different dims) coexist in
    /// separate per-dim shards.
    pub async fn rebuild_vector_plane_for_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> std::result::Result<VectorPlaneRebuildOutcome, QueryServiceError> {
        // Rebuild serialization is intentionally separate from the data-plane
        // read/write lock. Provider calls populate an opaque staging profile,
        // so queries keep reading the previous active profile until the
        // short atomic promotion below.
        let _rebuild_guard = self.vector_rebuild_guard(state, library_id).await?;
        discard_abandoned_vector_rebuilds(state, library_id).await?;
        let source_truth_version = load_vector_write_source_fence(state, library_id).await?;
        let active_chunk_count = state
            .document_store
            .count_active_chunks_by_library(library_id)
            .await
            .context("failed to count active chunks before vector-plane rebuild")?;
        if active_chunk_count == 0 {
            self.purge_empty_vector_plane_under_write_guard(
                state,
                library_id,
                source_truth_version,
            )
            .await?;
            return Ok(VectorPlaneRebuildOutcome {
                previous_dimensions: None,
                target_dimensions: None,
                indexes_recreated: false,
                libraries_rebuilt: 1,
                chunk_embeddings_rebuilt: 0,
                graph_node_embeddings_rebuilt: 0,
            });
        }
        let embedding_binding = resolve_active_embedding_binding(state, library_id).await?;
        let dimension_state =
            library_vector_index_dimension_state_for_binding(state, library_id, &embedding_binding)
                .await?;
        let active_embedding_profile_key = embedding_binding.embedding_execution_profile_key();
        let staging_embedding_model_key = vector_rebuild_staging_profile_key(
            &active_embedding_profile_key,
            Uuid::now_v7(),
            source_truth_version,
        );
        let previous_dimensions = None;

        let mut outcome = VectorPlaneRebuildOutcome {
            previous_dimensions,
            target_dimensions: None,
            indexes_recreated: false,
            libraries_rebuilt: 0,
            chunk_embeddings_rebuilt: 0,
            graph_node_embeddings_rebuilt: 0,
        };
        let mut prepared_dimensions = None;
        let staged_result = async {
            let (chunk_rebuild, target_dimensions) = self
                .rebuild_chunk_embeddings_with_dimension_state(
                    super::chunk_rebuild::ChunkEmbeddingRebuildInput {
                        state,
                        library_id,
                        dimension_state,
                        embedding_binding: &embedding_binding,
                        active_embedding_profile_key: &active_embedding_profile_key,
                        staging_embedding_model_key: &staging_embedding_model_key,
                    },
                    &mut prepared_dimensions,
                )
                .await?;
            let entity_rebuilt = self
                .rebuild_graph_node_embeddings_with_expected_dimensions(
                    super::graph_rebuild::GraphNodeEmbeddingRebuildInput {
                        state,
                        library_id,
                        expected_dimensions: target_dimensions.get(),
                        embedding_binding: &embedding_binding,
                        staging_embedding_model_key: &staging_embedding_model_key,
                    },
                    &mut prepared_dimensions,
                )
                .await?;
            ensure_embedding_profile_stable_after_rebuild(
                state,
                library_id,
                &active_embedding_profile_key,
            )
            .await?;
            ensure_rebuilt_chunk_embedding_inventory(
                state,
                library_id,
                &staging_embedding_model_key,
                chunk_rebuild.rebuilt,
            )
            .await?;
            let expected_chunk_count = u64::try_from(chunk_rebuild.rebuilt)
                .context("rebuilt chunk count overflowed u64")?;
            let expected_entity_count =
                u64::try_from(entity_rebuilt).context("rebuilt entity count overflowed u64")?;
            Ok::<_, QueryServiceError>({
                (
                    chunk_rebuild,
                    entity_rebuilt,
                    expected_chunk_count,
                    expected_entity_count,
                    target_dimensions,
                )
            })
        }
        .await;
        let (
            chunk_rebuild,
            entity_rebuilt,
            expected_chunk_count,
            expected_entity_count,
            target_dimensions,
        ) = discard_staged_rebuild_on_error(
            state,
            library_id,
            prepared_dimensions,
            &staging_embedding_model_key,
            staged_result,
        )
        .await?;
        if let Err(error) = self
            .promote_staged_vector_rebuild_under_write_guard(
                state,
                StagedVectorRebuildPromotion {
                    library_id,
                    dimensions: target_dimensions.get(),
                    active_embedding_profile_key: &active_embedding_profile_key,
                    staging_embedding_model_key: &staging_embedding_model_key,
                    expected_source_truth_version: source_truth_version,
                    chunk_count: Some(expected_chunk_count),
                    entity_count: Some(expected_entity_count),
                },
            )
            .await
        {
            return Err(discard_staged_vector_rebuild_if_prepared(
                state,
                library_id,
                prepared_dimensions,
                &staging_embedding_model_key,
                error,
            )
            .await);
        }
        invalidate_library_vector_index_dimensions(library_id);
        ensure_rebuilt_chunk_embedding_inventory(
            state,
            library_id,
            &active_embedding_profile_key,
            chunk_rebuild.rebuilt,
        )
        .await?;
        outcome.chunk_embeddings_rebuilt = chunk_rebuild.rebuilt;
        outcome.graph_node_embeddings_rebuilt = entity_rebuilt;
        outcome.target_dimensions = Some(target_dimensions.get());
        mark_revisions_vector_ready(state, &chunk_rebuild.touched_revision_ids)
            .await
            .context("failed to mark rebuilt revisions as vector-ready")?;
        invalidate_library_embedding_profile_inventory(library_id);
        outcome.libraries_rebuilt = 1;
        Ok(outcome)
    }
}

pub(super) fn finish_deferred_manifest_rebuild<T>(
    rebuild_result: std::result::Result<T, QueryServiceError>,
    reconciliation_result: std::result::Result<(), QueryServiceError>,
) -> std::result::Result<T, QueryServiceError> {
    match (rebuild_result, reconciliation_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(rebuild_error), Ok(())) => Err(rebuild_error),
        (Ok(_), Err(reconciliation_error)) => Err(reconciliation_error),
        (Err(rebuild_error), Err(reconciliation_error)) => {
            Err(QueryServiceError::Internal(anyhow!(
                "vector rebuild failed before manifest reconciliation: {rebuild_error}; manifest reconciliation also failed: {reconciliation_error}"
            )))
        }
    }
}

pub(super) async fn discard_staged_rebuild_on_error<T>(
    state: &AppState,
    library_id: Uuid,
    dimensions: Option<u64>,
    staging_embedding_model_key: &str,
    staged_result: std::result::Result<T, QueryServiceError>,
) -> std::result::Result<T, QueryServiceError> {
    match staged_result {
        Ok(result) => Ok(result),
        Err(error) => Err(discard_staged_vector_rebuild_if_prepared(
            state,
            library_id,
            dimensions,
            staging_embedding_model_key,
            error,
        )
        .await),
    }
}

pub(super) async fn discard_staged_vector_rebuild_if_prepared(
    state: &AppState,
    library_id: Uuid,
    dimensions: Option<u64>,
    staging_embedding_model_key: &str,
    primary_error: QueryServiceError,
) -> QueryServiceError {
    let Some(dimensions) = dimensions else {
        return primary_error;
    };
    match state
        .search_store
        .discard_staged_vector_rebuild(library_id, dimensions, staging_embedding_model_key)
        .await
    {
        Ok(()) => primary_error,
        Err(cleanup_error) => QueryServiceError::Internal(anyhow!(
            "staged vector rebuild failed: {primary_error}; staging cleanup also failed: {cleanup_error:#}"
        )),
    }
}

pub(super) async fn discard_abandoned_vector_rebuilds(
    state: &AppState,
    library_id: Uuid,
) -> std::result::Result<(), QueryServiceError> {
    let discarded = state
        .search_store
        .discard_abandoned_staged_vector_rebuilds(library_id)
        .await
        .context("failed to recover abandoned staged vector rebuilds")?;
    if discarded > 0 {
        tracing::warn!(
            library_id = %library_id,
            discarded,
            "removed durable staging residue before vector rebuild",
        );
    }
    Ok(())
}

pub(super) fn vector_rebuild_staging_profile_key(
    active_embedding_profile_key: &str,
    rebuild_id: Uuid,
    source_truth_version: i64,
) -> String {
    debug_assert!(source_truth_version > 0);
    let mut hasher = Sha256::new();
    hasher.update(b"ironrag.vector-rebuild-staging.v1");
    hasher.update(active_embedding_profile_key.as_bytes());
    hasher.update(rebuild_id.as_bytes());
    hasher.update(source_truth_version.to_be_bytes());
    let digest = hex::encode(hasher.finalize());
    let encoded_source_truth_version = u64::try_from(source_truth_version).unwrap_or(0);
    format!(
        "{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{:016x}{}",
        encoded_source_truth_version,
        &digest[..48],
    )
}

pub(super) async fn ensure_embedding_profile_stable_after_rebuild(
    state: &AppState,
    library_id: Uuid,
    rebuilt_profile_key: &str,
) -> std::result::Result<(), QueryServiceError> {
    let active_profile_key = resolve_active_embedding_binding(state, library_id)
        .await?
        .embedding_execution_profile_key();
    if active_profile_key == rebuilt_profile_key {
        return Ok(());
    }
    Err(QueryServiceError::StateConflict {
        message: format!(
            "active embedding execution profile changed while rebuilding library {library_id}; rerun `ironrag-maintenance rebuild vector-plane --source-library {library_id}`"
        ),
    })
}

pub(super) async fn ensure_rebuilt_chunk_embedding_inventory(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    rebuilt: usize,
) -> std::result::Result<(), QueryServiceError> {
    let expected = state
        .document_store
        .count_active_chunks_by_library(library_id)
        .await
        .context("failed to count active chunks after vector rebuild")?;
    let rebuilt = u64::try_from(rebuilt).map_err(|_| QueryServiceError::StateConflict {
        message: format!(
            "rebuilt chunk count overflowed for library {library_id}; rerun `ironrag-maintenance rebuild vector-plane --source-library {library_id}`"
        ),
    })?;
    validate_rebuilt_chunk_count(library_id, expected, rebuilt)?;
    let index_state = inspect_library_embedding_profile_indexed_uncached(
        state,
        library_id,
        embedding_profile_key,
    )
    .await?;
    match (expected, index_state) {
        (0, EmbeddingProfileIndexState::Empty)
        | (1.., EmbeddingProfileIndexState::Ready { .. }) => Ok(()),
        _ => Err(QueryServiceError::StateConflict {
            message: format!(
                "rebuilt vector inventory state does not match the active chunk inventory for library {library_id}; rerun `ironrag-maintenance rebuild vector-plane --source-library {library_id}`"
            ),
        }),
    }
}

pub(super) fn validate_rebuilt_chunk_count(
    library_id: Uuid,
    expected: u64,
    rebuilt: u64,
) -> std::result::Result<(), QueryServiceError> {
    if expected == rebuilt {
        return Ok(());
    }
    Err(QueryServiceError::StateConflict {
        message: format!(
            "active chunk inventory changed while rebuilding library {library_id} (expected {expected}, rebuilt {rebuilt}); rerun `ironrag-maintenance rebuild vector-plane --source-library {library_id}`"
        ),
    })
}

pub(super) async fn mark_revisions_vector_ready(
    state: &AppState,
    revision_ids: &BTreeSet<Uuid>,
) -> AnyhowResult<()> {
    for revision_id in revision_ids {
        let revision = state
            .document_store
            .get_revision(*revision_id)
            .await
            .with_context(|| format!("failed to load revision {}", revision_id))?
            .ok_or_else(|| anyhow!("knowledge revision {} not found", revision_id))?;
        let updated = state
            .document_store
            .update_revision_readiness(RevisionReadinessUpdate {
                revision_id: revision.revision_id,
                text_state: &revision.text_state,
                vector_state: "ready",
                graph_state: &revision.graph_state,
                text_readable_at: revision.text_readable_at,
                vector_ready_at: Some(Utc::now()),
                graph_ready_at: revision.graph_ready_at,
                superseded_by_revision_id: revision.superseded_by_revision_id,
            })
            .await
            .with_context(|| format!("failed to update vector readiness for {}", revision_id))?;
        if updated.is_none() {
            return Err(anyhow!(
                "knowledge revision {} disappeared during vector readiness update",
                revision_id
            ));
        }
    }
    Ok(())
}
