use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Executor, FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::domains::webhook::WebhookEvent;

use super::webhook_repository::{
    MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE, WebhookSubscriptionTargetRow,
};

mod delivery;
mod operator;

pub use delivery::*;
pub use operator::*;

pub const MAX_WEBHOOK_LIFECYCLE_OUTBOX_BATCH_LIMIT: i64 = 100;
pub const MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT: i64 = 500;
pub const MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES: usize = 64;

/// Valid persisted states accepted by the bounded operator audit filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookLifecycleOutboxDispatchState {
    Pending,
    Dispatching,
    Dispatched,
    DeadLetter,
    Resolved,
}

impl WebhookLifecycleOutboxDispatchState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatching => "dispatching",
            Self::Dispatched => "dispatched",
            Self::DeadLetter => "dead_letter",
            Self::Resolved => "resolved",
        }
    }
}

impl std::fmt::Display for WebhookLifecycleOutboxDispatchState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for WebhookLifecycleOutboxDispatchState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "pending" => Ok(Self::Pending),
            "dispatching" => Ok(Self::Dispatching),
            "dispatched" => Ok(Self::Dispatched),
            "dead_letter" => Ok(Self::DeadLetter),
            "resolved" => Ok(Self::Resolved),
            _ => Err(format!(
                "unknown webhook lifecycle outbox state `{value}`; expected pending, dispatching, dispatched, dead-letter, or resolved"
            )),
        }
    }
}

/// Stable keyset cursor for a descending immutable `(created_at, id)` audit
/// page. State-filter membership remains a live, best-effort view: rows that
/// change into or out of a filter while paging require a fresh audit pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WebhookLifecycleOutboxAuditCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

/// Deliberately redacted operator projection. It cannot carry event payloads,
/// target URLs, credentials, custom headers, lease owners/tokens, or raw
/// failure text.
#[derive(Debug, Clone, PartialEq, Eq, FromRow, Serialize)]
pub struct WebhookLifecycleOutboxAuditRow {
    pub id: Uuid,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub dispatch_state: String,
    pub dispatch_attempts: i32,
    pub last_error_code: Option<String>,
    pub resolution_reason_code: Option<String>,
    pub available_at: DateTime<Utc>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WebhookLifecycleOutboxRow {
    pub id: Uuid,
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub payload_json: serde_json::Value,
    pub dispatch_state: String,
    pub dispatch_attempts: i32,
    pub available_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_token: Option<Uuid>,
    pub leased_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub last_error: Option<String>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub resolution_reason_code: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct WebhookLifecycleOutboxLease {
    pub lease_token: Uuid,
    pub events: Vec<WebhookLifecycleOutboxRow>,
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test fixtures use descriptive panic paths for invariant violations"
)]
mod tests;
