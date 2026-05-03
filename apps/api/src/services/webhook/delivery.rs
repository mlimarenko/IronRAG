/// Delivery worker stage — executes one outbound HTTP delivery attempt.
///
/// Called from the canonical ingest worker when `job_kind = "webhook_delivery"`.
/// The job payload JSON carries `attempt_id` (UUID of the
/// `webhook_delivery_attempt` row) and `subscription_id`.
///
/// Retry / backoff logic:
///   delay = 2^min(attempt_number, 8) minutes, capped at 6 h.
///   After 8 attempts the delivery is marked `abandoned`.
use std::sync::Arc;

use anyhow::Context as _;
use chrono::Utc;
use tracing::info;

use crate::{
    app::state::AppState,
    infra::repositories::{
        ingest_repository::IngestJobRow,
        webhook_repository::{self, WebhookDeliveryAttemptRow, WebhookSubscriptionRow},
    },
    services::webhook::signature,
};

const MAX_ATTEMPTS: i32 = 8;
const MAX_DELAY_MINUTES: i64 = 360; // 6 h

pub async fn run_webhook_delivery_job(
    state: &Arc<AppState>,
    job: &IngestJobRow,
) -> anyhow::Result<()> {
    // The dedupe_key encodes the attempt_id: "wh-delivery-{sub_id}-{event_id}".
    // We recover the attempt by looking up via job_id.
    let attempt = find_attempt_for_job(state, job.id).await?;

    // Transition to 'delivering' now that the worker has leased the job.
    webhook_repository::mark_attempt_delivering(&state.persistence.postgres, attempt.id)
        .await
        .context("failed to mark delivery attempt as delivering")?;

    let sub = load_subscription(state, attempt.subscription_id).await?;

    let attempt_number = attempt.attempt_number + 1;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let body_bytes =
        serde_json::to_vec(&attempt.payload_json).context("failed to serialize webhook payload")?;

    let sig_header = signature::sign(sub.secret.as_bytes(), ts, &body_bytes);

    let mut req = state
        .canonical_services
        .webhook
        .http_client()
        .post(&attempt.target_url)
        .header("Content-Type", "application/json")
        .header(signature::header_name(), &sig_header)
        .body(body_bytes.clone());

    // Inject custom headers from subscription config.
    if let Some(headers_map) = attempt_custom_headers(&sub) {
        for (k, v) in headers_map {
            req = req.header(k, v);
        }
    }

    let response = req.send().await;

    match response {
        Ok(resp) => {
            let status = resp.status().as_u16() as i32;
            let body_excerpt =
                resp.text().await.unwrap_or_default().chars().take(512).collect::<String>();

            // 2xx → success; 408/429/5xx → retryable; all other 4xx → terminal failure.
            let retryable = matches!(status as u16, 408 | 429 | 500..=599);
            let success = (200..300).contains(&(status as u16));
            let terminal = !success && !retryable;

            let next_at = if retryable && attempt_number < MAX_ATTEMPTS {
                Some(next_attempt_at(attempt_number))
            } else {
                None
            };

            let final_state = if success {
                "delivered"
            } else if terminal || attempt_number >= MAX_ATTEMPTS {
                if retryable { "abandoned" } else { "failed" }
            } else {
                "failed"
            };

            info!(
                attempt_id = %attempt.id,
                subscription_id = %sub.id,
                status,
                final_state,
                attempt_number,
                retryable,
                "webhook delivery attempt completed"
            );

            webhook_repository::record_webhook_delivery_result(
                &state.persistence.postgres,
                attempt.id,
                final_state,
                attempt_number,
                Some(status),
                Some(body_excerpt.as_str()),
                None,
                next_at,
            )
            .await
            .context("failed to record webhook delivery result")?;

            if retryable && !terminal && attempt_number < MAX_ATTEMPTS {
                // Re-enqueue for retry at next_at.
                schedule_retry(state, &attempt, next_at).await?;
            }
        }
        Err(e) => {
            let error_msg = e.to_string();
            let next_at = if attempt_number < MAX_ATTEMPTS {
                Some(next_attempt_at(attempt_number))
            } else {
                None
            };

            let final_state = if attempt_number >= MAX_ATTEMPTS { "abandoned" } else { "failed" };

            tracing::warn!(
                attempt_id = %attempt.id,
                subscription_id = %sub.id,
                attempt_number,
                error = %error_msg,
                "webhook delivery HTTP request failed"
            );

            webhook_repository::record_webhook_delivery_result(
                &state.persistence.postgres,
                attempt.id,
                final_state,
                attempt_number,
                None,
                None,
                Some(error_msg.as_str()),
                next_at,
            )
            .await
            .context("failed to record webhook delivery failure")?;

            if attempt_number < MAX_ATTEMPTS {
                schedule_retry(state, &attempt, next_at).await?;
            }
        }
    }

    Ok(())
}

async fn find_attempt_for_job(
    state: &Arc<AppState>,
    job_id: uuid::Uuid,
) -> anyhow::Result<WebhookDeliveryAttemptRow> {
    // Find the delivery attempt linked to this job_id.
    // We query directly since there is a 1:1 relationship per enqueue.
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "select id, subscription_id, workspace_id, library_id,
                event_type, event_id, payload_json, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, response_body_excerpt, error_message,
                job_id, next_attempt_at, delivered_at, created_at, updated_at
         from webhook_delivery_attempt
         where job_id = $1
         limit 1",
    )
    .bind(job_id)
    .fetch_optional(&state.persistence.postgres)
    .await
    .context("failed to query delivery attempt by job_id")?
    .with_context(|| format!("no delivery attempt found for job_id={job_id}"))
}

async fn load_subscription(
    state: &Arc<AppState>,
    subscription_id: uuid::Uuid,
) -> anyhow::Result<WebhookSubscriptionRow> {
    webhook_repository::get_webhook_subscription_by_id(&state.persistence.postgres, subscription_id)
        .await
        .context("failed to load webhook subscription")?
        .with_context(|| format!("webhook subscription {subscription_id} not found"))
}

fn next_attempt_at(attempt_number: i32) -> chrono::DateTime<Utc> {
    let exp = attempt_number.min(MAX_ATTEMPTS) as u32;
    let delay_minutes = (2_i64.pow(exp)).min(MAX_DELAY_MINUTES);
    Utc::now() + chrono::Duration::minutes(delay_minutes)
}

fn attempt_custom_headers(sub: &WebhookSubscriptionRow) -> Option<Vec<(String, String)>> {
    let obj = sub.custom_headers_json.as_object()?;
    let headers: Vec<(String, String)> =
        obj.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned()))).collect();
    if headers.is_empty() { None } else { Some(headers) }
}

async fn schedule_retry(
    state: &Arc<AppState>,
    attempt: &WebhookDeliveryAttemptRow,
    available_at: Option<chrono::DateTime<Utc>>,
) -> anyhow::Result<()> {
    use crate::infra::repositories::ingest_repository::NewIngestJob;

    let new_job = crate::infra::repositories::ingest_repository::create_ingest_job(
        &state.persistence.postgres,
        &NewIngestJob {
            workspace_id: attempt.workspace_id,
            library_id: uuid::Uuid::nil(),
            mutation_id: None,
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: None,
            knowledge_revision_id: None,
            job_kind: "webhook_delivery".to_string(),
            queue_state: "queued".to_string(),
            priority: 5,
            dedupe_key: Some(format!(
                "wh-retry-{}-{}-{}",
                attempt.subscription_id,
                attempt.event_id,
                attempt.attempt_number + 1
            )),
            queued_at: None,
            available_at,
            completed_at: None,
        },
    )
    .await
    .context("failed to enqueue webhook retry job")?;

    // Link new retry job to the attempt row so the next worker pick can find it.
    // State is set back to 'pending' — mark_attempt_delivering fires when the worker leases.
    webhook_repository::link_attempt_to_job(&state.persistence.postgres, attempt.id, new_job.id)
        .await
        .context("failed to link retry job to delivery attempt")?;

    webhook_repository::record_webhook_delivery_result(
        &state.persistence.postgres,
        attempt.id,
        "pending",
        attempt.attempt_number,
        None,
        None,
        None,
        None,
    )
    .await
    .context("failed to reset delivery attempt state to pending for retry")?;

    Ok(())
}
