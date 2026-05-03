use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Outbound webhook subscription — one subscriber URL registered against a workspace/library.
#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookSubscription {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub target_url: String,
    /// HMAC-SHA256 secret — stored plaintext, never exposed in API responses.
    pub secret: String,
    /// Event type filter, e.g. `["revision.ready", "document.deleted"]`.
    pub event_types: Vec<String>,
    pub custom_headers_json: serde_json::Value,
    pub active: bool,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for WebhookSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSubscription")
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

/// Delivery state for one outbound delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookDeliveryState {
    Pending,
    Delivering,
    Delivered,
    Failed,
    Abandoned,
}

impl WebhookDeliveryState {
    #[must_use]
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivering => "delivering",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }

    #[must_use]
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "delivering" => Self::Delivering,
            "delivered" => Self::Delivered,
            "failed" => Self::Failed,
            "abandoned" => Self::Abandoned,
            _ => Self::Pending,
        }
    }
}

/// One outbound delivery attempt record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookDeliveryAttempt {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub event_type: String,
    pub event_id: String,
    pub payload_json: serde_json::Value,
    pub target_url: String,
    pub attempt_number: i32,
    pub delivery_state: WebhookDeliveryState,
    pub response_status: Option<i32>,
    pub response_body_excerpt: Option<String>,
    pub error_message: Option<String>,
    pub job_id: Option<Uuid>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Outbound event that will be fanned out to matching subscriptions.
#[derive(Debug, Clone)]
pub struct WebhookEvent {
    pub event_type: String,
    pub event_id: String,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub payload_json: serde_json::Value,
}
