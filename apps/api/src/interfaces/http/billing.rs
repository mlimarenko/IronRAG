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
        authorization::{
            POLICY_USAGE_READ, load_library_and_authorize, load_workspace_and_authorize,
        },
        router_support::ApiError,
    },
    services::ops::billing::{DocumentCostSummary, LibraryCostSummary, WorkspaceCostSummary},
};

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct LibraryCostQuery {
    pub library_id: Uuid,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkspaceCostQuery {
    pub workspace_id: Uuid,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/billing/executions/{execution_kind}/{execution_id}/provider-calls",
            get(list_provider_calls),
        )
        .route("/billing/executions/{execution_kind}/{execution_id}/charges", get(list_charges))
        .route("/billing/executions/{execution_kind}/{execution_id}", get(get_execution_cost))
        .route("/billing/library-document-costs", get(list_library_document_costs))
        .route("/billing/library-cost-summary", get(get_library_cost_summary))
        .route("/billing/workspace-cost-summary", get(get_workspace_cost_summary))
}

#[utoipa::path(
    get,
    path = "/v1/billing/executions/{executionKind}/{executionId}/provider-calls",
    tag = "billing",
    operation_id = "listBillingProviderCalls",
    params(
        ("executionKind" = String, Path, description = "Execution kind (query | runtime)"),
        ("executionId" = uuid::Uuid, Path, description = "Execution identifier"),
    ),
    responses(
        (status = 200, description = "Provider calls billed against the execution", body = [crate::domains::billing::BillingProviderCall]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the execution's library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_provider_calls",
    skip_all,
    fields(execution_kind = %execution_kind, execution_id = %execution_id)
)]
pub async fn list_provider_calls(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((execution_kind, execution_id)): Path<(String, Uuid)>,
) -> Result<Json<Vec<crate::domains::billing::BillingProviderCall>>, ApiError> {
    let library_id = state
        .canonical_services
        .billing
        .resolve_execution_library_id(&state, &execution_kind, execution_id)
        .await?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let calls = state
        .canonical_services
        .billing
        .list_execution_provider_calls(&state, &execution_kind, execution_id)
        .await?;
    Ok(Json(calls))
}

#[utoipa::path(
    get,
    path = "/v1/billing/executions/{executionKind}/{executionId}/charges",
    tag = "billing",
    operation_id = "listBillingCharges",
    params(
        ("executionKind" = String, Path, description = "Execution kind (query | runtime)"),
        ("executionId" = uuid::Uuid, Path, description = "Execution identifier"),
    ),
    responses(
        (status = 200, description = "Aggregated billing charges for the execution", body = [crate::domains::billing::BillingCharge]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the execution's library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_charges",
    skip_all,
    fields(execution_kind = %execution_kind, execution_id = %execution_id)
)]
pub async fn list_charges(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((execution_kind, execution_id)): Path<(String, Uuid)>,
) -> Result<Json<Vec<crate::domains::billing::BillingCharge>>, ApiError> {
    let library_id = state
        .canonical_services
        .billing
        .resolve_execution_library_id(&state, &execution_kind, execution_id)
        .await?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let charges = state
        .canonical_services
        .billing
        .list_execution_charges(&state, &execution_kind, execution_id)
        .await?;
    Ok(Json(charges))
}

#[utoipa::path(
    get,
    path = "/v1/billing/executions/{executionKind}/{executionId}",
    tag = "billing",
    operation_id = "getBillingExecutionCost",
    params(
        ("executionKind" = String, Path, description = "Execution kind (query | runtime)"),
        ("executionId" = uuid::Uuid, Path, description = "Execution identifier"),
    ),
    responses(
        (status = 200, description = "Total billing cost for the execution", body = crate::domains::billing::BillingExecutionCost),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the execution's library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_execution_cost",
    skip_all,
    fields(execution_kind = %execution_kind, execution_id = %execution_id)
)]
pub async fn get_execution_cost(
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

#[utoipa::path(
    get,
    path = "/v1/billing/library-document-costs",
    tag = "billing",
    operation_id = "listBillingLibraryDocumentCosts",
    params(LibraryCostQuery),
    responses(
        (status = 200, description = "Per-document cost summaries for the library", body = [crate::services::ops::billing::DocumentCostSummary]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_library_document_costs",
    skip_all,
    fields(library_id = %query.library_id, item_count)
)]
pub async fn list_library_document_costs(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<LibraryCostQuery>,
) -> Result<Json<Vec<DocumentCostSummary>>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_library_and_authorize(&auth, &state, query.library_id, POLICY_USAGE_READ).await?;
    let costs = state
        .canonical_services
        .billing
        .list_document_costs_for_library(&state, query.library_id)
        .await?;
    span.record("item_count", costs.len());
    Ok(Json(costs))
}

#[utoipa::path(
    get,
    path = "/v1/billing/library-cost-summary",
    tag = "billing",
    operation_id = "getLibraryCostSummary",
    params(LibraryCostQuery),
    responses(
        (status = 200, description = "Aggregated cost summary for the library", body = crate::services::ops::billing::LibraryCostSummary),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_library_cost_summary",
    skip_all,
    fields(library_id = %query.library_id)
)]
pub async fn get_library_cost_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<LibraryCostQuery>,
) -> Result<Json<LibraryCostSummary>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, query.library_id, POLICY_USAGE_READ).await?;
    let summary =
        state.canonical_services.billing.get_library_cost_summary(&state, query.library_id).await?;
    Ok(Json(summary))
}

#[utoipa::path(
    get,
    path = "/v1/billing/workspace-cost-summary",
    tag = "billing",
    operation_id = "getWorkspaceCostSummary",
    params(WorkspaceCostQuery),
    responses(
        (status = 200, description = "Aggregated cost summary for the workspace", body = crate::services::ops::billing::WorkspaceCostSummary),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the workspace"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_workspace_cost_summary",
    skip_all,
    fields(workspace_id = %query.workspace_id)
)]
pub async fn get_workspace_cost_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<WorkspaceCostQuery>,
) -> Result<Json<WorkspaceCostSummary>, ApiError> {
    let _ =
        load_workspace_and_authorize(&auth, &state, query.workspace_id, POLICY_USAGE_READ).await?;
    let summary = state
        .canonical_services
        .billing
        .get_workspace_cost_summary(&state, query.workspace_id)
        .await?;
    Ok(Json(summary))
}
