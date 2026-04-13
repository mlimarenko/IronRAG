use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{content_repository, ingest_repository},
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_DOCUMENTS_WRITE, load_canonical_content_document_and_authorize},
        router_support::ApiError,
    },
    services::content::service::{AdmitMutationCommand, ContentMutationAdmission},
};

use super::types::{
    ContentMutationDetailResponse, build_reprocess_revision_metadata,
    build_web_refetch_revision_metadata, map_mutation_admission,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchDeleteRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchDeleteResponse {
    pub deleted_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchDeleteResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchDeleteResult {
    pub document_id: Uuid,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchCancelRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchCancelResponse {
    pub cancelled_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchCancelResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchCancelResult {
    pub document_id: Uuid,
    pub jobs_cancelled: u64,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchReprocessRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchReprocessResponse {
    pub reprocessed_count: usize,
    pub failed_count: usize,
    /// Documents that no longer existed when the batch handler tried to load
    /// them (already deleted, tombstoned, or removed from the catalog by an
    /// earlier batch). They are not failures in the operational sense — the
    /// caller's UI snapshot is just stale — and are reported separately so
    /// the toast can render them as "already removed" instead of an alarm.
    pub skipped_count: usize,
    pub results: Vec<BatchReprocessResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchReprocessResult {
    pub document_id: Uuid,
    pub success: bool,
    /// `true` when the document had been removed before the batch handler
    /// could process it. Distinct from a real failure: the doc is already
    /// gone, retry has nothing to do.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation: Option<ContentMutationDetailResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(super) const BATCH_MAX_DOCUMENTS: usize = 1000;

pub(super) fn ensure_batch_document_limit(document_count: usize) -> Result<(), ApiError> {
    if document_count > BATCH_MAX_DOCUMENTS {
        return Err(ApiError::BadRequest(format!(
            "batch size exceeds maximum of {BATCH_MAX_DOCUMENTS} documents"
        )));
    }
    Ok(())
}

pub(super) async fn batch_delete_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchDeleteRequest>,
) -> Result<Json<BatchDeleteResponse>, ApiError> {
    ensure_batch_document_limit(request.document_ids.len())?;

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut deleted_count = 0usize;
    let mut failed_count = 0usize;

    for document_id in &request.document_ids {
        match load_canonical_content_document_and_authorize(
            &auth,
            &state,
            *document_id,
            POLICY_DOCUMENTS_WRITE,
        )
        .await
        {
            Ok(document) => {
                match state
                    .canonical_services
                    .content
                    .admit_mutation(
                        &state,
                        AdmitMutationCommand {
                            workspace_id: document.workspace_id,
                            library_id: document.library_id,
                            document_id: *document_id,
                            operation_kind: "delete".to_string(),
                            idempotency_key: None,
                            requested_by_principal_id: Some(auth.principal_id),
                            request_surface: "rest".to_string(),
                            source_identity: None,
                            revision: None,
                        },
                    )
                    .await
                {
                    Ok(_) => {
                        deleted_count += 1;
                        results.push(BatchDeleteResult {
                            document_id: *document_id,
                            success: true,
                            error: None,
                        });
                    }
                    Err(error) => {
                        failed_count += 1;
                        results.push(BatchDeleteResult {
                            document_id: *document_id,
                            success: false,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(error) => {
                failed_count += 1;
                results.push(BatchDeleteResult {
                    document_id: *document_id,
                    success: false,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(Json(BatchDeleteResponse { deleted_count, failed_count, results }))
}

pub(super) async fn batch_cancel_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchCancelRequest>,
) -> Result<Json<BatchCancelResponse>, ApiError> {
    ensure_batch_document_limit(request.document_ids.len())?;

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut cancelled_count = 0usize;
    let mut failed_count = 0usize;

    for document_id in &request.document_ids {
        match load_canonical_content_document_and_authorize(
            &auth,
            &state,
            *document_id,
            POLICY_DOCUMENTS_WRITE,
        )
        .await
        {
            Ok(_) => {
                match ingest_repository::cancel_jobs_for_document(
                    &state.persistence.postgres,
                    *document_id,
                )
                .await
                {
                    Ok(jobs_cancelled) => {
                        cancelled_count += 1;
                        results.push(BatchCancelResult {
                            document_id: *document_id,
                            jobs_cancelled,
                            success: true,
                            error: None,
                        });
                    }
                    Err(error) => {
                        failed_count += 1;
                        results.push(BatchCancelResult {
                            document_id: *document_id,
                            jobs_cancelled: 0,
                            success: false,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(error) => {
                failed_count += 1;
                results.push(BatchCancelResult {
                    document_id: *document_id,
                    jobs_cancelled: 0,
                    success: false,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(Json(BatchCancelResponse { cancelled_count, failed_count, results }))
}

pub(super) async fn batch_reprocess_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchReprocessRequest>,
) -> Result<Json<BatchReprocessResponse>, ApiError> {
    ensure_batch_document_limit(request.document_ids.len())?;

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut reprocessed_count = 0usize;
    let mut failed_count = 0usize;
    let mut skipped_count = 0usize;

    for document_id in &request.document_ids {
        match reprocess_single_document(&auth, &state, *document_id).await {
            Ok(admission) => {
                reprocessed_count += 1;
                results.push(BatchReprocessResult {
                    document_id: *document_id,
                    success: true,
                    skipped: false,
                    mutation: Some(map_mutation_admission(admission)),
                    error: None,
                });
            }
            Err(error) => {
                // A "not found" or "already deleted" outcome is not a real
                // failure: the document was removed (often by a previous
                // batch's orphan auto-tombstone) before this batch reached
                // it. Surface it as `skipped` so the toast can render it as
                // "already removed — refresh to update" instead of an alarm.
                if matches!(error, ApiError::NotFound(_)) {
                    skipped_count += 1;
                    results.push(BatchReprocessResult {
                        document_id: *document_id,
                        success: false,
                        skipped: true,
                        mutation: None,
                        error: Some(error.to_string()),
                    });
                } else {
                    failed_count += 1;
                    results.push(BatchReprocessResult {
                        document_id: *document_id,
                        success: false,
                        skipped: false,
                        mutation: None,
                        error: Some(error.to_string()),
                    });
                }
            }
        }
    }

    Ok(Json(BatchReprocessResponse {
        reprocessed_count,
        failed_count,
        skipped_count,
        results,
    }))
}

async fn reprocess_single_document(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
) -> Result<ContentMutationAdmission, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        auth,
        state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    // A previously-tombstoned document (orphan auto-fail, manual delete, or
    // a row left behind by an earlier batch) still has a `content_document`
    // row in Postgres but `document_state='deleted'`. The caller's UI
    // snapshot can be stale and include such a doc; from the retry path's
    // POV there is nothing to do — surface it as `NotFound` so the batch
    // handler counts it under `skipped_count` instead of `failed_count`.
    if let Some(row) = content_repository::get_document_by_id(
        &state.persistence.postgres,
        document_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        && (row.document_state == "deleted" || row.deleted_at.is_some())
    {
        return Err(ApiError::resource_not_found("document", document_id));
    }
    // Prefer the active (head-promoted) revision, but fall back to the latest
    // revision row when the head was never promoted — this is the shape for
    // documents whose previous ingest crashed mid-pipeline before the head
    // update ever fired. Without this fallback, retry would silently bail for
    // every document that got stuck before reaching `promote_document_head`.
    let active_revision = state
        .canonical_services
        .content
        .resolve_reprocess_revision(state, document_id)
        .await?;

    // Web-captured documents: retry means "go back to the site and pull the
    // current version", not "re-parse the same captured bytes". We re-fetch
    // the `source_uri`, persist a fresh snapshot under a new storage_key, and
    // build the reprocess metadata around the new blob. Diff-aware chunk
    // reuse still applies downstream: if the live site matches the previous
    // capture byte-for-byte at the chunk level, existing extractions are
    // copied into the new revision without LLM calls; only genuinely changed
    // chunks get a fresh run.
    //
    // Non-web documents continue to reuse the stored source — there is
    // nothing to "re-fetch" for an upload.
    let reprocess_metadata = if active_revision.content_source_kind == "web_page" {
        let source_uri = active_revision.source_uri.as_deref().ok_or_else(|| {
            ApiError::BadRequest(
                "web-captured document has no source_uri to re-fetch".to_string(),
            )
        })?;
        let refetched = state
            .canonical_services
            .web_ingest
            .refetch_document_source(
                state,
                document.workspace_id,
                document.library_id,
                source_uri,
            )
            .await?;
        build_web_refetch_revision_metadata(&active_revision, refetched)
    } else {
        let resolved_storage_key = state
            .canonical_services
            .content
            .resolve_revision_storage_key(state, active_revision.id)
            .await?;
        if active_revision.storage_key.is_none() && resolved_storage_key.is_none() {
            return Err(ApiError::BadRequest(
                "document has no stored source to reprocess".to_string(),
            ));
        }
        build_reprocess_revision_metadata(&active_revision, resolved_storage_key)
    };

    // Force-cancel any inflight ingest for this document before admitting a
    // new reprocess mutation. Without this, a document that is currently
    // stalled (`queue_state='leased'` + stale heartbeat, or mutation stuck in
    // `accepted`/`running` because the worker died mid-pipeline) makes
    // `ensure_document_accepts_new_mutation` raise `ConflictingMutation`, and
    // the batch-reprocess endpoint would silently count this document as
    // "failed" while telling the caller "success". Retry is an explicit user
    // intent — we honor it by terminating the stale mutation canonically.
    state
        .canonical_services
        .content
        .force_reset_inflight_for_retry(state, document_id)
        .await?;
    state
        .canonical_services
        .content
        .admit_mutation(
            state,
            AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "reprocess".to_string(),
                idempotency_key: None,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: Some(reprocess_metadata),
            },
        )
        .await
}
