use std::sync::Arc;
use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode};
use futures::{future::BoxFuture, stream::StreamExt};
use serde::{Deserialize, Serialize};
use tracing::{Instrument, Span, error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ops::{ASYNC_OP_STATUS_FAILED, ASYNC_OP_STATUS_READY},
    infra::repositories::{content_document_repository, ingest_repository, ops_repository},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            AuthorizedContentDocument, POLICY_DOCUMENTS_WRITE, load_content_document_and_authorize,
        },
        router_support::ApiError,
    },
    services::{
        content::service::{
            AdmitMutationCommand, ContentMutationAdmission, PersistedRevisionSourceGuard,
            RevisionAdmission,
        },
        ops::service::CreateAsyncOperationCommand,
    },
    shared::cancellation::{DetachedFinalizerGuard, spawn_detached_awaited},
};

use super::types::{build_reprocess_revision_metadata, build_web_refetch_revision_metadata};

mod delete;
mod reprocess;

use delete::execute_batch_delete;
use reprocess::execute_batch_reprocess;

/// Configured upper bound on `document_ids.len()` for any batch endpoint.
///
/// This is a `DoS` sanity check against an enormous JSON payload, NOT a
/// product limit on how many documents an operator can rerun. The async
/// batch-reprocess path schedules work lazily with `buffer_unordered` so
/// memory usage is bounded by `IRONRAG_BATCH_REPROCESS_PARALLELISM`, not
/// by the size of the id list.
pub(super) const BATCH_MAX_DOCUMENT_IDS: usize = 100_000;

/// Default concurrency for child mutations inside a batch rerun.
/// Respectful throughput limit — not maximum parallelism.
const BATCH_REPROCESS_DEFAULT_PARALLELISM: usize = 4;

/// Default concurrency for child mutations inside a batch delete.
/// Keep this lower than the foreground pool size because each delete
/// briefly holds a document advisory lock and performs follow-up cleanup.
const BATCH_DELETE_DEFAULT_PARALLELISM: usize = 4;

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

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchCancelRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchCancelResponse {
    pub cancelled_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchCancelResult>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchCancelResult {
    pub document_id: Uuid,
    pub jobs_cancelled: u64,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchReprocessRequest {
    pub document_ids: Vec<Uuid>,
}

/// Identity and bounds of one detached batch document fan-out.
pub struct BatchDocumentOperation {
    pub parent_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub principal_id: Uuid,
    pub document_ids: Vec<Uuid>,
    pub parallelism: usize,
}

/// Standard 202 Accepted response for async batch document operations.
///
/// The batch is executed on a background task; callers poll
/// `GET /v1/ops/operations/{batchOperationId}` to observe progress and the
/// final aggregated status. All child per-document mutations are linked back
/// to the parent via `parent_async_operation_id`, so child counts can be
/// aggregated with a single indexed query.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchDocumentOperationAcceptedResponse {
    pub batch_operation_id: Uuid,
    pub total: usize,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
}

#[tracing::instrument(
    level = "info",
    name = "http.batch_delete_documents",
    skip_all,
    fields(document_count = request.document_ids.len(), batch_operation_id)
)]
#[utoipa::path(
    post,
    path = "/v1/content/documents/batch-delete",
    tag = "content",
    operation_id = "batchDeleteContentDocuments",
    request_body = BatchDeleteRequest,
    responses(
        (status = 202, description = "Batch deletion accepted", body = BatchDocumentOperationAcceptedResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized"),
    ),
)]
pub async fn batch_delete_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchDeleteRequest>,
) -> Result<(StatusCode, Json<BatchDocumentOperationAcceptedResponse>), ApiError> {
    let span = tracing::Span::current();
    ensure_batch_document_id_limit(request.document_ids.len())?;

    let document_ids: Vec<Uuid> = request.document_ids.clone();
    let (workspace_id, library_id) =
        resolve_single_library_for_documents(&auth, &state, &document_ids).await?;

    let parent_operation = state
        .application_services
        .ops
        .create_async_operation(
            &state,
            CreateAsyncOperationCommand {
                workspace_id,
                library_id: Some(library_id),
                operation_kind: "batch_delete_documents".to_string(),
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
    let parallelism = batch_delete_parallelism();

    spawn_batch_delete_worker(
        state_for_task,
        BatchDocumentOperation {
            parent_id,
            workspace_id,
            library_id,
            principal_id,
            document_ids,
            parallelism,
        },
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(BatchDocumentOperationAcceptedResponse {
            batch_operation_id: parent_id,
            total,
            library_id,
            workspace_id,
        }),
    ))
}

#[tracing::instrument(
    level = "info",
    name = "http.batch_cancel_documents",
    skip_all,
    fields(document_count = request.document_ids.len(), cancelled_count, failed_count)
)]
#[utoipa::path(
    post,
    path = "/v1/content/documents/batch-cancel",
    tag = "content",
    operation_id = "batchCancelContentDocuments",
    request_body = BatchCancelRequest,
    responses(
        (status = 200, description = "Batch cancel result", body = BatchCancelResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized"),
    ),
)]
pub async fn batch_cancel_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchCancelRequest>,
) -> Result<Json<BatchCancelResponse>, ApiError> {
    let span = tracing::Span::current();
    ensure_batch_document_id_limit(request.document_ids.len())?;

    // IRG-203 SITE-4: the cancel fan-out mutates one document at a time. Run in
    // the caller future, an abort mid-fan-out cancelled some documents and
    // silently dropped the rest. Detach it; awaiting the handle keeps the reply
    // shape, while an abort detaches so every selected document is cancelled.
    //
    // The detached task owns a parent `ops_async_operation`: it creates it
    // before the fan-out and finalizes it (`ready`/`failed`) when the fan-out
    // returns, so an abort cannot leave untracked in-flight work. The row is
    // created inside the detached task, not by the caller — a row created by
    // the caller and then stranded by a caller abort would be the very class
    // bug this fixes. Its scope comes from the first selected document the
    // caller is authorized to write; a selection where no document authorizes
    // mutates nothing, so it creates no parent. The response shape is
    // deliberately unchanged: the operation is a tracking parent.
    const BATCH_CONCURRENCY: usize = 8;
    let auth_for_task = auth.clone();
    let state_for_task = state.clone();
    let document_ids = request.document_ids;

    let results: Vec<BatchCancelResult> = spawn_detached_awaited(
        Box::pin(async move {
            let parent_id = match first_authorized_document_scope(
                &auth_for_task,
                &state_for_task,
                &document_ids,
            )
            .await
            {
                Some((workspace_id, library_id)) => {
                    match state_for_task
                        .application_services
                        .ops
                        .create_library_fan_out_operation(
                            &state_for_task,
                            workspace_id,
                            library_id,
                            Some(auth_for_task.principal_id),
                            "batch_cancel_documents",
                        )
                        .await
                    {
                        Ok(operation) => Some(operation.id),
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                "failed to create batch cancel parent operation"
                            );
                            None
                        }
                    }
                }
                None => None,
            };
            // The abort guard is the abnormal-exit finalizer for the parent the
            // detached task created; a caller abort detaches the task, and a
            // runtime abort during shutdown drops it before the normal settle.
            let mut finalizer = parent_id.map(|parent_id| {
                fan_out_parent_abort_guard(&state_for_task, parent_id)
            });

            let results: Vec<BatchCancelResult> = futures::stream::iter(document_ids)
                .map(|document_id| {
                    let auth = &auth_for_task;
                    let state = &state_for_task;
                    async move {
                        let result = cancel_document_jobs(auth, state, document_id).await;
                        // One-shot probe: the first completion consumes it, so
                        // this pauses after the first cancel only. Compiled out
                        // of production builds.
                        #[cfg(feature = "test-support")]
                        {
                            crate::shared::cancellation::one_shot_pause_probe::pause()
                                .await;
                        }
                        result
                    }
                })
                .buffer_unordered(BATCH_CONCURRENCY)
                .collect()
                .await;

            if let Some(parent_id) = parent_id {
                let failed_count = results.iter().filter(|row| !row.success).count();
                // The cancel fan-out cancels ingest jobs, not child
                // `ops_async_operation` rows, so there is nothing to aggregate:
                // the observed fan-out outcome is the aggregate's input
                // (`childless_status`). The settle still owns the write.
                let (childless_status, fallback_failure_code) =
                    batch_cancel_parent_terminal(failed_count, results.len());
                match state_for_task
                    .application_services
                    .ops
                    .settle_fan_out_parent_from_children(
                        &state_for_task,
                        parent_id,
                        crate::services::ops::service::FanOutParentSettle {
                            fallback_failure_code: fallback_failure_code.as_deref(),
                            childless_status: Some(childless_status),
                            detail: None,
                            guard: ops_repository::FanOutSettleGuard::InFlight,
                        },
                    )
                    .await
                {
                    Ok(_) => {
                        if let Some(finalizer) = finalizer.as_mut() {
                            finalizer.disarm();
                        }
                    }
                    Err(error) => error!(
                        %parent_id,
                        error = %error,
                        "failed to settle batch cancel parent operation; the abort guard will mark it failed"
                    ),
                }
            }

            results
        }),
        span.clone(),
    )
    .await
    .map_err(|join_error| {
        tracing::error!(error = %join_error, "detached batch cancel task did not join");
        ApiError::internal_with_log(join_error, "detached batch cancel task join failed")
    })?;

    let cancelled_count = results.iter().filter(|row| row.success).count();
    let failed_count = results.len() - cancelled_count;

    span.record("cancelled_count", cancelled_count);
    span.record("failed_count", failed_count);
    Ok(Json(BatchCancelResponse { cancelled_count, failed_count, results }))
}

/// Resolves the workspace/library a batch-cancel parent operation is scoped to
/// from the first selected document the caller is authorized to write.
///
/// Returns `None` when no selected document authorizes: the fan-out then
/// mutates nothing, so there is no in-flight work to track and no parent row
/// to create.
async fn first_authorized_document_scope(
    auth: &AuthContext,
    state: &AppState,
    document_ids: &[Uuid],
) -> Option<(Uuid, Uuid)> {
    for document_id in document_ids {
        if let Ok(document) =
            load_content_document_and_authorize(auth, state, *document_id, POLICY_DOCUMENTS_WRITE)
                .await
        {
            return Some((document.workspace_id, document.library_id));
        }
    }
    None
}

/// Terminal `(status, failure_code)` for a batch-cancel tracking parent: ready
/// only when no selected document failed; a partial cancel is not a success.
fn batch_cancel_parent_terminal(
    failed_count: usize,
    total: usize,
) -> (&'static str, Option<String>) {
    if failed_count == 0 {
        (ASYNC_OP_STATUS_READY, None)
    } else {
        (ASYNC_OP_STATUS_FAILED, Some(format!("batch_cancel_failed:{failed_count}/{total}")))
    }
}

/// Cancels every ingest job attached to one document, authorizing the caller
/// first. Extracted from the fan-out so both the detached task and any future
/// caller share one implementation.
async fn cancel_document_jobs(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
) -> BatchCancelResult {
    match load_content_document_and_authorize(auth, state, document_id, POLICY_DOCUMENTS_WRITE)
        .await
    {
        Ok(_) => match ingest_repository::cancel_jobs_for_document(
            &state.persistence.postgres,
            document_id,
            ingest_repository::CancelDisposition::CANCELED_BY_REQUEST,
        )
        .await
        {
            Ok(jobs_cancelled) => {
                BatchCancelResult { document_id, jobs_cancelled, success: true, error: None }
            }
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
}

/// Shared async batch-reprocess handler.
///
/// Accepts an arbitrary list of documents, creates one **parent** `ops_async_operation`
/// row immediately, and spawns a background task that admits one child reprocess
/// mutation per document (linked back to the parent). The handler returns
/// `202 Accepted` the moment the parent row is persisted — callers poll the
/// parent operation via `GET /v1/ops/operations/{id}` to observe progress.
///
/// This is the shared mechanism for ANY future batch endpoint that needs
/// to fan out to many per-subject mutations. No dedicated `batch_job` table
/// exists; the parent/child async-op graph IS the tracking state.
#[tracing::instrument(
    level = "info",
    name = "http.batch_reprocess_documents",
    skip_all,
    fields(document_count = request.document_ids.len(), batch_operation_id)
)]
#[utoipa::path(
    post,
    path = "/v1/content/documents/batch-reprocess",
    tag = "content",
    operation_id = "batchReprocessContentDocuments",
    request_body = BatchReprocessRequest,
    responses(
        (status = 202, description = "Batch reprocess accepted", body = BatchDocumentOperationAcceptedResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized"),
    ),
)]
pub async fn batch_reprocess_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchReprocessRequest>,
) -> Result<(StatusCode, Json<BatchDocumentOperationAcceptedResponse>), ApiError> {
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
        .application_services
        .ops
        .create_async_operation(
            &state,
            CreateAsyncOperationCommand {
                workspace_id,
                library_id: Some(library_id),
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

    spawn_batch_reprocess_worker(
        state_for_task,
        BatchDocumentOperation {
            parent_id,
            workspace_id,
            library_id,
            principal_id,
            document_ids,
            parallelism,
        },
        timeout,
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(BatchDocumentOperationAcceptedResponse {
            batch_operation_id: parent_id,
            total,
            library_id,
            workspace_id,
        }),
    ))
}

/// Spawns the batch-delete worker with the abnormal-exit finalizer armed.
///
/// The worker is fully detached — the handler returns `202` without awaiting
/// it — and it is the only writer of its parent's terminal state. If the task is
/// dropped before that write, [`fan_out_parent_abort_guard`] owns it. The worker
/// disarms the guard by returning `true` once the parent has settled.
fn spawn_batch_delete_worker(
    state: AppState,
    operation: BatchDocumentOperation,
) -> tokio::task::JoinHandle<()> {
    let total = operation.document_ids.len();
    let parent_id = operation.parent_id;
    let parallelism = operation.parallelism;
    tokio::spawn(
        async move {
            let mut finalizer = fan_out_parent_abort_guard(&state, operation.parent_id);
            if execute_batch_delete(state, operation).await {
                finalizer.disarm();
            }
        }
        .instrument(tracing::info_span!(
            "batch_delete_documents.worker",
            batch_operation_id = %parent_id,
            total,
            parallelism
        )),
    )
}

/// Test seam: spawns the batch-delete worker exactly as the handler does and
/// returns its `JoinHandle`, so a test can abort the detached task and prove the
/// abnormal-exit guard settles the parent instead of stranding it.
#[cfg(feature = "test-support")]
pub fn spawn_batch_delete_worker_for_test(
    state: AppState,
    operation: BatchDocumentOperation,
) -> tokio::task::JoinHandle<()> {
    spawn_batch_delete_worker(state, operation)
}

/// Spawns the batch-reprocess worker with the abnormal-exit finalizer armed.
///
/// Same contract as [`spawn_batch_delete_worker`]: the worker settles its own
/// parent and disarms the guard; an aborted worker leaves the guard to derive
/// or force the terminal state.
fn spawn_batch_reprocess_worker(
    state: AppState,
    operation: BatchDocumentOperation,
    timeout: Duration,
) -> tokio::task::JoinHandle<()> {
    let total = operation.document_ids.len();
    let parent_id = operation.parent_id;
    let parallelism = operation.parallelism;
    tokio::spawn(
        async move {
            let mut finalizer = fan_out_parent_abort_guard(&state, operation.parent_id);
            if execute_batch_reprocess(state, operation, timeout).await {
                finalizer.disarm();
            }
        }
        .instrument(tracing::info_span!(
            "batch_reprocess_documents.worker",
            batch_operation_id = %parent_id,
            total,
            parallelism
        )),
    )
}

/// Abnormal-exit finalizer for a detached fan-out worker.
///
/// A batch worker is the only writer of its parent's terminal state, and no
/// other pass settles a `library`-subject fan-out parent
/// (`cancel_jobs_for_document` finalizes only `content_mutation` subjects). If
/// the task is dropped before it settles — the runtime aborting it on shutdown,
/// or a panic — the parent would stay `processing` forever. The guard settles
/// it through the shared aggregate: derived when the persisted children are all
/// terminal, otherwise force-failed with `batch_worker_aborted` so a poller
/// never sees a hung operation.
fn fan_out_parent_abort_guard(
    state: &AppState,
    parent_id: Uuid,
) -> DetachedFinalizerGuard<impl FnOnce() + Send + 'static> {
    let state = state.clone();
    DetachedFinalizerGuard::new(move || {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!(
                %parent_id,
                "fan-out worker aborted with no runtime to finalize its parent operation"
            );
            return;
        };
        runtime.spawn(async move {
            match state
                .application_services
                .ops
                .settle_fan_out_parent_from_children(
                    &state,
                    parent_id,
                    crate::services::ops::service::FanOutParentSettle {
                        fallback_failure_code: Some("batch_worker_aborted"),
                        childless_status: None,
                        detail: None,
                        guard: ops_repository::FanOutSettleGuard::InFlight,
                    },
                )
                .await
            {
                Ok(Some(settled)) => info!(
                    %parent_id,
                    status = %settled.status.as_str(),
                    "aborted fan-out parent settled"
                ),
                Ok(None) => {}
                Err(error) => error!(
                    %parent_id,
                    error = %error,
                    "abort guard failed to settle fan-out parent operation"
                ),
            }
        });
    })
}

fn batch_delete_parallelism() -> usize {
    std::env::var("IRONRAG_BATCH_DELETE_PARALLELISM")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(BATCH_DELETE_DEFAULT_PARALLELISM)
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

/// Loads the persisted document rows for the request and asserts that all
/// of them belong to the same library (and, therefore, the same workspace).
/// Callers expect a `BadRequest` when this invariant is violated so the UI
/// can surface a clear message instead of a silent per-doc failure fan-out.
async fn resolve_single_library_for_documents(
    auth: &AuthContext,
    state: &AppState,
    document_ids: &[Uuid],
) -> Result<(Uuid, Uuid), ApiError> {
    // Per-document auth + existence check via the shared authorizer. The
    // cost is bounded: this runs on the foreground path once per batch and
    // each call is a small indexed lookup, not a full pipeline.
    let mut library_id: Option<Uuid> = None;
    let mut workspace_id: Option<Uuid> = None;
    for document_id in document_ids {
        let document: AuthorizedContentDocument =
            load_content_document_and_authorize(auth, state, *document_id, POLICY_DOCUMENTS_WRITE)
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
                    "batch operation requires every document to belong to the same library"
                        .to_string(),
                ));
            }
        }
    }
    match (library_id, workspace_id) {
        (Some(library_id), Some(workspace_id)) => Ok((workspace_id, library_id)),
        _ => Err(ApiError::BadRequest("documentIds must not be empty".to_string())),
    }
}

/// Retry unit of work for a single document.
///
/// Used by BOTH the single-document `/content/documents/{id}/reprocess`
/// endpoint and the batch-reprocess fan-out. `parent_id` is `Some` only
/// when the retry is a child of a batch parent `async_operation`; for
/// direct single-document retries it stays `None`.
///
/// This function is also the path that handles failed documents whose
/// head was never promoted — it delegates to `resolve_reprocess_revision`
/// which falls back to the latest revision row when the head is `NULL`,
/// and to `force_reset_inflight_for_retry` so an earlier stuck mutation
/// doesn't block the new admission.
pub async fn reprocess_single_document(
    state: &AppState,
    parent_id: Option<Uuid>,
    principal_id: Uuid,
    idempotency_key: Option<String>,
    document_id: Uuid,
) -> Result<ContentMutationAdmission, ApiError> {
    reprocess_single_document_with_hook(
        state,
        document_id,
        ReprocessAdmissionContext { parent_id, principal_id, idempotency_key },
        None,
    )
    .await
}

/// Test seam: as [`reprocess_single_document`], but pauses after
/// `force_reset_inflight_for_retry`'s first commit and before its reconcile, so
/// a test can abort the caller inside the non-terminal window deterministically.
#[cfg(feature = "test-support")]
pub async fn reprocess_single_document_after_reset_cancel_for_test<F>(
    state: &AppState,
    parent_id: Option<Uuid>,
    principal_id: Uuid,
    idempotency_key: Option<String>,
    document_id: Uuid,
    after_reset_cancel: F,
) -> Result<ContentMutationAdmission, ApiError>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    reprocess_single_document_with_hook(
        state,
        document_id,
        ReprocessAdmissionContext { parent_id, principal_id, idempotency_key },
        Some(Box::pin(after_reset_cancel)),
    )
    .await
}

struct ReprocessAdmissionContext {
    parent_id: Option<Uuid>,
    principal_id: Uuid,
    idempotency_key: Option<String>,
}

async fn reprocess_single_document_with_hook(
    state: &AppState,
    document_id: Uuid,
    admission: ReprocessAdmissionContext,
    after_reset_cancel: Option<BoxFuture<'static, ()>>,
) -> Result<ContentMutationAdmission, ApiError> {
    let ReprocessAdmissionContext { parent_id, principal_id, idempotency_key } = admission;
    let document =
        content_document_repository::get_document_by_id(&state.persistence.postgres, document_id)
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
        state.application_services.content.resolve_reprocess_revision(state, document_id).await?;

    // Web-captured documents: retry means "go back to the site and pull the
    // current version", not "re-parse the same captured bytes". We re-fetch
    // the `source_uri`, persist a fresh snapshot under a new storage_key, and
    // build the reprocess metadata around the new blob.
    // A derived reprocess source may have just written a new blob, and the web
    // refetch path writes one too; keep each compensating guard alive until
    // `admit_mutation` below commits a revision that references it. The guard
    // owns the per-library storage lock through that commit.
    let staged_blob_guard: Option<PersistedRevisionSourceGuard>;
    let reprocess_metadata = if active_revision.content_source_kind == "web_page" {
        let source_uri = active_revision.source_uri.as_deref().ok_or_else(|| {
            ApiError::BadRequest("web-captured document has no source_uri to re-fetch".to_string())
        })?;
        let (refetched, refetched_guard) = state
            .application_services
            .web_ingest
            .refetch_document_source(state, document.workspace_id, document.library_id, source_uri)
            .await?;
        staged_blob_guard = Some(refetched_guard);
        build_web_refetch_revision_metadata(&active_revision, refetched)
    } else {
        let resolved = state
            .application_services
            .content
            .resolve_reprocess_revision_source(state, &active_revision)
            .await?;
        let Some(resolved) = resolved else {
            return Err(ApiError::BadRequest(
                "document has no stored source to reprocess".to_string(),
            ));
        };
        staged_blob_guard = resolved.guard;
        build_reprocess_revision_metadata(&active_revision, resolved.source)
    };

    // IRG-203 SITE-2: the reset+admit sequence commits across several awaits
    // (`cancel_jobs_for_document`, then the mutation reconciliation, then
    // admission). Run in the caller future, a client disconnect between those
    // awaits cancelled it and left the head mutation `accepted|running` with its
    // jobs already `canceled` — a non-terminal leftover. Detach it; awaiting the
    // handle keeps the reply shape, while an abort detaches the work so it
    // reaches a terminal state.
    let state_for_task = state.clone();
    let workspace_id = document.workspace_id;
    let library_id = document.library_id;
    let span = tracing::Span::current();
    spawn_detached_awaited(
        Box::pin(run_reset_and_admit(ResetAndAdmitInput {
            state: state_for_task,
            parent_id,
            principal_id,
            idempotency_key,
            document_id,
            workspace_id,
            library_id,
            reprocess_metadata,
            staged_blob_guard,
            after_reset_cancel,
        })),
        span,
    )
    .await
    .map_err(|join_error| {
        // A `JoinError` means the detached task panicked or the runtime aborted
        // it, never that this caller was dropped: the latter detaches it.
        tracing::error!(
            %document_id,
            error = %join_error,
            "detached reprocess reset+admit task did not join"
        );
        ApiError::internal_with_log(join_error, "detached reprocess reset+admit task join failed")
    })?
}

/// Runs the reset+admit half of a reprocess inside the detached task.
///
/// The [`DetachedFinalizerGuard`] is the abnormal-exit finalizer: if this task
/// unwinds before admission completes — a panic, or the runtime aborting it
/// during shutdown — the guard best-effort re-runs `force_reset_inflight_for_retry`
/// so a leased/accepted mutation cannot survive. It is disarmed the instant
/// admission returns, with no await in between, so the normal success path never
/// triggers it.
/// Inputs for the detached reprocess reset+admit task.
///
/// Grouping them keeps the ten positional arguments from being swapped at the
/// single call site and lets the task carry one payload.
struct ResetAndAdmitInput {
    state: AppState,
    parent_id: Option<Uuid>,
    principal_id: Uuid,
    idempotency_key: Option<String>,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    reprocess_metadata: RevisionAdmission,
    staged_blob_guard: Option<PersistedRevisionSourceGuard>,
    after_reset_cancel: Option<BoxFuture<'static, ()>>,
}

async fn run_reset_and_admit(
    input: ResetAndAdmitInput,
) -> Result<ContentMutationAdmission, ApiError> {
    let ResetAndAdmitInput {
        state,
        parent_id,
        principal_id,
        idempotency_key,
        document_id,
        workspace_id,
        library_id,
        reprocess_metadata,
        staged_blob_guard,
        after_reset_cancel,
    } = input;
    let mut finalizer = DetachedFinalizerGuard::new({
        let state = state.clone();
        move || {
            // Abnormal exit (panic, or the runtime aborting this detached task
            // during shutdown) before admission completes. `parent_id` is the
            // batch parent when this retry is a child; the child async operation
            // is only created inside `admit_mutation`, so the document id and
            // this parent id are the only identifiers available this early.
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                warn!(
                    %document_id,
                    parent_operation_id = ?parent_id,
                    "reprocess reset+admit aborted with no runtime to finalize; the stale-lease reaper will recover it",
                );
                return;
            };
            runtime.spawn(async move {
                match state
                    .application_services
                    .content
                    .force_reset_inflight_for_retry(&state, document_id)
                    .await
                {
                    Ok(()) => warn!(
                        %document_id,
                        parent_operation_id = ?parent_id,
                        "reprocess reset+admit task exited early; the abort guard re-ran the reset",
                    ),
                    Err(error) => error!(
                        %document_id,
                        parent_operation_id = ?parent_id,
                        error = %error,
                        "reprocess reset+admit abort guard failed to re-run the reset; the stale-lease reaper must recover it",
                    ),
                }
            });
        }
    });

    // Force-cancel any inflight ingest for this document before admitting a
    // new reprocess mutation. See single-document reprocess handler for the
    // reasoning — a stalled mutation would otherwise raise
    // `ConflictingMutation` and the child count would lie to the operator.
    state
        .application_services
        .content
        .force_reset_inflight_for_retry_inner(&state, document_id, async move {
            if let Some(after_reset_cancel) = after_reset_cancel {
                after_reset_cancel.await;
            }
        })
        .await?;

    let admission = state
        .application_services
        .content
        .admit_mutation(
            &state,
            AdmitMutationCommand {
                workspace_id,
                library_id,
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
        .await?;
    // IRG-203 D8: disarm the abnormal-exit finalizer as the FIRST instruction
    // after the admission commit returns. `admit_mutation` above is the commit;
    // there is no `.await` between its `?` and this call, so the task cannot be
    // aborted in a window that would re-run the reset against the mutation just
    // committed. The staged-blob guard is disarmed immediately after, likewise
    // without an intervening await. On the error path the `?` returns with the
    // finalizer still armed, so the guard runs as intended.
    finalizer.disarm();
    // Admission committed a revision that references the staged blob.
    if let Some(mut guard) = staged_blob_guard {
        guard.disarm();
    }
    Ok(admission)
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn batch_cancel_parent_terminal_is_ready_only_when_every_document_succeeds() {
        assert_eq!(batch_cancel_parent_terminal(0, 3), (ASYNC_OP_STATUS_READY, None));
        assert_eq!(
            batch_cancel_parent_terminal(1, 3),
            (ASYNC_OP_STATUS_FAILED, Some("batch_cancel_failed:1/3".to_string()))
        );
    }
}
