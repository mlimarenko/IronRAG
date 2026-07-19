use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ingest,
        knowledge::{KnowledgeLibraryGeneration, KnowledgeLibrarySummary},
        ops::{
            OpsAsyncOperation, OpsAsyncOperationProgress, OpsAsyncOperationStatus, OpsLibraryState,
            OpsLibraryWarning,
        },
    },
    infra::repositories::ingest_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_LIBRARY_WRITE, POLICY_OPERATION_READ, POLICY_USAGE_READ,
            authorize_library_permission, load_async_operation_and_authorize,
            load_library_and_authorize,
        },
        // `list_ingest_queue` + its query/response types moved to the
        // ingest domain (GET /v1/ingest/queue, was GET /v1/ops/ingest-queue)
        // — see the module-level comment in ingestion.rs. Imported back here
        // because the queue *mutation* handlers below (still `ops`-scoped
        // pending a future domain pass) return the refreshed queue by
        // calling straight back into it.
        ingestion::{IngestQueueQuery, IngestQueueResponse, list_ingest_queue},
        router_support::ApiError,
    },
};
use ironrag_contracts::{
    diagnostics::{MessageLevel, OperatorWarning},
    documents::{
        DashboardAttentionItem, DashboardSurface, DocumentSummary, WebIngestRunState,
        WebIngestRunSummary, WebRunCounts,
    },
    graph::{
        GraphConvergenceStatus, GraphGenerationSummary, GraphReadinessSummary, GraphStatus,
        GraphSurface,
    },
};

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpsLibraryStateSummaryResponse {
    pub library_id: Uuid,
    pub queue_depth: i64,
    pub running_attempts: i64,
    pub readable_document_count: i64,
    pub failed_document_count: i64,
    pub degraded_state: String,
    pub latest_knowledge_generation_id: Option<Uuid>,
    pub knowledge_generation_state: Option<String>,
    pub last_recomputed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpsLibraryWarningResponse {
    pub id: Uuid,
    pub library_id: Uuid,
    pub warning_kind: String,
    pub severity: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeGenerationResponse {
    pub id: Uuid,
    pub library_id: Uuid,
    pub generation_kind: String,
    pub generation_state: String,
    pub source_revision_id: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpsLibraryStateResponse {
    pub state: OpsLibraryStateSummaryResponse,
    pub knowledge_generations: Vec<KnowledgeGenerationResponse>,
    pub warnings: Vec<OpsLibraryWarningResponse>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveIngestQueueJobRequest {
    pub direction: IngestQueueMoveDirection,
}

#[derive(Debug, Clone, Copy, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngestQueueMoveDirection {
    Up,
    Down,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngestQueueBulkAction {
    RetryRequeue,
    Pause,
    Resume,
    Cancel,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkIngestQueueActionRequest {
    pub action: IngestQueueBulkAction,
    pub job_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngestQueueBulkResultStatus {
    Applied,
    Skipped,
    Failed,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestQueueBulkResultItem {
    pub job_id: Uuid,
    pub status: IngestQueueBulkResultStatus,
    pub reason_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkIngestQueueActionResponse {
    pub queue: IngestQueueResponse,
    pub results: Vec<IngestQueueBulkResultItem>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ops/operations/{operation_id}", get(get_async_operation))
        .route("/ops/libraries/{library_id}", get(get_library_state))
        .route("/ops/libraries/{library_id}/dashboard", get(get_library_dashboard))
        .route("/ops/ingest-queue/bulk", post(bulk_ingest_queue_action))
        .route("/ops/ingest-queue/jobs/{job_id}/move", post(move_ingest_queue_job))
        .route("/ops/ingest-queue/jobs/{job_id}/retry", post(retry_ingest_queue_job))
        .route("/ops/ingest-queue/jobs/{job_id}/pause", post(pause_ingest_queue_job))
        .route("/ops/ingest-queue/jobs/{job_id}/resume", post(resume_ingest_queue_job))
        .route("/ops/ingest-queue/jobs/{job_id}/cancel", post(cancel_ingest_queue_job))
}

/// Canonical async-operation polling payload. Exposes the raw parent row
/// plus aggregated child-operation counts, so any batch endpoint (batch
/// rerun, batch delete, future batch annotate, …) can be polled via the
/// same response shape. `progress` is populated whenever at least one child
/// operation references this row via `parent_async_operation_id`; for
/// non-batch operations it reports zeros across the board.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AsyncOperationDetailResponse {
    #[serde(flatten)]
    operation: OpsAsyncOperation,
    progress: OpsAsyncOperationProgress,
}

#[utoipa::path(
    get,
    path = "/v1/ops/operations/{operationId}",
    tag = "ops",
    operation_id = "getAsyncOperation",
    params(("operationId" = uuid::Uuid, Path, description = "Async operation identifier")),
    responses(
        (status = 200, description = "Async operation detail with aggregated child progress", body = AsyncOperationDetailResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the operation"),
        (status = 404, description = "Operation not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_async_operation",
    skip_all,
    fields(operation_id = %operation_id)
)]
pub async fn get_async_operation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(operation_id): Path<Uuid>,
) -> Result<Json<AsyncOperationDetailResponse>, ApiError> {
    let _ = load_async_operation_and_authorize(&auth, &state, operation_id, POLICY_OPERATION_READ)
        .await?;
    let mut operation =
        state.canonical_services.ops.get_async_operation(&state, operation_id).await?;
    let progress =
        state.canonical_services.ops.get_async_operation_progress(&state, operation_id).await?;

    // For parent batch ops (children present), the effective status is
    // usually derived from child progress. Batch delete is the exception:
    // after every child delete settles, the parent still runs one library
    // graph projection refresh. Until that parent-owned finalization writes
    // completed_at, reporting `ready` would be a false terminal state.
    if progress.total > 0
        && !matches!(operation.status.as_str(), "failed" | "canceled" | "superseded")
    {
        let pending = progress.total.saturating_sub(progress.completed + progress.failed);
        let parent_finalizing = operation.operation_kind == "batch_delete_documents"
            && operation.completed_at.is_none();
        let derived = if pending > 0 || parent_finalizing {
            OpsAsyncOperationStatus::Processing
        } else if progress.failed > 0 {
            OpsAsyncOperationStatus::Failed
        } else {
            OpsAsyncOperationStatus::Ready
        };
        if operation.status != derived {
            operation.status = derived;
        }
        if pending == 0 && !parent_finalizing && operation.completed_at.is_none() {
            operation.completed_at = Some(chrono::Utc::now());
        }
    }

    Ok(Json(AsyncOperationDetailResponse { operation, progress }))
}

#[utoipa::path(
    get,
    path = "/v1/ops/libraries/{libraryId}",
    tag = "ops",
    operation_id = "getLibraryState",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Library state snapshot with knowledge generations and warnings", body = OpsLibraryStateResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_library_state",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn get_library_state(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<OpsLibraryStateResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let snapshot_with_warnings = state
        .canonical_services
        .ops
        .get_library_state_snapshot_with_warnings(&state, library_id)
        .await?;
    let snapshot = snapshot_with_warnings.snapshot;
    Ok(Json(OpsLibraryStateResponse {
        state: map_ops_library_state(&snapshot.state),
        knowledge_generations: snapshot
            .knowledge_generations
            .iter()
            .map(map_knowledge_generation)
            .collect(),
        warnings: snapshot_with_warnings.warnings.iter().map(map_ops_warning).collect(),
    }))
}

#[utoipa::path(
    post,
    path = "/v1/ops/ingest-queue/jobs/{jobId}/move",
    tag = "ops",
    operation_id = "moveIngestQueueJob",
    params(("jobId" = uuid::Uuid, Path, description = "Queued or paused ingest job identifier")),
    request_body = MoveIngestQueueJobRequest,
    responses(
        (status = 200, description = "Updated active ingest queue", body = IngestQueueResponse),
        (status = 400, description = "Job is not queued/paused or direction is invalid"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot mutate the job's library"),
        (status = 404, description = "Job not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.move_ingest_queue_job", skip_all, fields(job_id = %job_id))]
pub async fn move_ingest_queue_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    Json(payload): Json<MoveIngestQueueJobRequest>,
) -> Result<Json<IngestQueueResponse>, ApiError> {
    let job = state.canonical_services.ingest.get_job(&state, job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_WRITE)?;
    let moved = ingest_repository::move_queued_ingest_job(
        &state.persistence.postgres,
        job_id,
        map_move_direction(payload.direction),
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    if moved.is_none() {
        return Err(ApiError::BadRequest(
            "Only queued or paused jobs can be reordered".to_string(),
        ));
    }
    list_ingest_queue(
        auth,
        State(state),
        Query(IngestQueueQuery { workspace_id: None, library_id: None }),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/v1/ops/ingest-queue/jobs/{jobId}/retry",
    tag = "ops",
    operation_id = "retryIngestQueueJob",
    params(("jobId" = uuid::Uuid, Path, description = "Queued, paused, or stale leased ingest job identifier")),
    responses(
        (status = 200, description = "Updated active ingest queue", body = IngestQueueResponse),
        (status = 400, description = "Job cannot be requeued from the ingest queue"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot mutate the job's library"),
        (status = 404, description = "Job not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.retry_ingest_queue_job", skip_all, fields(job_id = %job_id))]
pub async fn retry_ingest_queue_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<IngestQueueResponse>, ApiError> {
    let job = state.canonical_services.ingest.get_job(&state, job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_WRITE)?;
    state.canonical_services.ingest.retry_job(&state, job_id, None).await?;
    list_ingest_queue(
        auth,
        State(state),
        Query(IngestQueueQuery { workspace_id: None, library_id: None }),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/v1/ops/ingest-queue/bulk",
    tag = "ops",
    operation_id = "bulkIngestQueueAction",
    request_body = BulkIngestQueueActionRequest,
    responses(
        (status = 200, description = "Bulk action result and refreshed active ingest queue", body = BulkIngestQueueActionResponse),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.bulk_ingest_queue_action", skip_all, fields(action = ?payload.action, selected = payload.job_ids.len()))]
pub async fn bulk_ingest_queue_action(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<BulkIngestQueueActionRequest>,
) -> Result<Json<BulkIngestQueueActionResponse>, ApiError> {
    auth.require_any_scope(POLICY_USAGE_READ)?;
    let mut results = Vec::with_capacity(payload.job_ids.len());
    for job_id in payload.job_ids {
        let result = apply_bulk_queue_action(&auth, &state, payload.action, job_id).await;
        results.push(result);
    }
    let queue = list_ingest_queue(
        auth,
        State(state),
        Query(IngestQueueQuery { workspace_id: None, library_id: None }),
    )
    .await?
    .0;
    Ok(Json(BulkIngestQueueActionResponse { queue, results }))
}

async fn apply_bulk_queue_action(
    auth: &AuthContext,
    state: &AppState,
    action: IngestQueueBulkAction,
    job_id: Uuid,
) -> IngestQueueBulkResultItem {
    let job = match state.canonical_services.ingest.get_job(state, job_id).await {
        Ok(job) => job,
        Err(ApiError::NotFound(_)) => {
            return bulk_result(
                job_id,
                IngestQueueBulkResultStatus::Failed,
                Some("not_found"),
                Some("Job was not found".to_string()),
            );
        }
        Err(error) => {
            return bulk_result(
                job_id,
                IngestQueueBulkResultStatus::Failed,
                Some("load_failed"),
                Some(error.to_string()),
            );
        }
    };

    if authorize_library_permission(auth, job.workspace_id, job.library_id, POLICY_LIBRARY_WRITE)
        .is_err()
    {
        return bulk_result(
            job_id,
            IngestQueueBulkResultStatus::Failed,
            Some("forbidden"),
            Some("Caller cannot mutate this job's library".to_string()),
        );
    }

    let action_result = match action {
        IngestQueueBulkAction::RetryRequeue => {
            state.canonical_services.ingest.retry_job(state, job_id, None).await.map(|_| ())
        }
        IngestQueueBulkAction::Pause => {
            state.canonical_services.ingest.pause_job(state, job_id).await
        }
        IngestQueueBulkAction::Resume => {
            state.canonical_services.ingest.resume_job(state, job_id).await
        }
        IngestQueueBulkAction::Cancel => {
            ingest_repository::cancel_ingest_job(&state.persistence.postgres, job_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))
                .and_then(|changed| {
                    if changed == 0 {
                        Err(ApiError::BadRequest(
                            "Only queued, running, or paused jobs can be canceled".to_string(),
                        ))
                    } else {
                        Ok(())
                    }
                })
        }
    };

    match action_result {
        Ok(()) => bulk_result(job_id, IngestQueueBulkResultStatus::Applied, None, None),
        Err(ApiError::BadRequest(message) | ApiError::Conflict(message)) => bulk_result(
            job_id,
            IngestQueueBulkResultStatus::Skipped,
            Some(ineligible_reason_code(action, job.queue_state.as_str())),
            Some(message),
        ),
        Err(error) => bulk_result(
            job_id,
            IngestQueueBulkResultStatus::Failed,
            Some("mutation_failed"),
            Some(error.to_string()),
        ),
    }
}

fn bulk_result(
    job_id: Uuid,
    status: IngestQueueBulkResultStatus,
    reason_code: Option<&str>,
    message: Option<String>,
) -> IngestQueueBulkResultItem {
    IngestQueueBulkResultItem {
        job_id,
        status,
        reason_code: reason_code.map(str::to_string),
        message,
    }
}

fn ineligible_reason_code(action: IngestQueueBulkAction, queue_state: &str) -> &'static str {
    match (action, queue_state) {
        (_, "completed") => "terminal_completed",
        (_, "canceled") => "terminal_canceled",
        (IngestQueueBulkAction::RetryRequeue, "failed") => "terminal_failed",
        (IngestQueueBulkAction::RetryRequeue, "leased") => "lease_not_stale",
        (IngestQueueBulkAction::Pause, _) => "not_pausable",
        (IngestQueueBulkAction::Resume, _) => "not_resumable",
        (IngestQueueBulkAction::Cancel, _) => "not_cancelable",
        _ => "not_eligible",
    }
}

#[utoipa::path(
    post,
    path = "/v1/ops/ingest-queue/jobs/{jobId}/pause",
    tag = "ops",
    operation_id = "pauseIngestQueueJob",
    params(("jobId" = uuid::Uuid, Path, description = "Queued or running ingest job identifier")),
    responses(
        (status = 200, description = "Updated active ingest queue", body = IngestQueueResponse),
        (status = 400, description = "Job is not queued or running"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot mutate the job's library"),
        (status = 404, description = "Job not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.pause_ingest_queue_job", skip_all, fields(job_id = %job_id))]
pub async fn pause_ingest_queue_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<IngestQueueResponse>, ApiError> {
    let job = state.canonical_services.ingest.get_job(&state, job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_WRITE)?;
    state.canonical_services.ingest.pause_job(&state, job_id).await?;
    list_ingest_queue(
        auth,
        State(state),
        Query(IngestQueueQuery { workspace_id: None, library_id: None }),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/v1/ops/ingest-queue/jobs/{jobId}/resume",
    tag = "ops",
    operation_id = "resumeIngestQueueJob",
    params(("jobId" = uuid::Uuid, Path, description = "Paused ingest job identifier")),
    responses(
        (status = 200, description = "Updated active ingest queue", body = IngestQueueResponse),
        (status = 400, description = "Job is not paused"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot mutate the job's library"),
        (status = 404, description = "Job not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.resume_ingest_queue_job", skip_all, fields(job_id = %job_id))]
pub async fn resume_ingest_queue_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<IngestQueueResponse>, ApiError> {
    let job = state.canonical_services.ingest.get_job(&state, job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_WRITE)?;
    state.canonical_services.ingest.resume_job(&state, job_id).await?;
    list_ingest_queue(
        auth,
        State(state),
        Query(IngestQueueQuery { workspace_id: None, library_id: None }),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/v1/ops/ingest-queue/jobs/{jobId}/cancel",
    tag = "ops",
    operation_id = "cancelIngestQueueJob",
    params(("jobId" = uuid::Uuid, Path, description = "Active ingest job identifier")),
    responses(
        (status = 200, description = "Updated active ingest queue", body = IngestQueueResponse),
        (status = 400, description = "Job is already terminal"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot mutate the job's library"),
        (status = 404, description = "Job not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.cancel_ingest_queue_job", skip_all, fields(job_id = %job_id))]
pub async fn cancel_ingest_queue_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<IngestQueueResponse>, ApiError> {
    let job = state.canonical_services.ingest.get_job(&state, job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_WRITE)?;
    let canceled = ingest_repository::cancel_ingest_job(&state.persistence.postgres, job_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    if canceled == 0 {
        return Err(ApiError::BadRequest(
            "Only queued or running jobs can be canceled".to_string(),
        ));
    }
    list_ingest_queue(
        auth,
        State(state),
        Query(IngestQueueQuery { workspace_id: None, library_id: None }),
    )
    .await
}

#[utoipa::path(
    get,
    path = "/v1/ops/libraries/{libraryId}/dashboard",
    tag = "ops",
    operation_id = "getLibraryDashboard",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Library dashboard surface (canonical document metrics, attention items, recent documents, graph, web run summaries)", body = DashboardSurface),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_library_dashboard",
    skip_all,
    fields(library_id = %library_id, elapsed_ms)
)]
pub async fn get_library_dashboard(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<DashboardSurface>, ApiError> {
    let started_at = std::time::Instant::now();
    let span = tracing::Span::current();
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;

    // Canonical bounded fetch — no more `list_documents` enumeration.
    // Top 6 recent entries for the "Recent documents" strip + the
    // aggregate status counts for the dashboard tiles. The old path
    // spent ~7.5 s on a 5k-doc library because it enumerated every
    // document through the 6-call prefetch pipeline for stats that
    // are a single `COUNT(*) FILTER (...)` away.
    let recent_page_command = crate::services::content::service::ListDocumentsPageCommand {
        library_id,
        include_deleted: false,
        cursor: None,
        limit: 6,
        search: None,
        sort: crate::infra::repositories::content_repository::DocumentListSortColumn::CreatedAt,
        sort_desc: true,
        status_filter: Vec::new(),
        id_filter: Vec::new(),
    };
    let (
        recent_page,
        document_metrics,
        recent_web_runs,
        knowledge_summary,
        ops_snapshot_with_warnings,
    ) = tokio::try_join!(
        state.canonical_services.content.list_documents_page(&state, recent_page_command),
        async {
            crate::infra::repositories::content_repository::aggregate_library_document_metrics(
                &state.persistence.postgres,
                library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))
        },
        state.canonical_services.web_ingest.list_runs(&state, library_id, 6),
        state.canonical_services.knowledge.get_library_summary(&state, library_id),
        state.canonical_services.ops.get_library_state_snapshot_with_warnings(&state, library_id),
    )?;
    let ops_warnings = ops_snapshot_with_warnings.warnings;
    let ops_snapshot = ops_snapshot_with_warnings.snapshot;

    let recent_documents: Vec<DocumentSummary> =
        recent_page.items.into_iter().map(map_list_entry_to_dashboard_summary).collect();
    let warnings = map_operator_warnings(&ops_warnings, &ops_snapshot.state);
    let graph = map_graph_surface(&knowledge_summary, &ops_snapshot.state, warnings.first());
    let attention = build_attention_items_bounded(
        &ops_snapshot.state,
        &ops_warnings,
        &graph,
        &recent_documents,
    );
    span.record("elapsed_ms", started_at.elapsed().as_millis() as u64);

    Ok(Json(DashboardSurface {
        document_metrics,
        recent_documents,
        recent_web_runs: recent_web_runs.into_iter().map(map_web_run_summary).collect(),
        graph,
        attention,
        warnings,
    }))
}

/// Builds a `DocumentSummary` for the dashboard "Recent documents" strip
/// from a slim `ContentDocumentListEntry`. Fields that require a per-document
/// knowledge revision fetch are omitted — the dashboard surface does not
/// display them on this card.
fn map_list_entry_to_dashboard_summary(
    entry: crate::services::content::service::ContentDocumentListEntry,
) -> DocumentSummary {
    DocumentSummary {
        id: entry.id,
        workspace_id: Some(entry.workspace_id),
        library_id: Some(entry.library_id),
        file_name: entry.file_name,
        file_type: entry.file_type.unwrap_or_else(|| "unknown".to_string()),
        file_size: entry.file_size.unwrap_or(0),
        uploaded_at: entry.uploaded_at,
        status: entry.status,
        readiness: entry.readiness,
        stage_label: entry.stage,
        progress_percent: entry.progress_percent,
        cost_usd: None,
        failure_message: entry.failure_message.or(entry.failure_code),
        can_retry: entry.retryable,
        prepared_segment_count: None,
        technical_fact_count: None,
        source_format: None,
    }
}

fn build_attention_items_bounded(
    ops_state: &OpsLibraryState,
    warnings: &[OpsLibraryWarning],
    graph: &GraphSurface,
    recent_documents: &[DocumentSummary],
) -> Vec<DashboardAttentionItem> {
    let mut attention = Vec::new();
    let graph_coverage_gap_count = usize::try_from(graph.graph_sparse_document_count).unwrap_or(0);

    if ops_state.failed_document_count > 0 {
        attention.push(DashboardAttentionItem {
            code: "failed_documents".to_string(),
            title: "Failed documents need review".to_string(),
            detail: format!(
                "{} documents are currently failed in the active library.",
                ops_state.failed_document_count
            ),
            route_path: "/documents?status=failed".to_string(),
            level: MessageLevel::Error,
        });
    }

    if graph_coverage_gap_count > 0 {
        attention.push(DashboardAttentionItem {
            code: "graph_coverage_gap".to_string(),
            title: "Graph coverage remains partial".to_string(),
            detail: format!(
                "{graph_coverage_gap_count} readable documents still do not contribute to the graph."
            ),
            route_path: "/graph".to_string(),
            level: MessageLevel::Warning,
        });
    }

    if let Some(document) = recent_documents.iter().find(|document| document.can_retry) {
        attention.push(DashboardAttentionItem {
            code: "retryable_document".to_string(),
            title: "A document can be retried".to_string(),
            detail: format!(
                "{} reported a retryable failure or stalled ingest step.",
                document.file_name
            ),
            route_path: "/documents?status=failed".to_string(),
            level: MessageLevel::Warning,
        });
    }

    attention.extend(warnings.iter().map(map_attention_item));
    attention.sort_by(|left, right| {
        attention_priority(right.level)
            .cmp(&attention_priority(left.level))
            .then_with(|| left.code.cmp(&right.code))
    });
    attention.dedup_by(|left, right| left.code == right.code);
    attention.truncate(6);
    attention
}

fn map_ops_library_state(state: &OpsLibraryState) -> OpsLibraryStateSummaryResponse {
    OpsLibraryStateSummaryResponse {
        library_id: state.library_id,
        queue_depth: state.queue_depth,
        running_attempts: state.running_attempts,
        readable_document_count: state.readable_document_count,
        failed_document_count: state.failed_document_count,
        degraded_state: state.degraded_state.clone(),
        latest_knowledge_generation_id: state.latest_knowledge_generation_id,
        knowledge_generation_state: state.knowledge_generation_state.clone(),
        last_recomputed_at: state.last_recomputed_at,
    }
}

fn map_knowledge_generation(
    generation: &KnowledgeLibraryGeneration,
) -> KnowledgeGenerationResponse {
    KnowledgeGenerationResponse {
        id: generation.id,
        library_id: generation.library_id,
        generation_kind: generation.generation_kind.clone(),
        generation_state: generation.generation_state.clone(),
        source_revision_id: generation.source_revision_id,
        created_at: generation.created_at,
        completed_at: generation.completed_at,
    }
}

fn map_ops_warning(warning: &OpsLibraryWarning) -> OpsLibraryWarningResponse {
    OpsLibraryWarningResponse {
        id: warning.id,
        library_id: warning.library_id,
        warning_kind: warning.warning_kind.clone(),
        severity: warning.severity.clone(),
        created_at: warning.created_at,
        resolved_at: warning.resolved_at,
    }
}

fn map_attention_item(warning: &OpsLibraryWarning) -> DashboardAttentionItem {
    let (title, detail, route_path) = match warning.warning_kind.as_str() {
        "stale_vectors" => (
            "Vector rebuild is still running",
            "Some readable documents have not converged onto current vector state yet.",
            "/documents?status=processing",
        ),
        "stale_relations" => (
            "Graph rebuild is still running",
            "The graph remains behind the readable document set for this library.",
            "/graph",
        ),
        "failed_rebuilds" => (
            "Recent rebuild failed",
            "At least one recent ingestion rebuild failed and needs operator review.",
            "/graph",
        ),
        "bundle_assembly_failures" => (
            "Context bundle assembly failed",
            "Recent bundle assembly failed and downstream graph context may be incomplete.",
            "/graph",
        ),
        _ => (
            "Operator warning",
            "The backend reported a library warning that needs attention.",
            "/documents",
        ),
    };

    DashboardAttentionItem {
        code: warning.warning_kind.clone(),
        title: title.to_string(),
        detail: detail.to_string(),
        route_path: route_path.to_string(),
        level: severity_level(&warning.severity),
    }
}

fn map_operator_warnings(
    warnings: &[OpsLibraryWarning],
    ops_state: &OpsLibraryState,
) -> Vec<OperatorWarning> {
    let mut mapped = warnings
        .iter()
        .map(|warning| OperatorWarning {
            code: warning.warning_kind.clone(),
            level: severity_level(&warning.severity),
            title: humanize_warning_kind(&warning.warning_kind),
            detail: format!(
                "Library {} reported {} at {}.",
                warning.library_id,
                warning.warning_kind.replace('_', " "),
                warning.created_at.to_rfc3339()
            ),
        })
        .collect::<Vec<_>>();

    if ops_state.degraded_state != "healthy" {
        mapped.insert(
            0,
            OperatorWarning {
                code: format!("library_{}", ops_state.degraded_state),
                level: if matches!(
                    ops_state.degraded_state.as_str(),
                    "degraded" | "processing" | "rebuilding"
                ) {
                    MessageLevel::Warning
                } else {
                    MessageLevel::Error
                },
                title: humanize_warning_kind(&format!("library_{}", ops_state.degraded_state)),
                detail: format!(
                    "Queue depth: {}. Running attempts: {}. Failed documents: {}.",
                    ops_state.queue_depth,
                    ops_state.running_attempts,
                    ops_state.failed_document_count
                ),
            },
        );
    }

    mapped
}

fn map_graph_surface(
    summary: &KnowledgeLibrarySummary,
    ops_state: &OpsLibraryState,
    first_warning: Option<&OperatorWarning>,
) -> GraphSurface {
    let total_documents = summary.document_counts_by_readiness.values().copied().sum::<i64>();
    let readable_without_graph_count =
        summary.document_counts_by_readiness.get("readable").copied().unwrap_or(0);
    let status = if total_documents == 0 {
        GraphStatus::Empty
    } else if ops_state.degraded_state == "rebuilding" || ops_state.running_attempts > 0 {
        if summary.graph_ready_document_count > 0 {
            GraphStatus::Rebuilding
        } else {
            GraphStatus::Building
        }
    } else if summary.graph_ready_document_count > 0
        && summary.graph_sparse_document_count == 0
        && readable_without_graph_count == 0
    {
        GraphStatus::Ready
    } else if summary.graph_ready_document_count > 0
        || summary.graph_sparse_document_count > 0
        || readable_without_graph_count > 0
    {
        GraphStatus::Partial
    } else if ops_state.failed_document_count > 0 {
        GraphStatus::Failed
    } else {
        GraphStatus::Building
    };

    let convergence_status = match status {
        GraphStatus::Ready => Some(GraphConvergenceStatus::Current),
        GraphStatus::Partial | GraphStatus::Building | GraphStatus::Rebuilding => {
            Some(GraphConvergenceStatus::Partial)
        }
        GraphStatus::Failed | GraphStatus::Stale => Some(GraphConvergenceStatus::Degraded),
        GraphStatus::Empty => None,
    };

    GraphSurface {
        library_id: summary.library_id,
        status,
        convergence_status,
        warning: first_warning.map(|warning| warning.detail.clone()),
        node_count: saturating_i32_from_i64(summary.node_count),
        relation_count: saturating_i32_from_i64(summary.edge_count),
        edge_count: saturating_i32_from_i64(summary.edge_count),
        graph_ready_document_count: saturating_i32_from_i64(summary.graph_ready_document_count),
        graph_sparse_document_count: saturating_i32_from_i64(summary.graph_sparse_document_count),
        typed_fact_document_count: saturating_i32_from_i64(summary.typed_fact_document_count),
        updated_at: Some(summary.updated_at),
        nodes: Vec::new(),
        edges: Vec::new(),
        readiness_summary: Some(GraphReadinessSummary {
            library_id: summary.library_id,
            document_counts_by_readiness: summary
                .document_counts_by_readiness
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            graph_ready_document_count: summary.graph_ready_document_count,
            graph_sparse_document_count: summary.graph_sparse_document_count,
            typed_fact_document_count: summary.typed_fact_document_count,
            latest_generation: summary.latest_generation.as_ref().map(|generation| {
                GraphGenerationSummary {
                    generation_id: Some(generation.id),
                    active_graph_generation: 1,
                    degraded_state: Some(ops_state.degraded_state.clone()),
                    updated_at: generation.completed_at.or(Some(generation.created_at)),
                }
            }),
            updated_at: Some(summary.updated_at),
        }),
    }
}

fn map_web_run_summary(summary: ingest::WebIngestRunSummary) -> WebIngestRunSummary {
    WebIngestRunSummary {
        run_id: summary.run_id,
        library_id: summary.library_id,
        mode: summary.mode,
        boundary_policy: summary.boundary_policy,
        max_depth: summary.max_depth,
        max_pages: summary.max_pages,
        crawl_filter: map_contract_web_url_filter(summary.crawl_filter),
        materialization_filter: map_contract_web_url_filter(summary.materialization_filter),
        run_state: map_web_run_state(&summary.run_state),
        seed_url: summary.seed_url,
        counts: WebRunCounts {
            discovered: saturating_i32_from_i64(summary.counts.discovered),
            eligible: saturating_i32_from_i64(summary.counts.eligible),
            processed: saturating_i32_from_i64(summary.counts.processed),
            queued: saturating_i32_from_i64(summary.counts.queued),
            processing: saturating_i32_from_i64(summary.counts.processing),
            duplicates: saturating_i32_from_i64(summary.counts.duplicates),
            excluded: saturating_i32_from_i64(summary.counts.excluded),
            blocked: saturating_i32_from_i64(summary.counts.blocked),
            failed: saturating_i32_from_i64(summary.counts.failed),
            canceled: saturating_i32_from_i64(summary.counts.canceled),
        },
        last_activity_at: summary.last_activity_at,
    }
}

fn map_contract_web_url_filter(
    filter: crate::shared::web::ingest::WebIngestUrlFilter,
) -> ironrag_contracts::documents::WebIngestUrlFilter {
    ironrag_contracts::documents::WebIngestUrlFilter {
        allow_patterns: filter.allow_patterns.into_iter().map(map_contract_web_pattern).collect(),
        block_patterns: filter.block_patterns.into_iter().map(map_contract_web_pattern).collect(),
    }
}

fn map_contract_web_pattern(
    pattern: crate::shared::web::ingest::WebIngestPattern,
) -> ironrag_contracts::documents::WebIngestPattern {
    ironrag_contracts::documents::WebIngestPattern {
        kind: pattern.kind,
        value: pattern.value,
        source: pattern.source,
    }
}

const fn map_move_direction(
    direction: IngestQueueMoveDirection,
) -> ingest_repository::QueueMoveDirection {
    match direction {
        IngestQueueMoveDirection::Up => ingest_repository::QueueMoveDirection::Up,
        IngestQueueMoveDirection::Down => ingest_repository::QueueMoveDirection::Down,
        IngestQueueMoveDirection::Top => ingest_repository::QueueMoveDirection::Top,
        IngestQueueMoveDirection::Bottom => ingest_repository::QueueMoveDirection::Bottom,
    }
}

fn severity_level(value: &str) -> MessageLevel {
    match value {
        "error" => MessageLevel::Error,
        "warning" => MessageLevel::Warning,
        _ => MessageLevel::Info,
    }
}

fn map_web_run_state(value: &str) -> WebIngestRunState {
    match value {
        "accepted" => WebIngestRunState::Accepted,
        "discovering" => WebIngestRunState::Discovering,
        "completed" => WebIngestRunState::Completed,
        "completed_partial" => WebIngestRunState::CompletedPartial,
        "failed" => WebIngestRunState::Failed,
        "canceled" => WebIngestRunState::Canceled,
        _ => WebIngestRunState::Processing,
    }
}

fn humanize_warning_kind(value: &str) -> String {
    value
        .split('_')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            chars.next().map_or_else(String::new, |first| {
                format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const fn attention_priority(level: MessageLevel) -> u8 {
    match level {
        MessageLevel::Error => 3,
        MessageLevel::Warning => 2,
        MessageLevel::Info => 1,
    }
}

fn saturating_i32_from_i64(value: i64) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| if value.is_negative() { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
