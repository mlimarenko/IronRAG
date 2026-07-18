use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;
use zeroize::Zeroize as _;

use crate::{
    infra::repositories::{
        catalog_repository,
        ingest_repository::{NewIngestJob, create_ingest_job_with_executor},
    },
    shared::secret_encryption::{EncryptedSecret, SecretPurpose},
};

#[cfg(feature = "test-support")]
use crate::infra::repositories::ingest_repository::get_ingest_job_by_dedupe_key;

/// Hard fanout bound for one workspace. Existing rows remain readable during
/// upgrade, while every new activation is serialized and enforced here.
pub const MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE: i64 = 100;
/// Hard storage bound for active and inactive subscriptions combined. Inactive
/// rows retain delivery/audit history and therefore are never age-purged
/// implicitly; callers must delete them explicitly before creating more.
pub const MAX_TOTAL_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE: i64 = 1_000;
/// Repository-side ceiling for public list surfaces. HTTP callers may choose a
/// smaller page, but no accidental internal caller can issue an unbounded scan.
pub const MAX_WEBHOOK_API_PAGE_SIZE: u32 = 200;
pub const WEBHOOK_DELIVERY_LEASE_TIMEOUT_SECONDS: i64 = 300;
const ACTIVE_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR: &str = "active webhook subscription quota exceeded";
const TOTAL_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR: &str = "total webhook subscription quota exceeded";
const DRAINING_WEBHOOK_SUBSCRIPTION_REACTIVATION_ERROR: &str =
    "draining webhook subscription cannot be reactivated";

#[must_use]
pub fn is_active_webhook_subscription_quota_error(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Protocol(message) if message == ACTIVE_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR)
        || matches!(
            error,
            sqlx::Error::Database(database_error)
                if database_error.constraint()
                    == Some("webhook_subscription_active_workspace_quota")
        )
}

#[must_use]
pub fn is_total_webhook_subscription_quota_error(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Protocol(message) if message == TOTAL_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR)
        || matches!(
            error,
            sqlx::Error::Database(database_error)
                if database_error.constraint()
                    == Some("webhook_subscription_total_workspace_quota")
        )
}

#[must_use]
pub fn is_draining_webhook_subscription_reactivation_error(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Protocol(message)
            if message == DRAINING_WEBHOOK_SUBSCRIPTION_REACTIVATION_ERROR
    )
}

fn validate_encrypted_webhook_secret_context(
    subscription_id: Uuid,
    secret: Option<&EncryptedSecret>,
) -> Result<(), sqlx::Error> {
    if secret.is_some_and(|secret| {
        !secret.is_bound_to(SecretPurpose::WebhookSigningSecret, subscription_id)
    }) {
        return Err(sqlx::Error::Protocol(
            "encrypted webhook credential context does not match target row".to_string(),
        ));
    }
    Ok(())
}

fn validate_encrypted_webhook_headers_context(
    subscription_id: Uuid,
    headers: Option<&EncryptedSecret>,
) -> Result<(), sqlx::Error> {
    if headers.is_some_and(|headers| {
        !headers.is_bound_to(SecretPurpose::WebhookCustomHeaders, subscription_id)
    }) {
        return Err(sqlx::Error::Protocol(
            "encrypted webhook custom-header context does not match target row".to_string(),
        ));
    }
    Ok(())
}

fn scrub_json_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(value) => value.zeroize(),
        serde_json::Value::Array(values) => {
            for value in values {
                scrub_json_strings(value);
            }
        }
        serde_json::Value::Object(values) => {
            let values = std::mem::take(values);
            for (mut key, mut value) in values {
                key.zeroize();
                scrub_json_strings(&mut value);
            }
        }
        _ => {}
    }
}

// ============================================================================
// Row types
// ============================================================================

#[derive(FromRow)]
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

/// Secret-free projection used by every subscription-management API.
///
/// Keep this separate from [`WebhookSubscriptionRow`]: the latter is a
/// delivery credential object and must only be materialized on the worker
/// claim path.
#[derive(FromRow)]
pub struct WebhookSubscriptionViewRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub target_url: String,
    pub event_types: Vec<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for WebhookSubscriptionViewRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookSubscriptionViewRow")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("display_name", &self.display_name)
            .field("target_url", &"<redacted>")
            .field("event_types", &self.event_types)
            .field("active", &self.active)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// Bounded page returned by the subscription management projection.
pub struct WebhookSubscriptionViewPage {
    pub rows: Vec<WebhookSubscriptionViewRow>,
    /// `(created_at, id)` of the last returned row, present only when more
    /// rows exist beyond this page.
    pub next_cursor: Option<(DateTime<Utc>, Uuid)>,
}

impl Drop for WebhookSubscriptionRow {
    fn drop(&mut self) {
        self.secret.zeroize();
        scrub_json_strings(&mut self.custom_headers_json);
    }
}

impl std::fmt::Debug for WebhookSubscriptionRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSubscriptionRow")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("display_name", &self.display_name)
            .field("target_url", &"<redacted>")
            .field("secret", &"<redacted>")
            .field("event_types", &self.event_types)
            .field("custom_headers_json", &"<redacted>")
            .field("active", &self.active)
            .field("created_by_principal_id", &self.created_by_principal_id)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Clone, FromRow)]
pub struct WebhookDeliveryAttemptRow {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub event_type: String,
    pub event_id: String,
    pub occurred_at: DateTime<Utc>,
    pub payload_json: serde_json::Value,
    pub target_url: String,
    pub attempt_number: i32,
    pub delivery_state: String,
    pub response_status: Option<i32>,
    pub response_body_excerpt: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub job_id: Option<Uuid>,
    pub delivery_lease_token: Option<Uuid>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Data-minimized delivery-attempt projection for the management API. The
/// signed payload, remote response body, queue identifiers and lease token are
/// deliberately absent.
#[derive(FromRow)]
pub struct WebhookDeliveryAttemptViewRow {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event_type: String,
    pub event_id: String,
    pub target_url: String,
    pub attempt_number: i32,
    pub delivery_state: String,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl std::fmt::Debug for WebhookDeliveryAttemptViewRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookDeliveryAttemptViewRow")
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

/// Bounded page returned by the delivery-attempt management projection.
pub struct WebhookDeliveryAttemptViewPage {
    pub rows: Vec<WebhookDeliveryAttemptViewRow>,
    /// `(created_at, id)` of the last returned row, present only when more
    /// rows exist beyond this page.
    pub next_cursor: Option<(DateTime<Utc>, Uuid)>,
}

impl std::fmt::Debug for WebhookDeliveryAttemptRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookDeliveryAttemptRow")
            .field("id", &self.id)
            .field("subscription_id", &self.subscription_id)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("event_type", &self.event_type)
            .field("event_id", &self.event_id)
            .field("occurred_at", &self.occurred_at)
            .field("payload_json", &"<redacted>")
            .field("target_url", &"<redacted>")
            .field("attempt_number", &self.attempt_number)
            .field("delivery_state", &self.delivery_state)
            .field("response_status", &self.response_status)
            .field(
                "response_body_excerpt",
                &self.response_body_excerpt.as_ref().map(|_| "<redacted>"),
            )
            .field("error_code", &self.error_code)
            .field("error_message", &self.error_message.as_ref().map(|_| "<redacted>"))
            .field("job_id", &self.job_id)
            .field("delivery_lease_token", &self.delivery_lease_token.map(|_| "<redacted>"))
            .field("next_attempt_at", &self.next_attempt_at)
            .field("delivered_at", &self.delivered_at)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Clone, FromRow)]
pub struct WebhookSubscriptionTargetRow {
    pub id: Uuid,
    pub target_url: String,
}

impl std::fmt::Debug for WebhookSubscriptionTargetRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookSubscriptionTargetRow")
            .field("id", &self.id)
            .field("target_url", &"<redacted>")
            .finish()
    }
}

// ============================================================================
// Input types
// ============================================================================

#[derive(Clone)]
pub struct NewWebhookSubscription {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub target_url: String,
    pub secret: EncryptedSecret,
    pub event_types: Vec<String>,
    pub custom_headers_json: EncryptedSecret,
    pub created_by_principal_id: Option<Uuid>,
}

impl std::fmt::Debug for NewWebhookSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewWebhookSubscription")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("display_name", &self.display_name)
            .field("target_url", &"<redacted>")
            .field("secret", &"<redacted>")
            .field("event_types", &self.event_types)
            .field("custom_headers_json", &"<redacted>")
            .field("created_by_principal_id", &self.created_by_principal_id)
            .finish()
    }
}

#[derive(Clone)]
pub struct UpdateWebhookSubscription {
    pub display_name: Option<String>,
    pub target_url: Option<String>,
    pub secret: Option<EncryptedSecret>,
    pub event_types: Option<Vec<String>>,
    pub custom_headers_json: Option<EncryptedSecret>,
    pub active: Option<bool>,
}

impl std::fmt::Debug for UpdateWebhookSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateWebhookSubscription")
            .field("display_name", &self.display_name)
            .field("target_url", &self.target_url.as_ref().map(|_| "<redacted>"))
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .field("event_types", &self.event_types)
            .field("custom_headers_json", &self.custom_headers_json.as_ref().map(|_| "<redacted>"))
            .field("active", &self.active)
            .finish()
    }
}

#[derive(Clone)]
pub struct NewWebhookDeliveryAttempt {
    pub subscription_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub event_type: String,
    pub event_id: String,
    pub occurred_at: DateTime<Utc>,
    pub payload_json: serde_json::Value,
    pub target_url: String,
}

impl std::fmt::Debug for NewWebhookDeliveryAttempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewWebhookDeliveryAttempt")
            .field("subscription_id", &self.subscription_id)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("event_type", &self.event_type)
            .field("event_id", &self.event_id)
            .field("occurred_at", &self.occurred_at)
            .field("payload_json", &"<redacted>")
            .field("target_url", &"<redacted>")
            .finish()
    }
}

/// Fenced completion of exactly one claimed delivery job.
pub struct WebhookDeliveryCompletion<'a> {
    pub attempt_id: Uuid,
    pub job_id: Uuid,
    pub lease_token: Uuid,
    pub delivery_state: &'a str,
    pub attempt_number: i32,
    pub response_status: Option<i32>,
    pub error_code: Option<&'a str>,
    pub error_summary: Option<&'a str>,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

/// Queue ownership that must be terminalized in the same transaction as a
/// delivery retry handoff.
pub struct WebhookRetryHandoff<'a> {
    pub ingest_attempt_id: Uuid,
    pub expected_queue_lease_token: &'a str,
}

impl std::fmt::Debug for WebhookRetryHandoff<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookRetryHandoff")
            .field("ingest_attempt_id", &self.ingest_attempt_id)
            .field("expected_queue_lease_token", &"<redacted>")
            .finish()
    }
}

/// Result of committing a retry and retiring its current queue lease.
pub enum WebhookRetryHandoffOutcome {
    RetryScheduled(WebhookDeliveryAttemptRow),
    CompletionRecorded(WebhookDeliveryAttemptRow),
    OwnershipLost,
}

impl std::fmt::Debug for WebhookRetryHandoffOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RetryScheduled(row) => formatter
                .debug_tuple("WebhookRetryHandoffOutcome::RetryScheduled")
                .field(&row.id)
                .finish(),
            Self::CompletionRecorded(row) => formatter
                .debug_tuple("WebhookRetryHandoffOutcome::CompletionRecorded")
                .field(&row.id)
                .finish(),
            Self::OwnershipLost => formatter.write_str("WebhookRetryHandoffOutcome::OwnershipLost"),
        }
    }
}

impl std::fmt::Debug for WebhookDeliveryCompletion<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookDeliveryCompletion")
            .field("attempt_id", &self.attempt_id)
            .field("job_id", &self.job_id)
            .field("lease_token", &"<redacted>")
            .field("delivery_state", &self.delivery_state)
            .field("attempt_number", &self.attempt_number)
            .field("response_status", &self.response_status)
            .field("error_code", &self.error_code)
            .field("error_summary", &self.error_summary.map(|_| "<redacted>"))
            .field("next_attempt_at", &self.next_attempt_at)
            .finish()
    }
}

/// Result of serializing a delivery claim with subscription deletion.
pub enum WebhookDeliveryClaimOutcome {
    Claimed {
        attempt: Box<WebhookDeliveryAttemptRow>,
        subscription: Box<WebhookSubscriptionRow>,
    },
    /// Another worker still owns a non-stale delivery lease. The queue job
    /// must be deferred until `retry_at`, never finalized as succeeded.
    InFlight {
        attempt_id: Uuid,
        retry_at: DateTime<Utc>,
    },
    /// The delivery is already terminal, so replaying the queue job requires
    /// no second HTTP request.
    Terminal {
        attempt_id: Uuid,
        delivery_state: String,
    },
    /// The subscription/attempt disappeared through an atomic cancellation.
    Canceled,
}

impl std::fmt::Debug for WebhookDeliveryClaimOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claimed { attempt, subscription } => formatter
                .debug_struct("WebhookDeliveryClaimOutcome::Claimed")
                .field("attempt_id", &attempt.id)
                .field("subscription_id", &subscription.id)
                .finish(),
            Self::InFlight { attempt_id, retry_at } => formatter
                .debug_struct("WebhookDeliveryClaimOutcome::InFlight")
                .field("attempt_id", attempt_id)
                .field("retry_at", retry_at)
                .finish(),
            Self::Terminal { attempt_id, delivery_state } => formatter
                .debug_struct("WebhookDeliveryClaimOutcome::Terminal")
                .field("attempt_id", attempt_id)
                .field("delivery_state", delivery_state)
                .finish(),
            Self::Canceled => formatter.write_str("WebhookDeliveryClaimOutcome::Canceled"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteWebhookSubscriptionOutcome {
    Deleted,
    NotFound,
    Draining { in_flight_deliveries: u64 },
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
) -> Result<WebhookSubscriptionViewRow, sqlx::Error> {
    validate_encrypted_webhook_secret_context(input.id, Some(&input.secret))?;
    validate_encrypted_webhook_headers_context(input.id, Some(&input.custom_headers_json))?;
    let mut transaction = postgres.begin().await?;
    let row = sqlx::query_as::<_, WebhookSubscriptionViewRow>(
        "insert into webhook_subscription (
            id, workspace_id, library_id, display_name, target_url, secret,
            event_types, custom_headers_json, created_by_principal_id
        ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        returning
            id, workspace_id, library_id, display_name, target_url,
            event_types, active, created_at, updated_at",
    )
    .bind(input.id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(&input.display_name)
    .bind(&input.target_url)
    .bind(input.secret.as_str())
    .bind(&input.event_types)
    .bind(serde_json::Value::String(input.custom_headers_json.as_str().to_owned()))
    .bind(input.created_by_principal_id)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(row)
}

/// Loads a secret-free subscription view by id, restricted to workspace ids
/// already authorized by the caller. `None` means instance-wide access for a
/// system administrator; an empty slice intentionally matches no rows.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the subscription row.
pub async fn get_webhook_subscription_view_by_id_in_workspace_scope(
    postgres: &PgPool,
    id: Uuid,
    authorized_workspace_ids: Option<&[Uuid]>,
) -> Result<Option<WebhookSubscriptionViewRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookSubscriptionViewRow>(
        "select id, workspace_id, library_id, display_name, target_url,
                event_types, active, created_at, updated_at
         from webhook_subscription
         where id = $1
           and ($2::uuid[] is null or workspace_id = any($2))",
    )
    .bind(id)
    .bind(authorized_workspace_ids)
    .fetch_optional(postgres)
    .await
}

/// Lists secret-free active subscription targets matching one lifecycle event.
/// Workspace-wide subscriptions and subscriptions scoped to `library_id` are
/// both included.
///
/// # Errors
/// Returns any `SQLx` error raised while querying subscription rows.
pub async fn list_active_webhook_subscription_targets_for_event(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    event_type: &str,
) -> Result<Vec<WebhookSubscriptionTargetRow>, sqlx::Error> {
    let targets = sqlx::query_as::<_, WebhookSubscriptionTargetRow>(
        "select id, target_url
         from webhook_subscription
         where workspace_id = $1
           and active = true
           and $3 = any(event_types)
           and (library_id is null or library_id = $2)
         order by id
         limit $4",
    )
    .bind(workspace_id)
    .bind(library_id)
    .bind(event_type)
    .bind(MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE + 1)
    .fetch_all(postgres)
    .await?;
    if i64::try_from(targets.len()).unwrap_or(i64::MAX)
        > MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE
    {
        return Err(sqlx::Error::Protocol(
            "webhook event fanout exceeds the active subscription quota".to_string(),
        ));
    }
    Ok(targets)
}

/// Lists one bounded keyset page of secret-free subscriptions for a workspace
/// (all states), preserving the existing oldest-first ordering.
///
/// # Errors
/// Returns any `SQLx` error raised while querying subscription rows.
pub async fn list_webhook_subscriptions_by_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<WebhookSubscriptionViewPage, sqlx::Error> {
    let (cursor_created_at, cursor_id) = cursor.unzip();
    let bounded_limit = limit.clamp(1, MAX_WEBHOOK_API_PAGE_SIZE);
    // Fetch `limit + 1` rows so a next-page cursor can be reported without a
    // second round-trip; the extra row is trimmed below.
    let fetch_limit = i64::from(bounded_limit) + 1;
    let mut rows = sqlx::query_as::<_, WebhookSubscriptionViewRow>(
        "select id, workspace_id, library_id, display_name, target_url,
                event_types, active, created_at, updated_at
         from webhook_subscription
         where workspace_id = $1
           and (
                $2::timestamptz is null
                or (created_at, id) > ($2, $3)
         )
         order by created_at asc, id asc
         limit $4",
    )
    .bind(workspace_id)
    .bind(cursor_created_at)
    .bind(cursor_id)
    .bind(fetch_limit)
    .fetch_all(postgres)
    .await?;
    let limit_usize = usize::try_from(bounded_limit).unwrap_or(usize::MAX);
    let next_cursor = if rows.len() > limit_usize {
        rows.truncate(limit_usize);
        rows.last().map(|row| (row.created_at, row.id))
    } else {
        None
    };
    Ok(WebhookSubscriptionViewPage { rows, next_cursor })
}

/// Patches a webhook subscription; only supplied fields are updated.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the subscription row.
pub async fn update_webhook_subscription(
    postgres: &PgPool,
    id: Uuid,
    input: &UpdateWebhookSubscription,
) -> Result<Option<WebhookSubscriptionViewRow>, sqlx::Error> {
    update_webhook_subscription_in_scope(postgres, id, None, input).await
}

/// Applies a subscription patch only inside the already-authorized workspace.
/// This is the only update entry point used by the public global-id API.
pub async fn update_webhook_subscription_in_workspace(
    postgres: &PgPool,
    id: Uuid,
    workspace_id: Uuid,
    input: &UpdateWebhookSubscription,
) -> Result<Option<WebhookSubscriptionViewRow>, sqlx::Error> {
    update_webhook_subscription_in_scope(postgres, id, Some(workspace_id), input).await
}

async fn update_webhook_subscription_in_scope(
    postgres: &PgPool,
    id: Uuid,
    workspace_id: Option<Uuid>,
    input: &UpdateWebhookSubscription,
) -> Result<Option<WebhookSubscriptionViewRow>, sqlx::Error> {
    validate_encrypted_webhook_secret_context(id, input.secret.as_ref())?;
    validate_encrypted_webhook_headers_context(id, input.custom_headers_json.as_ref())?;
    let mut transaction = postgres.begin().await?;
    if input.active == Some(true) {
        let delete_requested_at = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "select delete_requested_at
             from webhook_subscription
             where id = $1
               and ($2::uuid is null or workspace_id = $2)
             for update",
        )
        .bind(id)
        .bind(workspace_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(delete_requested_at) = delete_requested_at else {
            transaction.rollback().await?;
            return Ok(None);
        };
        if delete_requested_at.is_some() {
            transaction.rollback().await?;
            return Err(sqlx::Error::Protocol(
                DRAINING_WEBHOOK_SUBSCRIPTION_REACTIVATION_ERROR.to_string(),
            ));
        }
    }
    let row = sqlx::query_as::<_, WebhookSubscriptionViewRow>(
        "update webhook_subscription
         set display_name        = coalesce($2, display_name),
             target_url          = coalesce($3, target_url),
             secret              = coalesce($4, secret),
             event_types         = coalesce($5, event_types),
             custom_headers_json = coalesce($6, custom_headers_json),
             active              = coalesce($7, active),
             updated_at          = now()
         where id = $1
           and ($8::uuid is null or workspace_id = $8)
           and not (coalesce($7, false) and delete_requested_at is not null)
         returning
             id, workspace_id, library_id, display_name, target_url,
             event_types, active, created_at, updated_at",
    )
    .bind(id)
    .bind(input.display_name.as_deref())
    .bind(input.target_url.as_deref())
    .bind(input.secret.as_ref().map(EncryptedSecret::as_str))
    .bind(input.event_types.as_deref())
    .bind(
        input
            .custom_headers_json
            .as_ref()
            .map(|headers| serde_json::Value::String(headers.as_str().to_owned())),
    )
    .bind(input.active)
    .bind(workspace_id)
    .fetch_optional(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(row)
}

/// Atomically fences new claims, cancels linked queue work, then hard-deletes
/// a subscription only when no current delivery lease can still perform HTTP.
///
/// Locking the subscription first prevents a concurrent delivery-attempt
/// insert from slipping between cancellation and the cascading delete. Leased
/// workers observe the canonical `canceled` job state, while the delivery
/// ownership CAS prevents any already in-flight response from scheduling a
/// retry after its attempt row is deleted. A current delivery returns
/// [`DeleteWebhookSubscriptionOutcome::Draining`] and leaves the now-inactive
/// subscription in place; the operator retries deletion after that lease
/// terminalizes. Lease age alone is never treated as owner acknowledgement.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the subscription row.
pub async fn delete_webhook_subscription(
    postgres: &PgPool,
    id: Uuid,
) -> Result<DeleteWebhookSubscriptionOutcome, sqlx::Error> {
    delete_webhook_subscription_in_scope(postgres, id, None).await
}

/// Deletes a subscription only inside the already-authorized workspace. This
/// prevents global UUIDs from becoming a cross-tenant existence oracle.
pub async fn delete_webhook_subscription_in_workspace(
    postgres: &PgPool,
    id: Uuid,
    workspace_id: Uuid,
) -> Result<DeleteWebhookSubscriptionOutcome, sqlx::Error> {
    delete_webhook_subscription_in_scope(postgres, id, Some(workspace_id)).await
}

async fn delete_webhook_subscription_in_scope(
    postgres: &PgPool,
    id: Uuid,
    workspace_id: Option<Uuid>,
) -> Result<DeleteWebhookSubscriptionOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let locked = sqlx::query_scalar::<_, Uuid>(
        "select id
         from webhook_subscription
         where id = $1
           and ($2::uuid is null or workspace_id = $2)
         for update",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if locked.is_none() {
        transaction.commit().await?;
        return Ok(DeleteWebhookSubscriptionOutcome::NotFound);
    }

    // `active=false` is the durable delete fence. A claim serialized after
    // this transaction may still terminalize queue state, but cannot send.
    sqlx::query(
        "update webhook_subscription
         set active = false,
             delete_requested_at = coalesce(delete_requested_at, now()),
             updated_at = now()
         where id = $1
           and ($2::uuid is null or workspace_id = $2)",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&mut *transaction)
    .await?;

    // Global lock order for deletion and delivery deferral is subscription ->
    // queue job -> ingest attempt. Preserve it before touching attempts so a
    // concurrent duplicate-worker deferral cannot form a job/attempt cycle.
    let linked_job_ids = sqlx::query_scalar::<_, Uuid>(
        "select job.id
         from ingest_job job
         join webhook_delivery_attempt delivery on delivery.job_id = job.id
         where delivery.subscription_id = $1
           and job.job_kind = 'webhook_delivery'::ingest_job_kind
         order by job.id
         for update of job",
    )
    .bind(id)
    .fetch_all(&mut *transaction)
    .await?;

    // Lock current delivery rows after the subscription and queue jobs. A
    // lease age is deliberately not used as proof that HTTP cannot still
    // happen: only owner terminalization/acknowledgement permits hard delete.
    let in_flight_deliveries = sqlx::query_scalar::<_, Uuid>(
        "select id
         from webhook_delivery_attempt
         where subscription_id = $1
           and delivery_state = 'delivering'
         order by id
         for update",
    )
    .bind(id)
    .fetch_all(&mut *transaction)
    .await?;
    let preserve_in_flight = !in_flight_deliveries.is_empty();

    sqlx::query(
        "update ingest_attempt attempt
         set attempt_state = 'canceled',
             failure_class = 'webhook_delivery',
             failure_code = 'subscription_deleted',
             failure_message = 'Webhook delivery was canceled because its subscription was deleted',
             finished_at = now()
         where attempt_state in ('leased', 'running')
           and attempt.job_id = any($2)
           and (
               not $3::bool
               or not exists (
                   select 1
                   from webhook_delivery_attempt delivery
                   where delivery.subscription_id = $1
                     and delivery.job_id = attempt.job_id
                     and delivery.delivery_state = 'delivering'
               )
           )",
    )
    .bind(id)
    .bind(&linked_job_ids)
    .bind(preserve_in_flight)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "update ingest_job job
         set queue_state = 'canceled',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where job.job_kind = 'webhook_delivery'::ingest_job_kind
           and job.queue_state in ('queued', 'leased', 'paused', 'failed')
           and job.id = any($2)
           and (
               not $3::bool
               or not exists (
                   select 1
                   from webhook_delivery_attempt delivery
                   where delivery.subscription_id = $1
                     and delivery.job_id = job.id
                     and delivery.delivery_state = 'delivering'
               )
           )",
    )
    .bind(id)
    .bind(&linked_job_ids)
    .bind(preserve_in_flight)
    .execute(&mut *transaction)
    .await?;
    if !in_flight_deliveries.is_empty() {
        let count = u64::try_from(in_flight_deliveries.len()).unwrap_or(u64::MAX);
        transaction.commit().await?;
        return Ok(DeleteWebhookSubscriptionOutcome::Draining { in_flight_deliveries: count });
    }

    let result = sqlx::query(
        "delete from webhook_subscription
         where id = $1
           and ($2::uuid is null or workspace_id = $2)",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    if result.rows_affected() == 1 {
        Ok(DeleteWebhookSubscriptionOutcome::Deleted)
    } else {
        Ok(DeleteWebhookSubscriptionOutcome::NotFound)
    }
}

/// Explicitly acknowledges a stuck draining owner and abandons its delivery.
///
/// This is an operator-only recovery primitive, not an automatic lease reaper:
/// a remote endpoint may already have received the request even though the
/// owner never persisted its result. Callers must require an explicit
/// at-least-once/duplicate-delivery risk acknowledgement before invoking it.
pub async fn force_abandon_draining_webhook_deliveries(
    postgres: &PgPool,
    subscription_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let draining = sqlx::query_scalar::<_, Uuid>(
        "select id
         from webhook_subscription
         where id = $1
           and active = false
           and delete_requested_at is not null
         for update",
    )
    .bind(subscription_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if draining.is_none() {
        transaction.rollback().await?;
        return Ok(0);
    }

    let job_ids = sqlx::query_scalar::<_, Uuid>(
        "select job.id
         from ingest_job job
         join webhook_delivery_attempt delivery on delivery.job_id = job.id
         where delivery.subscription_id = $1
         order by job.id
         for update of job",
    )
    .bind(subscription_id)
    .fetch_all(&mut *transaction)
    .await?;
    let delivery_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from webhook_delivery_attempt
         where subscription_id = $1
           and delivery_state = 'delivering'
         order by id
         for update",
    )
    .bind(subscription_id)
    .fetch_all(&mut *transaction)
    .await?;
    if delivery_ids.is_empty() {
        transaction.commit().await?;
        return Ok(0);
    }

    sqlx::query(
        "update webhook_delivery_attempt
         set delivery_state = 'abandoned',
             delivery_lease_token = null,
             next_attempt_at = null,
             error_code = 'operator_force_abandoned',
             error_message = 'Webhook delivery was explicitly abandoned by an operator',
             updated_at = now()
         where id = any($1)",
    )
    .bind(&delivery_ids)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update ingest_job
         set queue_state = 'canceled',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = any($1)
           and queue_state in ('queued', 'leased', 'paused', 'failed')",
    )
    .bind(&job_ids)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update ingest_attempt
         set attempt_state = 'canceled',
             failure_class = 'webhook_delivery',
             failure_code = 'operator_force_abandoned',
             failure_message = 'Webhook delivery ownership was explicitly abandoned by an operator',
             finished_at = now(),
             retryable = false
         where job_id = any($1)
           and attempt_state in ('leased', 'running')",
    )
    .bind(&job_ids)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    u64::try_from(delivery_ids.len())
        .map_err(|_| sqlx::Error::Protocol("webhook abandon count overflow".to_string()))
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
    create_webhook_delivery_attempt_with_executor(postgres, input).await
}

async fn create_webhook_delivery_attempt_with_executor<'e, E>(
    executor: E,
    input: &NewWebhookDeliveryAttempt,
) -> Result<WebhookDeliveryAttemptRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "insert into webhook_delivery_attempt (
            subscription_id, workspace_id, library_id,
            event_type, event_id, occurred_at, payload_json, target_url
        ) values ($1, $2, $3, $4, $5, $6, $7, $8)
        returning
            id, subscription_id, workspace_id, library_id,
            event_type, event_id, occurred_at, payload_json, target_url,
            attempt_number, delivery_state::text as delivery_state,
            response_status, response_body_excerpt, error_code, error_message,
            job_id, delivery_lease_token, next_attempt_at,
            delivered_at, created_at, updated_at",
    )
    .bind(input.subscription_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(&input.event_type)
    .bind(&input.event_id)
    .bind(input.occurred_at)
    .bind(input.payload_json.clone())
    .bind(&input.target_url)
    .fetch_one(executor)
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
                event_type, event_id, occurred_at, payload_json, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, response_body_excerpt, error_code, error_message,
                job_id, delivery_lease_token, next_attempt_at,
                delivered_at, created_at, updated_at
         from webhook_delivery_attempt
         where id = $1",
    )
    .bind(id)
    .fetch_optional(postgres)
    .await
}

/// Lists one bounded, newest-first keyset page of data-minimized delivery
/// attempts for one subscription and authorized workspace.
///
/// # Errors
/// Returns any `SQLx` error raised while querying attempt rows.
pub async fn list_webhook_delivery_attempts_by_subscription(
    postgres: &PgPool,
    subscription_id: Uuid,
    workspace_id: Uuid,
    state_filter: Option<&str>,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<WebhookDeliveryAttemptViewPage, sqlx::Error> {
    let (cursor_created_at, cursor_id) = cursor.unzip();
    let bounded_limit = limit.clamp(1, MAX_WEBHOOK_API_PAGE_SIZE);
    // Fetch `limit + 1` rows so a next-page cursor can be reported without a
    // second round-trip; the extra row is trimmed below.
    let fetch_limit = i64::from(bounded_limit) + 1;
    let mut rows = sqlx::query_as::<_, WebhookDeliveryAttemptViewRow>(
        "select id, subscription_id, event_type, event_id, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, error_message, delivered_at,
                next_attempt_at, created_at
         from webhook_delivery_attempt
         where subscription_id = $1
           and workspace_id = $2
           and (
                $3::text is null
                or delivery_state = $3::webhook_delivery_state
           )
           and (
                $4::timestamptz is null
                or (created_at, id) < ($4, $5)
           )
         order by created_at desc, id desc
         limit $6",
    )
    .bind(subscription_id)
    .bind(workspace_id)
    .bind(state_filter)
    .bind(cursor_created_at)
    .bind(cursor_id)
    .bind(fetch_limit)
    .fetch_all(postgres)
    .await?;
    let limit_usize = usize::try_from(bounded_limit).unwrap_or(usize::MAX);
    let next_cursor = if rows.len() > limit_usize {
        rows.truncate(limit_usize);
        rows.last().map(|row| (row.created_at, row.id))
    } else {
        None
    };
    Ok(WebhookDeliveryAttemptViewPage { rows, next_cursor })
}

/// Links a delivery attempt to its ingest job by setting the `job_id` FK.
///
/// Does NOT change `delivery_state` — state transitions are handled separately
/// by `claim_attempt_for_delivery` (when the worker leases the job) and
/// `record_webhook_delivery_result` (when the attempt completes).
///
/// # Errors
/// Returns any `SQLx` error raised while updating the attempt row.
pub async fn link_attempt_to_job(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    link_attempt_to_job_with_executor(postgres, attempt_id, job_id).await
}

async fn link_attempt_to_job_with_executor<'e, E>(
    executor: E,
    attempt_id: Uuid,
    job_id: Uuid,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set job_id = $2,
             updated_at = now()
         where id = $1
           and delivery_state in ('pending', 'failed')
           and delivery_lease_token is null
           and (job_id is null or job_id = $2)
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, occurred_at, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_code, error_message,
             job_id, delivery_lease_token, next_attempt_at,
             delivered_at, created_at, updated_at",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(executor)
    .await
}

/// Atomically creates a delivery attempt, its queue job, and their link.
///
/// The ingest queue's `(library_id, dedupe_key)` uniqueness contract makes
/// event publication idempotent. If a duplicate job is rejected, this
/// transaction also rolls back the otherwise orphaned attempt row.
pub async fn enqueue_webhook_delivery(
    postgres: &PgPool,
    attempt_input: &NewWebhookDeliveryAttempt,
    job_input: &NewIngestJob,
) -> Result<WebhookDeliveryAttemptRow, sqlx::Error> {
    if attempt_input.workspace_id != job_input.workspace_id
        || attempt_input.library_id != job_input.library_id
        || job_input.job_kind != "webhook_delivery"
    {
        return Err(sqlx::Error::Protocol(
            "webhook attempt and queue job scopes must match".to_string(),
        ));
    }
    let mut transaction = postgres.begin().await?;
    let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        attempt_input.workspace_id,
        attempt_input.library_id,
    )
    .await?;
    if !parent_locked {
        transaction.rollback().await?;
        return Err(sqlx::Error::Protocol(
            "webhook delivery library scope is unavailable".to_string(),
        ));
    }
    let attempt =
        match create_webhook_delivery_attempt_with_executor(&mut *transaction, attempt_input).await
        {
            Ok(attempt) => attempt,
            Err(error) if is_webhook_delivery_event_identity_conflict(&error) => {
                transaction.rollback().await?;
                return find_compatible_linked_delivery_attempt(postgres, attempt_input)
                    .await?
                    .ok_or(error);
            }
            Err(error) => return Err(error),
        };
    let job = match create_ingest_job_with_executor(&mut *transaction, job_input).await {
        Ok(job) => job,
        Err(error) if is_ingest_job_dedupe_conflict(&error) => {
            transaction.rollback().await?;
            return find_compatible_linked_delivery_attempt(postgres, attempt_input)
                .await?
                .ok_or(error);
        }
        Err(error) => return Err(error),
    };
    let linked = link_attempt_to_job_with_executor(&mut *transaction, attempt.id, job.id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    transaction.commit().await?;
    Ok(linked)
}

fn is_ingest_job_dedupe_conflict(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database_error)
            if database_error.constraint() == Some("idx_ingest_job_dedupe_key")
    )
}

fn is_webhook_delivery_event_identity_conflict(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database_error)
            if database_error.constraint()
                == Some("webhook_delivery_attempt_subscription_event_key")
    )
}

async fn find_linked_delivery_attempt(
    postgres: &PgPool,
    subscription_id: Uuid,
    event_id: &str,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "select id, subscription_id, workspace_id, library_id,
                event_type, event_id, occurred_at, payload_json, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, response_body_excerpt, error_code, error_message,
                job_id, delivery_lease_token, next_attempt_at,
                delivered_at, created_at, updated_at
         from webhook_delivery_attempt
         where subscription_id = $1
           and event_id = $2
           and job_id is not null
         order by created_at asc, id asc
         limit 1",
    )
    .bind(subscription_id)
    .bind(event_id)
    .fetch_optional(postgres)
    .await
}

async fn find_compatible_linked_delivery_attempt(
    postgres: &PgPool,
    expected: &NewWebhookDeliveryAttempt,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let existing =
        find_linked_delivery_attempt(postgres, expected.subscription_id, &expected.event_id)
            .await?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    if existing.workspace_id != expected.workspace_id
        || existing.library_id != Some(expected.library_id)
        || existing.event_type != expected.event_type
        || existing.occurred_at.timestamp_micros() != expected.occurred_at.timestamp_micros()
        || existing.payload_json != expected.payload_json
    {
        return Err(sqlx::Error::Protocol(
            "webhook delivery event identity was reused with incompatible content".to_string(),
        ));
    }
    Ok(Some(existing))
}

/// Claims a delivery attempt for one exact queue job and returns the coherent
/// subscription snapshot protected by the same transaction.
///
/// A recent `delivering` row cannot be claimed twice. A row older than the
/// canonical five-minute stale-lease window may be reclaimed after a worker
/// crash.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the attempt row.
pub async fn claim_attempt_for_delivery(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
) -> Result<WebhookDeliveryClaimOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    // Lock order is subscription -> delivery attempt, matching deletion. A
    // delete that wins this lock removes/cancels the row before claim; a claim
    // that wins publishes `delivering` before delete decides whether it may
    // return success.
    let subscription = sqlx::query_as::<_, WebhookSubscriptionRow>(
        "select
            subscription.id,
            subscription.workspace_id,
            subscription.library_id,
            subscription.display_name,
            subscription.target_url,
            subscription.secret,
            subscription.event_types,
            subscription.custom_headers_json,
            subscription.active,
            subscription.created_by_principal_id,
            subscription.created_at,
            subscription.updated_at
         from webhook_subscription subscription
         join webhook_delivery_attempt delivery
           on delivery.subscription_id = subscription.id
         where delivery.id = $1
           and delivery.job_id = $2
         for share of subscription",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(subscription) = subscription else {
        transaction.commit().await?;
        return Ok(WebhookDeliveryClaimOutcome::Canceled);
    };

    let lease_token = Uuid::now_v7();
    let claimed = sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set delivery_state = 'delivering',
             target_url = $3,
             delivery_lease_token = $4,
             response_status = null,
             response_body_excerpt = null,
             error_code = null,
             error_message = null,
             next_attempt_at = null,
             updated_at = now()
         where id = $1
           and job_id = $2
           and (
                delivery_state in ('pending', 'failed')
                or (
                    delivery_state = 'delivering'
                    and delivery_lease_token is not null
                    and updated_at < now() - make_interval(secs => $5::double precision)
                    and $6::bool
                )
           )
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, occurred_at, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_code, error_message,
             job_id, delivery_lease_token, next_attempt_at,
             delivered_at, created_at, updated_at",
    )
    .bind(attempt_id)
    .bind(job_id)
    .bind(&subscription.target_url)
    .bind(lease_token)
    .bind(WEBHOOK_DELIVERY_LEASE_TIMEOUT_SECONDS as f64)
    .bind(subscription.active)
    .fetch_optional(&mut *transaction)
    .await?;
    if let Some(attempt) = claimed {
        transaction.commit().await?;
        return Ok(WebhookDeliveryClaimOutcome::Claimed {
            attempt: Box::new(attempt),
            subscription: Box::new(subscription),
        });
    }

    let current = sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
        "select delivery_state::text,
                case
                    when delivery_state <> 'delivering' then null
                    when delivery_lease_token is null
                        then now() + make_interval(secs => $4::double precision)
                    when $3::bool then greatest(
                        updated_at + make_interval(secs => $4::double precision),
                        now()
                    )
                    else now() + make_interval(secs => $4::double precision)
                end as retry_at
         from webhook_delivery_attempt
         where id = $1
           and job_id = $2",
    )
    .bind(attempt_id)
    .bind(job_id)
    .bind(subscription.active)
    .bind(WEBHOOK_DELIVERY_LEASE_TIMEOUT_SECONDS as f64)
    .fetch_optional(&mut *transaction)
    .await?;
    transaction.commit().await?;
    let Some((delivery_state, retry_at)) = current else {
        return Ok(WebhookDeliveryClaimOutcome::Canceled);
    };
    if delivery_state == "delivering" {
        let retry_at = retry_at.ok_or_else(|| {
            sqlx::Error::Protocol("delivering webhook lease is missing database retry time".into())
        })?;
        return Ok(WebhookDeliveryClaimOutcome::InFlight { attempt_id, retry_at });
    }
    Ok(WebhookDeliveryClaimOutcome::Terminal { attempt_id, delivery_state })
}

fn validate_webhook_delivery_completion(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    validate_webhook_completion_state(completion)?;
    validate_webhook_completion_failure(completion)?;
    validate_webhook_completion_delivery(completion)?;
    validate_webhook_completion_failure_fields(completion)
}

fn webhook_completion_protocol_error(message: &str) -> sqlx::Error {
    sqlx::Error::Protocol(message.into())
}

fn validate_webhook_completion_state(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    if !matches!(completion.delivery_state, "delivered" | "failed" | "abandoned")
        || completion.attempt_number < 1
    {
        return Err(webhook_completion_protocol_error(
            "webhook completion must be terminal for one attempted delivery",
        ));
    }
    Ok(())
}

fn validate_webhook_completion_failure(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    if completion.error_code.is_some() != completion.error_summary.is_some() {
        return Err(webhook_completion_protocol_error(
            "webhook completion error code and summary must be supplied together",
        ));
    }
    match (completion.delivery_state, completion.error_code.is_some()) {
        ("delivered", true) => Err(webhook_completion_protocol_error(
            "delivered webhook completion cannot contain a failure",
        )),
        ("delivered", false) | (_, true) => Ok(()),
        (_, false) => Err(webhook_completion_protocol_error(
            "failed webhook completion requires a typed failure",
        )),
    }
}

fn validate_webhook_completion_delivery(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    if completion.delivery_state == "delivered"
        && (!completion.response_status.is_some_and(|status| (200..300).contains(&status))
            || completion.next_attempt_at.is_some())
    {
        return Err(webhook_completion_protocol_error(
            "delivered webhook completion requires a successful status and no retry",
        ));
    }
    if completion.delivery_state == "abandoned" && completion.next_attempt_at.is_some() {
        return Err(webhook_completion_protocol_error(
            "abandoned webhook completion cannot schedule a retry",
        ));
    }
    Ok(())
}

fn validate_webhook_completion_failure_fields(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    if completion.error_code.is_some_and(|code| !canonical_webhook_failure_code(code)) {
        return Err(webhook_completion_protocol_error(
            "webhook completion failure code is not canonical",
        ));
    }
    if completion.error_summary.is_some_and(|summary| summary.len() > 512) {
        return Err(webhook_completion_protocol_error(
            "webhook completion failure summary exceeds 512 bytes",
        ));
    }
    Ok(())
}

fn canonical_webhook_failure_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))
        })
}

async fn record_webhook_delivery_result_with_executor<'e, E>(
    executor: E,
    completion: &WebhookDeliveryCompletion<'_>,
    replacement_job_id: Option<Uuid>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set delivery_state         = $4::webhook_delivery_state,
             attempt_number         = $5,
             response_status        = $6,
             response_body_excerpt  = null,
             error_code             = $7,
             error_message          = $8,
             next_attempt_at        = $9,
             job_id                 = coalesce($10, job_id),
             delivery_lease_token   = null,
             delivered_at           = case when $4 = 'delivered' then now() else delivered_at end,
             updated_at             = now()
         where id = $1
           and job_id = $2
           and delivery_lease_token = $3
           and delivery_state = 'delivering'
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, occurred_at, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_code, error_message,
             job_id, delivery_lease_token, next_attempt_at,
             delivered_at, created_at, updated_at",
    )
    .bind(completion.attempt_id)
    .bind(completion.job_id)
    .bind(completion.lease_token)
    .bind(completion.delivery_state)
    .bind(completion.attempt_number)
    .bind(completion.response_status)
    .bind(completion.error_code)
    .bind(completion.error_summary)
    .bind(completion.next_attempt_at)
    .bind(replacement_job_id)
    .fetch_optional(executor)
    .await
}

/// Records a delivery result only while the caller owns the exact attempt/job
/// lease. A stale worker receives `None` and cannot overwrite a newer result.
///
/// # Errors
/// Returns a protocol error for an invalid completion or any `SQLx` error raised
/// while updating the attempt row.
pub async fn record_webhook_delivery_result(
    postgres: &PgPool,
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    validate_webhook_delivery_completion(completion)?;
    record_webhook_delivery_result_with_executor(postgres, completion, None).await
}

/// Releases an owned delivery claim when execution is canceled before a
/// response can be classified. The attempt number is intentionally unchanged:
/// no delivery result was observed, and the current ingest lease decides
/// whether the queue job is canceled, paused, or retried.
pub async fn release_webhook_delivery_claim(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
    lease_token: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update webhook_delivery_attempt
         set delivery_state = 'pending',
             delivery_lease_token = null,
             response_status = null,
             response_body_excerpt = null,
             error_code = null,
             error_message = null,
             next_attempt_at = null,
             updated_at = now()
         where id = $1
           and job_id = $2
           and delivery_lease_token = $3
           and delivery_state = 'delivering'",
    )
    .bind(attempt_id)
    .bind(job_id)
    .bind(lease_token)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Atomically records a retryable delivery result, links the delivery to its
/// replacement job, and terminalizes the exact current ingest attempt/job.
///
/// There is no committed state in which the delivery points at the replacement
/// while the old queue lease remains recoverable. A process crash or an
/// ambiguous database response therefore cannot make the old job consume its
/// retry budget after `job_id` has moved.
pub async fn record_webhook_delivery_failure_and_handoff_retry(
    postgres: &PgPool,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
    job_input: &NewIngestJob,
) -> Result<WebhookRetryHandoffOutcome, sqlx::Error> {
    validate_webhook_delivery_completion(completion)?;
    if completion.delivery_state != "failed" || completion.next_attempt_at.is_none() {
        return Err(sqlx::Error::Protocol(
            "webhook retry handoff requires a failed completion with a retry timestamp".into(),
        ));
    }
    if job_input.dedupe_key.is_none() {
        return Err(sqlx::Error::Protocol("webhook retry handoff requires a dedupe key".into()));
    }

    let mut transaction = postgres.begin().await?;
    let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        job_input.workspace_id,
        job_input.library_id,
    )
    .await?;
    if !parent_locked {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    }
    match lock_subscription_for_retry_completion(&mut transaction, completion).await? {
        None => {
            transaction.rollback().await?;
            return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
        }
        Some(false) => {
            let terminal_completion = WebhookDeliveryCompletion {
                attempt_id: completion.attempt_id,
                job_id: completion.job_id,
                lease_token: completion.lease_token,
                delivery_state: completion.delivery_state,
                attempt_number: completion.attempt_number,
                response_status: completion.response_status,
                error_code: completion.error_code,
                error_summary: completion.error_summary,
                next_attempt_at: None,
            };
            let recorded = record_webhook_delivery_result_with_executor(
                &mut *transaction,
                &terminal_completion,
                None,
            )
            .await?;
            let Some(recorded) = recorded else {
                transaction.rollback().await?;
                return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
            };
            transaction.commit().await?;
            return Ok(WebhookRetryHandoffOutcome::CompletionRecorded(recorded));
        }
        Some(true) => {}
    }

    let owns_job = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_job
         where id = $1
           and job_kind = 'webhook_delivery'::ingest_job_kind
           and queue_state = 'leased'
           and queue_lease_token = $2
         for update",
    )
    .bind(completion.job_id)
    .bind(handoff.expected_queue_lease_token)
    .fetch_optional(&mut *transaction)
    .await?;
    if owns_job.is_none() {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    }

    let owns_ingest_attempt = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_attempt
         where id = $1
           and job_id = $2
           and attempt_state = 'leased'
         for update",
    )
    .bind(handoff.ingest_attempt_id)
    .bind(completion.job_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if owns_ingest_attempt.is_none() {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    }

    let retry_job_id = get_or_create_compatible_retry_job(&mut transaction, job_input).await?;
    let recorded = record_webhook_delivery_result_with_executor(
        &mut *transaction,
        completion,
        Some(retry_job_id),
    )
    .await?;
    let Some(recorded) = recorded else {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    };

    let finalized_attempt = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'succeeded',
             current_stage = 'webhook_delivery',
             heartbeat_at = now(),
             finished_at = now(),
             failure_class = null,
             failure_code = null,
             failure_message = null,
             progress_percent = 100,
             retryable = false
         where id = $1
           and job_id = $2
           and attempt_state = 'leased'",
    )
    .bind(handoff.ingest_attempt_id)
    .bind(completion.job_id)
    .execute(&mut *transaction)
    .await?;
    if finalized_attempt.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    }

    let finalized_job = sqlx::query(
        "update ingest_job
         set queue_state = 'completed',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and job_kind = 'webhook_delivery'::ingest_job_kind
           and queue_state = 'leased'
           and queue_lease_token = $2",
    )
    .bind(completion.job_id)
    .bind(handoff.expected_queue_lease_token)
    .execute(&mut *transaction)
    .await?;
    if finalized_job.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    }

    transaction.commit().await?;
    Ok(WebhookRetryHandoffOutcome::RetryScheduled(recorded))
}

async fn get_or_create_compatible_retry_job(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    input: &NewIngestJob,
) -> Result<Uuid, sqlx::Error> {
    let retry_job = sqlx::query_as::<_, (Uuid, Uuid, Uuid, String, String, Option<String>)>(
        "insert into ingest_job (
            id, workspace_id, library_id, mutation_id, connector_id,
            async_operation_id, knowledge_document_id, knowledge_revision_id,
            job_kind, queue_state, priority, dedupe_key,
            queued_at, available_at, completed_at
         ) values (
            $1, $2, $3, $4, $5, $6, $7, $8,
            $9::ingest_job_kind, $10::ingest_queue_state, $11, $12,
            coalesce($13, now()), coalesce($14, now()), $15
         )
         on conflict (library_id, dedupe_key) where dedupe_key is not null
         do update set dedupe_key = excluded.dedupe_key
         returning id, workspace_id, library_id,
                   job_kind::text, queue_state::text, dedupe_key",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
    .bind(input.connector_id)
    .bind(input.async_operation_id)
    .bind(input.knowledge_document_id)
    .bind(input.knowledge_revision_id)
    .bind(&input.job_kind)
    .bind(&input.queue_state)
    .bind(input.priority)
    .bind(&input.dedupe_key)
    .bind(input.queued_at)
    .bind(input.available_at)
    .bind(input.completed_at)
    .fetch_one(&mut **transaction)
    .await?;
    if retry_job.1 != input.workspace_id
        || retry_job.2 != input.library_id
        || retry_job.3 != input.job_kind
        || retry_job.5 != input.dedupe_key
        || !matches!(retry_job.4.as_str(), "queued" | "leased" | "paused")
    {
        return Err(sqlx::Error::Protocol(
            "webhook retry dedupe key resolved to an incompatible queue job".into(),
        ));
    }
    Ok(retry_job.0)
}

/// Atomically records a retryable failure and transfers the attempt to a new
/// queue job. The retry row is rolled back when the completion token is stale,
/// so a worker that lost ownership cannot schedule follow-up work.
///
/// # Errors
/// Returns a protocol error for invalid retry input or any SQLx error raised by
/// the fenced completion/queue transaction.
#[cfg(feature = "test-support")]
pub async fn record_webhook_delivery_failure_and_enqueue_retry_detached(
    postgres: &PgPool,
    completion: &WebhookDeliveryCompletion<'_>,
    job_input: &NewIngestJob,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    validate_webhook_retry_input(completion, job_input)?;
    let mut transaction = postgres.begin().await?;
    match lock_subscription_for_retry_completion(&mut transaction, completion).await? {
        None => {
            transaction.rollback().await?;
            return Ok(None);
        }
        Some(false) => {
            return record_inactive_detached_retry(transaction, completion).await;
        }
        Some(true) => {}
    }
    let retry_job = match create_ingest_job_with_executor(&mut *transaction, job_input).await {
        Ok(job) => job,
        Err(error) if is_ingest_job_dedupe_conflict(&error) => {
            transaction.rollback().await?;
            return record_existing_detached_retry(postgres, completion, job_input, error).await;
        }
        Err(error) => return Err(error),
    };
    record_detached_retry_in_transaction(transaction, completion, Some(retry_job.id)).await
}

#[cfg(feature = "test-support")]
fn validate_webhook_retry_input(
    completion: &WebhookDeliveryCompletion<'_>,
    job_input: &NewIngestJob,
) -> Result<(), sqlx::Error> {
    validate_webhook_delivery_completion(completion)?;
    if completion.delivery_state != "failed" || completion.next_attempt_at.is_none() {
        return Err(sqlx::Error::Protocol(
            "webhook retry requires a failed completion with a next-attempt timestamp".into(),
        ));
    }
    if job_input.dedupe_key.is_none() {
        return Err(sqlx::Error::Protocol("webhook retry job requires a dedupe key".into()));
    }
    Ok(())
}

#[cfg(feature = "test-support")]
async fn record_inactive_detached_retry(
    transaction: sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let terminal_completion = terminal_webhook_completion(completion);
    record_detached_retry_in_transaction(transaction, &terminal_completion, None).await
}

#[cfg(feature = "test-support")]
fn terminal_webhook_completion<'a>(
    completion: &'a WebhookDeliveryCompletion<'a>,
) -> WebhookDeliveryCompletion<'a> {
    WebhookDeliveryCompletion {
        attempt_id: completion.attempt_id,
        job_id: completion.job_id,
        lease_token: completion.lease_token,
        delivery_state: completion.delivery_state,
        attempt_number: completion.attempt_number,
        response_status: completion.response_status,
        error_code: completion.error_code,
        error_summary: completion.error_summary,
        next_attempt_at: None,
    }
}

#[cfg(feature = "test-support")]
async fn record_existing_detached_retry(
    postgres: &PgPool,
    completion: &WebhookDeliveryCompletion<'_>,
    job_input: &NewIngestJob,
    conflict: sqlx::Error,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let dedupe_key = job_input
        .dedupe_key
        .as_deref()
        .ok_or_else(|| sqlx::Error::Protocol("webhook retry job requires a dedupe key".into()))?;
    let existing = get_ingest_job_by_dedupe_key(postgres, job_input.library_id, dedupe_key)
        .await?
        .ok_or(conflict)?;
    validate_compatible_retry_job(&existing, job_input, dedupe_key)?;
    let mut transaction = postgres.begin().await?;
    match lock_subscription_for_retry_completion(&mut transaction, completion).await? {
        None => {
            transaction.rollback().await?;
            Ok(None)
        }
        Some(false) => record_inactive_detached_retry(transaction, completion).await,
        Some(true) => {
            record_detached_retry_in_transaction(transaction, completion, Some(existing.id)).await
        }
    }
}

#[cfg(feature = "test-support")]
fn validate_compatible_retry_job(
    existing: &crate::infra::repositories::ingest_repository::IngestJobRow,
    input: &NewIngestJob,
    dedupe_key: &str,
) -> Result<(), sqlx::Error> {
    let is_compatible = existing.workspace_id == input.workspace_id
        && existing.library_id == input.library_id
        && existing.job_kind == input.job_kind
        && existing.dedupe_key.as_deref() == Some(dedupe_key)
        && matches!(existing.queue_state.as_str(), "queued" | "leased" | "paused");
    if !is_compatible {
        return Err(sqlx::Error::Protocol(
            "webhook retry dedupe key resolved to an incompatible queue job".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "test-support")]
async fn record_detached_retry_in_transaction(
    mut transaction: sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    retry_job_id: Option<Uuid>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let recorded =
        record_webhook_delivery_result_with_executor(&mut *transaction, completion, retry_job_id)
            .await?;
    if recorded.is_none() {
        transaction.rollback().await?;
        return Ok(None);
    }
    transaction.commit().await?;
    Ok(recorded)
}

async fn lock_subscription_for_retry_completion(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<Option<bool>, sqlx::Error> {
    let subscription_active = sqlx::query_scalar::<_, bool>(
        "select subscription.active
         from webhook_delivery_attempt delivery
         join webhook_subscription subscription
           on subscription.id = delivery.subscription_id
         where delivery.id = $1
           and delivery.job_id = $2
           and delivery.delivery_lease_token = $3
           and delivery.delivery_state = 'delivering'
         for key share of subscription",
    )
    .bind(completion.attempt_id)
    .bind(completion.job_id)
    .bind(completion.lease_token)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(subscription_active)
}

#[cfg(test)]
mod debug_redaction_tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        ACTIVE_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR, TOTAL_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR,
        WebhookDeliveryAttemptRow, WebhookDeliveryAttemptViewRow, WebhookSubscriptionViewRow,
        is_active_webhook_subscription_quota_error, is_total_webhook_subscription_quota_error,
    };

    #[test]
    fn active_subscription_quota_has_a_narrow_typed_classifier() {
        let quota = sqlx::Error::Protocol(ACTIVE_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR.to_string());
        let unrelated = sqlx::Error::Protocol("unrelated".to_string());

        assert!(is_active_webhook_subscription_quota_error(&quota));
        assert!(!is_active_webhook_subscription_quota_error(&unrelated));
    }

    #[test]
    fn total_subscription_quota_has_a_narrow_typed_classifier() {
        let quota = sqlx::Error::Protocol(TOTAL_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR.to_string());
        let unrelated = sqlx::Error::Protocol("unrelated".to_string());

        assert!(is_total_webhook_subscription_quota_error(&quota));
        assert!(!is_total_webhook_subscription_quota_error(&unrelated));
    }

    #[test]
    fn management_projection_debug_redacts_remote_urls_and_errors() {
        let now = Utc::now();
        let subscription = WebhookSubscriptionViewRow {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: None,
            display_name: "synthetic".to_string(),
            target_url: "https://example.invalid/?token=subscription-secret".to_string(),
            event_types: vec!["revision.ready".to_string()],
            active: true,
            created_at: now,
            updated_at: now,
        };
        let attempt = WebhookDeliveryAttemptViewRow {
            id: Uuid::now_v7(),
            subscription_id: subscription.id,
            event_type: "revision.ready".to_string(),
            event_id: Uuid::now_v7().to_string(),
            target_url: "https://example.invalid/?token=attempt-secret".to_string(),
            attempt_number: 1,
            delivery_state: "failed".to_string(),
            response_status: Some(500),
            error_message: Some("remote-error-secret".to_string()),
            delivered_at: None,
            next_attempt_at: None,
            created_at: now,
        };

        let debug = format!("{subscription:?} {attempt:?}");
        for secret in ["subscription-secret", "attempt-secret", "remote-error-secret"] {
            assert!(!debug.contains(secret), "Debug exposed {secret}");
        }
    }

    #[test]
    fn delivery_attempt_debug_redacts_remote_and_payload_content() {
        let secret_payload = "synthetic-payload-secret";
        let secret_target = "https://example.invalid/private?token=synthetic-query-secret";
        let secret_response = "synthetic-response-secret";
        let secret_error = "synthetic-error-secret";
        let lease_token = Uuid::now_v7();
        let now = Utc::now();
        let row = WebhookDeliveryAttemptRow {
            id: Uuid::now_v7(),
            subscription_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Some(Uuid::now_v7()),
            event_type: "revision.ready".to_string(),
            event_id: Uuid::now_v7().to_string(),
            occurred_at: now,
            payload_json: json!({ "secret": secret_payload }),
            target_url: secret_target.to_string(),
            attempt_number: 1,
            delivery_state: "delivering".to_string(),
            response_status: None,
            response_body_excerpt: Some(secret_response.to_string()),
            error_code: Some("transport_request".to_string()),
            error_message: Some(secret_error.to_string()),
            job_id: Some(Uuid::now_v7()),
            delivery_lease_token: Some(lease_token),
            next_attempt_at: None,
            delivered_at: None,
            created_at: now,
            updated_at: now,
        };

        let debug = format!("{row:?}");
        let lease_token_text = lease_token.to_string();
        for secret in [
            secret_payload,
            secret_target,
            secret_response,
            secret_error,
            lease_token_text.as_str(),
        ] {
            assert!(!debug.contains(secret), "Debug exposed {secret}");
        }
    }
}
