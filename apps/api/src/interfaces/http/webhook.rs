//! HTTP routes for the outbound webhook subsystem.
//!
//! ## Outbound subscriptions CRUD (requires POLICY_WORKSPACE_ADMIN)
//!
//! `POST   /v1/webhooks/subscriptions`
//! `GET    /v1/webhooks/subscriptions`
//! `GET    /v1/webhooks/subscriptions/{id}`
//! `PATCH  /v1/webhooks/subscriptions/{id}`
//! `DELETE /v1/webhooks/subscriptions/{id}`
//! `GET    /v1/webhooks/subscriptions/{id}/attempts`

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, patch, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::webhook_repository::{
        self, NewWebhookSubscription, UpdateWebhookSubscription,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_WORKSPACE_ADMIN, load_workspace_and_authorize},
        router_support::ApiError,
    },
    services::webhook::ssrf,
};

pub fn router() -> Router<AppState> {
    Router::new()
        // Outbound subscription management
        .route("/webhooks/subscriptions", post(create_subscription))
        .route("/webhooks/subscriptions", get(list_subscriptions))
        .route("/webhooks/subscriptions/{id}", get(get_subscription))
        .route("/webhooks/subscriptions/{id}", patch(update_subscription))
        .route("/webhooks/subscriptions/{id}", delete(delete_subscription))
        .route("/webhooks/subscriptions/{id}/attempts", get(list_delivery_attempts))
}

// ============================================================================
// Outbound subscription management
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubscriptionRequest {
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub target_url: String,
    pub secret: String,
    pub event_types: Vec<String>,
    #[serde(default)]
    pub custom_headers: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSubscriptionRequest {
    pub display_name: Option<String>,
    pub target_url: Option<String>,
    pub secret: Option<String>,
    pub event_types: Option<Vec<String>>,
    pub custom_headers: Option<serde_json::Value>,
    pub active: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub target_url: String,
    pub event_types: Vec<String>,
    pub active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSubscriptionsQuery {
    pub workspace_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAttemptsQuery {
    pub state: Option<String>,
}

async fn create_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<CreateSubscriptionRequest>,
) -> Result<(StatusCode, Json<SubscriptionResponse>), ApiError> {
    load_workspace_and_authorize(&auth, &state, req.workspace_id, POLICY_WORKSPACE_ADMIN).await?;

    // Validate required fields before touching the DB to avoid 5xx from CHECK constraints.
    if req.event_types.is_empty() {
        return Err(ApiError::BadRequest("event_types must be non-empty".to_string()));
    }
    if !(req.target_url.starts_with("http://") || req.target_url.starts_with("https://")) {
        return Err(ApiError::BadRequest(
            "target_url must start with http:// or https://".to_string(),
        ));
    }

    // SSRF protection: reject private or loopback target addresses.
    ssrf::validate_target_url(&req.target_url).await.map_err(ApiError::BadRequest)?;

    let row = webhook_repository::create_webhook_subscription(
        &state.persistence.postgres,
        &NewWebhookSubscription {
            workspace_id: req.workspace_id,
            library_id: req.library_id,
            display_name: req.display_name,
            target_url: req.target_url,
            secret: req.secret,
            event_types: req.event_types,
            custom_headers_json: req.custom_headers,
            created_by_principal_id: Some(auth.principal_id),
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok((StatusCode::CREATED, Json(subscription_row_to_response(row))))
}

async fn list_subscriptions(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(q): Query<ListSubscriptionsQuery>,
) -> Result<Json<Vec<SubscriptionResponse>>, ApiError> {
    load_workspace_and_authorize(&auth, &state, q.workspace_id, POLICY_WORKSPACE_ADMIN).await?;

    let rows = webhook_repository::list_webhook_subscriptions_by_workspace(
        &state.persistence.postgres,
        q.workspace_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok(Json(rows.into_iter().map(subscription_row_to_response).collect()))
}

async fn get_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<SubscriptionResponse>, ApiError> {
    let row = webhook_repository::get_webhook_subscription_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("webhook_subscription", id))?;

    load_workspace_and_authorize(&auth, &state, row.workspace_id, POLICY_WORKSPACE_ADMIN).await?;

    Ok(Json(subscription_row_to_response(row)))
}

async fn update_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSubscriptionRequest>,
) -> Result<Json<SubscriptionResponse>, ApiError> {
    let existing =
        webhook_repository::get_webhook_subscription_by_id(&state.persistence.postgres, id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("webhook_subscription", id))?;

    load_workspace_and_authorize(&auth, &state, existing.workspace_id, POLICY_WORKSPACE_ADMIN)
        .await?;

    // Validate supplied fields before touching the DB.
    if let Some(ref et) = req.event_types {
        if et.is_empty() {
            return Err(ApiError::BadRequest("event_types must be non-empty".to_string()));
        }
    }
    if let Some(ref url) = req.target_url {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ApiError::BadRequest(
                "target_url must start with http:// or https://".to_string(),
            ));
        }
        // SSRF protection on updated target_url.
        ssrf::validate_target_url(url).await.map_err(ApiError::BadRequest)?;
    }

    let row = webhook_repository::update_webhook_subscription(
        &state.persistence.postgres,
        id,
        &UpdateWebhookSubscription {
            display_name: req.display_name,
            target_url: req.target_url,
            secret: req.secret,
            event_types: req.event_types,
            custom_headers_json: req.custom_headers,
            active: req.active,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("webhook_subscription", id))?;

    Ok(Json(subscription_row_to_response(row)))
}

async fn delete_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let existing =
        webhook_repository::get_webhook_subscription_by_id(&state.persistence.postgres, id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("webhook_subscription", id))?;

    load_workspace_and_authorize(&auth, &state, existing.workspace_id, POLICY_WORKSPACE_ADMIN)
        .await?;

    webhook_repository::delete_webhook_subscription(&state.persistence.postgres, id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok(StatusCode::NO_CONTENT)
}

async fn list_delivery_attempts(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(id): Path<Uuid>,
    Query(q): Query<ListAttemptsQuery>,
) -> Result<Json<Vec<DeliveryAttemptResponse>>, ApiError> {
    let sub = webhook_repository::get_webhook_subscription_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("webhook_subscription", id))?;

    load_workspace_and_authorize(&auth, &state, sub.workspace_id, POLICY_WORKSPACE_ADMIN).await?;

    let rows = webhook_repository::list_webhook_delivery_attempts_by_subscription(
        &state.persistence.postgres,
        id,
        q.state.as_deref(),
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    let resp = rows
        .into_iter()
        .map(|r| DeliveryAttemptResponse {
            id: r.id,
            subscription_id: r.subscription_id,
            event_type: r.event_type,
            event_id: r.event_id,
            target_url: r.target_url,
            attempt_number: r.attempt_number,
            delivery_state: r.delivery_state,
            response_status: r.response_status,
            error_message: r.error_message,
            delivered_at: r.delivered_at,
            next_attempt_at: r.next_attempt_at,
            created_at: r.created_at,
        })
        .collect();

    Ok(Json(resp))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryAttemptResponse {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event_type: String,
    pub event_id: String,
    pub target_url: String,
    pub attempt_number: i32,
    pub delivery_state: String,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub delivered_at: Option<chrono::DateTime<chrono::Utc>>,
    pub next_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

fn subscription_row_to_response(
    row: webhook_repository::WebhookSubscriptionRow,
) -> SubscriptionResponse {
    SubscriptionResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        display_name: row.display_name,
        target_url: row.target_url,
        event_types: row.event_types,
        active: row.active,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}
