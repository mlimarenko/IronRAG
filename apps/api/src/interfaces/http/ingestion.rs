use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::revision_text_state_is_readable,
    domains::ingest::{IngestAttempt, IngestJob, IngestStageEvent},
    domains::ops::OpsAsyncOperation,
    infra::repositories::ingest_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_LIBRARY_READ, POLICY_USAGE_READ, authorize_library_permission,
            load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::ingest::service::QUEUE_STALE_LEASE_SECONDS,
};

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestJobResponse {
    pub job: IngestJob,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_attempt: Option<IngestAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation: Option<OpsAsyncOperation>,
    pub readiness: IngestReadinessResponse,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestAttemptResponse {
    pub job: IngestJob,
    pub attempt: IngestAttempt,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation: Option<OpsAsyncOperation>,
    pub readiness: IngestReadinessResponse,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestStageTimelineResponse {
    pub job: IngestJob,
    pub attempt: IngestAttempt,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation: Option<OpsAsyncOperation>,
    pub readiness: IngestReadinessResponse,
    pub stages: Vec<IngestStageEvent>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestReadinessResponse {
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub text_state: Option<String>,
    pub vector_state: Option<String>,
    pub graph_state: Option<String>,
    pub text_ready: bool,
    pub vector_ready: bool,
    pub graph_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by_revision_id: Option<Uuid>,
}

// ============================================================================
// GET /v1/ingest/libraries/{libraryId}/jobs
// ============================================================================

const INGEST_QUEUE_STATE_VALUES: &[&str] =
    &["queued", "leased", "completed", "failed", "canceled", "paused"];

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListIngestJobsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    /// Comma-separated list of `ingest_queue_state` values to keep. Empty or
    /// absent = no filter. Accepted values: queued, leased, completed,
    /// failed, canceled, paused.
    pub status: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestJobListPageResponse {
    pub items: Vec<IngestJobResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IngestJobListCursor {
    queued_at: chrono::DateTime<chrono::Utc>,
    job_id: Uuid,
}

fn parse_ingest_queue_state_filter(raw: &str) -> Result<Vec<String>, ApiError> {
    let mut out = Vec::new();
    for token in raw.split(',').map(str::trim).filter(|value| !value.is_empty()) {
        if !INGEST_QUEUE_STATE_VALUES.contains(&token) {
            return Err(ApiError::BadRequest(format!(
                "unknown status filter value `{token}`; allowed: {}",
                INGEST_QUEUE_STATE_VALUES.join(", ")
            )));
        }
        out.push(token.to_string());
    }
    Ok(out)
}

// ============================================================================
// Opaque cursors for the ingest jobs/attempts list surfaces.
//
// Same shape as the canonical `/v1/content/libraries/{libraryId}/documents`
// cursor: base64(json(...)), opaque from the client's perspective, only
// valid against the server version that produced it. Any decode failure is
// surfaced as a `BadRequest` — callers are expected to drop the cursor and
// start from the top instead of pretending the page succeeded.
// ============================================================================

fn encode_page_cursor<T: Serialize>(cursor: &T) -> String {
    use base64::Engine;
    // Both cursor shapes below are plain structs of infallibly-serializable
    // fields (`DateTime<Utc>` / `Uuid` / `i32`), so `to_vec` can only fail on
    // I/O errors which `Vec<u8>` never produces. Fall back to an empty token
    // on the (unreachable) error path so the caller keeps paginating rather
    // than panicking on the hot path.
    let json = serde_json::to_vec(cursor).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

fn decode_page_cursor<T: serde::de::DeserializeOwned>(token: &str) -> Result<T, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid cursor encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid cursor payload".to_string()))
}

// ============================================================================
// GET /v1/ingest/jobs/{jobId}/attempts
// ============================================================================

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListIngestAttemptsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestAttemptListPageResponse {
    pub items: Vec<IngestAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IngestAttemptListCursor {
    attempt_number: i32,
    attempt_id: Uuid,
}

// ============================================================================
// GET /v1/ingest/queue
//
// Relocated from `GET /v1/ops/ingest-queue` — the live "currently active"
// queue read model belongs to the ingest domain, not ops (see
// memory/2026-07-17-rest-api-query-refactor-plan.md §4 Ingest / §8
// Ingest-Ops disposition table). The queue mutation endpoints
// (`/v1/ops/ingest-queue/jobs/{jobId}/move|pause|resume|cancel`,
// `/v1/ops/ingest-queue/bulk`) are out of this domain pass's scope and
// remain under `ops` for now; they call back into `list_ingest_queue` below
// to return the refreshed queue after mutating.
// ============================================================================

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct IngestQueueQuery {
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestQueueSummaryResponse {
    pub running: i64,
    pub queued: i64,
    pub paused: i64,
    pub total: i64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestQueueItemResponse {
    pub job_id: Uuid,
    pub workspace_id: Uuid,
    pub workspace_name: String,
    pub library_id: Uuid,
    pub library_name: String,
    pub document_id: Option<Uuid>,
    pub document_name: String,
    pub job_kind: String,
    pub queue_state: String,
    pub queue_position: Option<i64>,
    pub queued_at: chrono::DateTime<chrono::Utc>,
    pub available_at: chrono::DateTime<chrono::Utc>,
    pub attempt_id: Option<Uuid>,
    pub attempt_state: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    pub current_stage: Option<String>,
    pub progress_percent: Option<i32>,
    pub attempt_number: Option<i32>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub can_retry_requeue: bool,
    pub can_pause: bool,
    pub can_resume: bool,
    pub can_cancel: bool,
    pub has_stale_queue_lease: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestQueueResponse {
    pub summary: IngestQueueSummaryResponse,
    pub items: Vec<IngestQueueItemResponse>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ingest/libraries/{library_id}/jobs", get(list_jobs))
        .route("/ingest/jobs/{job_id}", get(get_job))
        .route("/ingest/jobs/{job_id}/attempts", get(list_job_attempts))
        .route("/ingest/attempts/{attempt_id}", get(get_attempt))
        .route("/ingest/attempts/{attempt_id}/stages", get(list_stage_events))
        .route("/ingest/queue", get(list_ingest_queue))
}

/// Paginated job history for one library, newest first.
///
/// Replaces the old flat `GET /v1/ingest/jobs?workspaceId=&libraryId=` — the
/// handler 400-ed without `libraryId` in practice, so it was already a
/// lyingly-optional query parameter. `libraryId` is now an honest required
/// path segment (memory/2026-07-17-rest-api-query-refactor-plan.md §8:
/// "RESTORED+RENAME").
#[utoipa::path(
    get,
    path = "/v1/ingest/libraries/{libraryId}/jobs",
    tag = "ingest",
    operation_id = "listIngestJobs",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library that owns the ingest job collection"),
        ListIngestJobsQuery,
    ),
    responses(
        (status = 200, description = "Paginated ingest jobs for the library, newest first", body = IngestJobListPageResponse),
        (status = 400, description = "Invalid cursor or status filter value"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_jobs",
    skip_all,
    fields(library_id = %library_id, limit, item_count)
)]
pub async fn list_jobs(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<ListIngestJobsQuery>,
) -> Result<Json<IngestJobListPageResponse>, ApiError> {
    const DEFAULT_LIMIT: u32 = 50;
    const MAX_LIMIT: u32 = 500;

    let span = tracing::Span::current();
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    span.record("limit", limit);

    let cursor = match query.cursor.as_deref() {
        Some(token) => {
            let decoded: IngestJobListCursor = decode_page_cursor(token)?;
            Some((decoded.queued_at, decoded.job_id))
        }
        None => None,
    };

    let status_filter = match query.status.as_deref() {
        None => Vec::new(),
        Some(raw) => parse_ingest_queue_state_filter(raw)?,
    };

    let (handles, has_more) = state
        .canonical_services
        .ingest
        .list_job_handles_page(&state, library.id, cursor, i64::from(limit), &status_filter)
        .await?;

    let mut items = Vec::with_capacity(handles.len());
    for handle in handles {
        items.push(map_job_handle(&state, handle).await?);
    }

    let next_cursor = if has_more {
        items.last().map(|item| {
            encode_page_cursor(&IngestJobListCursor {
                queued_at: item.job.queued_at,
                job_id: item.job.id,
            })
        })
    } else {
        None
    };

    span.record("item_count", items.len());
    Ok(Json(IngestJobListPageResponse { items, next_cursor }))
}

#[utoipa::path(
    get,
    path = "/v1/ingest/jobs/{jobId}",
    tag = "ingest",
    operation_id = "getIngestJob",
    params(("jobId" = uuid::Uuid, Path, description = "Ingest job identifier")),
    responses(
        (status = 200, description = "Ingest job with latest attempt and async operation", body = IngestJobResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the job's library"),
        (status = 404, description = "Job not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_job",
    skip_all,
    fields(job_id = %job_id)
)]
pub async fn get_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<IngestJobResponse>, ApiError> {
    let handle = state.canonical_services.ingest.get_job_handle(&state, job_id).await?;
    authorize_library_permission(
        &auth,
        handle.job.workspace_id,
        handle.job.library_id,
        POLICY_LIBRARY_READ,
    )?;
    Ok(Json(map_job_handle(&state, handle).await?))
}

/// Paginated attempt history for one job, newest first. Net-new: a job
/// previously had Get (`GET .../jobs/{jobId}`, latest attempt only) and
/// Create-via-retry (`POST .../jobs/{jobId}/attempts`), but no List — an
/// operator diagnosing a flaky ingest had no way to see the full retry
/// history in one call.
#[utoipa::path(
    get,
    path = "/v1/ingest/jobs/{jobId}/attempts",
    tag = "ingest",
    operation_id = "listIngestJobAttempts",
    params(
        ("jobId" = uuid::Uuid, Path, description = "Ingest job identifier"),
        ListIngestAttemptsQuery,
    ),
    responses(
        (status = 200, description = "Paginated attempt history for the job, newest first", body = IngestAttemptListPageResponse),
        (status = 400, description = "Invalid cursor"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the job's library"),
        (status = 404, description = "Job not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_job_attempts",
    skip_all,
    fields(job_id = %job_id, limit, item_count)
)]
pub async fn list_job_attempts(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    Query(query): Query<ListIngestAttemptsQuery>,
) -> Result<Json<IngestAttemptListPageResponse>, ApiError> {
    const DEFAULT_LIMIT: u32 = 50;
    const MAX_LIMIT: u32 = 500;

    let span = tracing::Span::current();
    let job = state.canonical_services.ingest.get_job(&state, job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_READ)?;

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    span.record("limit", limit);

    let cursor = match query.cursor.as_deref() {
        Some(token) => {
            let decoded: IngestAttemptListCursor = decode_page_cursor(token)?;
            Some((decoded.attempt_number, decoded.attempt_id))
        }
        None => None,
    };

    let (items, has_more) = state
        .canonical_services
        .ingest
        .list_attempts_page(&state, job_id, cursor, i64::from(limit))
        .await?;

    let next_cursor = if has_more {
        items.last().map(|attempt| {
            encode_page_cursor(&IngestAttemptListCursor {
                attempt_number: attempt.attempt_number,
                attempt_id: attempt.id,
            })
        })
    } else {
        None
    };

    span.record("item_count", items.len());
    Ok(Json(IngestAttemptListPageResponse { items, next_cursor }))
}

#[utoipa::path(
    get,
    path = "/v1/ingest/attempts/{attemptId}",
    tag = "ingest",
    operation_id = "getIngestAttempt",
    params(("attemptId" = uuid::Uuid, Path, description = "Ingest attempt identifier")),
    responses(
        (status = 200, description = "Ingest attempt detail with parent job and async operation", body = IngestAttemptResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the attempt's library"),
        (status = 404, description = "Attempt not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_attempt",
    skip_all,
    fields(attempt_id = %attempt_id)
)]
pub async fn get_attempt(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(attempt_id): Path<Uuid>,
) -> Result<Json<IngestAttemptResponse>, ApiError> {
    let handle = state.canonical_services.ingest.get_attempt_handle(&state, attempt_id).await?;
    authorize_library_permission(
        &auth,
        handle.job.workspace_id,
        handle.job.library_id,
        POLICY_LIBRARY_READ,
    )?;
    Ok(Json(map_attempt_handle(&state, handle).await?))
}

#[utoipa::path(
    get,
    path = "/v1/ingest/attempts/{attemptId}/stages",
    tag = "ingest",
    operation_id = "listIngestStageEvents",
    params(("attemptId" = uuid::Uuid, Path, description = "Ingest attempt identifier")),
    responses(
        (status = 200, description = "Stage timeline for the attempt", body = IngestStageTimelineResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the attempt's library"),
        (status = 404, description = "Attempt not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_stage_events",
    skip_all,
    fields(attempt_id = %attempt_id)
)]
pub async fn list_stage_events(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(attempt_id): Path<Uuid>,
) -> Result<Json<IngestStageTimelineResponse>, ApiError> {
    let handle = state.canonical_services.ingest.get_attempt_handle(&state, attempt_id).await?;
    authorize_library_permission(
        &auth,
        handle.job.workspace_id,
        handle.job.library_id,
        POLICY_LIBRARY_READ,
    )?;
    let stages = state.canonical_services.ingest.list_stage_events(&state, attempt_id).await?;
    let readiness = build_readiness_response(
        &state,
        handle.job.knowledge_document_id,
        handle.job.knowledge_revision_id,
    )
    .await?;
    Ok(Json(IngestStageTimelineResponse {
        job: handle.job,
        attempt: handle.attempt,
        async_operation: handle.async_operation,
        readiness,
        stages,
    }))
}

/// Relocated from `GET /v1/ops/ingest-queue` (see the module-level comment
/// above). Stays `pub` (not `pub(crate)`) because the still-`ops`-scoped
/// queue mutation handlers (`move`/`retry`/`pause`/`resume`/`cancel`/`bulk`,
/// a sibling module) call back into this function to return the refreshed
/// queue after mutating.
#[utoipa::path(
    get,
    path = "/v1/ingest/queue",
    tag = "ingest",
    operation_id = "listIngestQueue",
    params(IngestQueueQuery),
    responses(
        (status = 200, description = "Active ingest queue visible to the caller", body = IngestQueueResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized to read operations"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_ingest_queue",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id, item_count)
)]
pub async fn list_ingest_queue(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<IngestQueueQuery>,
) -> Result<Json<IngestQueueResponse>, ApiError> {
    auth.require_any_scope(POLICY_USAGE_READ)?;
    if let Some(library_id) = query.library_id {
        let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    }

    let rows = ingest_repository::list_active_ingest_queue(
        &state.persistence.postgres,
        query.workspace_id,
        query.library_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    let mut running = 0_i64;
    let mut queued = 0_i64;
    let mut paused = 0_i64;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        if !auth.has_library_permission(row.workspace_id, row.library_id, POLICY_USAGE_READ) {
            continue;
        }
        match row.queue_state.as_str() {
            "leased" => running += 1,
            "queued" => queued += 1,
            "paused" => paused += 1,
            _ => {}
        }
        items.push(map_ingest_queue_item(row));
    }
    tracing::Span::current().record("item_count", items.len());
    Ok(Json(IngestQueueResponse {
        summary: IngestQueueSummaryResponse {
            running,
            queued,
            paused,
            total: running + queued + paused,
        },
        items,
    }))
}

fn map_ingest_queue_item(row: ingest_repository::IngestQueueItemRow) -> IngestQueueItemResponse {
    let has_active_attempt =
        row.attempt_state.as_deref().is_some_and(|state| state == "leased" || state == "running");
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(QUEUE_STALE_LEASE_SECONDS);
    let lease_started_at = row.queue_leased_at.unwrap_or(row.queued_at);
    let has_stale_queue_lease =
        row.queue_state == "leased" && lease_started_at < cutoff && !has_active_attempt;
    let can_retry_requeue = match row.queue_state.as_str() {
        "queued" | "paused" => true,
        "leased" => has_stale_queue_lease,
        _ => false,
    };
    let can_pause = row.queue_state == "queued" || row.queue_state == "leased";
    let can_resume = row.queue_state == "paused" && !has_active_attempt;
    let can_cancel = matches!(row.queue_state.as_str(), "queued" | "leased" | "paused");
    IngestQueueItemResponse {
        job_id: row.job_id,
        workspace_id: row.workspace_id,
        workspace_name: row.workspace_name,
        library_id: row.library_id,
        library_name: row.library_name,
        document_id: row.knowledge_document_id,
        document_name: row.document_name.unwrap_or_else(|| row.job_kind.clone()),
        job_kind: row.job_kind,
        queue_state: row.queue_state,
        queue_position: row.queue_position,
        queued_at: row.queued_at,
        available_at: row.available_at,
        attempt_id: row.attempt_id,
        attempt_state: row.attempt_state,
        started_at: row.started_at,
        heartbeat_at: row.heartbeat_at,
        current_stage: row.current_stage,
        progress_percent: row.progress_percent,
        attempt_number: row.attempt_number,
        failure_code: row.failure_code,
        failure_message: row.failure_message,
        can_retry_requeue,
        can_pause,
        can_resume,
        can_cancel,
        has_stale_queue_lease,
    }
}

async fn map_job_handle(
    state: &AppState,
    handle: crate::services::ingest::service::IngestJobHandle,
) -> Result<IngestJobResponse, ApiError> {
    let readiness = build_readiness_response(
        state,
        handle.job.knowledge_document_id,
        handle.job.knowledge_revision_id,
    )
    .await?;
    Ok(IngestJobResponse {
        job: handle.job,
        latest_attempt: handle.latest_attempt,
        async_operation: handle.async_operation,
        readiness,
    })
}

async fn map_attempt_handle(
    state: &AppState,
    handle: crate::services::ingest::service::IngestAttemptHandle,
) -> Result<IngestAttemptResponse, ApiError> {
    let readiness = build_readiness_response(
        state,
        handle.job.knowledge_document_id,
        handle.job.knowledge_revision_id,
    )
    .await?;
    Ok(IngestAttemptResponse {
        job: handle.job,
        attempt: handle.attempt,
        async_operation: handle.async_operation,
        readiness,
    })
}

async fn build_readiness_response(
    state: &AppState,
    knowledge_document_id: Option<Uuid>,
    knowledge_revision_id: Option<Uuid>,
) -> Result<IngestReadinessResponse, ApiError> {
    let mut text_state = None;
    let mut vector_state = None;
    let mut graph_state = None;
    let mut text_ready = false;
    let mut vector_ready = false;
    let mut graph_ready = false;
    let mut superseded_by_revision_id = None;
    let mut document_id = knowledge_document_id;

    if let Some(revision_id) = knowledge_revision_id {
        let revision = state
            .document_store
            .get_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", revision_id))?;
        document_id = Some(revision.document_id);
        text_state = Some(revision.text_state.clone());
        vector_state = Some(revision.vector_state.clone());
        graph_state = Some(revision.graph_state.clone());
        text_ready = revision_text_state_is_readable(&revision.text_state);
        vector_ready = matches!(revision.vector_state.as_str(), "ready");
        graph_ready = matches!(revision.graph_state.as_str(), "ready");
        superseded_by_revision_id = revision.superseded_by_revision_id;
    }

    Ok(IngestReadinessResponse {
        knowledge_document_id: document_id,
        knowledge_revision_id,
        text_state,
        vector_state,
        graph_state,
        text_ready,
        vector_ready,
        graph_ready,
        superseded_by_revision_id,
    })
}
