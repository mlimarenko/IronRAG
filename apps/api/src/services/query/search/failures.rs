use super::*;

pub(super) async fn fail_embed_chunks_after_cleanup<T>(
    state: &AppState,
    revision_id: Uuid,
    expected_source_truth_version: i64,
    ingest_attempt: IngestVectorWriteFence,
    cleanup_owned_vector_ids: &BTreeSet<Uuid>,
    error: anyhow::Error,
) -> std::result::Result<T, QueryServiceError> {
    if cleanup_owned_vector_ids.is_empty() {
        return Err(error.into());
    }
    let revision = match state.document_store.get_revision(revision_id).await {
        Ok(Some(revision)) => revision,
        Ok(None) => {
            return Err(error
                .context(format!(
                    "failed to fence partial-vector cleanup: revision {revision_id} disappeared"
                ))
                .into());
        }
        Err(fence_error) => {
            return Err(error
                .context(format!(
                    "failed to load revision {revision_id} before partial-vector cleanup: {fence_error:#}"
                ))
                .into());
        }
    };
    let _vector_guard = match state
        .application_services
        .search
        .vector_plane_write_guard(state, revision.library_id)
        .await
    {
        Ok(guard) => guard,
        Err(fence_error) => {
            return Err(error
                .context(format!(
                    "failed to acquire the vector cleanup guard for revision {revision_id}: {fence_error:#}"
                ))
                .into());
        }
    };
    let vector_ids = cleanup_owned_vector_ids.iter().copied().collect::<Vec<_>>();
    match state
        .search_store
        .delete_attempt_owned_chunk_vectors_by_ids_fenced(
            revision.library_id,
            &vector_ids,
            expected_source_truth_version,
            ingest_attempt,
        )
        .await
    {
        Ok(Some(outcome)) if outcome.deleted > 0 => {
            invalidate_library_embedding_profile_inventory(revision.library_id);
            log_removed_attempt_owned_vectors(revision_id, outcome.deleted);
        }
        Ok(None) => {
            log_preserved_partial_vectors(revision_id, ingest_attempt.attempt_id);
        }
        Ok(Some(_)) => {}
        Err(cleanup_error) => {
            return Err(error.context(format!(
                "failed to remove partial chunk vectors for revision {revision_id}: {cleanup_error:#}"
            )).into());
        }
    }
    Err(error.into())
}

fn log_removed_attempt_owned_vectors(revision_id: Uuid, deleted: u64) {
    tracing::warn!(
        revision_id = %revision_id,
        deleted = deleted,
        "removed only attempt-owned chunk vectors after failed embed_chunk stage",
    );
}

fn log_preserved_partial_vectors(revision_id: Uuid, attempt_id: Uuid) {
    tracing::warn!(
        revision_id = %revision_id,
        attempt_id = %attempt_id,
        "preserved partial chunk vectors because ingest authority moved to a retry",
    );
}

/// Decide cleanup from the preserved typed error, never from its display text.
pub(super) async fn fail_embed_chunks_by_typed_policy<T>(
    state: &AppState,
    revision_id: Uuid,
    expected_source_truth_version: i64,
    ingest_attempt: IngestVectorWriteFence,
    cleanup_owned_vector_ids: &BTreeSet<Uuid>,
    error: anyhow::Error,
) -> std::result::Result<T, QueryServiceError> {
    let error = QueryServiceError::from(error);
    if error.preserves_partial_vectors() {
        return fail_embed_chunks_preserving_partial(revision_id, error);
    }
    fail_embed_chunks_after_cleanup(
        state,
        revision_id,
        expected_source_truth_version,
        ingest_attempt,
        cleanup_owned_vector_ids,
        anyhow::Error::new(error),
    )
    .await
}

/// Preserve completed batches only for an explicitly typed provider failure.
/// Readiness remains count-gated, so a partial revision cannot become ready.
pub(super) fn fail_embed_chunks_preserving_partial<T>(
    revision_id: Uuid,
    error: QueryServiceError,
) -> std::result::Result<T, QueryServiceError> {
    debug_assert!(error.preserves_partial_vectors());
    tracing::warn!(
        revision_id = %revision_id,
        error = %error,
        "embed_chunk stage failed transiently; preserving persisted chunk vectors for retry",
    );
    Err(error)
}

/// Cancellation and lease loss deliberately preserve rows already committed by
/// this attempt. The retry/count gate owns recovery; destructive cleanup here
/// could race a replacement attempt after the cancellation signal was raised.
pub(super) fn fail_embed_chunks_cancelled<T>(
    revision_id: Uuid,
) -> std::result::Result<T, QueryServiceError> {
    tracing::warn!(
        revision_id = %revision_id,
        "embed_chunk stage cancelled; preserving attempt-owned vectors for retry or GC",
    );
    Err(QueryServiceError::Cancelled)
}

pub(super) fn fail_embed_chunks_if_cancelled(
    revision_id: Uuid,
    cancellation_token: &CancellationToken,
) -> std::result::Result<(), QueryServiceError> {
    if cancellation_token.is_cancelled() {
        fail_embed_chunks_cancelled(revision_id)
    } else {
        Ok(())
    }
}
