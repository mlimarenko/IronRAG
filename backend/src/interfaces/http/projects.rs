use axum::{
    Json, Router,
    extract::{Path, Query, State},
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
pub struct ProjectSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Deserialize)]
pub struct ListProjectsQuery {
    pub workspace_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct CreateProjectRequest {
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct ProjectReadinessSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub ingestion_jobs: usize,
    pub sources: usize,
    pub documents: usize,
    pub ready_for_query: bool,
    pub indexing_state: String,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}/readiness", get(get_project_readiness))
}

async fn list_projects(
    State(state): State<AppState>,
    Query(query): Query<ListProjectsQuery>,
) -> Result<Json<Vec<ProjectSummary>>, ApiError> {
    let items = repositories::list_projects(&state.persistence.postgres, query.workspace_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| ProjectSummary {
            id: row.id,
            workspace_id: row.workspace_id,
            slug: row.slug,
            name: row.name,
            description: row.description,
        })
        .collect();

    Ok(Json(items))
}

async fn create_project(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<Json<ProjectSummary>, ApiError> {
    auth.require_any_scope(&["projects:write", "workspace:admin"])?;
    auth.require_workspace_access(payload.workspace_id)?;

    let row = repositories::create_project(
        &state.persistence.postgres,
        payload.workspace_id,
        &payload.slug,
        &payload.name,
        payload.description.as_deref(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(ProjectSummary {
        id: row.id,
        workspace_id: row.workspace_id,
        slug: row.slug,
        name: row.name,
        description: row.description,
    }))
}

async fn get_project_readiness(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ProjectReadinessSummary>, ApiError> {
    auth.require_any_scope(&["documents:read", "query:run", "workspace:admin"])?;

    let project = repositories::get_project_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {id} not found")))?;
    auth.require_workspace_access(project.workspace_id)?;

    let sources = repositories::list_sources(&state.persistence.postgres, Some(id))
        .await
        .map_err(|_| ApiError::Internal)?;
    let ingestion_jobs = repositories::list_ingestion_jobs(&state.persistence.postgres, Some(id))
        .await
        .map_err(|_| ApiError::Internal)?;
    let documents = repositories::list_documents(&state.persistence.postgres, Some(id))
        .await
        .map_err(|_| ApiError::Internal)?;

    let latest_job = ingestion_jobs.iter().max_by_key(|job| job.created_at);
    let latest_status = latest_job.map(|job| job.status.as_str());
    let ready_for_query = !documents.is_empty() && matches!(latest_status, Some("completed"));
    let indexing_state = if documents.is_empty() {
        "not_indexed"
    } else {
        match latest_status {
            Some("completed") => "indexed",
            Some("partial") => "partial",
            Some("queued" | "running" | "validating") => "ingesting",
            Some("retryable_failed" | "failed" | "canceled") => "stale",
            Some(_) | None => "ingesting",
        }
    };

    Ok(Json(ProjectReadinessSummary {
        id: project.id,
        workspace_id: project.workspace_id,
        slug: project.slug,
        name: project.name,
        ingestion_jobs: ingestion_jobs.len(),
        sources: sources.len(),
        documents: documents.len(),
        ready_for_query,
        indexing_state: indexing_state.to_string(),
    }))
}
