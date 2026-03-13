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

    fn ingestion_job(status: &str, created_at: chrono::DateTime<Utc>) -> repositories::IngestionJobRow {
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
        }
    }

    fn summarize_readiness(
        project: repositories::ProjectRow,
        sources: usize,
        documents: usize,
        ingestion_jobs: Vec<repositories::IngestionJobRow>,
    ) -> ProjectReadinessSummary {
        let latest_job = ingestion_jobs.iter().max_by_key(|job| job.created_at);
        let latest_status = latest_job.map(|job| job.status.as_str());
        let ready_for_query = documents > 0 && matches!(latest_status, Some("completed"));
        let indexing_state = if documents == 0 {
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
        }
    }

    #[test]
    fn readiness_uses_latest_ingestion_job_status() {
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

        assert!(!summary.ready_for_query);
        assert_eq!(summary.indexing_state, "stale");
    }

    #[test]
    fn readiness_marks_completed_latest_job_as_indexed() {
        let summary = summarize_readiness(
            project(),
            1,
            2,
            vec![ingestion_job("completed", Utc::now())],
        );

        assert!(summary.ready_for_query);
        assert_eq!(summary.indexing_state, "indexed");
    }
}
