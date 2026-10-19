//! Delivery worker stage for one outbound webhook attempt.
//!
//! The ingest queue invokes this module for `webhook_delivery` jobs.
//! Every side effect is fenced by `(attempt_id, job_id, delivery_lease_token)`;
//! a worker whose lease was reclaimed may finish its HTTP request, but cannot
//! overwrite the winner or enqueue another retry.

use std::sync::Arc;

use anyhow::Context as _;
use chrono::Utc;
use http::HeaderValue;
use reqwest::Url;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    app::state::AppState,
    domains::webhook::delivery_payload_from_persisted_parts,
    infra::repositories::{
        ingest_repository::{IngestJobRow, NewIngestJob},
        webhook_repository::{
            self, WebhookDeliveryAttemptRow, WebhookDeliveryClaimOutcome,
            WebhookDeliveryCompletion, WebhookRetryHandoff, WebhookRetryHandoffOutcome,
            WebhookSubscriptionRow,
        },
    },
    services::webhook::{
        custom_headers,
        error::{WebhookDeliveryFailure, WebhookDeliveryFailureCode, WebhookServiceError},
        signature,
    },
    shared::{
        outbound_http::{
            build_no_redirect_public_http_client, read_response_text_excerpt_with_limit,
            resolve_public_http_url,
        },
        secret_encryption::SecretPurpose,
    },
};

const MAX_ATTEMPTS: i32 = 8;
const MAX_DELAY_MINUTES: i64 = 128;
const MAX_WEBHOOK_RESPONSE_BODY_BYTES: u64 = 64 * 1024;
const WEBHOOK_RESPONSE_EXCERPT_CHARS: usize = 512;
const WEBHOOK_EVENT_TYPE_HEADER: &str = "X-Ironrag-Event-Type";
const WEBHOOK_EVENT_ID_HEADER: &str = "X-Ironrag-Event-Id";

#[derive(Debug, Clone, Copy)]
enum WebhookTargetPolicy {
    PublicOnly,
    #[cfg(feature = "test-support")]
    LoopbackTestOnly,
}

enum PreparedWebhookTarget {
    Public(crate::shared::outbound_http::ResolvedPublicHttpUrl),
    #[cfg(feature = "test-support")]
    Loopback(Url),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookDeliveryJobOutcome {
    NeedsIngestFinalization,
    IngestAlreadyFinalized,
}

#[derive(Clone, Copy)]
struct DeliveryIngestLease<'a> {
    ingest_attempt_id: uuid::Uuid,
    expected_queue_lease_token: &'a str,
}

/// Everything a terminal delivery write needs except the failing outcome
/// itself: the owned attempt, the queue job it fences, and the ingest lease
/// that must be terminalized in the same retry-handoff transaction.
#[derive(Clone, Copy)]
struct DeliveryAttemptContext<'a> {
    attempt: &'a WebhookDeliveryAttemptRow,
    job_id: uuid::Uuid,
    ingest_lease: Option<DeliveryIngestLease<'a>>,
    lease_token: uuid::Uuid,
    attempt_number: i32,
}

/// Outcome of resolving the subscription target: either a prepared target, or
/// a terminal delivery outcome already recorded by the caller.
enum TargetResolution {
    Ready(PreparedWebhookTarget),
    Failed(WebhookDeliveryJobOutcome),
}

/// A signed, header-populated request plus the plaintext custom headers that
/// must stay alive (and be zeroized) until the response is classified.
struct PreparedDeliveryRequest {
    request: reqwest::RequestBuilder,
    custom_headers: custom_headers::SensitiveWebhookHeaders,
}

/// Result of local request preparation: a ready request, or the static failure
/// and retryability decision that terminalizes the attempt.
enum DeliveryPreparation {
    Ready(Box<PreparedDeliveryRequest>),
    Failed { failure: WebhookDeliveryFailure, retryable: bool },
}

impl PreparedWebhookTarget {
    fn url(&self) -> &Url {
        match self {
            Self::Public(target) => target.url(),
            #[cfg(feature = "test-support")]
            Self::Loopback(target) => target,
        }
    }

    fn build_client(&self) -> Result<reqwest::Client, reqwest::Error> {
        match self {
            Self::Public(target) => build_no_redirect_public_http_client(
                target,
                std::time::Duration::from_secs(30),
                std::time::Duration::from_secs(10),
                Some("ironrag-webhook/1.0"),
            ),
            #[cfg(feature = "test-support")]
            Self::Loopback(_) => reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("ironrag-webhook-test/1.0")
                .build(),
        }
    }
}

pub async fn run_webhook_delivery_job(
    state: &Arc<AppState>,
    job: &IngestJobRow,
    ingest_attempt_id: uuid::Uuid,
    expected_queue_lease_token: &str,
    cancellation_token: &CancellationToken,
) -> Result<WebhookDeliveryJobOutcome, WebhookServiceError> {
    run_webhook_delivery_job_with_target_policy(
        state,
        job,
        WebhookTargetPolicy::PublicOnly,
        Some(DeliveryIngestLease { ingest_attempt_id, expected_queue_lease_token }),
        cancellation_token,
    )
    .await
}

/// Integration-test entry point that permits only a literal loopback target.
///
/// This never changes the production entry point's public-network policy and
/// is absent from normal builds unless `test-support` is explicitly enabled.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub async fn run_webhook_delivery_job_with_loopback_test_transport(
    state: &Arc<AppState>,
    job: &IngestJobRow,
) -> Result<WebhookDeliveryJobOutcome, WebhookServiceError> {
    let cancellation_token = CancellationToken::new();
    run_webhook_delivery_job_with_target_policy(
        state,
        job,
        WebhookTargetPolicy::LoopbackTestOnly,
        None,
        &cancellation_token,
    )
    .await
}

/// Integration-test entry point for the production ingest-lease handoff path
/// while retaining the loopback-only transport policy.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub async fn run_webhook_delivery_job_with_loopback_test_transport_and_ingest_lease(
    state: &Arc<AppState>,
    job: &IngestJobRow,
    ingest_attempt_id: uuid::Uuid,
    expected_queue_lease_token: &str,
    cancellation_token: &CancellationToken,
) -> Result<WebhookDeliveryJobOutcome, WebhookServiceError> {
    run_webhook_delivery_job_with_target_policy(
        state,
        job,
        WebhookTargetPolicy::LoopbackTestOnly,
        Some(DeliveryIngestLease { ingest_attempt_id, expected_queue_lease_token }),
        cancellation_token,
    )
    .await
}

async fn run_webhook_delivery_job_with_target_policy(
    state: &Arc<AppState>,
    job: &IngestJobRow,
    target_policy: WebhookTargetPolicy,
    ingest_lease: Option<DeliveryIngestLease<'_>>,
    cancellation_token: &CancellationToken,
) -> Result<WebhookDeliveryJobOutcome, WebhookServiceError> {
    let pending_attempt = find_attempt_for_job(state, job.id).await?;

    // Claim before policy resolution or any fallible local preparation. Every
    // branch below can therefore terminalize through the same ownership CAS,
    // and concurrent duplicate workers perform no outbound request.
    let Some((attempt, subscription)) =
        claim_delivery_attempt(state, job, &pending_attempt).await?
    else {
        return Ok(WebhookDeliveryJobOutcome::NeedsIngestFinalization);
    };
    let lease_token =
        attempt.delivery_lease_token.ok_or_else(|| WebhookServiceError::StateConflict {
            message: "claimed webhook delivery is missing its ownership token".to_string(),
        })?;
    let context = DeliveryAttemptContext {
        attempt: &attempt,
        job_id: job.id,
        ingest_lease,
        lease_token,
        attempt_number: attempt.attempt_number + 1,
    };

    if !subscription.active {
        return finish_delivery_failure(
            state,
            &context,
            None,
            WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::SubscriptionInactive),
            false,
        )
        .await;
    }

    let allow_http = std::env::var("IRONRAG_WEBHOOK_ALLOW_HTTP").is_ok_and(|value| value == "1");
    let resolved_target = match resolve_webhook_target(
        state,
        &context,
        &subscription.target_url,
        allow_http,
        target_policy,
        cancellation_token,
    )
    .await?
    {
        TargetResolution::Ready(target) => target,
        TargetResolution::Failed(outcome) => return Ok(outcome),
    };

    let preparation =
        prepare_delivery_request(state, &context, &subscription, &resolved_target).await;
    let (request, custom_headers) = match preparation {
        DeliveryPreparation::Ready(prepared) => (prepared.request, prepared.custom_headers),
        DeliveryPreparation::Failed { failure, retryable } => {
            return finish_delivery_failure(state, &context, None, failure, retryable).await;
        }
    };
    let response = tokio::select! {
        () = cancellation_token.cancelled() => {
            drop(custom_headers);
            release_canceled_delivery_claim(state, &attempt, job.id, lease_token).await?;
            return Err(WebhookServiceError::DeliveryCanceled {
                attempt_id: attempt.id,
                job_id: job.id,
            });
        }
        response = crate::observability::inject_trace_context(request).send() => response,
    };
    drop(custom_headers);

    match response {
        Ok(response) => {
            handle_delivery_response(state, &context, response, cancellation_token).await
        }
        Err(error) => {
            finish_delivery_failure(state, &context, None, classify_transport_failure(&error), true)
                .await
        }
    }
}

/// Claims the delivery attempt for this queue job, or reports that the work is
/// already terminal. Cancellation performed atomically by the repository is
/// treated the same as a terminal replay: the queue job is complete.
async fn claim_delivery_attempt(
    state: &Arc<AppState>,
    job: &IngestJobRow,
    pending_attempt: &WebhookDeliveryAttemptRow,
) -> Result<
    Option<(Box<WebhookDeliveryAttemptRow>, Box<WebhookSubscriptionRow>)>,
    WebhookServiceError,
> {
    match webhook_repository::claim_attempt_for_delivery(
        &state.persistence.postgres,
        pending_attempt.id,
        job.id,
    )
    .await
    .context("failed to claim webhook delivery attempt")?
    {
        WebhookDeliveryClaimOutcome::Claimed { attempt, subscription } => {
            Ok(Some((attempt, subscription)))
        }
        WebhookDeliveryClaimOutcome::InFlight { attempt_id, retry_at } => {
            Err(WebhookServiceError::DeliveryLeaseInFlight { attempt_id, job_id: job.id, retry_at })
        }
        WebhookDeliveryClaimOutcome::Terminal { attempt_id, delivery_state } => {
            log_duplicate_terminal_claim(job.id, attempt_id, &delivery_state);
            Ok(None)
        }
        WebhookDeliveryClaimOutcome::Canceled => {
            log_claim_canceled_before_http(pending_attempt.id, job.id);
            Ok(None)
        }
    }
}

fn log_duplicate_terminal_claim(job_id: uuid::Uuid, attempt_id: uuid::Uuid, delivery_state: &str) {
    info!(
        %attempt_id,
        job_id = %job_id,
        %delivery_state,
        "webhook delivery attempt was already terminal; completing duplicate queue work"
    );
}

fn log_claim_canceled_before_http(attempt_id: uuid::Uuid, job_id: uuid::Uuid) {
    info!(
        attempt_id = %attempt_id,
        job_id = %job_id,
        "webhook delivery claim was canceled before HTTP could start"
    );
}

/// Resolves the subscription target under the configured policy. Cancellation
/// releases the claim and returns a canceled error; a resolution failure is
/// terminalized through the shared failure handler.
async fn resolve_webhook_target(
    state: &Arc<AppState>,
    context: &DeliveryAttemptContext<'_>,
    raw_url: &str,
    allow_http: bool,
    target_policy: WebhookTargetPolicy,
    cancellation_token: &CancellationToken,
) -> Result<TargetResolution, WebhookServiceError> {
    let resolved = tokio::select! {
        () = cancellation_token.cancelled() => None,
        resolved = prepare_webhook_target(raw_url, allow_http, target_policy) => Some(resolved),
    };
    let Some(resolved) = resolved else {
        release_canceled_delivery_claim(
            state,
            context.attempt,
            context.job_id,
            context.lease_token,
        )
        .await?;
        return Err(WebhookServiceError::DeliveryCanceled {
            attempt_id: context.attempt.id,
            job_id: context.job_id,
        });
    };
    match resolved {
        Ok(target) => Ok(TargetResolution::Ready(target)),
        Err(error) => {
            let terminal = error.is_terminal_policy_rejection();
            let failure = if terminal {
                WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::TargetPolicyRejected)
            } else {
                WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::TargetResolutionFailed)
            };
            let outcome = finish_delivery_failure(state, context, None, failure, !terminal).await?;
            Ok(TargetResolution::Failed(outcome))
        }
    }
}

/// Encodes the persisted delivery, signs it, and builds the request. Any
/// failure short-circuits to the static code and retryability decision the
/// caller records; no DB write happens here.
async fn prepare_delivery_request(
    state: &Arc<AppState>,
    context: &DeliveryAttemptContext<'_>,
    subscription: &WebhookSubscriptionRow,
    resolved_target: &PreparedWebhookTarget,
) -> DeliveryPreparation {
    let Some((body_bytes, event_type_header, event_id_header)) =
        encode_persisted_delivery(context.attempt)
    else {
        return DeliveryPreparation::Failed {
            failure: WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::PayloadEncodingFailed),
            retryable: false,
        };
    };

    let Ok(signing_secret) = state.credential_cipher.decrypt(
        SecretPurpose::WebhookSigningSecret,
        subscription.id,
        &subscription.secret,
    ) else {
        return DeliveryPreparation::Failed {
            failure: credential_unavailable_failure(),
            retryable: true,
        };
    };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let signature_header =
        signature::sign(signing_secret.expose_secret().as_bytes(), timestamp, &body_bytes);
    drop(signing_secret);

    let Ok(client) = resolved_target.build_client() else {
        return DeliveryPreparation::Failed {
            failure: WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::ClientSetupFailed),
            retryable: true,
        };
    };
    let Ok(custom_headers) = custom_headers::decrypt_and_validate_stored(
        &state.credential_cipher,
        subscription.id,
        &subscription.custom_headers_json,
    ) else {
        return DeliveryPreparation::Failed {
            failure: credential_unavailable_failure(),
            retryable: true,
        };
    };

    let mut request = client
        .post(resolved_target.url().clone())
        .header("Content-Type", "application/json")
        .header(signature::header_name(), signature_header)
        .header(WEBHOOK_EVENT_TYPE_HEADER, event_type_header)
        .header(WEBHOOK_EVENT_ID_HEADER, event_id_header)
        .body(body_bytes);
    for (name, value) in custom_headers.iter() {
        request = request.header(name, value);
    }
    DeliveryPreparation::Ready(Box::new(PreparedDeliveryRequest { request, custom_headers }))
}

/// Classifies one HTTP response. A status exists, so it is the authoritative
/// result: 2xx records success, anything else terminalizes with retryability
/// derived from the status alone.
async fn handle_delivery_response(
    state: &Arc<AppState>,
    context: &DeliveryAttemptContext<'_>,
    response: reqwest::Response,
    cancellation_token: &CancellationToken,
) -> Result<WebhookDeliveryJobOutcome, WebhookServiceError> {
    let status = i32::from(response.status().as_u16());
    // Drain a bounded response body for connection reuse, but never persist
    // remote-controlled response text. Once a response status exists it is the
    // authoritative delivery result; cancellation may stop the optional drain
    // but must not make the request replay.
    tokio::select! {
        () = cancellation_token.cancelled() => {}
        _ = read_response_text_excerpt_with_limit(
            response,
            MAX_WEBHOOK_RESPONSE_BODY_BYTES,
            WEBHOOK_RESPONSE_EXCERPT_CHARS,
        ) => {}
    }
    let success = (200..300).contains(&status);
    if success {
        record_owned_delivery_result(
            state,
            context.attempt,
            context.job_id,
            context.lease_token,
            context.attempt_number,
            status,
        )
        .await?;
        return Ok(WebhookDeliveryJobOutcome::NeedsIngestFinalization);
    }
    let retryable = matches!(status, 408 | 429 | 500..=599);
    finish_delivery_failure(
        state,
        context,
        Some(status),
        WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::RemoteHttpStatus),
        retryable,
    )
    .await
}

fn encode_persisted_delivery(
    attempt: &WebhookDeliveryAttemptRow,
) -> Option<(Vec<u8>, HeaderValue, HeaderValue)> {
    if !attempt.payload_json.is_object()
        || !matches!(attempt.event_type.as_str(), "revision.ready" | "document.deleted")
    {
        return None;
    }
    let library_id = attempt.library_id?;
    let event_type_header = HeaderValue::from_str(&attempt.event_type).ok()?;
    let event_id_header = HeaderValue::from_str(&attempt.event_id).ok()?;
    let payload = delivery_payload_from_persisted_parts(
        &attempt.payload_json,
        &attempt.event_type,
        &attempt.event_id,
        attempt.occurred_at,
        attempt.workspace_id,
        library_id,
    );
    let body = serde_json::to_vec(&payload).ok()?;
    Some((body, event_type_header, event_id_header))
}

const fn credential_unavailable_failure() -> WebhookDeliveryFailure {
    WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::CredentialUnavailable)
}

fn classify_transport_failure(error: &reqwest::Error) -> WebhookDeliveryFailure {
    if error.is_timeout() {
        WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::TransportTimeout)
    } else if error.is_connect() {
        WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::TransportConnect)
    } else {
        WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::TransportRequest)
    }
}

async fn record_owned_delivery_result(
    state: &Arc<AppState>,
    attempt: &WebhookDeliveryAttemptRow,
    job_id: uuid::Uuid,
    lease_token: uuid::Uuid,
    attempt_number: i32,
    response_status: i32,
) -> Result<(), WebhookServiceError> {
    let completion = WebhookDeliveryCompletion {
        attempt_id: attempt.id,
        job_id,
        lease_token,
        delivery_state: "delivered",
        attempt_number,
        response_status: Some(response_status),
        error_code: None,
        error_summary: None,
        next_attempt_at: None,
    };
    let recorded = webhook_repository::record_webhook_delivery_result(
        &state.persistence.postgres,
        &completion,
    )
    .await
    .context("failed to record webhook delivery result")?;
    if recorded.is_some() {
        log_delivery_completed(attempt.id, job_id, response_status, attempt_number);
    } else {
        log_delivery_result_discarded(attempt.id, job_id);
    }
    Ok(())
}

fn log_delivery_completed(
    attempt_id: uuid::Uuid,
    job_id: uuid::Uuid,
    response_status: i32,
    attempt_number: i32,
) {
    info!(
        attempt_id = %attempt_id,
        job_id = %job_id,
        response_status,
        attempt_number,
        "webhook delivery attempt completed"
    );
}

fn log_delivery_result_discarded(attempt_id: uuid::Uuid, job_id: uuid::Uuid) {
    info!(
        attempt_id = %attempt_id,
        job_id = %job_id,
        "discarded webhook success from a worker that no longer owns the delivery"
    );
}

async fn finish_delivery_failure(
    state: &Arc<AppState>,
    context: &DeliveryAttemptContext<'_>,
    response_status: Option<i32>,
    failure: WebhookDeliveryFailure,
    retryable: bool,
) -> Result<WebhookDeliveryJobOutcome, WebhookServiceError> {
    let attempt = context.attempt;
    let retry_library_id = attempt.library_id;
    let should_retry =
        retryable && context.attempt_number < MAX_ATTEMPTS && retry_library_id.is_some();
    let next_attempt_at = should_retry.then(|| next_attempt_at(context.attempt_number));
    let delivery_state =
        if retryable && context.attempt_number >= MAX_ATTEMPTS { "abandoned" } else { "failed" };
    let completion = WebhookDeliveryCompletion {
        attempt_id: attempt.id,
        job_id: context.job_id,
        lease_token: context.lease_token,
        delivery_state,
        attempt_number: context.attempt_number,
        response_status,
        error_code: Some(failure.code().as_str()),
        error_summary: Some(failure.summary()),
        next_attempt_at,
    };

    let (recorded, outcome) = match (should_retry, retry_library_id) {
        (true, Some(retry_library_id)) => {
            let retry_job =
                build_retry_job(attempt, retry_library_id, context.attempt_number, next_attempt_at);
            record_retry_handoff(state, context, &completion, &retry_job).await?
        }
        _ => (
            webhook_repository::record_webhook_delivery_result(
                &state.persistence.postgres,
                &completion,
            )
            .await
            .context("failed to record webhook delivery failure")?,
            WebhookDeliveryJobOutcome::NeedsIngestFinalization,
        ),
    };

    if recorded.is_some() {
        log_delivery_failure_recorded(
            attempt,
            context.job_id,
            &failure,
            context.attempt_number,
            retryable,
        );
    } else {
        log_delivery_failure_discarded(attempt, context.job_id, &failure);
    }
    Ok(outcome)
}

/// Records a retryable failure and hands the queue retry off in the same
/// transaction as terminalizing the current ingest lease. The detached
/// (test-only, no ingest lease) path enqueues a replacement job directly.
async fn record_retry_handoff(
    state: &Arc<AppState>,
    context: &DeliveryAttemptContext<'_>,
    completion: &WebhookDeliveryCompletion<'_>,
    retry_job: &NewIngestJob,
) -> Result<(Option<WebhookDeliveryAttemptRow>, WebhookDeliveryJobOutcome), WebhookServiceError> {
    let Some(ingest_lease) = context.ingest_lease else {
        #[cfg(feature = "test-support")]
        {
            let recorded =
                webhook_repository::record_webhook_delivery_failure_and_enqueue_retry_detached(
                    &state.persistence.postgres,
                    completion,
                    retry_job,
                )
                .await
                .context("failed to record detached test webhook retry")?;
            return Ok((recorded, WebhookDeliveryJobOutcome::NeedsIngestFinalization));
        }
        #[cfg(not(feature = "test-support"))]
        {
            return Err(WebhookServiceError::StateConflict {
                message: "webhook retry is missing its current ingest lease".to_string(),
            });
        }
    };
    let outcome = webhook_repository::record_webhook_delivery_failure_and_handoff_retry(
        &state.persistence.postgres,
        completion,
        &WebhookRetryHandoff {
            ingest_attempt_id: ingest_lease.ingest_attempt_id,
            expected_queue_lease_token: ingest_lease.expected_queue_lease_token,
        },
        retry_job,
    )
    .await
    .context("failed to atomically hand off webhook retry queue ownership")?;
    Ok(match outcome {
        WebhookRetryHandoffOutcome::RetryScheduled(recorded) => {
            (Some(recorded), WebhookDeliveryJobOutcome::IngestAlreadyFinalized)
        }
        WebhookRetryHandoffOutcome::CompletionRecorded(recorded) => {
            (Some(recorded), WebhookDeliveryJobOutcome::NeedsIngestFinalization)
        }
        WebhookRetryHandoffOutcome::OwnershipLost => {
            (None, WebhookDeliveryJobOutcome::NeedsIngestFinalization)
        }
    })
}

fn log_delivery_failure_recorded(
    attempt: &WebhookDeliveryAttemptRow,
    job_id: uuid::Uuid,
    failure: &WebhookDeliveryFailure,
    attempt_number: i32,
    retryable: bool,
) {
    warn!(
        attempt_id = %attempt.id,
        subscription_id = %attempt.subscription_id,
        job_id = %job_id,
        attempt_number,
        retryable,
        failure_code = failure.code().as_str(),
        "webhook delivery attempt failed"
    );
}

fn log_delivery_failure_discarded(
    attempt: &WebhookDeliveryAttemptRow,
    job_id: uuid::Uuid,
    failure: &WebhookDeliveryFailure,
) {
    info!(
        attempt_id = %attempt.id,
        job_id = %job_id,
        failure_code = failure.code().as_str(),
        "discarded webhook failure from a worker that no longer owns the delivery"
    );
}

async fn release_canceled_delivery_claim(
    state: &Arc<AppState>,
    attempt: &WebhookDeliveryAttemptRow,
    job_id: uuid::Uuid,
    lease_token: uuid::Uuid,
) -> Result<(), WebhookServiceError> {
    let released = webhook_repository::release_webhook_delivery_claim(
        &state.persistence.postgres,
        attempt.id,
        job_id,
        lease_token,
    )
    .await
    .context("failed to release canceled webhook delivery ownership")?;
    if !released {
        info!(
            attempt_id = %attempt.id,
            %job_id,
            "canceled webhook worker no longer owned the delivery claim"
        );
    }
    Ok(())
}

fn build_retry_job(
    attempt: &WebhookDeliveryAttemptRow,
    library_id: uuid::Uuid,
    attempt_number: i32,
    available_at: Option<chrono::DateTime<Utc>>,
) -> NewIngestJob {
    NewIngestJob {
        workspace_id: attempt.workspace_id,
        library_id,
        mutation_id: None,
        mutation_item_id: None,
        connector_id: None,
        async_operation_id: None,
        knowledge_document_id: None,
        knowledge_revision_id: None,
        job_kind: "webhook_delivery".to_string(),
        queue_state: "queued".to_string(),
        priority: 5,
        dedupe_key: Some(format!(
            "wh-retry-{}-{}-{attempt_number}",
            attempt.subscription_id, attempt.event_id
        )),
        queued_at: None,
        available_at,
        completed_at: None,
    }
}

async fn prepare_webhook_target(
    raw_url: &str,
    allow_http: bool,
    policy: WebhookTargetPolicy,
) -> Result<PreparedWebhookTarget, crate::shared::outbound_http::PublicHttpUrlError> {
    match policy {
        WebhookTargetPolicy::PublicOnly => {
            resolve_public_http_url(raw_url, allow_http).await.map(PreparedWebhookTarget::Public)
        }
        #[cfg(feature = "test-support")]
        WebhookTargetPolicy::LoopbackTestOnly => prepare_loopback_test_target(raw_url),
    }
}

#[cfg(feature = "test-support")]
fn prepare_loopback_test_target(
    raw_url: &str,
) -> Result<PreparedWebhookTarget, crate::shared::outbound_http::PublicHttpUrlError> {
    use crate::shared::outbound_http::PublicHttpUrlError;

    let parsed = Url::parse(raw_url.trim())
        .map_err(|_| PublicHttpUrlError::InvalidUrl("redacted invalid target".to_string()))?;
    if parsed.scheme() != "http" {
        return Err(PublicHttpUrlError::ForbiddenScheme(parsed.scheme().to_string()));
    }
    let host = parsed.host_str().ok_or(PublicHttpUrlError::MissingHost)?;
    let address = host.parse::<std::net::IpAddr>().map_err(|_| {
        PublicHttpUrlError::InvalidUrl(
            "loopback test transport requires a literal IP address".to_string(),
        )
    })?;
    if !address.is_loopback() {
        return Err(PublicHttpUrlError::InvalidUrl(
            "loopback test transport rejected a non-loopback address".to_string(),
        ));
    }
    parsed.port_or_known_default().ok_or(PublicHttpUrlError::MissingPort)?;
    Ok(PreparedWebhookTarget::Loopback(parsed))
}

async fn find_attempt_for_job(
    state: &Arc<AppState>,
    job_id: uuid::Uuid,
) -> Result<WebhookDeliveryAttemptRow, WebhookServiceError> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "select id, subscription_id, workspace_id, library_id,
                event_type, event_id, occurred_at, payload_json, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, response_body_excerpt, error_code, error_message,
                job_id, delivery_lease_token, next_attempt_at,
                delivered_at, created_at, updated_at
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

fn next_attempt_at(attempt_number: i32) -> chrono::DateTime<Utc> {
    let exponent = attempt_number.min(MAX_ATTEMPTS) as u32;
    let delay_minutes = (2_i64.pow(exponent)).min(MAX_DELAY_MINUTES);
    Utc::now() + chrono::Duration::minutes(delay_minutes)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::{credential_unavailable_failure, encode_persisted_delivery};
    use crate::infra::repositories::webhook_repository::WebhookDeliveryAttemptRow;

    fn synthetic_attempt() -> WebhookDeliveryAttemptRow {
        let now = Utc::now();
        WebhookDeliveryAttemptRow {
            id: Uuid::now_v7(),
            subscription_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Some(Uuid::now_v7()),
            event_type: "revision.ready".to_string(),
            event_id: format!("revision.ready:{}", Uuid::now_v7()),
            occurred_at: now,
            payload_json: json!({
                "event_id": "spoofed",
                "workspace_id": Uuid::nil(),
                "revision_id": Uuid::now_v7(),
            }),
            target_url: "https://example.invalid/webhook".to_string(),
            attempt_number: 0,
            delivery_state: "delivering".to_string(),
            response_status: None,
            response_body_excerpt: None,
            error_code: None,
            error_message: None,
            job_id: Some(Uuid::now_v7()),
            delivery_lease_token: Some(Uuid::now_v7()),
            next_attempt_at: None,
            delivered_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn persisted_failures_are_static_and_url_free() {
        let failure = credential_unavailable_failure();

        assert_eq!(failure.code().as_str(), "credential_unavailable");
        assert!(!failure.summary().contains("http"));
    }

    #[test]
    fn delivery_rebuilds_persisted_body_and_event_headers_from_relational_fields() {
        let attempt = synthetic_attempt();

        let (body, event_type_header, event_id_header) =
            encode_persisted_delivery(&attempt).expect("valid attempt");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("normalized JSON");

        assert_eq!(payload["event_type"], attempt.event_type);
        assert_eq!(payload["event_id"], attempt.event_id);
        assert_eq!(payload["occurred_at"], attempt.occurred_at.to_rfc3339());
        assert_eq!(payload["workspace_id"], attempt.workspace_id.to_string());
        assert_eq!(payload["library_id"], attempt.library_id.expect("library").to_string());
        assert_eq!(event_type_header.to_str().expect("event type header"), "revision.ready");
        assert_eq!(event_id_header.to_str().expect("event id header"), attempt.event_id);
    }

    #[test]
    fn delivery_rejects_non_object_or_header_unsafe_legacy_metadata() {
        let mut attempt = synthetic_attempt();
        attempt.payload_json = json!(["not", "an", "object"]);
        assert!(encode_persisted_delivery(&attempt).is_none());

        attempt.payload_json = json!({});
        attempt.event_id = "revision.ready:unsafe\r\nX-Injected: yes".to_string();
        assert!(encode_persisted_delivery(&attempt).is_none());
    }
}
