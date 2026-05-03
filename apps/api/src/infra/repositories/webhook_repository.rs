use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

// ============================================================================
// Row types
// ============================================================================

#[derive(Clone, FromRow)]
pub struct WebhookSubscriptionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub target_url: String,
    pub secret: String,
    pub event_types: Vec<String>,
    pub custom_headers_json: serde_json::Value,
    pub active: bool,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for WebhookSubscriptionRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSubscriptionRow")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("display_name", &self.display_name)
            .field("target_url", &self.target_url)
            .field("secret", &"<redacted>")
            .field("event_types", &self.event_types)
            .field("custom_headers_json", &self.custom_headers_json)
            .field("active", &self.active)
            .field("created_by_principal_id", &self.created_by_principal_id)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct WebhookDeliveryAttemptRow {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub event_type: String,
    pub event_id: String,
    pub payload_json: serde_json::Value,
    pub target_url: String,
    pub attempt_number: i32,
    pub delivery_state: String,
    pub response_status: Option<i32>,
    pub response_body_excerpt: Option<String>,
    pub error_message: Option<String>,
    pub job_id: Option<Uuid>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ============================================================================
// Input types
// ============================================================================

#[derive(Debug, Clone)]
pub struct NewWebhookSubscription {
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub target_url: String,
    pub secret: String,
    pub event_types: Vec<String>,
    pub custom_headers_json: serde_json::Value,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateWebhookSubscription {
    pub display_name: Option<String>,
    pub target_url: Option<String>,
    pub secret: Option<String>,
    pub event_types: Option<Vec<String>>,
    pub custom_headers_json: Option<serde_json::Value>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct NewWebhookDeliveryAttempt {
    pub subscription_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub event_type: String,
    pub event_id: String,
    pub payload_json: serde_json::Value,
    pub target_url: String,
}

// ============================================================================
// webhook_subscription CRUD
// ============================================================================

/// Creates a new outbound webhook subscription.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the subscription row.
pub async fn create_webhook_subscription(
    postgres: &PgPool,
    input: &NewWebhookSubscription,
) -> Result<WebhookSubscriptionRow, sqlx::Error> {
    sqlx::query_as::<_, WebhookSubscriptionRow>(
        "insert into webhook_subscription (
            workspace_id, library_id, display_name, target_url, secret,
            event_types, custom_headers_json, created_by_principal_id
        ) values ($1, $2, $3, $4, $5, $6, $7, $8)
        returning
            id, workspace_id, library_id, display_name, target_url, secret,
            event_types, custom_headers_json, active,
            created_by_principal_id, created_at, updated_at",
    )
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(&input.display_name)
    .bind(&input.target_url)
    .bind(&input.secret)
    .bind(&input.event_types)
    .bind(input.custom_headers_json.clone())
    .bind(input.created_by_principal_id)
    .fetch_one(postgres)
    .await
}

/// Loads one webhook subscription by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the subscription row.
pub async fn get_webhook_subscription_by_id(
    postgres: &PgPool,
    id: Uuid,
) -> Result<Option<WebhookSubscriptionRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookSubscriptionRow>(
        "select id, workspace_id, library_id, display_name, target_url, secret,
                event_types, custom_headers_json, active,
                created_by_principal_id, created_at, updated_at
         from webhook_subscription
         where id = $1",
    )
    .bind(id)
    .fetch_optional(postgres)
    .await
}

/// Lists active subscriptions matching a workspace and optional library filter.
///
/// When `library_id` is `Some`, returns workspace-wide subscriptions (library_id
/// IS NULL) plus subscriptions scoped to that specific library.
///
/// # Errors
/// Returns any `SQLx` error raised while querying subscription rows.
pub async fn list_active_webhook_subscriptions_for_event(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Option<Uuid>,
    event_type: &str,
) -> Result<Vec<WebhookSubscriptionRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookSubscriptionRow>(
        "select id, workspace_id, library_id, display_name, target_url, secret,
                event_types, custom_headers_json, active,
                created_by_principal_id, created_at, updated_at
         from webhook_subscription
         where workspace_id = $1
           and active = true
           and $3 = any(event_types)
           and (library_id is null or library_id = $2)",
    )
    .bind(workspace_id)
    .bind(library_id)
    .bind(event_type)
    .fetch_all(postgres)
    .await
}

/// Lists all subscriptions for a workspace (all states).
///
/// # Errors
/// Returns any `SQLx` error raised while querying subscription rows.
pub async fn list_webhook_subscriptions_by_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<WebhookSubscriptionRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookSubscriptionRow>(
        "select id, workspace_id, library_id, display_name, target_url, secret,
                event_types, custom_headers_json, active,
                created_by_principal_id, created_at, updated_at
         from webhook_subscription
         where workspace_id = $1
         order by created_at asc, id asc",
    )
    .bind(workspace_id)
    .fetch_all(postgres)
    .await
}

/// Patches a webhook subscription; only supplied fields are updated.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the subscription row.
pub async fn update_webhook_subscription(
    postgres: &PgPool,
    id: Uuid,
    input: &UpdateWebhookSubscription,
) -> Result<Option<WebhookSubscriptionRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookSubscriptionRow>(
        "update webhook_subscription
         set display_name        = coalesce($2, display_name),
             target_url          = coalesce($3, target_url),
             secret              = coalesce($4, secret),
             event_types         = coalesce($5, event_types),
             custom_headers_json = coalesce($6, custom_headers_json),
             active              = coalesce($7, active),
             updated_at          = now()
         where id = $1
         returning
             id, workspace_id, library_id, display_name, target_url, secret,
             event_types, custom_headers_json, active,
             created_by_principal_id, created_at, updated_at",
    )
    .bind(id)
    .bind(input.display_name.as_deref())
    .bind(input.target_url.as_deref())
    .bind(input.secret.as_deref())
    .bind(input.event_types.as_deref())
    .bind(input.custom_headers_json.clone())
    .bind(input.active)
    .fetch_optional(postgres)
    .await
}

/// Hard-deletes a webhook subscription by id.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the subscription row.
pub async fn delete_webhook_subscription(postgres: &PgPool, id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("delete from webhook_subscription where id = $1")
        .bind(id)
        .execute(postgres)
        .await?;
    Ok(result.rows_affected())
}

// ============================================================================
// webhook_delivery_attempt
// ============================================================================

/// Creates a pending delivery attempt row (before the job is enqueued).
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the attempt row.
pub async fn create_webhook_delivery_attempt(
    postgres: &PgPool,
    input: &NewWebhookDeliveryAttempt,
) -> Result<WebhookDeliveryAttemptRow, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "insert into webhook_delivery_attempt (
            subscription_id, workspace_id, library_id,
            event_type, event_id, payload_json, target_url
        ) values ($1, $2, $3, $4, $5, $6, $7)
        returning
            id, subscription_id, workspace_id, library_id,
            event_type, event_id, payload_json, target_url,
            attempt_number, delivery_state::text as delivery_state,
            response_status, response_body_excerpt, error_message,
            job_id, next_attempt_at, delivered_at, created_at, updated_at",
    )
    .bind(input.subscription_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(&input.event_type)
    .bind(&input.event_id)
    .bind(input.payload_json.clone())
    .bind(&input.target_url)
    .fetch_one(postgres)
    .await
}

/// Loads one delivery attempt by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the attempt row.
pub async fn get_webhook_delivery_attempt_by_id(
    postgres: &PgPool,
    id: Uuid,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "select id, subscription_id, workspace_id, library_id,
                event_type, event_id, payload_json, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, response_body_excerpt, error_message,
                job_id, next_attempt_at, delivered_at, created_at, updated_at
         from webhook_delivery_attempt
         where id = $1",
    )
    .bind(id)
    .fetch_optional(postgres)
    .await
}

/// Lists delivery attempts for one subscription, optionally filtered by state.
///
/// # Errors
/// Returns any `SQLx` error raised while querying attempt rows.
pub async fn list_webhook_delivery_attempts_by_subscription(
    postgres: &PgPool,
    subscription_id: Uuid,
    state_filter: Option<&str>,
) -> Result<Vec<WebhookDeliveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "select id, subscription_id, workspace_id, library_id,
                event_type, event_id, payload_json, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, response_body_excerpt, error_message,
                job_id, next_attempt_at, delivered_at, created_at, updated_at
         from webhook_delivery_attempt
         where subscription_id = $1
           and ($2::text is null or delivery_state::text = $2)
         order by created_at desc, id desc
         limit 200",
    )
    .bind(subscription_id)
    .bind(state_filter)
    .fetch_all(postgres)
    .await
}

/// Links a delivery attempt to its ingest job by setting the `job_id` FK.
///
/// Does NOT change `delivery_state` — state transitions are handled separately
/// by `mark_attempt_delivering` (when the worker leases the job) and
/// `record_webhook_delivery_result` (when the attempt completes).
///
/// # Errors
/// Returns any `SQLx` error raised while updating the attempt row.
pub async fn link_attempt_to_job(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set job_id = $2,
             updated_at = now()
         where id = $1
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_message,
             job_id, next_attempt_at, delivered_at, created_at, updated_at",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(postgres)
    .await
}

/// Transitions a delivery attempt to `delivering` state when the worker leases the job.
///
/// Called at the start of `run_webhook_delivery_job` before the HTTP request is made.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the attempt row.
pub async fn mark_attempt_delivering(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set delivery_state = 'delivering',
             updated_at = now()
         where id = $1
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_message,
             job_id, next_attempt_at, delivered_at, created_at, updated_at",
    )
    .bind(attempt_id)
    .fetch_optional(postgres)
    .await
}

/// Records the result of one HTTP delivery attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the attempt row.
pub async fn record_webhook_delivery_result(
    postgres: &PgPool,
    attempt_id: Uuid,
    delivery_state: &str,
    attempt_number: i32,
    response_status: Option<i32>,
    response_body_excerpt: Option<&str>,
    error_message: Option<&str>,
    next_attempt_at: Option<DateTime<Utc>>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set delivery_state         = $2::webhook_delivery_state,
             attempt_number         = $3,
             response_status        = $4,
             response_body_excerpt  = $5,
             error_message          = $6,
             next_attempt_at        = $7,
             delivered_at           = case when $2 = 'delivered' then now() else delivered_at end,
             updated_at             = now()
         where id = $1
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_message,
             job_id, next_attempt_at, delivered_at, created_at, updated_at",
    )
    .bind(attempt_id)
    .bind(delivery_state)
    .bind(attempt_number)
    .bind(response_status)
    .bind(response_body_excerpt)
    .bind(error_message)
    .bind(next_attempt_at)
    .fetch_optional(postgres)
    .await
}
