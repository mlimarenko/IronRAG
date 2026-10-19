//! Outbound webhook publisher — fire-and-forget fanout that enqueues a
//! `webhook_delivery` ingest job for every active subscription that matches
//! the outgoing event.
//!
//! Errors are logged at WARN level and returned to the caller so durable relay
//! state can be retried without losing successful per-recipient enqueues.

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    domains::webhook::WebhookEvent,
    infra::repositories::{
        ingest_repository::NewIngestJob,
        webhook_repository::{self, NewWebhookDeliveryAttempt, WebhookSubscriptionTargetRow},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookFanoutFailureKind {
    SubscriptionLookup,
    RecipientSnapshotLookup,
    DeliveryEnqueue,
    InvalidEventPayload,
    Cancelled,
    Unclassified,
}

#[derive(Clone, PartialEq, Eq)]
pub struct WebhookFanoutFailure {
    kind: WebhookFanoutFailureKind,
    diagnostic: String,
}

impl WebhookFanoutFailure {
    #[must_use]
    pub fn new(kind: WebhookFanoutFailureKind, diagnostic: impl Into<String>) -> Self {
        Self { kind, diagnostic: diagnostic.into() }
    }

    #[must_use]
    pub const fn kind(&self) -> WebhookFanoutFailureKind {
        self.kind
    }

    /// In-memory operator context. Durable state must use the typed kind only.
    #[must_use]
    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

impl std::fmt::Debug for WebhookFanoutFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookFanoutFailure")
            .field("kind", &self.kind)
            .field("diagnostic", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookTargetFanoutOutcome {
    Completed(Vec<WebhookFanoutFailure>),
    Cancelled,
}

/// Fenced targeted fanout used by the lifecycle-outbox relay.
///
/// Cancellation is checked before each recipient and races every enqueue.
/// Dropping an in-flight SQL future is safe; deterministic delivery dedupe
/// makes a retry converge if the database committed immediately before the
/// cancellation became observable.
pub async fn publish_webhook_event_to_targets_cancellable(
    postgres: &PgPool,
    event: &WebhookEvent,
    subscriptions: &[WebhookSubscriptionTargetRow],
    cancellation_token: &CancellationToken,
) -> WebhookTargetFanoutOutcome {
    if cancellation_token.is_cancelled() {
        return WebhookTargetFanoutOutcome::Cancelled;
    }
    let mut failures = Vec::new();
    let Some(persisted_payload) = event.persisted_delivery_payload() else {
        return WebhookTargetFanoutOutcome::Completed(vec![WebhookFanoutFailure::new(
            WebhookFanoutFailureKind::InvalidEventPayload,
            "expected a JSON object",
        )]);
    };

    for sub in subscriptions {
        if cancellation_token.is_cancelled() {
            return WebhookTargetFanoutOutcome::Cancelled;
        }
        let attempt_input = NewWebhookDeliveryAttempt {
            subscription_id: sub.id,
            workspace_id: event.workspace_id,
            library_id: event.library_id,
            event_type: event.event_type.clone(),
            event_id: event.event_id.clone(),
            occurred_at: event.occurred_at,
            payload_json: persisted_payload.clone(),
            target_url: sub.target_url.clone(),
        };
        let job_input = NewIngestJob {
            workspace_id: event.workspace_id,
            library_id: event.library_id,
            mutation_id: None,
            mutation_item_id: None,
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: None,
            knowledge_revision_id: None,
            job_kind: "webhook_delivery".to_string(),
            queue_state: "queued".to_string(),
            priority: 5,
            dedupe_key: Some(format!("wh-delivery-{}-{}", sub.id, event.event_id)),
            queued_at: None,
            available_at: None,
            completed_at: None,
        };

        let enqueue_result = tokio::select! {
            () = cancellation_token.cancelled() => {
                return WebhookTargetFanoutOutcome::Cancelled;
            }
            result = webhook_repository::enqueue_webhook_delivery(
                postgres,
                &attempt_input,
                &job_input,
            ) => result,
        };
        if let Err(error) = enqueue_result {
            warn!(
                subscription_id = %sub.id,
                event_type = %event.event_type,
                error = %error,
                "webhook: failed to atomically enqueue delivery"
            );
            failures.push(WebhookFanoutFailure::new(
                WebhookFanoutFailureKind::DeliveryEnqueue,
                error.to_string(),
            ));
        }
    }

    WebhookTargetFanoutOutcome::Completed(failures)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test fixtures require descriptive failures")]
mod tests {
    use chrono::Utc;
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn pre_cancelled_fanout_never_touches_postgres() {
        let postgres = PgPoolOptions::new()
            .connect_lazy("postgres://localhost/webhook-cancellation-test")
            .expect("valid lazy PostgreSQL URL");
        let cancellation_token = CancellationToken::new();
        cancellation_token.cancel();
        let event = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: format!("revision.ready:cancellation:{}", Uuid::now_v7()),
            occurred_at: Utc::now(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            payload_json: serde_json::json!({ "revisionId": Uuid::now_v7() }),
        };
        let targets = [WebhookSubscriptionTargetRow {
            id: Uuid::now_v7(),
            target_url: "https://example.invalid/webhook".to_string(),
        }];

        let outcome = publish_webhook_event_to_targets_cancellable(
            &postgres,
            &event,
            &targets,
            &cancellation_token,
        )
        .await;

        assert_eq!(outcome, WebhookTargetFanoutOutcome::Cancelled);
        postgres.close().await;
    }
}
