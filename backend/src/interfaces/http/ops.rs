use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::KnowledgeLibraryGeneration,
    domains::ops::{OpsLibraryState, OpsLibraryWarning},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_USAGE_READ, load_async_operation_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpsLibraryStateResponse {
    pub state: OpsLibraryState,
    pub knowledge_generations: Vec<KnowledgeLibraryGeneration>,
    pub warnings: Vec<OpsLibraryWarning>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ops/operations/{operation_id}", get(get_async_operation))
        .route("/ops/libraries/{library_id}", get(get_library_state))
}

async fn get_async_operation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(operation_id): Path<Uuid>,
) -> Result<Json<crate::domains::ops::OpsAsyncOperation>, ApiError> {
    let _ =
        load_async_operation_and_authorize(&auth, &state, operation_id, POLICY_USAGE_READ).await?;
    let operation = state.canonical_services.ops.get_async_operation(&state, operation_id).await?;
    Ok(Json(operation))
}

async fn get_library_state(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<OpsLibraryStateResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let snapshot =
        state.canonical_services.ops.get_library_state_snapshot(&state, library_id).await?;
    let warnings = state.canonical_services.ops.list_library_warnings(&state, library_id).await?;
    Ok(Json(OpsLibraryStateResponse {
        state: snapshot.state,
        knowledge_generations: snapshot.knowledge_generations,
        warnings,
    }))
}
