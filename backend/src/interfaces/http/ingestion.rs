use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ingestion_state::IngestionLifecycleState,
    infra::repositories,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    shared::retry::is_retryable_ingestion_state,
};

#[derive(Serialize)]
pub struct SourceSummary {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_kind: String,
    pub label: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct IngestionJobSummary {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub trigger_kind: String,
    pub status: String,
    pub stage: String,
}

#[derive(Deserialize)]
pub struct ProjectScopedQuery {
    pub project_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct CreateSourceRequest {
    pub project_id: Uuid,
    pub source_kind: String,
    pub label: String,
}

#[derive(Deserialize)]
pub struct CreateIngestionJobRequest {
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub trigger_kind: String,
    pub requested_by: Option<String>,
}

#[derive(Serialize)]
pub struct IngestionJobDetail {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub trigger_kind: String,
    pub status: String,
    pub stage: String,
    pub requested_by: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub retryable: bool,
    pub lifecycle: IngestionLifecycleState,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/sources", get(list_sources).post(create_source))
        .route("/ingestion-jobs", get(list_ingestion_jobs).post(create_ingestion_job))
        .route("/ingestion-jobs/{id}", get(get_ingestion_job_detail))
        .route("/ingestion-jobs/{id}/retry", axum::routing::post(retry_ingestion_job))
}

async fn list_sources(
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<Vec<SourceSummary>>, ApiError> {
    let items = repositories::list_sources(&state.persistence.postgres, query.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| SourceSummary {
            id: row.id,
            project_id: row.project_id,
            source_kind: row.source_kind,
            label: row.label,
            status: row.status,
        })
        .collect();

    Ok(Json(items))
}

async fn create_source(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateSourceRequest>,
) -> Result<Json<SourceSummary>, ApiError> {
    auth.require_any_scope(&["documents:write", "workspace:admin"])?;
    let project = repositories::get_project_by_id(&state.persistence.postgres, payload.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", payload.project_id)))?;
    auth.require_workspace_access(project.workspace_id)?;

    let row = repositories::create_source(
        &state.persistence.postgres,
        payload.project_id,
        &payload.source_kind,
        &payload.label,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(SourceSummary {
        id: row.id,
        project_id: row.project_id,
        source_kind: row.source_kind,
        label: row.label,
        status: row.status,
    }))
}

async fn list_ingestion_jobs(
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<Vec<IngestionJobSummary>>, ApiError> {
    let items = repositories::list_ingestion_jobs(&state.persistence.postgres, query.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| IngestionJobSummary {
            id: row.id,
            project_id: row.project_id,
            source_id: row.source_id,
            trigger_kind: row.trigger_kind,
            status: row.status,
            stage: row.stage,
        })
        .collect();

    Ok(Json(items))
}

async fn create_ingestion_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateIngestionJobRequest>,
) -> Result<Json<IngestionJobSummary>, ApiError> {
    auth.require_any_scope(&["documents:write", "workspace:admin"])?;
    let project = repositories::get_project_by_id(&state.persistence.postgres, payload.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", payload.project_id)))?;
    auth.require_workspace_access(project.workspace_id)?;

    let row = repositories::create_ingestion_job(
        &state.persistence.postgres,
        payload.project_id,
        payload.source_id,
        &payload.trigger_kind,
        payload.requested_by.as_deref(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(IngestionJobSummary {
        id: row.id,
        project_id: row.project_id,
        source_id: row.source_id,
        trigger_kind: row.trigger_kind,
        status: row.status,
        stage: row.stage,
    }))
}

async fn get_ingestion_job_detail(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<IngestionJobDetail>, ApiError> {
    auth.require_any_scope(&["documents:read", "documents:write", "workspace:admin"])?;

    let row = repositories::get_ingestion_job_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("ingestion_job {id} not found")))?;

    let project = repositories::get_project_by_id(&state.persistence.postgres, row.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", row.project_id)))?;
    auth.require_workspace_access(project.workspace_id)?;

    Ok(Json(map_ingestion_job_detail(row)))
}

async fn retry_ingestion_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<IngestionJobDetail>, ApiError> {
    auth.require_any_scope(&["documents:write", "workspace:admin"])?;

    let row = repositories::get_ingestion_job_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("ingestion_job {id} not found")))?;

    let project = repositories::get_project_by_id(&state.persistence.postgres, row.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", row.project_id)))?;
    auth.require_workspace_access(project.workspace_id)?;

    if !is_retryable_ingestion_state(&row.status) {
        return Err(ApiError::BadRequest(format!("ingestion_job {id} is not currently retryable")));
    }

    let retried = repositories::create_ingestion_job(
        &state.persistence.postgres,
        row.project_id,
        row.source_id,
        &row.trigger_kind,
        row.requested_by.as_deref(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(map_ingestion_job_detail(retried)))
}

fn map_ingestion_job_detail(row: repositories::IngestionJobRow) -> IngestionJobDetail {
    let retryable = is_retryable_ingestion_state(&row.status);
    let lifecycle = match row.status.as_str() {
        "queued" => IngestionLifecycleState::Queued,
        "validating" => IngestionLifecycleState::Validating,
        "running" => IngestionLifecycleState::Running,
        "partial" => IngestionLifecycleState::Partial,
        "completed" => IngestionLifecycleState::Completed,
        "retryable_failed" => IngestionLifecycleState::RetryableFailed,
        "canceled" => IngestionLifecycleState::Canceled,
        _ => IngestionLifecycleState::Failed,
    };

    IngestionJobDetail {
        id: row.id,
        project_id: row.project_id,
        source_id: row.source_id,
        trigger_kind: row.trigger_kind,
        status: row.status,
        stage: row.stage,
        requested_by: row.requested_by,
        error_message: row.error_message,
        started_at: row.started_at,
        finished_at: row.finished_at,
        retryable,
        lifecycle,
    }
}
