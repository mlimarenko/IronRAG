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
    services::webhook::{error::WebhookServiceError, signature},
    shared::outbound_http::{
        build_no_redirect_public_http_client, read_response_text_excerpt_with_limit,
        resolve_public_http_url,
    },
};

const MAX_ATTEMPTS: i32 = 8;
const MAX_DELAY_MINUTES: i64 = 360; // 6 h
const MAX_WEBHOOK_RESPONSE_BODY_BYTES: u64 = 64 * 1024;
const WEBHOOK_RESPONSE_EXCERPT_CHARS: usize = 512;

pub async fn run_webhook_delivery_job(
    state: &Arc<AppState>,
    job: &IngestJobRow,
) -> Result<(), WebhookServiceError> {
    // The dedupe_key encodes the attempt_id: "wh-delivery-{sub_id}-{event_id}".
    // We recover the attempt by looking up via job_id.
    let attempt = find_attempt_for_job(state, job.id).await?;
    let sub = load_subscription(state, attempt.subscription_id).await?;
    let attempt_number = attempt.attempt_number + 1;

    let allow_http = std::env::var("IRONRAG_WEBHOOK_ALLOW_HTTP").map(|v| v == "1").unwrap_or(false);
    let resolved_target = match resolve_public_http_url(&attempt.target_url, allow_http).await {
        Ok(value) => value,
        Err(error) => {
            let error_msg = format!("webhook target_url rejected before delivery: {error}");
            let retryable = !error.is_terminal_policy_rejection();
            record_webhook_delivery_failure(
                state,
                &attempt,
                attempt_number,
                attempt.subscription_id,
                error_msg.as_str(),
                retryable,
            )
            .await?;
            return Ok(());
        }
    };

    // Transition to 'delivering' only after policy checks that can fail before
    // the request. This keeps rejected targets from getting stuck in-flight.
    webhook_repository::mark_attempt_delivering(&state.persistence.postgres, attempt.id)
        .await
        .context("failed to mark delivery attempt as delivering")?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let body_bytes =
        serde_json::to_vec(&attempt.payload_json).context("failed to serialize webhook payload")?;

    let sig_header = signature::sign(sub.secret.as_bytes(), ts, &body_bytes);

    let client = build_no_redirect_public_http_client(
        &resolved_target,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(10),
        Some("ironrag-webhook/1.0"),
    )
    .map_err(|error| WebhookServiceError::Internal(anyhow::anyhow!(error)))?;

    let mut req = client
        .post(resolved_target.url().clone())
        .header("Content-Type", "application/json")
        .header(signature::header_name(), &sig_header)
        .body(body_bytes.clone());

    // Inject custom headers from subscription config.
    if let Some(headers_map) = attempt_custom_headers(&sub) {
        for (k, v) in headers_map {
            req = req.header(k, v);
        }
    }

    let req = crate::observability::inject_trace_context(req);
    let response = req.send().await;

    match response {
        Ok(resp) => {
            let status = resp.status().as_u16() as i32;
            let body_excerpt = read_response_text_excerpt_with_limit(
                resp,
                MAX_WEBHOOK_RESPONSE_BODY_BYTES,
                WEBHOOK_RESPONSE_EXCERPT_CHARS,
            )
            .await
            .unwrap_or_default();

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
                schedule_retry(state, &attempt, attempt_number, next_at).await?;
            }
        }
        Err(e) => {
            let error_msg = e.to_string();
            record_webhook_delivery_failure(
                state,
                &attempt,
                attempt_number,
                sub.id,
                &error_msg,
                true,
            )
            .await?;
        }
    }

    Ok(())
}

async fn record_webhook_delivery_failure(
    state: &Arc<AppState>,
    attempt: &WebhookDeliveryAttemptRow,
    attempt_number: i32,
    subscription_id: uuid::Uuid,
    error_msg: &str,
    retryable: bool,
) -> Result<(), WebhookServiceError> {
    let should_retry = retryable && attempt_number < MAX_ATTEMPTS;
    let next_at = if should_retry { Some(next_attempt_at(attempt_number)) } else { None };

    let final_state =
        if retryable && attempt_number >= MAX_ATTEMPTS { "abandoned" } else { "failed" };

    tracing::warn!(
        attempt_id = %attempt.id,
        subscription_id = %subscription_id,
        attempt_number,
        retryable,
        error = %error_msg,
        "webhook delivery attempt failed"
    );

    webhook_repository::record_webhook_delivery_result(
        &state.persistence.postgres,
        attempt.id,
        final_state,
        attempt_number,
        None,
        None,
        Some(error_msg),
        next_at,
    )
    .await
    .context("failed to record webhook delivery failure")?;

    if should_retry {
        schedule_retry(state, attempt, attempt_number, next_at).await?;
    }

    Ok(())
}

async fn find_attempt_for_job(
    state: &Arc<AppState>,
    job_id: uuid::Uuid,
) -> Result<WebhookDeliveryAttemptRow, WebhookServiceError> {
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
    .map_err(WebhookServiceError::Repository)?
    .ok_or(WebhookServiceError::DeliveryAttemptNotFound { job_id })
}

async fn load_subscription(
    state: &Arc<AppState>,
    subscription_id: uuid::Uuid,
) -> Result<WebhookSubscriptionRow, WebhookServiceError> {
    webhook_repository::get_webhook_subscription_by_id(&state.persistence.postgres, subscription_id)
        .await
        .map_err(WebhookServiceError::Repository)?
        .ok_or(WebhookServiceError::SubscriptionNotFound { subscription_id })
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
    attempt_number: i32,
    available_at: Option<chrono::DateTime<Utc>>,
) -> Result<(), WebhookServiceError> {
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
                attempt.subscription_id, attempt.event_id, attempt_number
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
        attempt_number,
        None,
        None,
        None,
        None,
    )
    .await
    .context("failed to reset delivery attempt state to pending for retry")?;

    Ok(())
}
