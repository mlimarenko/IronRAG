use std::sync::Arc;
use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode};
use chrono::Utc;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{Instrument, Span, error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{content_repository, ingest_repository},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            AuthorizedContentDocument, POLICY_DOCUMENTS_WRITE,
            load_canonical_content_document_and_authorize,
        },
        router_support::ApiError,
    },
    services::{
        content::service::{AdmitMutationCommand, ContentMutationAdmission},
        ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    },
};

use super::types::{build_reprocess_revision_metadata, build_web_refetch_revision_metadata};

/// Canonical upper bound on `document_ids.len()` for any batch endpoint.
///
/// This is a DoS sanity check against an enormous JSON payload, NOT a
/// product limit on how many documents an operator can rerun. The async
/// batch-reprocess path schedules work lazily with `buffer_unordered` so
/// memory usage is bounded by `IRONRAG_BATCH_REPROCESS_PARALLELISM`, not
/// by the size of the id list.
pub(super) const BATCH_MAX_DOCUMENT_IDS: usize = 100_000;

/// Default concurrency for child mutations inside a batch rerun.
/// Respectful throughput limit — not maximum parallelism.
const BATCH_REPROCESS_DEFAULT_PARALLELISM: usize = 4;

/// Default total wall-clock budget for a batch rerun. The parent
/// `ops_async_operation` is force-failed with `batch_timeout` when exceeded.
const BATCH_REPROCESS_DEFAULT_TIMEOUT_SECS: u64 = 60 * 60;

pub(super) fn ensure_batch_document_id_limit(document_count: usize) -> Result<(), ApiError> {
    if document_count == 0 {
        return Err(ApiError::BadRequest("documentIds must not be empty".to_string()));
    }
    if document_count > BATCH_MAX_DOCUMENT_IDS {
        return Err(ApiError::BadRequest(format!(
            "batch size exceeds maximum of {BATCH_MAX_DOCUMENT_IDS} document ids"
        )));
    }
    Ok(())
}

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

/// Canonical 202 Accepted response for `POST /content/documents/batch-reprocess`.
///
/// The batch is executed on a background task; callers poll
/// `GET /v1/ops/operations/{batchOperationId}` to observe progress and the
/// final aggregated status. All child per-document mutations are linked back
/// to the parent via `parent_async_operation_id`, so child counts can be
/// aggregated with a single indexed query.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchReprocessAcceptedResponse {
    pub batch_operation_id: Uuid,
    pub total: usize,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
}

#[tracing::instrument(
    level = "info",
    name = "http.batch_delete_documents",
    skip_all,
    fields(document_count = request.document_ids.len(), deleted_count, failed_count)
)]
pub(super) async fn batch_delete_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchDeleteRequest>,
) -> Result<Json<BatchDeleteResponse>, ApiError> {
    let span = tracing::Span::current();
    ensure_batch_document_id_limit(request.document_ids.len())?;

    // Fan out per-document authorize + admit_mutation with bounded
    // concurrency. The previous sequential loop turned a 100-document
    // batch into 200+ serialized DB round-trips; for the canonical
    // fix we keep the same per-document authorization semantics (one
    // bad doc can't leak info about others) but run them in parallel.
    // `buffer_unordered` caps concurrency so we don't flood the pool.
    const BATCH_CONCURRENCY: usize = 8;
    let auth_ref = &auth;
    let state_ref = &state;
    let results: Vec<BatchDeleteResult> =
        futures::stream::iter(request.document_ids.iter().copied())
            .map(|document_id| async move {
                match load_canonical_content_document_and_authorize(
                    auth_ref,
                    state_ref,
                    document_id,
                    POLICY_DOCUMENTS_WRITE,
                )
                .await
                {
                    Ok(document) => {
                        match state_ref
                            .canonical_services
                            .content
                            .admit_mutation(
                                state_ref,
                                AdmitMutationCommand {
                                    workspace_id: document.workspace_id,
                                    library_id: document.library_id,
                                    document_id,
                                    operation_kind: "delete".to_string(),
                                    idempotency_key: None,
                                    requested_by_principal_id: Some(auth_ref.principal_id),
                                    request_surface: "rest".to_string(),
                                    source_identity: None,
                                    revision: None,
                                    parent_async_operation_id: None,
                                },
                            )
                            .await
                        {
                            Ok(_) => BatchDeleteResult { document_id, success: true, error: None },
                            Err(error) => BatchDeleteResult {
                                document_id,
                                success: false,
                                error: Some(error.to_string()),
                            },
                        }
                    }
                    Err(error) => BatchDeleteResult {
                        document_id,
                        success: false,
                        error: Some(error.to_string()),
                    },
                }
            })
            .buffer_unordered(BATCH_CONCURRENCY)
            .collect()
            .await;

    let deleted_count = results.iter().filter(|row| row.success).count();
    let failed_count = results.len() - deleted_count;

    span.record("deleted_count", deleted_count);
    span.record("failed_count", failed_count);
    Ok(Json(BatchDeleteResponse { deleted_count, failed_count, results }))
}

#[tracing::instrument(
    level = "info",
    name = "http.batch_cancel_documents",
    skip_all,
    fields(document_count = request.document_ids.len(), cancelled_count, failed_count)
)]
pub(super) async fn batch_cancel_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchCancelRequest>,
) -> Result<Json<BatchCancelResponse>, ApiError> {
    let span = tracing::Span::current();
    ensure_batch_document_id_limit(request.document_ids.len())?;

    const BATCH_CONCURRENCY: usize = 8;
    let auth_ref = &auth;
    let state_ref = &state;
    let results: Vec<BatchCancelResult> =
        futures::stream::iter(request.document_ids.iter().copied())
            .map(|document_id| async move {
                match load_canonical_content_document_and_authorize(
                    auth_ref,
                    state_ref,
                    document_id,
                    POLICY_DOCUMENTS_WRITE,
                )
                .await
                {
                    Ok(_) => match ingest_repository::cancel_jobs_for_document(
                        &state_ref.persistence.postgres,
                        document_id,
                    )
                    .await
                    {
                        Ok(jobs_cancelled) => BatchCancelResult {
                            document_id,
                            jobs_cancelled,
                            success: true,
                            error: None,
                        },
                        Err(error) => BatchCancelResult {
                            document_id,
                            jobs_cancelled: 0,
                            success: false,
                            error: Some(error.to_string()),
                        },
                    },
                    Err(error) => BatchCancelResult {
                        document_id,
                        jobs_cancelled: 0,
                        success: false,
                        error: Some(error.to_string()),
                    },
                }
            })
            .buffer_unordered(BATCH_CONCURRENCY)
            .collect()
            .await;

    let cancelled_count = results.iter().filter(|row| row.success).count();
    let failed_count = results.len() - cancelled_count;

    span.record("cancelled_count", cancelled_count);
    span.record("failed_count", failed_count);
    Ok(Json(BatchCancelResponse { cancelled_count, failed_count, results }))
}

/// Canonical async batch-reprocess handler.
///
/// Accepts an arbitrary list of documents, creates one **parent** `ops_async_operation`
/// row immediately, and spawns a background task that admits one child reprocess
/// mutation per document (linked back to the parent). The handler returns
/// `202 Accepted` the moment the parent row is persisted — callers poll the
/// parent operation via `GET /v1/ops/operations/{id}` to observe progress.
///
/// This is the canonical mechanism for ANY future batch endpoint that needs
/// to fan out to many per-subject mutations. No dedicated `batch_job` table
/// exists; the parent/child async-op graph IS the tracking state.
#[tracing::instrument(
    level = "info",
    name = "http.batch_reprocess_documents",
    skip_all,
    fields(document_count = request.document_ids.len(), batch_operation_id)
)]
pub(super) async fn batch_reprocess_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchReprocessRequest>,
) -> Result<(StatusCode, Json<BatchReprocessAcceptedResponse>), ApiError> {
    ensure_batch_document_id_limit(request.document_ids.len())?;
    let span = Span::current();

    // Resolve and authorize the library. A batch rerun targets ONE library;
    // heterogeneous batches are rejected upfront so the parent async_op
    // can be scoped to a single library/workspace. Per-document authorization
    // is still repeated inside the spawned task for defense-in-depth.
    let document_ids: Vec<Uuid> = request.document_ids.clone();
    let (workspace_id, library_id) =
        resolve_single_library_for_documents(&auth, &state, &document_ids).await?;

    let parent_operation = state
        .canonical_services
        .ops
        .create_async_operation(
            &state,
            CreateAsyncOperationCommand {
                workspace_id,
                library_id,
                operation_kind: "batch_reprocess_documents".to_string(),
                surface_kind: "rest".to_string(),
                requested_by_principal_id: Some(auth.principal_id),
                status: "processing".to_string(),
                subject_kind: "library".to_string(),
                subject_id: Some(library_id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await?;
    let parent_id = parent_operation.id;
    span.record("batch_operation_id", parent_id.to_string().as_str());

    let total = document_ids.len();
    let principal_id = auth.principal_id;
    let state_for_task = state.clone();
    let parallelism = batch_reprocess_parallelism();
    let timeout = batch_reprocess_timeout();

    tokio::spawn(
        async move {
            execute_batch_reprocess(
                state_for_task,
                parent_id,
                workspace_id,
                library_id,
                principal_id,
                document_ids,
                parallelism,
                timeout,
            )
            .await;
        }
        .instrument(tracing::info_span!(
            "batch_reprocess_documents.worker",
            batch_operation_id = %parent_id,
            total,
            parallelism
        )),
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(BatchReprocessAcceptedResponse {
            batch_operation_id: parent_id,
            total,
            library_id,
            workspace_id,
        }),
    ))
}

fn batch_reprocess_parallelism() -> usize {
    std::env::var("IRONRAG_BATCH_REPROCESS_PARALLELISM")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(BATCH_REPROCESS_DEFAULT_PARALLELISM)
}

fn batch_reprocess_timeout() -> Duration {
    let secs = std::env::var("IRONRAG_BATCH_REPROCESS_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(BATCH_REPROCESS_DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Loads the canonical document rows for the request and asserts that all
/// of them belong to the same library (and, therefore, the same workspace).
/// Callers expect a `BadRequest` when this invariant is violated so the UI
/// can surface a clear message instead of a silent per-doc failure fan-out.
async fn resolve_single_library_for_documents(
    auth: &AuthContext,
    state: &AppState,
    document_ids: &[Uuid],
) -> Result<(Uuid, Uuid), ApiError> {
    // Per-document auth + existence check via the canonical authorizer. The
    // cost is bounded: this runs on the foreground path once per batch and
    // each call is a small indexed lookup, not a full pipeline.
    let mut library_id: Option<Uuid> = None;
    let mut workspace_id: Option<Uuid> = None;
    for document_id in document_ids {
        let document: AuthorizedContentDocument = load_canonical_content_document_and_authorize(
            auth,
            state,
            *document_id,
            POLICY_DOCUMENTS_WRITE,
        )
        .await?;
        match (library_id, workspace_id) {
            (None, _) => {
                library_id = Some(document.library_id);
                workspace_id = Some(document.workspace_id);
            }
            (Some(expected_library), Some(expected_workspace))
                if expected_library == document.library_id
                    && expected_workspace == document.workspace_id => {}
            _ => {
                return Err(ApiError::BadRequest(
                    "batch rerun requires every document to belong to the same library".to_string(),
                ));
            }
        }
    }
    match (library_id, workspace_id) {
        (Some(library_id), Some(workspace_id)) => Ok((workspace_id, library_id)),
        _ => Err(ApiError::BadRequest("documentIds must not be empty".to_string())),
    }
}

/// Background executor for the batch rerun. Drives children through
/// `reprocess_single_document`, tracks counts, and settles the parent
/// `ops_async_operation` at the end with either `ready` (all children
/// succeeded) or `failed` (at least one child failed). A whole-batch
/// timeout is enforced via `tokio::time::timeout`; on timeout the parent
/// is marked failed with `batch_timeout`.
#[allow(clippy::too_many_arguments)]
async fn execute_batch_reprocess(
    state: AppState,
    parent_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    principal_id: Uuid,
    document_ids: Vec<Uuid>,
    parallelism: usize,
    timeout: Duration,
) {
    let state = Arc::new(state);
    let total = document_ids.len();
    info!(%parent_id, %library_id, %workspace_id, total, parallelism, "batch reprocess started");

    let worker = async {
        let failure_count = run_batch_reprocess_children(
            state.clone(),
            parent_id,
            principal_id,
            document_ids,
            parallelism,
        )
        .await;

        // Canonical semantics: the parent row tracks the ADMIT phase. It
        // stays in `processing` until every child async_op reaches a
        // terminal state — the GET /ops/operations/{id} endpoint derives
        // the effective final status from the child progress aggregate.
        // We only touch the parent here when the admit phase itself
        // surfaces non-recoverable fan-out errors, i.e. every single doc
        // failed to even enter the pipeline. Mixed / partial admit
        // failures still leave the parent as `processing`; the children
        // that DID admit will settle on their own and the aggregate will
        // reflect both buckets correctly.
        if failure_count == total && total > 0 {
            if let Err(error) = state
                .canonical_services
                .ops
                .update_async_operation(
                    state.as_ref(),
                    UpdateAsyncOperationCommand {
                        operation_id: parent_id,
                        status: "failed".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: Some(format!("admit_failed:{failure_count}/{total}")),
                    },
                )
                .await
            {
                error!(%parent_id, error = %error, "failed to settle batch parent async operation");
            }
        }
        info!(%parent_id, failures = failure_count, total, "batch reprocess admit phase completed");
    };

    match tokio::time::timeout(timeout, worker).await {
        Ok(()) => {}
        Err(_) => {
            warn!(
                %parent_id,
                timeout_secs = timeout.as_secs(),
                "batch reprocess exceeded wall-clock budget; marking parent failed"
            );
            if let Err(error) = state
                .canonical_services
                .ops
                .update_async_operation(
                    state.as_ref(),
                    UpdateAsyncOperationCommand {
                        operation_id: parent_id,
                        status: "failed".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: Some("batch_timeout".to_string()),
                    },
                )
                .await
            {
                error!(%parent_id, error = %error, "failed to mark batch parent as timed out");
            }
        }
    }
}

/// Runs the child mutations in bounded parallel and returns the failure count.
/// Successes and failures are both tracked via child `ops_async_operation`
/// rows created by `admit_mutation`; this function is only responsible for
/// driving the fan-out and surfacing an overall failure tally so the parent
/// can settle.
async fn run_batch_reprocess_children(
    state: Arc<AppState>,
    parent_id: Uuid,
    principal_id: Uuid,
    document_ids: Vec<Uuid>,
    parallelism: usize,
) -> usize {
    use futures::stream;

    let parallelism = parallelism.max(1);
    let failure_count = stream::iter(document_ids.into_iter())
        .map(|document_id| {
            let state = state.clone();
            async move {
                match reprocess_single_document(
                    &state,
                    Some(parent_id),
                    principal_id,
                    None,
                    document_id,
                )
                .await
                {
                    Ok(_) => 0usize,
                    Err(error) => {
                        error!(%parent_id, %document_id, error = %error, "batch child rerun failed");
                        1usize
                    }
                }
            }
        })
        .buffer_unordered(parallelism)
        .collect::<Vec<usize>>()
        .await;
    failure_count.into_iter().sum()
}

/// Canonical retry unit of work for a single document.
///
/// Used by BOTH the single-document `/content/documents/{id}/reprocess`
/// endpoint and the batch-reprocess fan-out. `parent_id` is `Some` only
/// when the retry is a child of a batch parent async_operation; for
/// direct single-document retries it stays `None`.
///
/// This function is also the path that handles failed documents whose
/// head was never promoted — it delegates to `resolve_reprocess_revision`
/// which falls back to the latest revision row when the head is `NULL`,
/// and to `force_reset_inflight_for_retry` so an earlier stuck mutation
/// doesn't block the new admission.
pub(super) async fn reprocess_single_document(
    state: &AppState,
    parent_id: Option<Uuid>,
    principal_id: Uuid,
    idempotency_key: Option<String>,
    document_id: Uuid,
) -> Result<ContentMutationAdmission, ApiError> {
    let document = content_repository::get_document_by_id(&state.persistence.postgres, document_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
    // A previously-tombstoned document (orphan auto-fail, manual delete, or
    // a row left behind by an earlier batch) still has a `content_document`
    // row in Postgres but `document_state='deleted'`. From the retry path's
    // POV there is nothing to do — skip silently (no child mutation, no
    // failure) so stale selections from the UI do not generate noise.
    if document.document_state == "deleted" || document.deleted_at.is_some() {
        return Err(ApiError::resource_not_found("document", document_id));
    }

    let active_revision =
        state.canonical_services.content.resolve_reprocess_revision(state, document_id).await?;

    // Web-captured documents: retry means "go back to the site and pull the
    // current version", not "re-parse the same captured bytes". We re-fetch
    // the `source_uri`, persist a fresh snapshot under a new storage_key, and
    // build the reprocess metadata around the new blob.
    let reprocess_metadata = if active_revision.content_source_kind == "web_page" {
        let source_uri = active_revision.source_uri.as_deref().ok_or_else(|| {
            ApiError::BadRequest("web-captured document has no source_uri to re-fetch".to_string())
        })?;
        let refetched = state
            .canonical_services
            .web_ingest
            .refetch_document_source(state, document.workspace_id, document.library_id, source_uri)
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
    // new reprocess mutation. See single-document reprocess handler for the
    // reasoning — a stalled mutation would otherwise raise
    // `ConflictingMutation` and the child count would lie to the operator.
    state.canonical_services.content.force_reset_inflight_for_retry(state, document_id).await?;
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
                idempotency_key,
                requested_by_principal_id: Some(principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: Some(reprocess_metadata),
                parent_async_operation_id: parent_id,
            },
        )
        .await
}
