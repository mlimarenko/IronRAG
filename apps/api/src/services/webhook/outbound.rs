/// Outbound webhook publisher — fire-and-forget fanout that enqueues a
/// `webhook_delivery` ingest job for every active subscription that matches
/// the outgoing event.
///
/// This runs as an async task spawned from the ingest pipeline and document
/// service hook points.  Errors are logged at WARN level and do NOT propagate
/// to the caller so ingest / delete operations are never blocked or rolled
/// back by a delivery scheduling failure.
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use crate::{
    domains::webhook::WebhookEvent,
    infra::repositories::{
        ingest_repository::{self, NewIngestJob},
        webhook_repository::{self, NewWebhookDeliveryAttempt},
    },
};

/// Publish one outbound webhook event by fanning out to all matching
/// subscriptions.
///
/// For each matching subscription a `webhook_delivery_attempt` row is created
/// and a `webhook_delivery` ingest job is enqueued.  On any per-subscription
/// error the error is logged and processing continues — partial fan-out is
/// preferred over total failure.
///
/// # Errors
/// Returns a `Vec` of per-subscription error strings so callers can log them.
/// The canonical hook-point callers (worker.rs, document.rs) deliberately
/// ignore the return value.
pub async fn publish_webhook_event(postgres: &PgPool, event: &WebhookEvent) -> Vec<String> {
    let subs = match webhook_repository::list_active_webhook_subscriptions_for_event(
        postgres,
        event.workspace_id,
        event.library_id,
        &event.event_type,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            warn!(
                workspace_id = %event.workspace_id,
                event_type = %event.event_type,
                error = %e,
                "webhook: failed to list active subscriptions for outbound event"
            );
            return vec![format!("list_subscriptions: {e}")];
        }
    };

    let mut errors = Vec::new();

    for sub in &subs {
        // 1. Create a pending delivery attempt row.
        let attempt = match webhook_repository::create_webhook_delivery_attempt(
            postgres,
            &NewWebhookDeliveryAttempt {
                subscription_id: sub.id,
                workspace_id: event.workspace_id,
                library_id: event.library_id,
                event_type: event.event_type.clone(),
                event_id: event.event_id.clone(),
                payload_json: event.payload_json.clone(),
                target_url: sub.target_url.clone(),
            },
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                warn!(
                    subscription_id = %sub.id,
                    event_type = %event.event_type,
                    error = %e,
                    "webhook: failed to create delivery attempt row"
                );
                errors.push(format!("create_attempt[{}]: {e}", sub.id));
                continue;
            }
        };

        // 2. Enqueue a webhook_delivery ingest job.
        let job = match ingest_repository::create_ingest_job(
            postgres,
            &NewIngestJob {
                workspace_id: event.workspace_id,
                library_id: Uuid::nil(), // delivery jobs are not library-scoped
                mutation_id: None,
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
            },
        )
        .await
        {
            Ok(j) => j,
            Err(e) => {
                warn!(
                    subscription_id = %sub.id,
                    attempt_id = %attempt.id,
                    error = %e,
                    "webhook: failed to enqueue delivery job"
                );
                errors.push(format!("create_job[{}]: {e}", sub.id));
                continue;
            }
        };

        // 3. Link the job to the attempt (no state change; delivering is set when worker leases).
        if let Err(e) = webhook_repository::link_attempt_to_job(postgres, attempt.id, job.id).await
        {
            warn!(
                attempt_id = %attempt.id,
                job_id = %job.id,
                error = %e,
                "webhook: failed to link delivery job to attempt row"
            );
            errors.push(format!("link_job[{}]: {e}", sub.id));
        }
    }

    errors
}
