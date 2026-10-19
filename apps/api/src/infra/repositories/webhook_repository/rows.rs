use super::*;

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
