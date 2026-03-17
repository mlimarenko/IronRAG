use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::ModelPricingCatalogEntryRow,
    interfaces::http::{
        auth::AuthContext, authorization::POLICY_PROVIDERS_ADMIN, router_support::ApiError,
    },
    services::pricing_catalog,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PricingCatalogQuery {
    workspace_id: Option<Uuid>,
    provider_kind: Option<String>,
    model_name: Option<String>,
    capability: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertPricingEntryRequest {
    workspace_id: Option<Uuid>,
    provider_kind: String,
    model_name: String,
    capability: String,
    billing_unit: String,
    input_price: Option<f64>,
    output_price: Option<f64>,
    currency: String,
    note: Option<String>,
    effective_from: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PricingCatalogResponse {
    rows: Vec<PricingCatalogEntryResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PricingCatalogEntryResponse {
    id: Uuid,
    workspace_id: Option<Uuid>,
    provider_kind: String,
    model_name: String,
    capability: String,
    billing_unit: String,
    input_price: Option<String>,
    output_price: Option<String>,
    currency: String,
    status: String,
    source_kind: String,
    note: Option<String>,
    effective_from: DateTime<Utc>,
    effective_to: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route(
            "/runtime/admin/model-pricing",
            axum::routing::get(list_model_pricing).post(create_model_pricing),
        )
        .route(
            "/runtime/admin/model-pricing/{pricing_id}",
            axum::routing::put(update_model_pricing).delete(delete_model_pricing),
        )
}

async fn list_model_pricing(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<PricingCatalogQuery>,
) -> Result<Json<PricingCatalogResponse>, ApiError> {
    auth.require_any_scope(POLICY_PROVIDERS_ADMIN)?;
    let rows = pricing_catalog::list_pricing_entries_filtered(
        &state,
        pricing_catalog::PricingCatalogFilters {
            workspace_id: query.workspace_id,
            provider_kind: query.provider_kind,
            model_name: query.model_name,
            capability: query.capability,
            billing_unit: None,
            status: query.status,
        },
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let rows = rows.into_iter().map(map_pricing_row).collect();
    Ok(Json(PricingCatalogResponse { rows }))
}

async fn create_model_pricing(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<UpsertPricingEntryRequest>,
) -> Result<(StatusCode, Json<PricingCatalogEntryResponse>), ApiError> {
    auth.require_any_scope(POLICY_PROVIDERS_ADMIN)?;
    let row = pricing_catalog::create_pricing_entry(&state, normalize_upsert_request(payload)?)
        .await
        .map_err(map_pricing_error)?;
    Ok((StatusCode::CREATED, Json(map_pricing_row(row))))
}

async fn update_model_pricing(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(pricing_id): Path<Uuid>,
    Json(payload): Json<UpsertPricingEntryRequest>,
) -> Result<Json<PricingCatalogEntryResponse>, ApiError> {
    auth.require_any_scope(POLICY_PROVIDERS_ADMIN)?;
    let row = pricing_catalog::update_pricing_entry(
        &state,
        pricing_id,
        normalize_upsert_request(payload)?,
    )
    .await
    .map_err(map_pricing_error)?;
    let Some(row) = row else {
        return Err(ApiError::NotFound("pricing entry not found".into()));
    };
    Ok(Json(map_pricing_row(row)))
}

async fn delete_model_pricing(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(pricing_id): Path<Uuid>,
) -> Result<Json<PricingCatalogEntryResponse>, ApiError> {
    auth.require_any_scope(POLICY_PROVIDERS_ADMIN)?;
    let row = pricing_catalog::deactivate_pricing_entry(&state, pricing_id)
        .await
        .map_err(map_pricing_error)?
        .ok_or_else(|| ApiError::NotFound("pricing entry not found".into()))?;
    Ok(Json(map_pricing_row(row)))
}

fn normalize_upsert_request(
    payload: UpsertPricingEntryRequest,
) -> Result<pricing_catalog::UpsertPricingCatalogEntry, ApiError> {
    let provider_kind = payload.provider_kind.trim().to_lowercase();
    let model_name = payload.model_name.trim().to_string();
    let capability = payload.capability.trim().to_lowercase();
    let billing_unit = payload.billing_unit.trim().to_lowercase();
    let currency = payload.currency.trim().to_uppercase();
    if provider_kind.is_empty()
        || model_name.is_empty()
        || capability.is_empty()
        || billing_unit.is_empty()
    {
        return Err(ApiError::BadRequest(
            "provider, model, capability, and billing unit are required".into(),
        ));
    }
    if currency.is_empty() {
        return Err(ApiError::BadRequest("currency is required".into()));
    }

    Ok(pricing_catalog::UpsertPricingCatalogEntry {
        workspace_id: payload.workspace_id,
        provider_kind,
        model_name,
        capability,
        billing_unit,
        input_price: payload
            .input_price
            .map(|value| {
                Decimal::from_f64_retain(value)
                    .context("invalid input price")
                    .map_err(|_| ApiError::BadRequest("invalid input price".into()))
            })
            .transpose()?,
        output_price: payload
            .output_price
            .map(|value| {
                Decimal::from_f64_retain(value)
                    .context("invalid output price")
                    .map_err(|_| ApiError::BadRequest("invalid output price".into()))
            })
            .transpose()?,
        currency,
        source_kind: "manual".to_string(),
        note: payload.note.map(|note| note.trim().to_string()).filter(|note| !note.is_empty()),
        effective_from: payload.effective_from,
    })
}

fn map_pricing_row(row: ModelPricingCatalogEntryRow) -> PricingCatalogEntryResponse {
    PricingCatalogEntryResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        provider_kind: row.provider_kind,
        model_name: row.model_name,
        capability: row.capability,
        billing_unit: row.billing_unit,
        input_price: row.input_price.map(|value| value.normalize().to_string()),
        output_price: row.output_price.map(|value| value.normalize().to_string()),
        currency: row.currency,
        status: row.status,
        source_kind: row.source_kind,
        note: row.note,
        effective_from: row.effective_from,
        effective_to: row.effective_to,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_pricing_error(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    let normalized = message.to_ascii_lowercase();
    if message.to_ascii_lowercase().contains("missing price") {
        return ApiError::MissingPrice(message);
    }
    if normalized.contains("already exists") || normalized.contains("overlap") {
        return ApiError::Conflict(message);
    }
    if normalized.contains("required")
        || normalized.contains("invalid")
        || normalized.contains("must be later")
        || normalized.contains("identity cannot change")
        || normalized.contains("only active pricing entries")
    {
        return ApiError::BadRequest(message);
    }
    ApiError::Internal
}
