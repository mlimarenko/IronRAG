use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
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

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/sources", get(list_sources).post(create_source))
        .route("/ingestion-jobs", get(list_ingestion_jobs).post(create_ingestion_job))
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
