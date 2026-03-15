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
    pub latest_ingestion_status: Option<String>,
    pub active_ingestion_jobs: usize,
    pub completed_ingestion_jobs: usize,
    pub failed_ingestion_jobs: usize,
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

    Ok(Json(summarize_readiness(project, sources.len(), documents.len(), ingestion_jobs)))
}

fn summarize_readiness(
    project: repositories::ProjectRow,
    sources: usize,
    documents: usize,
    ingestion_jobs: Vec<repositories::IngestionJobRow>,
) -> ProjectReadinessSummary {
    let latest_job = ingestion_jobs.iter().max_by_key(|job| job.created_at);
    let latest_ingestion_status = latest_job.map(|job| job.status.clone());
    let active_ingestion_jobs = ingestion_jobs
        .iter()
        .filter(|job| matches!(job.status.as_str(), "queued" | "running" | "validating"))
        .count();
    let completed_ingestion_jobs =
        ingestion_jobs.iter().filter(|job| job.status == "completed").count();
    let failed_ingestion_jobs = ingestion_jobs
        .iter()
        .filter(|job| matches!(job.status.as_str(), "retryable_failed" | "failed" | "canceled"))
        .count();

    let ready_for_query = documents > 0 && active_ingestion_jobs == 0;
    let indexing_state = if documents == 0 {
        if active_ingestion_jobs > 0 { "ingesting" } else { "not_indexed" }
    } else if active_ingestion_jobs > 0 {
        if completed_ingestion_jobs > 0 { "partially_indexed" } else { "ingesting" }
    } else if failed_ingestion_jobs > 0 {
        if completed_ingestion_jobs > 0 { "indexed_with_failures" } else { "stale" }
    } else {
        "indexed"
    };

    ProjectReadinessSummary {
        id: project.id,
        workspace_id: project.workspace_id,
        slug: project.slug,
        name: project.name,
        ingestion_jobs: ingestion_jobs.len(),
        sources,
        documents,
        ready_for_query,
        indexing_state: indexing_state.into(),
        latest_ingestion_status,
        active_ingestion_jobs,
        completed_ingestion_jobs,
        failed_ingestion_jobs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn project() -> repositories::ProjectRow {
        repositories::ProjectRow {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            slug: "demo".into(),
            name: "Demo".into(),
            description: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn ingestion_job(
        status: &str,
        created_at: chrono::DateTime<Utc>,
    ) -> repositories::IngestionJobRow {
        repositories::IngestionJobRow {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            source_id: None,
            trigger_kind: "manual".into(),
            status: status.into(),
            stage: "done".into(),
            requested_by: None,
            error_message: None,
            started_at: None,
            finished_at: None,
            created_at,
            updated_at: created_at,
            idempotency_key: None,
            parent_job_id: None,
            attempt_count: 0,
            worker_id: None,
            lease_expires_at: None,
            heartbeat_at: None,
            payload_json: serde_json::json!({}),
            result_json: serde_json::json!({}),
        }
    }

    #[test]
    fn readiness_stays_query_ready_when_indexed_docs_exist_but_latest_job_failed() {
        let now = Utc::now();
        let summary = summarize_readiness(
            project(),
            1,
            3,
            vec![
                ingestion_job("completed", now - Duration::minutes(10)),
                ingestion_job("failed", now),
            ],
        );

        assert!(summary.ready_for_query);
        assert_eq!(summary.indexing_state, "indexed_with_failures");
        assert_eq!(summary.latest_ingestion_status.as_deref(), Some("failed"));
        assert_eq!(summary.completed_ingestion_jobs, 1);
        assert_eq!(summary.failed_ingestion_jobs, 1);
    }

    #[test]
    fn readiness_marks_completed_library_as_indexed() {
        let summary =
            summarize_readiness(project(), 1, 2, vec![ingestion_job("completed", Utc::now())]);

        assert!(summary.ready_for_query);
        assert_eq!(summary.indexing_state, "indexed");
        assert_eq!(summary.active_ingestion_jobs, 0);
    }

    #[test]
    fn readiness_marks_inflight_jobs_as_partially_indexed_when_docs_already_exist() {
        let now = Utc::now();
        let summary = summarize_readiness(
            project(),
            1,
            2,
            vec![
                ingestion_job("completed", now - Duration::minutes(10)),
                ingestion_job("running", now),
            ],
        );

        assert!(!summary.ready_for_query);
        assert_eq!(summary.indexing_state, "partially_indexed");
        assert_eq!(summary.active_ingestion_jobs, 1);
        assert_eq!(summary.completed_ingestion_jobs, 1);
    }
}
