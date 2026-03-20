use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_USAGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BillingLibraryQuery {
    library_id: Option<Uuid>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/billing/provider-calls", get(list_provider_calls))
        .route("/billing/charges", get(list_charges))
        .route("/billing/executions/{execution_kind}/{execution_id}", get(get_execution_cost))
}

async fn list_provider_calls(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<BillingLibraryQuery>,
) -> Result<Json<Vec<crate::domains::billing::BillingProviderCall>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let calls = state.canonical_services.billing.list_provider_calls(&state, library_id).await?;
    Ok(Json(calls))
}

async fn list_charges(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<BillingLibraryQuery>,
) -> Result<Json<Vec<crate::domains::billing::BillingCharge>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let charges = state.canonical_services.billing.list_charges(&state, library_id).await?;
    Ok(Json(charges))
}

async fn get_execution_cost(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((execution_kind, execution_id)): Path<(String, Uuid)>,
) -> Result<Json<crate::domains::billing::BillingExecutionCost>, ApiError> {
    let library_id = state
        .canonical_services
        .billing
        .resolve_execution_library_id(&state, &execution_kind, execution_id)
        .await?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let cost = state
        .canonical_services
        .billing
        .get_execution_cost(&state, &execution_kind, execution_id)
        .await?;
    Ok(Json(cost))
}
