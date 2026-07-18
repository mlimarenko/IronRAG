//! Bounded, redacted operator controls for the lifecycle webhook outbox.
//!
//! Audit never loads delivery payloads or subscription configuration. Repair
//! either requeues an exact dead-letter row or records an explicit terminal
//! resolution. Neither operation performs network I/O or mutates another
//! outbox state.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::infra::repositories::webhook_outbox_repository::{self, WebhookLifecycleOutboxAuditRow};

pub use crate::infra::repositories::webhook_outbox_repository::{
    MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT,
    MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES, WebhookLifecycleOutboxAuditCursor,
    WebhookLifecycleOutboxDispatchState,
};

pub const DEFAULT_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT: i64 = 100;

#[derive(Debug, Clone, Copy)]
pub struct WebhookLifecycleOutboxAuditOptions {
    pub dispatch_state: Option<WebhookLifecycleOutboxDispatchState>,
    pub library_id: Option<Uuid>,
    pub cursor: Option<WebhookLifecycleOutboxAuditCursor>,
    pub limit: i64,
}

impl Default for WebhookLifecycleOutboxAuditOptions {
    fn default() -> Self {
        Self {
            dispatch_state: Some(WebhookLifecycleOutboxDispatchState::DeadLetter),
            library_id: None,
            cursor: None,
            limit: DEFAULT_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookLifecycleOutboxAuditReport {
    pub state_filter: Option<WebhookLifecycleOutboxDispatchState>,
    pub library_filter: Option<Uuid>,
    pub limit: i64,
    pub returned: usize,
    pub has_more: bool,
    pub next_cursor: Option<WebhookLifecycleOutboxAuditCursor>,
    pub entries: Vec<WebhookLifecycleOutboxAuditRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookLifecycleOutboxRepairReport {
    pub outbox_id: Uuid,
    pub requeued: bool,
    pub entry: Option<WebhookLifecycleOutboxAuditRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookLifecycleOutboxResolutionReport {
    pub outbox_id: Uuid,
    pub reason_code: String,
    pub resolved: bool,
    pub entry: Option<WebhookLifecycleOutboxAuditRow>,
}

#[must_use]
pub const fn bounded_audit_limit(limit: i64) -> i64 {
    if limit < 1 {
        1
    } else if limit > MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT {
        MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT
    } else {
        limit
    }
}

fn bounded_page(
    candidates: Vec<WebhookLifecycleOutboxAuditRow>,
    limit: i64,
) -> (Vec<WebhookLifecycleOutboxAuditRow>, bool, Option<WebhookLifecycleOutboxAuditCursor>) {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = candidates.len() > limit;
    let entries = candidates.into_iter().take(limit).collect::<Vec<_>>();
    let next_cursor = if has_more {
        entries.last().map(|entry| WebhookLifecycleOutboxAuditCursor {
            created_at: entry.created_at,
            id: entry.id,
        })
    } else {
        None
    };
    (entries, has_more, next_cursor)
}

/// Returns a bounded redacted inventory. No secret-bearing columns are
/// selected by the repository query.
pub async fn audit_webhook_lifecycle_outbox(
    postgres: &PgPool,
    options: WebhookLifecycleOutboxAuditOptions,
) -> Result<WebhookLifecycleOutboxAuditReport, sqlx::Error> {
    let limit = bounded_audit_limit(options.limit);
    let candidates = webhook_outbox_repository::audit_webhook_lifecycle_outbox(
        postgres,
        options.dispatch_state,
        options.library_id,
        options.cursor,
        limit.saturating_add(1),
    )
    .await?;
    let (entries, has_more, next_cursor) = bounded_page(candidates, limit);
    Ok(WebhookLifecycleOutboxAuditReport {
        state_filter: options.dispatch_state,
        library_filter: options.library_id,
        limit,
        returned: entries.len(),
        has_more,
        next_cursor,
        entries,
    })
}

/// Requeues one exact dead-letter row. This only mutates durable queue state;
/// delivery remains the responsibility of the normal worker loop.
pub async fn requeue_dead_letter_webhook_lifecycle_outbox(
    postgres: &PgPool,
    outbox_id: Uuid,
) -> Result<WebhookLifecycleOutboxRepairReport, sqlx::Error> {
    let entry = webhook_outbox_repository::requeue_dead_letter_webhook_lifecycle_outbox(
        postgres, outbox_id,
    )
    .await?;
    Ok(WebhookLifecycleOutboxRepairReport { outbox_id, requeued: entry.is_some(), entry })
}

/// Records an explicit, durable terminal resolution for one exact dead-letter
/// row. The repository atomically appends the redacted global audit event.
pub async fn resolve_dead_letter_webhook_lifecycle_outbox(
    postgres: &PgPool,
    outbox_id: Uuid,
    reason_code: &str,
) -> Result<WebhookLifecycleOutboxResolutionReport, sqlx::Error> {
    let entry = webhook_outbox_repository::resolve_dead_letter_webhook_lifecycle_outbox(
        postgres,
        outbox_id,
        reason_code,
    )
    .await?;
    Ok(WebhookLifecycleOutboxResolutionReport {
        outbox_id,
        reason_code: reason_code.to_string(),
        resolved: entry.is_some(),
        entry,
    })
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::*;

    fn redacted_entry() -> WebhookLifecycleOutboxAuditRow {
        let timestamp = Utc.timestamp_opt(1_750_000_000, 0).single().expect("valid timestamp");
        WebhookLifecycleOutboxAuditRow {
            id: Uuid::nil(),
            event_type: "revision.ready".to_string(),
            occurred_at: timestamp,
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            dispatch_state: "dead_letter".to_string(),
            dispatch_attempts: 8,
            last_error_code: Some("fanout_failed".to_string()),
            resolution_reason_code: None,
            available_at: timestamp,
            lease_expires_at: None,
            dispatched_at: None,
            resolved_at: None,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    #[test]
    fn audit_limit_is_bounded() {
        assert_eq!(bounded_audit_limit(i64::MIN), 1);
        assert_eq!(bounded_audit_limit(25), 25);
        assert_eq!(bounded_audit_limit(i64::MAX), MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT);
    }

    #[test]
    fn state_parser_accepts_cli_and_database_spellings() {
        assert_eq!("dead-letter".parse(), Ok(WebhookLifecycleOutboxDispatchState::DeadLetter));
        assert_eq!("dead_letter".parse(), Ok(WebhookLifecycleOutboxDispatchState::DeadLetter));
        assert_eq!("resolved".parse(), Ok(WebhookLifecycleOutboxDispatchState::Resolved));
        assert!("unknown".parse::<WebhookLifecycleOutboxDispatchState>().is_err());
    }

    #[test]
    fn serialized_reports_cannot_contain_secret_bearing_fields() {
        let report = WebhookLifecycleOutboxAuditReport {
            state_filter: Some(WebhookLifecycleOutboxDispatchState::DeadLetter),
            library_filter: None,
            limit: 100,
            returned: 1,
            has_more: false,
            next_cursor: None,
            entries: vec![redacted_entry()],
        };
        let json = serde_json::to_value(report).expect("serialize redacted report");
        let rendered = json.to_string();
        for forbidden in [
            "payload_json",
            "event_id",
            "target_url",
            "secret",
            "custom_headers",
            "lease_owner",
            "lease_token",
        ] {
            assert!(!rendered.contains(forbidden), "report leaked forbidden field {forbidden}");
        }
        assert!(!rendered.contains("\"last_error\":"), "report leaked raw failure text");
        assert!(rendered.contains("\"last_error_code\":\"fanout_failed\""));
    }

    #[test]
    fn page_uses_last_emitted_row_as_stable_cursor() {
        let first = redacted_entry();
        let second = WebhookLifecycleOutboxAuditRow {
            id: Uuid::from_u128(3),
            created_at: first.created_at - chrono::Duration::seconds(1),
            updated_at: first.updated_at + chrono::Duration::seconds(1),
            ..first.clone()
        };
        let (entries, has_more, next_cursor) = bounded_page(vec![first.clone(), second], 1);
        assert!(has_more);
        assert_eq!(entries, vec![first.clone()]);
        assert_eq!(
            next_cursor,
            Some(WebhookLifecycleOutboxAuditCursor { created_at: first.created_at, id: first.id })
        );
    }
}
