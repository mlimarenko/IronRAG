use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ingestion_state::IngestionLifecycleState,
    infra::repositories,
    interfaces::http::{
        auth::AuthContext, authorization::load_ingestion_job_and_authorize,
        router_support::ApiError,
    },
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
    let project_id = query.project_id;
    let items: Vec<SourceSummary> =
        repositories::list_sources(&state.persistence.postgres, query.project_id)
            .await
            .map_err(|error| {
                log_ingestion_internal_error("list sources", project_id, None, None, &error);
                ApiError::Internal
            })?
            .into_iter()
            .map(map_source_summary)
            .collect();

    info!(
        project_id = ?project_id,
        source_count = items.len(),
        "listed ingestion sources",
    );

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
        .map_err(|error| {
            log_ingestion_internal_error(
                "load project for source creation",
                Some(payload.project_id),
                None,
                Some(auth.token_id),
                &error,
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", payload.project_id)))?;
    auth.require_workspace_access(project.workspace_id)?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %payload.project_id,
        source_kind = %payload.source_kind,
        label = %payload.label,
        "accepted source creation request",
    );

    let row = repositories::create_source(
        &state.persistence.postgres,
        payload.project_id,
        &payload.source_kind,
        &payload.label,
    )
    .await
    .map_err(|error| {
        log_ingestion_internal_error(
            "create source",
            Some(payload.project_id),
            None,
            Some(auth.token_id),
            &error,
        );
        ApiError::Internal
    })?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %row.project_id,
        source_id = %row.id,
        source_kind = %row.source_kind,
        status = %row.status,
        "created source",
    );

    Ok(Json(map_source_summary(row)))
}

async fn list_ingestion_jobs(
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<Vec<IngestionJobSummary>>, ApiError> {
    let project_id = query.project_id;
    let items: Vec<IngestionJobSummary> =
        repositories::list_ingestion_jobs(&state.persistence.postgres, query.project_id)
            .await
            .map_err(|error| {
                log_ingestion_internal_error("list ingestion jobs", project_id, None, None, &error);
                ApiError::Internal
            })?
            .into_iter()
            .map(map_ingestion_job_summary)
            .collect();

    info!(
        project_id = ?project_id,
        ingestion_job_count = items.len(),
        "listed ingestion jobs",
    );

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
        .map_err(|error| {
            log_ingestion_internal_error(
                "load project for ingestion job creation",
                Some(payload.project_id),
                None,
                Some(auth.token_id),
                &error,
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", payload.project_id)))?;
    auth.require_workspace_access(project.workspace_id)?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %payload.project_id,
        source_id = ?payload.source_id,
        trigger_kind = %payload.trigger_kind,
        requested_by_present = payload.requested_by.is_some(),
        "accepted ingestion job creation request",
    );

    let row = repositories::create_ingestion_job(
        &state.persistence.postgres,
        payload.project_id,
        payload.source_id,
        &payload.trigger_kind,
        payload.requested_by.as_deref(),
        None,
        None,
        serde_json::json!({}),
    )
    .await
    .map_err(|error| {
        log_ingestion_internal_error(
            "create ingestion job",
            Some(payload.project_id),
            None,
            Some(auth.token_id),
            &error,
        );
        ApiError::Internal
    })?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %row.project_id,
        source_id = ?row.source_id,
        ingestion_job_id = %row.id,
        trigger_kind = %row.trigger_kind,
        status = %row.status,
        stage = %row.stage,
        "created ingestion job",
    );

    Ok(Json(map_ingestion_job_summary(row)))
}

async fn get_ingestion_job_detail(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<IngestionJobDetail>, ApiError> {
    auth.require_any_scope(&["documents:read", "documents:write", "workspace:admin"])?;

    let row = repositories::get_ingestion_job_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            log_ingestion_internal_error(
                "load ingestion job detail",
                None,
                Some(id),
                Some(auth.token_id),
                &error,
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("ingestion_job {id} not found")))?;

    let project = repositories::get_project_by_id(&state.persistence.postgres, row.project_id)
        .await
        .map_err(|error| {
            log_ingestion_internal_error(
                "load project for ingestion job detail",
                Some(row.project_id),
                Some(row.id),
                Some(auth.token_id),
                &error,
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", row.project_id)))?;
    auth.require_workspace_access(project.workspace_id)?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %row.project_id,
        ingestion_job_id = %row.id,
        source_id = ?row.source_id,
        status = %row.status,
        stage = %row.stage,
        retryable = is_retryable_ingestion_state(&row.status),
        "loaded ingestion job detail",
    );

    Ok(Json(map_ingestion_job_detail(row)))
}

async fn retry_ingestion_job(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<IngestionJobDetail>, ApiError> {
    let (row, project) = load_ingestion_job_and_authorize(
        &auth,
        &state,
        id,
        &["documents:write", "workspace:admin"],
    )
    .await?;

    if !is_retryable_ingestion_state(&row.status) {
        warn!(
            auth_token_id = %auth.token_id,
            workspace_id = %project.workspace_id,
            project_id = %row.project_id,
            ingestion_job_id = %row.id,
            status = %row.status,
            stage = %row.stage,
            "rejecting ingestion job retry request because job is not retryable",
        );
        return Err(ApiError::BadRequest(format!("ingestion_job {id} is not currently retryable")));
    }

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %row.project_id,
        ingestion_job_id = %row.id,
        source_id = ?row.source_id,
        trigger_kind = %row.trigger_kind,
        attempt_count = row.attempt_count,
        "accepted ingestion job retry request",
    );
    let retry_idempotency_key =
        row.idempotency_key.as_deref().map(|key| format!("{key}:retry:{}", Uuid::now_v7()));
    let retried = repositories::create_ingestion_job(
        &state.persistence.postgres,
        row.project_id,
        row.source_id,
        &row.trigger_kind,
        row.requested_by.as_deref(),
        Some(row.id),
        retry_idempotency_key.as_deref(),
        row.payload_json.clone(),
    )
    .await
    .map_err(|error| {
        log_ingestion_internal_error(
            "create retry ingestion job",
            Some(row.project_id),
            Some(row.id),
            Some(auth.token_id),
            &error,
        );
        ApiError::Internal
    })?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %retried.project_id,
        ingestion_job_id = %retried.id,
        parent_job_id = ?retried.parent_job_id,
        source_id = ?retried.source_id,
        trigger_kind = %retried.trigger_kind,
        status = %retried.status,
        stage = %retried.stage,
        "created retry ingestion job",
    );

    Ok(Json(map_ingestion_job_detail(retried)))
}

fn map_source_summary(row: repositories::SourceRow) -> SourceSummary {
    SourceSummary {
        id: row.id,
        project_id: row.project_id,
        source_kind: row.source_kind,
        label: row.label,
        status: row.status,
    }
}

fn map_ingestion_job_summary(row: repositories::IngestionJobRow) -> IngestionJobSummary {
    IngestionJobSummary {
        id: row.id,
        project_id: row.project_id,
        source_id: row.source_id,
        trigger_kind: row.trigger_kind,
        status: row.status,
        stage: row.stage,
    }
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

fn log_ingestion_internal_error(
    operation: &'static str,
    project_id: Option<Uuid>,
    ingestion_job_id: Option<Uuid>,
    auth_token_id: Option<Uuid>,
    error: &impl std::fmt::Debug,
) {
    error!(
        operation,
        project_id = ?project_id,
        ingestion_job_id = ?ingestion_job_id,
        auth_token_id = ?auth_token_id,
        ?error,
        "ingestion http operation failed",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn ingestion_job(status: &str) -> repositories::IngestionJobRow {
        repositories::IngestionJobRow {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            source_id: None,
            trigger_kind: "manual".into(),
            status: status.into(),
            stage: "stage".into(),
            requested_by: Some("tester".into()),
            error_message: None,
            started_at: None,
            finished_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
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
    fn maps_retryable_failed_state() {
        let detail = map_ingestion_job_detail(ingestion_job("retryable_failed"));

        assert!(detail.retryable);
        assert!(matches!(detail.lifecycle, IngestionLifecycleState::RetryableFailed));
    }

    #[test]
    fn maps_unknown_state_to_failed_lifecycle() {
        let detail = map_ingestion_job_detail(ingestion_job("totally_unknown"));

        assert!(!detail.retryable);
        assert!(matches!(detail.lifecycle, IngestionLifecycleState::Failed));
    }
}
