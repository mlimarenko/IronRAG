pub mod types;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use uuid::Uuid;

use self::types::{
    BillingChargePage, BillingPageQuery, BillingProviderCallPage, DocumentCostPage,
    decode_keyset_cursor, decode_offset_cursor, encode_keyset_cursor, encode_offset_cursor,
};
use crate::{
    app::state::AppState,
    domains::billing::{BillingExecutionCost, BillingExecutionOwnerKind},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_USAGE_READ, load_library_and_authorize, load_workspace_and_authorize,
        },
        router_support::ApiError,
    },
    services::ops::billing::{LibraryCostSummary, WorkspaceCostSummary},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/billing/executions/{execution_kind}/{execution_id}/provider-calls",
            get(list_provider_calls),
        )
        .route("/billing/executions/{execution_kind}/{execution_id}/charges", get(list_charges))
        .route("/billing/executions/{execution_kind}/{execution_id}", get(get_execution_cost))
        .route("/billing/libraries/{library_id}/document-costs", get(list_library_document_costs))
        .route("/billing/libraries/{library_id}/cost-summary", get(get_library_cost_summary))
        .route("/billing/workspaces/{workspace_id}/cost-summary", get(get_workspace_cost_summary))
}

#[utoipa::path(
    get,
    path = "/v1/billing/executions/{executionKind}/{executionId}/provider-calls",
    tag = "billing",
    operation_id = "listBillingProviderCalls",
    params(
        ("executionKind" = BillingExecutionOwnerKind, Path, description = "Kind of execution the provider calls are attributed to"),
        ("executionId" = uuid::Uuid, Path, description = "Execution identifier"),
        BillingPageQuery,
    ),
    responses(
        (status = 200, description = "Page of provider calls billed against the execution", body = BillingProviderCallPage),
        (status = 400, description = "Invalid cursor"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the execution's library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_provider_calls",
    skip_all,
    fields(execution_kind = execution_kind.as_str(), execution_id = %execution_id, item_count)
)]
pub async fn list_provider_calls(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((execution_kind, execution_id)): Path<(BillingExecutionOwnerKind, Uuid)>,
    Query(query): Query<BillingPageQuery>,
) -> Result<Json<BillingProviderCallPage>, ApiError> {
    let span = tracing::Span::current();
    let library_id = state
        .canonical_services
        .billing
        .resolve_execution_library_id(&state, execution_kind.as_str(), execution_id)
        .await?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let cursor = query.cursor.as_deref().map(decode_keyset_cursor).transpose()?;
    let page = state
        .canonical_services
        .billing
        .list_execution_provider_calls_page(
            &state,
            execution_kind,
            execution_id,
            cursor,
            i64::from(query.limit()),
            query.include_total(),
        )
        .await?;
    span.record("item_count", page.items.len());
    let next_cursor = page
        .has_more
        .then(|| page.items.last().map(|item| encode_keyset_cursor(item.started_at, item.id)))
        .flatten();
    Ok(Json(BillingProviderCallPage { items: page.items, next_cursor, total: page.total }))
}

#[utoipa::path(
    get,
    path = "/v1/billing/executions/{executionKind}/{executionId}/charges",
    tag = "billing",
    operation_id = "listBillingCharges",
    params(
        ("executionKind" = BillingExecutionOwnerKind, Path, description = "Kind of execution the charges are attributed to"),
        ("executionId" = uuid::Uuid, Path, description = "Execution identifier"),
        BillingPageQuery,
    ),
    responses(
        (status = 200, description = "Page of aggregated billing charges for the execution", body = BillingChargePage),
        (status = 400, description = "Invalid cursor"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the execution's library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_charges",
    skip_all,
    fields(execution_kind = execution_kind.as_str(), execution_id = %execution_id, item_count)
)]
pub async fn list_charges(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((execution_kind, execution_id)): Path<(BillingExecutionOwnerKind, Uuid)>,
    Query(query): Query<BillingPageQuery>,
) -> Result<Json<BillingChargePage>, ApiError> {
    let span = tracing::Span::current();
    let library_id = state
        .canonical_services
        .billing
        .resolve_execution_library_id(&state, execution_kind.as_str(), execution_id)
        .await?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let cursor = query.cursor.as_deref().map(decode_keyset_cursor).transpose()?;
    let page = state
        .canonical_services
        .billing
        .list_execution_charges_page(
            &state,
            execution_kind,
            execution_id,
            cursor,
            i64::from(query.limit()),
            query.include_total(),
        )
        .await?;
    span.record("item_count", page.items.len());
    let next_cursor = page
        .has_more
        .then(|| page.items.last().map(|item| encode_keyset_cursor(item.priced_at, item.id)))
        .flatten();
    Ok(Json(BillingChargePage { items: page.items, next_cursor, total: page.total }))
}

#[utoipa::path(
    get,
    path = "/v1/billing/executions/{executionKind}/{executionId}",
    tag = "billing",
    operation_id = "getBillingExecutionCost",
    params(
        ("executionKind" = BillingExecutionOwnerKind, Path, description = "Kind of execution the cost is attributed to"),
        ("executionId" = uuid::Uuid, Path, description = "Execution identifier"),
    ),
    responses(
        (status = 200, description = "Total billing cost for the execution", body = BillingExecutionCost),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the execution's library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_execution_cost",
    skip_all,
    fields(execution_kind = execution_kind.as_str(), execution_id = %execution_id)
)]
pub async fn get_execution_cost(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((execution_kind, execution_id)): Path<(BillingExecutionOwnerKind, Uuid)>,
) -> Result<Json<BillingExecutionCost>, ApiError> {
    let library_id = state
        .canonical_services
        .billing
        .resolve_execution_library_id(&state, execution_kind.as_str(), execution_id)
        .await?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let cost = state
        .canonical_services
        .billing
        .get_execution_cost(&state, execution_kind.as_str(), execution_id)
        .await?;
    Ok(Json(cost))
}

#[utoipa::path(
    get,
    path = "/v1/billing/libraries/{libraryId}/document-costs",
    tag = "billing",
    operation_id = "listBillingLibraryDocumentCosts",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library that owns the documents"),
        BillingPageQuery,
    ),
    responses(
        (status = 200, description = "Page of per-document cost summaries for the library", body = DocumentCostPage),
        (status = 400, description = "Invalid cursor"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_library_document_costs",
    skip_all,
    fields(library_id = %library_id, item_count)
)]
pub async fn list_library_document_costs(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<BillingPageQuery>,
) -> Result<Json<DocumentCostPage>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let offset = query.cursor.as_deref().map(decode_offset_cursor).transpose()?.unwrap_or(0);
    let limit = query.limit();
    let page = state
        .canonical_services
        .billing
        .list_document_costs_for_library_page(
            &state,
            library_id,
            offset,
            limit as usize,
            query.include_total(),
        )
        .await?;
    span.record("item_count", page.items.len());
    let next_cursor = page.has_more.then(|| encode_offset_cursor(offset + page.items.len()));
    Ok(Json(DocumentCostPage { items: page.items, next_cursor, total: page.total }))
}

#[utoipa::path(
    get,
    path = "/v1/billing/libraries/{libraryId}/cost-summary",
    tag = "billing",
    operation_id = "getLibraryCostSummary",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library the cost summary is scoped to"),
    ),
    responses(
        (status = 200, description = "Aggregated cost summary for the library", body = LibraryCostSummary),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_library_cost_summary",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn get_library_cost_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<LibraryCostSummary>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let summary =
        state.canonical_services.billing.get_library_cost_summary(&state, library_id).await?;
    Ok(Json(summary))
}

#[utoipa::path(
    get,
    path = "/v1/billing/workspaces/{workspaceId}/cost-summary",
    tag = "billing",
    operation_id = "getWorkspaceCostSummary",
    params(
        ("workspaceId" = uuid::Uuid, Path, description = "Workspace the cost summary is scoped to"),
    ),
    responses(
        (status = 200, description = "Aggregated cost summary for the workspace", body = WorkspaceCostSummary),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the workspace"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_workspace_cost_summary",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
pub async fn get_workspace_cost_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<WorkspaceCostSummary>, ApiError> {
    let _ = load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_USAGE_READ).await?;
    let summary =
        state.canonical_services.billing.get_workspace_cost_summary(&state, workspace_id).await?;
    Ok(Json(summary))
}
