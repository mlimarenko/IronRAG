use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ingest::{WebDiscoveredPage, WebIngestRun, WebIngestRunReceipt, WebIngestRunSummary},
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE, load_library_and_authorize},
        router_support::ApiError,
    },
    services::ingest::web::CreateWebIngestRunCommand,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListWebIngestRunsQuery {
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateWebIngestRunRequest {
    pub library_id: Uuid,
    pub seed_url: String,
    pub mode: String,
    pub boundary_policy: Option<String>,
    pub max_depth: Option<i32>,
    pub max_pages: Option<i32>,
    pub idempotency_key: Option<String>,
}

pub(super) async fn create_web_ingest_run(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<CreateWebIngestRunRequest>,
) -> Result<(StatusCode, Json<WebIngestRunReceipt>), ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, request.library_id, POLICY_LIBRARY_WRITE).await?;
    let run = state
        .canonical_services
        .web_ingest
        .create_run(
            &state,
            CreateWebIngestRunCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                seed_url: request.seed_url,
                mode: request.mode,
                boundary_policy: request.boundary_policy,
                max_depth: request.max_depth,
                max_pages: request.max_pages,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                idempotency_key: request.idempotency_key,
            },
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(WebIngestRunReceipt {
            run_id: run.run_id,
            library_id: run.library_id,
            mode: run.mode,
            run_state: run.run_state,
            async_operation_id: run.async_operation_id,
            counts: run.counts,
            failure_code: run.failure_code,
            cancel_requested_at: run.cancel_requested_at,
        }),
    ))
}

pub(super) async fn list_web_ingest_runs(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListWebIngestRunsQuery>,
) -> Result<Json<Vec<WebIngestRunSummary>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let runs = state.canonical_services.web_ingest.list_runs(&state, library.id).await?;
    Ok(Json(runs))
}

pub(super) async fn get_web_ingest_run(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<WebIngestRun>, ApiError> {
    let run = state.canonical_services.web_ingest.get_run(&state, run_id).await?;
    let _library =
        load_library_and_authorize(&auth, &state, run.library_id, POLICY_LIBRARY_READ).await?;
    Ok(Json(run))
}

pub(super) async fn list_web_ingest_run_pages(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<Vec<WebDiscoveredPage>>, ApiError> {
    let run = state.canonical_services.web_ingest.get_run(&state, run_id).await?;
    let _library =
        load_library_and_authorize(&auth, &state, run.library_id, POLICY_LIBRARY_READ).await?;
    let pages = state.canonical_services.web_ingest.list_pages(&state, run_id).await?;
    Ok(Json(pages))
}

pub(super) async fn cancel_web_ingest_run(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<(StatusCode, Json<WebIngestRunReceipt>), ApiError> {
    let run = state.canonical_services.web_ingest.get_run(&state, run_id).await?;
    let _library =
        load_library_and_authorize(&auth, &state, run.library_id, POLICY_LIBRARY_WRITE).await?;
    let receipt = state.canonical_services.web_ingest.cancel_run(&state, run_id).await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}
