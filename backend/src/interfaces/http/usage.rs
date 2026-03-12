use axum::{
    Json, Router,
    extract::{Path, Query, State},
};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
};

#[derive(Deserialize)]
pub struct ProjectScopedQuery {
    pub project_id: Option<Uuid>,
}

#[derive(Serialize)]
pub struct UsageEventSummary {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_account_id: Option<Uuid>,
    pub model_profile_id: Option<Uuid>,
    pub usage_kind: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub raw_usage_json: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub struct CostLedgerSummary {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub usage_event_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub currency: String,
    pub estimated_cost: f64,
    pub pricing_snapshot_json: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub struct UsageCostTotals {
    pub project_id: Option<Uuid>,
    pub usage_events: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub estimated_cost: f64,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/usage-events", axum::routing::get(list_usage_events))
        .route("/usage-events/{id}", axum::routing::get(get_usage_event))
        .route("/cost-ledger", axum::routing::get(list_cost_ledger))
        .route("/cost-ledger/{id}", axum::routing::get(get_cost_ledger))
        .route("/usage-summary", axum::routing::get(get_usage_summary))
}

async fn list_usage_events(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<Vec<UsageEventSummary>>, ApiError> {
    auth.require_any_scope(&["usage:read", "workspace:admin"])?;

    let items = repositories::list_usage_events(&state.persistence.postgres, query.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(map_usage_event)
        .collect();

    Ok(Json(items))
}

async fn get_usage_event(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<UsageEventSummary>, ApiError> {
    auth.require_any_scope(&["usage:read", "workspace:admin"])?;

    let row = repositories::get_usage_event_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("usage_event {id} not found")))?;

    Ok(Json(map_usage_event(row)))
}

async fn list_cost_ledger(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<Vec<CostLedgerSummary>>, ApiError> {
    auth.require_any_scope(&["usage:read", "workspace:admin"])?;

    let items = repositories::list_cost_ledger(&state.persistence.postgres, query.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(map_cost_ledger)
        .collect();

    Ok(Json(items))
}

async fn get_cost_ledger(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CostLedgerSummary>, ApiError> {
    auth.require_any_scope(&["usage:read", "workspace:admin"])?;

    let row = repositories::get_cost_ledger_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("cost_ledger {id} not found")))?;

    Ok(Json(map_cost_ledger(row)))
}

async fn get_usage_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<UsageCostTotals>, ApiError> {
    auth.require_any_scope(&["usage:read", "workspace:admin"])?;

    let totals = repositories::get_usage_cost_totals(&state.persistence.postgres, query.project_id)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(UsageCostTotals {
        project_id: query.project_id,
        usage_events: totals.usage_events,
        prompt_tokens: totals.prompt_tokens.unwrap_or(0),
        completion_tokens: totals.completion_tokens.unwrap_or(0),
        total_tokens: totals.total_tokens.unwrap_or(0),
        estimated_cost: decimal_to_f64(totals.estimated_cost),
    }))
}

fn map_usage_event(row: repositories::UsageEventRow) -> UsageEventSummary {
    UsageEventSummary {
        id: row.id,
        workspace_id: row.workspace_id,
        project_id: row.project_id,
        provider_account_id: row.provider_account_id,
        model_profile_id: row.model_profile_id,
        usage_kind: row.usage_kind,
        prompt_tokens: row.prompt_tokens,
        completion_tokens: row.completion_tokens,
        total_tokens: row.total_tokens,
        raw_usage_json: row.raw_usage_json,
        created_at: row.created_at,
    }
}

fn map_cost_ledger(row: repositories::CostLedgerRow) -> CostLedgerSummary {
    CostLedgerSummary {
        id: row.id,
        workspace_id: row.workspace_id,
        project_id: row.project_id,
        usage_event_id: row.usage_event_id,
        provider_kind: row.provider_kind,
        model_name: row.model_name,
        currency: row.currency,
        estimated_cost: decimal_to_f64(row.estimated_cost),
        pricing_snapshot_json: row.pricing_snapshot_json,
        created_at: row.created_at,
    }
}

fn decimal_to_f64(value: rust_decimal::Decimal) -> f64 {
    value.to_f64().unwrap_or_default()
}
