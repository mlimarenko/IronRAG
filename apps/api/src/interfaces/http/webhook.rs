//! HTTP routes for the outbound webhook subsystem.
//!
//! ## Outbound subscriptions CRUD (requires `POLICY_WORKSPACE_ADMIN`)
//!
//! `POST   /v1/webhooks/workspaces/{workspaceId}/subscriptions`
//! `GET    /v1/webhooks/workspaces/{workspaceId}/subscriptions`
//! `GET    /v1/webhooks/subscriptions/{subscriptionId}`
//! `PATCH  /v1/webhooks/subscriptions/{subscriptionId}`
//! `DELETE /v1/webhooks/subscriptions/{subscriptionId}`
//! `GET    /v1/webhooks/subscriptions/{subscriptionId}/attempts`
//!
//! Inbound vendor events intentionally have no placeholder route. Connector
//! middleware uses the authenticated upload/replace/delete APIs until a real
//! durable receiver contract exists.

use std::collections::HashSet;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize as _;

const MAX_WEBHOOK_DISPLAY_NAME_CHARS: usize = 128;
const MAX_WEBHOOK_TARGET_URL_BYTES: usize = 2_048;
const MIN_WEBHOOK_SIGNING_SECRET_BYTES: usize = 32;
const MAX_WEBHOOK_EVENT_TYPES: usize = 8;
const DEFAULT_WEBHOOK_SUBSCRIPTION_PAGE_SIZE: u32 = 100;
const DEFAULT_WEBHOOK_ATTEMPT_PAGE_SIZE: u32 = 200;
const SUPPORTED_WEBHOOK_EVENT_TYPES: &[&str] = &["document.deleted", "revision.ready"];
const SUPPORTED_WEBHOOK_DELIVERY_STATES: &[&str] =
    &["pending", "delivering", "delivered", "failed", "abandoned"];

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
    services::webhook::{custom_headers, ssrf},
    shared::secret_encryption::SecretPurpose,
};

pub fn router() -> Router<AppState> {
    Router::new()
        // Outbound subscription management
        .route(
            "/webhooks/workspaces/{workspace_id}/subscriptions",
            post(create_subscription).get(list_subscriptions),
        )
        .route("/webhooks/subscriptions/{subscription_id}", get(get_subscription))
        .route("/webhooks/subscriptions/{subscription_id}", patch(update_subscription))
        .route("/webhooks/subscriptions/{subscription_id}", delete(delete_subscription))
        .route("/webhooks/subscriptions/{subscription_id}/attempts", get(list_delivery_attempts))
}

fn scrub_plaintext_secret(secret: &mut String) {
    secret.zeroize();
}

fn empty_custom_headers() -> serde_json::Value {
    serde_json::json!({})
}

fn validate_display_name(display_name: &mut String) -> Result<(), ApiError> {
    let normalized = display_name.trim();
    if normalized.is_empty() || normalized.chars().count() > MAX_WEBHOOK_DISPLAY_NAME_CHARS {
        return Err(ApiError::BadRequest(format!(
            "display_name must contain 1 to {MAX_WEBHOOK_DISPLAY_NAME_CHARS} characters"
        )));
    }
    if normalized.len() != display_name.len() {
        *display_name = normalized.to_owned();
    }
    Ok(())
}

fn validate_target_url_syntax(target_url: &str) -> Result<(), ApiError> {
    if target_url.len() > MAX_WEBHOOK_TARGET_URL_BYTES {
        return Err(ApiError::BadRequest("target_url exceeds 2048 bytes".to_string()));
    }
    let parsed = reqwest::Url::parse(target_url)
        .map_err(|_| ApiError::BadRequest("target_url must be a valid HTTP(S) URL".to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ApiError::BadRequest("target_url must use http:// or https://".to_string()));
    }
    if parsed.host_str().is_none() {
        return Err(ApiError::BadRequest("target_url must include a host".to_string()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ApiError::BadRequest(
            "target_url must not contain credentials; use custom_headers".to_string(),
        ));
    }
    if parsed.fragment().is_some() {
        return Err(ApiError::BadRequest("target_url must not contain a fragment".to_string()));
    }
    Ok(())
}

fn validate_signing_secret(secret: &str) -> Result<(), ApiError> {
    if secret.len() < MIN_WEBHOOK_SIGNING_SECRET_BYTES {
        return Err(ApiError::BadRequest(
            "secret must contain at least 32 bytes of entropy-bearing material".to_string(),
        ));
    }
    Ok(())
}

fn validate_event_types(event_types: &[String]) -> Result<(), ApiError> {
    if event_types.is_empty() || event_types.len() > MAX_WEBHOOK_EVENT_TYPES {
        return Err(ApiError::BadRequest(format!(
            "event_types must contain 1 to {MAX_WEBHOOK_EVENT_TYPES} entries"
        )));
    }
    let mut seen = HashSet::with_capacity(event_types.len());
    for event_type in event_types {
        if !SUPPORTED_WEBHOOK_EVENT_TYPES.contains(&event_type.as_str()) {
            return Err(ApiError::BadRequest("event_types contains an unsupported event".into()));
        }
        if !seen.insert(event_type.as_str()) {
            return Err(ApiError::BadRequest("event_types must not contain duplicates".into()));
        }
    }
    Ok(())
}

async fn validate_library_scope(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(library_id) = library_id else {
        return Ok(());
    };
    let belongs_to_workspace = sqlx::query_scalar::<_, bool>(
        "select exists(
            select 1
            from catalog_library
            where id = $1 and workspace_id = $2
        )",
    )
    .bind(library_id)
    .bind(workspace_id)
    .fetch_one(&state.persistence.postgres)
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    if !belongs_to_workspace {
        return Err(ApiError::BadRequest(
            "library_id is not available in the authorized workspace".to_string(),
        ));
    }
    Ok(())
}

// ============================================================================
// Opaque cursor for webhook subscription and delivery-attempt keyset
// pagination.
//
// The cursor is base64(json({"t": "<rfc3339 created_at>", "i": "<uuid>"})),
// mirroring the content document list cursor
// (interfaces/http/content/types.rs) and the audit event list cursor
// (interfaces/http/audit.rs). Opaque to clients; any decode failure is a
// `BadRequest` rather than silently restarting from the top.
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
struct WebhookListCursor {
    #[serde(rename = "t")]
    created_at: DateTime<Utc>,
    #[serde(rename = "i")]
    id: Uuid,
}

fn encode_webhook_list_cursor(cursor: &WebhookListCursor) -> String {
    use base64::Engine;
    let json = serde_json::to_vec(cursor).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

fn decode_webhook_list_cursor(token: &str) -> Result<WebhookListCursor, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid cursor encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid cursor payload".to_string()))
}

fn validate_attempt_state_filter(state: Option<&str>) -> Result<(), ApiError> {
    if state.is_none_or(|state| SUPPORTED_WEBHOOK_DELIVERY_STATES.contains(&state)) {
        return Ok(());
    }
    Err(ApiError::BadRequest(
        "state must be one of pending, delivering, delivered, failed, abandoned".to_string(),
    ))
}

/// Returns the workspace-id predicate for a global webhook UUID lookup.
/// `None` represents a true system-wide administrator; an empty vector is an
/// intentional deny-all scope. Filtering happens in SQL before any row (and in
/// particular any target URL) is materialized.
fn webhook_admin_workspace_scope(auth: &AuthContext) -> Option<Vec<Uuid>> {
    if !auth.role_permits_write() {
        return Some(Vec::new());
    }
    let has_system_admin_grant = auth.grants.iter().any(|grant| {
        grant.resource_kind == "system"
            && POLICY_WORKSPACE_ADMIN.contains(&grant.permission_kind.as_str())
    });
    if auth.is_system_admin || has_system_admin_grant {
        return None;
    }
    Some(
        auth.visible_workspace_ids
            .iter()
            .copied()
            .filter(|workspace_id| {
                auth.has_workspace_permission(*workspace_id, POLICY_WORKSPACE_ADMIN)
            })
            .collect(),
    )
}

async fn load_authorized_subscription_view(
    state: &AppState,
    auth: &AuthContext,
    subscription_id: Uuid,
) -> Result<webhook_repository::WebhookSubscriptionViewRow, ApiError> {
    let workspace_scope = webhook_admin_workspace_scope(auth);
    webhook_repository::get_webhook_subscription_view_by_id_in_workspace_scope(
        &state.persistence.postgres,
        subscription_id,
        workspace_scope.as_deref(),
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("webhook_subscription", subscription_id))
}

fn map_webhook_subscription_write_error(error: sqlx::Error) -> ApiError {
    if webhook_repository::is_active_webhook_subscription_quota_error(&error) {
        ApiError::Conflict(format!(
            "workspace supports at most {} active webhook subscriptions",
            webhook_repository::MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE,
        ))
    } else if webhook_repository::is_total_webhook_subscription_quota_error(&error) {
        ApiError::Conflict(format!(
            "workspace supports at most {} webhook subscriptions in all states; delete inactive subscriptions before creating more",
            webhook_repository::MAX_TOTAL_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE,
        ))
    } else if webhook_repository::is_draining_webhook_subscription_reactivation_error(&error) {
        ApiError::Conflict("draining webhook subscription cannot be reactivated".to_string())
    } else {
        ApiError::internal_with_log(error, "internal")
    }
}

// ============================================================================
// Outbound subscription management
// ============================================================================

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubscriptionRequest {
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub target_url: String,
    pub secret: String,
    pub event_types: Vec<String>,
    #[serde(default = "empty_custom_headers")]
    pub custom_headers: serde_json::Value,
}

impl std::fmt::Debug for CreateSubscriptionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreateSubscriptionRequest")
            .field("library_id", &self.library_id)
            .field("display_name", &self.display_name)
            .field("target_url", &"<redacted>")
            .field("secret", &"<redacted>")
            .field("event_types", &self.event_types)
            .field("custom_headers", &"<redacted>")
            .finish()
    }
}

impl Drop for CreateSubscriptionRequest {
    fn drop(&mut self) {
        self.secret.zeroize();
        custom_headers::scrub_json_strings(&mut self.custom_headers);
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSubscriptionRequest {
    pub display_name: Option<String>,
    pub target_url: Option<String>,
    pub secret: Option<String>,
    pub event_types: Option<Vec<String>>,
    pub custom_headers: Option<serde_json::Value>,
    pub active: Option<bool>,
}

impl std::fmt::Debug for UpdateSubscriptionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateSubscriptionRequest")
            .field("display_name", &self.display_name)
            .field("target_url", &self.target_url.as_ref().map(|_| "<redacted>"))
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .field("event_types", &self.event_types)
            .field("custom_headers", &self.custom_headers.as_ref().map(|_| "<redacted>"))
            .field("active", &self.active)
            .finish()
    }
}

impl Drop for UpdateSubscriptionRequest {
    fn drop(&mut self) {
        if let Some(secret) = self.secret.as_mut() {
            secret.zeroize();
        }
        if let Some(custom_headers) = self.custom_headers.as_mut() {
            custom_headers::scrub_json_strings(custom_headers);
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
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

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListSubscriptionsQuery {
    /// Page size, clamped to 1..=200. Defaults to 100.
    pub limit: Option<u32>,
    /// Opaque keyset continuation token from a previous page's
    /// `nextCursor`. Absent starts from the oldest subscription.
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListAttemptsQuery {
    /// Optional delivery-state filter.
    pub state: Option<String>,
    /// Page size, clamped to 1..=200. Defaults to 200.
    pub limit: Option<u32>,
    /// Opaque keyset continuation token from a previous page's
    /// `nextCursor`. Absent starts from the newest attempt.
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionListPageResponse {
    pub items: Vec<SubscriptionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryAttemptListPageResponse {
    pub items: Vec<DeliveryAttemptResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[utoipa::path(
    post,
    path = "/v1/webhooks/workspaces/{workspaceId}/subscriptions",
    tag = "webhooks",
    operation_id = "createWebhookSubscription",
    params(("workspaceId" = uuid::Uuid, Path, description = "Workspace that owns the subscription")),
    request_body = CreateSubscriptionRequest,
    responses(
        (status = 201, description = "Created outbound webhook subscription", body = SubscriptionResponse),
        (status = 400, description = "Request body is invalid"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a workspace administrator"),
        (status = 409, description = "Workspace webhook subscription quota is exhausted"),
    ),
)]
pub async fn create_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(workspace_id): Path<Uuid>,
    Json(mut req): Json<CreateSubscriptionRequest>,
) -> Result<axum::response::Response, ApiError> {
    load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_ADMIN).await?;
    if !state.settings.credential_encryption_write_enabled {
        return Err(ApiError::credential_encryption_writes_disabled());
    }
    validate_display_name(&mut req.display_name)?;
    validate_target_url_syntax(&req.target_url)?;
    validate_signing_secret(&req.secret)?;
    validate_event_types(&req.event_types)?;
    validate_library_scope(&state, workspace_id, req.library_id).await?;
    let subscription_id = Uuid::now_v7();
    let serialized_custom_headers = custom_headers::validate_and_serialize(&req.custom_headers)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let encrypted_secret = state
        .credential_cipher
        .encrypt(SecretPurpose::WebhookSigningSecret, subscription_id, &req.secret)
        .map_err(ApiError::from_secret_encryption)?;
    let encrypted_custom_headers = state
        .credential_cipher
        .encrypt(
            SecretPurpose::WebhookCustomHeaders,
            subscription_id,
            serialized_custom_headers.as_str(),
        )
        .map_err(ApiError::from_secret_encryption)?;
    drop(serialized_custom_headers);
    // The encrypted copy is now self-contained. Scrub the HTTP plaintext
    // before DNS resolution yields so it never survives an SSRF-policy await.
    scrub_plaintext_secret(&mut req.secret);
    custom_headers::scrub_json_strings(&mut req.custom_headers);

    // SSRF protection: reject private or loopback target addresses.
    ssrf::validate_target_url(&req.target_url).await.map_err(ApiError::BadRequest)?;

    let row = webhook_repository::create_webhook_subscription(
        &state.persistence.postgres,
        &NewWebhookSubscription {
            id: subscription_id,
            workspace_id,
            library_id: req.library_id,
            display_name: std::mem::take(&mut req.display_name),
            target_url: std::mem::take(&mut req.target_url),
            secret: encrypted_secret,
            event_types: std::mem::take(&mut req.event_types),
            custom_headers_json: encrypted_custom_headers,
            created_by_principal_id: Some(auth.principal_id),
        },
    )
    .await
    .map_err(map_webhook_subscription_write_error)?;

    let location = format!("/v1/webhooks/subscriptions/{}", row.id);
    let body = subscription_row_to_response(row);
    let mut response = (StatusCode::CREATED, Json(body)).into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&location) {
        response.headers_mut().insert(axum::http::header::LOCATION, value);
    }
    Ok(response)
}

#[utoipa::path(
    get,
    path = "/v1/webhooks/workspaces/{workspaceId}/subscriptions",
    tag = "webhooks",
    operation_id = "listWebhookSubscriptions",
    params(
        ("workspaceId" = uuid::Uuid, Path, description = "Workspace that owns the subscriptions"),
        ListSubscriptionsQuery,
    ),
    responses(
        (status = 200, description = "Outbound webhook subscriptions for a workspace", body = SubscriptionListPageResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a workspace administrator"),
    ),
)]
pub async fn list_subscriptions(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(workspace_id): Path<Uuid>,
    Query(q): Query<ListSubscriptionsQuery>,
) -> Result<Json<SubscriptionListPageResponse>, ApiError> {
    load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_ADMIN).await?;
    let cursor = match q.cursor.as_deref() {
        Some(token) => {
            let WebhookListCursor { created_at, id } = decode_webhook_list_cursor(token)?;
            Some((created_at, id))
        }
        None => None,
    };
    let limit = q
        .limit
        .unwrap_or(DEFAULT_WEBHOOK_SUBSCRIPTION_PAGE_SIZE)
        .clamp(1, webhook_repository::MAX_WEBHOOK_API_PAGE_SIZE);

    let page = webhook_repository::list_webhook_subscriptions_by_workspace(
        &state.persistence.postgres,
        workspace_id,
        cursor,
        limit,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    let next_cursor = page
        .next_cursor
        .map(|(created_at, id)| encode_webhook_list_cursor(&WebhookListCursor { created_at, id }));
    let items = page.rows.into_iter().map(subscription_row_to_response).collect();
    Ok(Json(SubscriptionListPageResponse { items, next_cursor }))
}

#[utoipa::path(
    get,
    path = "/v1/webhooks/subscriptions/{subscriptionId}",
    tag = "webhooks",
    operation_id = "getWebhookSubscription",
    params(("subscriptionId" = uuid::Uuid, Path, description = "Webhook subscription identifier")),
    responses(
        (status = 200, description = "Outbound webhook subscription", body = SubscriptionResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Webhook subscription not found"),
    ),
)]
pub async fn get_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(subscription_id): Path<Uuid>,
) -> Result<Json<SubscriptionResponse>, ApiError> {
    let row = load_authorized_subscription_view(&state, &auth, subscription_id).await?;

    Ok(Json(subscription_row_to_response(row)))
}

#[utoipa::path(
    patch,
    path = "/v1/webhooks/subscriptions/{subscriptionId}",
    tag = "webhooks",
    operation_id = "updateWebhookSubscription",
    params(("subscriptionId" = uuid::Uuid, Path, description = "Webhook subscription identifier")),
    request_body = UpdateSubscriptionRequest,
    responses(
        (status = 200, description = "Updated outbound webhook subscription", body = SubscriptionResponse),
        (status = 400, description = "Request body is invalid"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Webhook subscription not found"),
        (status = 409, description = "Webhook subscription is draining or quota enforcement rejected reactivation"),
    ),
)]
pub async fn update_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(subscription_id): Path<Uuid>,
    Json(mut req): Json<UpdateSubscriptionRequest>,
) -> Result<Json<SubscriptionResponse>, ApiError> {
    let existing = load_authorized_subscription_view(&state, &auth, subscription_id).await?;
    if (req.secret.is_some() || req.custom_headers.is_some())
        && !state.settings.credential_encryption_write_enabled
    {
        return Err(ApiError::credential_encryption_writes_disabled());
    }

    if let Some(display_name) = req.display_name.as_mut() {
        validate_display_name(display_name)?;
    }
    if let Some(ref et) = req.event_types {
        validate_event_types(et)?;
    }
    if let Some(secret) = req.secret.as_deref() {
        validate_signing_secret(secret)?;
    }
    let encrypted_secret = req
        .secret
        .as_deref()
        .map(|plaintext| {
            state.credential_cipher.encrypt(
                SecretPurpose::WebhookSigningSecret,
                existing.id,
                plaintext,
            )
        })
        .transpose()
        .map_err(ApiError::from_secret_encryption)?;
    let encrypted_custom_headers = if let Some(value) = req.custom_headers.as_ref() {
        let serialized = custom_headers::validate_and_serialize(value)
            .map_err(|error| ApiError::BadRequest(error.to_string()))?;
        let encrypted = state
            .credential_cipher
            .encrypt(SecretPurpose::WebhookCustomHeaders, existing.id, serialized.as_str())
            .map_err(ApiError::from_secret_encryption)?;
        drop(serialized);
        Some(encrypted)
    } else {
        None
    };
    if let Some(secret) = req.secret.as_mut() {
        // Keep plaintext lifetime independent of optional target validation:
        // the encrypted copy above is all the repository needs from here on.
        scrub_plaintext_secret(secret);
    }
    if let Some(value) = req.custom_headers.as_mut() {
        custom_headers::scrub_json_strings(value);
    }
    if let Some(ref url) = req.target_url {
        validate_target_url_syntax(url)?;
        // SSRF protection on updated target_url.
        ssrf::validate_target_url(url).await.map_err(ApiError::BadRequest)?;
    }

    let row = webhook_repository::update_webhook_subscription_in_workspace(
        &state.persistence.postgres,
        subscription_id,
        existing.workspace_id,
        &UpdateWebhookSubscription {
            display_name: req.display_name.take(),
            target_url: req.target_url.take(),
            secret: encrypted_secret,
            event_types: req.event_types.take(),
            custom_headers_json: encrypted_custom_headers,
            active: req.active,
        },
    )
    .await
    .map_err(map_webhook_subscription_write_error)?
    .ok_or_else(|| ApiError::resource_not_found("webhook_subscription", subscription_id))?;

    Ok(Json(subscription_row_to_response(row)))
}

#[utoipa::path(
    delete,
    path = "/v1/webhooks/subscriptions/{subscriptionId}",
    tag = "webhooks",
    operation_id = "deleteWebhookSubscription",
    params(("subscriptionId" = uuid::Uuid, Path, description = "Webhook subscription identifier")),
    responses(
        (status = 204, description = "Webhook subscription deleted"),
        (status = 202, description = "Deletion accepted; an already-owned delivery is still draining. Retry DELETE until it returns 204"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Webhook subscription not found"),
    ),
)]
pub async fn delete_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(subscription_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let existing = load_authorized_subscription_view(&state, &auth, subscription_id).await?;

    let outcome = webhook_repository::delete_webhook_subscription_in_workspace(
        &state.persistence.postgres,
        subscription_id,
        existing.workspace_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    match outcome {
        webhook_repository::DeleteWebhookSubscriptionOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        webhook_repository::DeleteWebhookSubscriptionOutcome::NotFound => {
            Err(ApiError::resource_not_found("webhook_subscription", subscription_id))
        }
        webhook_repository::DeleteWebhookSubscriptionOutcome::Draining { in_flight_deliveries } => {
            tracing::info!(
                subscription_id = %subscription_id,
                in_flight_deliveries,
                "webhook subscription deletion accepted and is draining"
            );
            Ok(StatusCode::ACCEPTED)
        }
    }
}

#[utoipa::path(
    get,
    path = "/v1/webhooks/subscriptions/{subscriptionId}/attempts",
    tag = "webhooks",
    operation_id = "listWebhookDeliveryAttempts",
    params(
        ("subscriptionId" = uuid::Uuid, Path, description = "Webhook subscription identifier"),
        ListAttemptsQuery,
    ),
    responses(
        (status = 200, description = "Delivery attempts for the outbound webhook subscription", body = DeliveryAttemptListPageResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Webhook subscription not found"),
    ),
)]
pub async fn list_delivery_attempts(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(subscription_id): Path<Uuid>,
    Query(q): Query<ListAttemptsQuery>,
) -> Result<Json<DeliveryAttemptListPageResponse>, ApiError> {
    let sub = load_authorized_subscription_view(&state, &auth, subscription_id).await?;
    validate_attempt_state_filter(q.state.as_deref())?;
    let cursor = match q.cursor.as_deref() {
        Some(token) => {
            let WebhookListCursor { created_at, id } = decode_webhook_list_cursor(token)?;
            Some((created_at, id))
        }
        None => None,
    };
    let limit = q
        .limit
        .unwrap_or(DEFAULT_WEBHOOK_ATTEMPT_PAGE_SIZE)
        .clamp(1, webhook_repository::MAX_WEBHOOK_API_PAGE_SIZE);

    let page = webhook_repository::list_webhook_delivery_attempts_by_subscription(
        &state.persistence.postgres,
        subscription_id,
        sub.workspace_id,
        q.state.as_deref(),
        cursor,
        limit,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    let next_cursor = page
        .next_cursor
        .map(|(created_at, id)| encode_webhook_list_cursor(&WebhookListCursor { created_at, id }));
    let items = page
        .rows
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

    Ok(Json(DeliveryAttemptListPageResponse { items, next_cursor }))
}

#[derive(Serialize, utoipa::ToSchema)]
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

impl std::fmt::Debug for DeliveryAttemptResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeliveryAttemptResponse")
            .field("id", &self.id)
            .field("subscription_id", &self.subscription_id)
            .field("event_type", &self.event_type)
            .field("event_id", &self.event_id)
            .field("target_url", &"<redacted>")
            .field("attempt_number", &self.attempt_number)
            .field("delivery_state", &self.delivery_state)
            .field("response_status", &self.response_status)
            .field("error_message", &self.error_message.as_ref().map(|_| "<redacted>"))
            .field("delivered_at", &self.delivered_at)
            .field("next_attempt_at", &self.next_attempt_at)
            .field("created_at", &self.created_at)
            .finish()
    }
}

fn subscription_row_to_response(
    mut row: webhook_repository::WebhookSubscriptionViewRow,
) -> SubscriptionResponse {
    SubscriptionResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        display_name: std::mem::take(&mut row.display_name),
        target_url: std::mem::take(&mut row.target_url),
        event_types: std::mem::take(&mut row.event_types),
        active: row.active,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

#[cfg(test)]
mod secret_lifetime_tests {
    use uuid::Uuid;

    use super::{
        CreateSubscriptionRequest, WebhookListCursor, decode_webhook_list_cursor,
        encode_webhook_list_cursor, scrub_plaintext_secret, validate_attempt_state_filter,
        validate_display_name, validate_event_types, validate_signing_secret,
        validate_target_url_syntax,
    };

    #[test]
    fn keyset_cursor_round_trips_and_rejects_garbage() {
        let created_at = chrono::Utc::now();
        let id = Uuid::now_v7();

        let token = encode_webhook_list_cursor(&WebhookListCursor { created_at, id });
        let decoded = decode_webhook_list_cursor(&token).expect("round trip");
        assert_eq!(decoded.created_at, created_at);
        assert_eq!(decoded.id, id);

        assert!(decode_webhook_list_cursor("not-a-valid-cursor").is_err());
    }

    #[test]
    fn delivery_attempt_state_filter_is_bounded_to_the_database_enum() {
        assert!(validate_attempt_state_filter(None).is_ok());
        assert!(validate_attempt_state_filter(Some("delivered")).is_ok());
        assert!(validate_attempt_state_filter(Some("unknown")).is_err());
    }

    #[test]
    fn plaintext_scrubber_clears_the_request_buffer() {
        let mut secret = "synthetic-webhook-secret".to_string();

        scrub_plaintext_secret(&mut secret);

        assert!(secret.is_empty());
    }

    #[test]
    fn subscription_debug_redacts_secret_url_credentials_and_custom_headers() {
        let request = CreateSubscriptionRequest {
            library_id: None,
            display_name: "synthetic".into(),
            target_url: "https://user:url-secret@host.example/?token=query-secret".into(),
            secret: "signing-secret-regression".into(),
            event_types: vec!["revision.ready".into()],
            custom_headers: serde_json::json!({
                "Authorization": "Bearer custom-header-secret"
            }),
        };

        let debug = format!("{request:?}");
        for secret in
            ["url-secret", "query-secret", "signing-secret-regression", "custom-header-secret"]
        {
            assert!(!debug.contains(secret), "Debug exposed {secret}");
        }
    }

    #[test]
    fn omitted_custom_headers_default_to_an_empty_object() {
        let request: CreateSubscriptionRequest = serde_json::from_value(serde_json::json!({
            "libraryId": null,
            "displayName": "synthetic",
            "targetUrl": "https://example.com/hook",
            "secret": "x".repeat(super::MIN_WEBHOOK_SIGNING_SECRET_BYTES),
            "eventTypes": ["revision.ready"]
        }))
        .expect("request without customHeaders should deserialize");

        assert_eq!(request.custom_headers, serde_json::json!({}));
    }

    #[test]
    fn webhook_boundary_rejects_weak_or_ambiguous_configuration() {
        assert!(validate_signing_secret("too-short").is_err());
        assert!(validate_event_types(&["revision.ready".into(), "revision.ready".into()]).is_err());
        assert!(validate_event_types(&["unknown.event".into()]).is_err());
        assert!(validate_target_url_syntax("https://user:secret@example.com/hook").is_err());
        assert!(validate_target_url_syntax("https://example.com/hook#fragment").is_err());
    }

    #[test]
    fn webhook_boundary_normalizes_display_name_and_accepts_canonical_events() {
        let mut display_name = "  Neutral Receiver  ".to_string();
        validate_display_name(&mut display_name).expect("valid display name");
        validate_event_types(&["revision.ready".into(), "document.deleted".into()])
            .expect("canonical events");
        validate_signing_secret("synthetic-signing-secret-at-least-32-bytes")
            .expect("strong synthetic secret");

        assert_eq!(display_name, "Neutral Receiver");
    }
}
