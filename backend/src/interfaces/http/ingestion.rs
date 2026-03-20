use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ingest::{IngestAttempt, IngestJob, IngestStageEvent},
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_LIBRARY_READ, authorize_library_permission},
        router_support::ApiError,
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListIngestJobsQuery {
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ingest/jobs", get(list_jobs))
        .route("/ingest/jobs/{job_id}", get(get_job))
        .route("/ingest/attempts/{attempt_id}", get(get_attempt))
        .route("/ingest/attempts/{attempt_id}/stages", get(list_stage_events))
}

async fn list_jobs(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListIngestJobsQuery>,
) -> Result<Json<Vec<IngestJob>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let library = state.canonical_services.catalog.get_library(&state, library_id).await?;
    authorize_library_permission(&auth, library.workspace_id, library.id, POLICY_LIBRARY_READ)?;

    let jobs = state
        .canonical_services
        .ingest
        .list_jobs(&state, query.workspace_id, Some(library_id))
        .await?;
    Ok(Json(
        jobs.into_iter()
            .filter(|job| {
                auth.has_library_permission(job.workspace_id, job.library_id, POLICY_LIBRARY_READ)
            })
            .collect(),
    ))
}

async fn get_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<IngestJob>, ApiError> {
    let job = state.canonical_services.ingest.get_job(&state, job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_READ)?;
    Ok(Json(job))
}

async fn get_attempt(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(attempt_id): Path<Uuid>,
) -> Result<Json<IngestAttempt>, ApiError> {
    let attempt = state.canonical_services.ingest.get_attempt(&state, attempt_id).await?;
    let job = state.canonical_services.ingest.get_job(&state, attempt.job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_READ)?;
    Ok(Json(attempt))
}

async fn list_stage_events(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(attempt_id): Path<Uuid>,
) -> Result<Json<Vec<IngestStageEvent>>, ApiError> {
    let attempt = state.canonical_services.ingest.get_attempt(&state, attempt_id).await?;
    let job = state.canonical_services.ingest.get_job(&state, attempt.job_id).await?;
    authorize_library_permission(&auth, job.workspace_id, job.library_id, POLICY_LIBRARY_READ)?;
    let stages = state.canonical_services.ingest.list_stage_events(&state, attempt_id).await?;
    Ok(Json(stages))
}
