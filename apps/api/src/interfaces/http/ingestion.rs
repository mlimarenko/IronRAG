use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::revision_text_state_is_readable,
    domains::ingest::{IngestAttempt, IngestJob, IngestStageEvent},
    domains::ops::OpsAsyncOperation,
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

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestJobResponse {
    pub job: IngestJob,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_attempt: Option<IngestAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation: Option<OpsAsyncOperation>,
    pub readiness: IngestReadinessResponse,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestAttemptResponse {
    pub job: IngestJob,
    pub attempt: IngestAttempt,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation: Option<OpsAsyncOperation>,
    pub readiness: IngestReadinessResponse,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestStageTimelineResponse {
    pub job: IngestJob,
    pub attempt: IngestAttempt,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation: Option<OpsAsyncOperation>,
    pub readiness: IngestReadinessResponse,
    pub stages: Vec<IngestStageEvent>,
}

#[derive(Debug, Clone, serde::Serialize)]
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

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ingest/jobs", get(list_jobs))
        .route("/ingest/jobs/{job_id}", get(get_job))
        .route("/ingest/attempts/{attempt_id}", get(get_attempt))
        .route("/ingest/attempts/{attempt_id}/stages", get(list_stage_events))
}

#[tracing::instrument(
    level = "info",
    name = "http.list_jobs",
    skip_all,
    fields(library_id = ?query.library_id, item_count)
)]
async fn list_jobs(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListIngestJobsQuery>,
) -> Result<Json<Vec<IngestJobResponse>>, ApiError> {
    let span = tracing::Span::current();
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let library = state.canonical_services.catalog.get_library(&state, library_id).await?;
    authorize_library_permission(&auth, library.workspace_id, library.id, POLICY_LIBRARY_READ)?;

    let jobs = state
        .canonical_services
        .ingest
        .list_job_handles(&state, query.workspace_id, Some(library_id))
        .await?;
    let mut responses = Vec::with_capacity(jobs.len());
    for handle in jobs {
        if auth.has_library_permission(
            handle.job.workspace_id,
            handle.job.library_id,
            POLICY_LIBRARY_READ,
        ) {
            responses.push(map_job_handle(&state, handle).await?);
        }
    }
    span.record("item_count", responses.len());
    Ok(Json(responses))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_job",
    skip_all,
    fields(job_id = %job_id)
)]
async fn get_job(
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

#[tracing::instrument(
    level = "info",
    name = "http.get_attempt",
    skip_all,
    fields(attempt_id = %attempt_id)
)]
async fn get_attempt(
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

#[tracing::instrument(
    level = "info",
    name = "http.list_stage_events",
    skip_all,
    fields(attempt_id = %attempt_id)
)]
async fn list_stage_events(
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
            .arango_document_store
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
