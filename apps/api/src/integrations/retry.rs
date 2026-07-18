use rand::RngExt as _;
use std::{future::Future, time::Duration};
use thiserror::Error;

use crate::shared::outbound_http::PublicHttpUrlError;
use reqwest::{
    StatusCode,
    header::{HeaderMap, RETRY_AFTER},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Retry,
    Abort,
    RetryAfter(Duration),
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy<E = ProviderCallError> {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub jitter: bool,
    pub classify: fn(&E) -> RetryDecision,
}

impl<E> RetryPolicy<E> {
    #[must_use]
    pub const fn new(
        max_attempts: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
        jitter: bool,
        classify: fn(&E) -> RetryDecision,
    ) -> Self {
        Self { max_attempts, initial_backoff, max_backoff, jitter, classify }
    }

    #[must_use]
    pub const fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            jitter: false,
            classify: abort_retry,
        }
    }
}

impl Default for RetryPolicy<ProviderCallError> {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(5),
            jitter: true,
            classify: classify_provider_call_error,
        }
    }
}

pub async fn with_retry<F, Fut, T, E>(mut call: F, policy: RetryPolicy<E>) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut attempt = 1_u32;

    loop {
        match call().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if attempt >= max_attempts {
                    return Err(error);
                }

                let delay = match (policy.classify)(&error) {
                    RetryDecision::Abort => return Err(error),
                    RetryDecision::Retry => retry_delay(&policy, attempt),
                    RetryDecision::RetryAfter(delay) => delay.min(policy.max_backoff),
                };

                tokio::time::sleep(delay).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ProviderCallError {
    #[error("{context}: {source}")]
    Transport {
        context: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{context}: {source}")]
    ResponseBody {
        context: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{context}: {source}")]
    ResponsePolicy {
        context: String,
        #[source]
        source: PublicHttpUrlError,
    },
    #[error("{context}: {source}")]
    ResponseJson {
        context: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("provider request failed: provider={provider_kind} status={status} detail={detail}")]
    HttpStatus {
        provider_kind: String,
        status: StatusCode,
        retry_after: Option<Duration>,
        detail: String,
    },
    #[error("{message}")]
    Protocol { message: String },
}

impl ProviderCallError {
    #[must_use]
    pub fn transport(context: impl Into<String>, source: reqwest::Error) -> Self {
        Self::Transport { context: context.into(), source }
    }

    #[must_use]
    pub fn response_body(context: impl Into<String>, source: reqwest::Error) -> Self {
        Self::ResponseBody { context: context.into(), source }
    }

    #[must_use]
    pub fn response_policy(context: impl Into<String>, source: PublicHttpUrlError) -> Self {
        Self::ResponsePolicy { context: context.into(), source }
    }

    #[must_use]
    pub fn response_json(context: impl Into<String>, source: reqwest::Error) -> Self {
        Self::ResponseJson { context: context.into(), source }
    }

    #[must_use]
    pub fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Json { context: context.into(), source }
    }

    #[must_use]
    pub fn http_status(
        provider_kind: impl Into<String>,
        status: StatusCode,
        headers: &HeaderMap,
        detail: impl Into<String>,
    ) -> Self {
        Self::HttpStatus {
            provider_kind: provider_kind.into(),
            status,
            retry_after: retry_after_from_headers(headers),
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol { message: message.into() }
    }
}

#[must_use]
pub fn provider_http_status_error(
    provider_kind: &str,
    status: StatusCode,
    headers: &HeaderMap,
    body_text: &str,
) -> ProviderCallError {
    let detail = serde_json::from_str::<serde_json::Value>(body_text)
        .map_or_else(|_| body_text.to_string(), |body| body.to_string());
    ProviderCallError::http_status(
        provider_kind,
        status,
        headers,
        sanitize_provider_error_detail(&detail),
    )
}

#[must_use]
pub fn sanitize_provider_error_detail(message: &str) -> String {
    if message.is_empty() {
        "upstream provider request failed; response body was empty".to_string()
    } else {
        "upstream provider request failed; response details were redacted".to_string()
    }
}

const fn abort_retry<E>(_error: &E) -> RetryDecision {
    RetryDecision::Abort
}

fn classify_provider_call_error(error: &ProviderCallError) -> RetryDecision {
    match error {
        ProviderCallError::Transport { source, .. }
        | ProviderCallError::ResponseBody { source, .. } => {
            if is_retryable_transport_error(source) {
                RetryDecision::Retry
            } else {
                RetryDecision::Abort
            }
        }
        ProviderCallError::ResponsePolicy { source, .. } => {
            if matches!(source, PublicHttpUrlError::BodyReadFailed(_)) {
                RetryDecision::Retry
            } else {
                RetryDecision::Abort
            }
        }
        ProviderCallError::HttpStatus { status, retry_after, .. } => {
            if *status == StatusCode::TOO_MANY_REQUESTS {
                retry_after.map_or(RetryDecision::Retry, RetryDecision::RetryAfter)
            } else if *status == StatusCode::REQUEST_TIMEOUT || status.is_server_error() {
                RetryDecision::Retry
            } else {
                RetryDecision::Abort
            }
        }
        ProviderCallError::ResponseJson { .. }
        | ProviderCallError::Json { .. }
        | ProviderCallError::Protocol { .. } => RetryDecision::Abort,
    }
}

fn retry_delay<E>(policy: &RetryPolicy<E>, attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(31);
    let multiplier = 1_u128 << exponent;
    let millis = policy.initial_backoff.as_millis().saturating_mul(multiplier);
    let capped_millis = millis.min(policy.max_backoff.as_millis()).min(u128::from(u64::MAX));
    let delay = Duration::from_millis(capped_millis as u64);
    if !policy.jitter || delay.is_zero() {
        return delay;
    }

    let max_millis = delay.as_millis().min(u128::from(u64::MAX)) as u64;
    Duration::from_millis(rand::rng().random_range(0..=max_millis))
}

fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = chrono::DateTime::parse_from_rfc2822(value).ok()?.with_timezone(&chrono::Utc);
    let now = chrono::Utc::now();
    if retry_at <= now {
        return Some(Duration::ZERO);
    }
    retry_at.signed_duration_since(now).to_std().ok()
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_body()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };
    use std::time::Instant;

    #[tokio::test]
    async fn succeeds_on_first_attempt_without_retry() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_for_call = Arc::clone(&attempts);

        let value = with_retry(
            || {
                let attempts_for_call = Arc::clone(&attempts_for_call);
                async move {
                    attempts_for_call.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ProviderCallError>("ok")
                }
            },
            RetryPolicy::default(),
        )
        .await
        .expect("first call should succeed");

        assert_eq!(value, "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_429_once_after_retry_after_header() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_for_call = Arc::clone(&attempts);
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "1".parse().expect("retry-after header should parse"));
        let started = Instant::now();

        let value = with_retry(
            || {
                let attempts_for_call = Arc::clone(&attempts_for_call);
                let headers = headers.clone();
                async move {
                    if attempts_for_call.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(ProviderCallError::http_status(
                            "provider-alpha",
                            StatusCode::TOO_MANY_REQUESTS,
                            &headers,
                            "rate limited",
                        ))
                    } else {
                        Ok("ok")
                    }
                }
            },
            RetryPolicy {
                max_attempts: 2,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::from_secs(5),
                jitter: false,
                classify: classify_provider_call_error,
            },
        )
        .await
        .expect("second call should succeed");

        assert_eq!(value, "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(started.elapsed() >= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn retries_5xx_until_max_attempts_then_bubbles_error() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_for_call = Arc::clone(&attempts);
        let headers = HeaderMap::new();

        let error = with_retry(
            || {
                let attempts_for_call = Arc::clone(&attempts_for_call);
                let headers = headers.clone();
                async move {
                    attempts_for_call.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(ProviderCallError::http_status(
                        "provider-alpha",
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &headers,
                        "server error",
                    ))
                }
            },
            RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::ZERO,
                jitter: false,
                classify: classify_provider_call_error,
            },
        )
        .await
        .expect_err("third failed attempt should bubble");

        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(matches!(
            error,
            ProviderCallError::HttpStatus { status: StatusCode::INTERNAL_SERVER_ERROR, .. }
        ));
    }

    #[tokio::test]
    async fn aborts_4xx_without_retry() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_for_call = Arc::clone(&attempts);
        let headers = HeaderMap::new();

        let error = with_retry(
            || {
                let attempts_for_call = Arc::clone(&attempts_for_call);
                let headers = headers.clone();
                async move {
                    attempts_for_call.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(ProviderCallError::http_status(
                        "provider-alpha",
                        StatusCode::BAD_REQUEST,
                        &headers,
                        "bad request",
                    ))
                }
            },
            RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::ZERO,
                jitter: false,
                classify: classify_provider_call_error,
            },
        )
        .await
        .expect_err("bad request should not retry");

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(matches!(
            error,
            ProviderCallError::HttpStatus { status: StatusCode::BAD_REQUEST, .. }
        ));
    }

    #[test]
    fn protocol_message_text_never_controls_retryability() {
        let error = ProviderCallError::protocol(
            "connection reset unexpected eof http2 sendrequest error sending request",
        );

        assert_eq!(classify_provider_call_error(&error), RetryDecision::Abort);
    }

    #[test]
    fn provider_error_detail_is_redacted_without_a_marker_dictionary() {
        let detail = sanitize_provider_error_detail("arbitrary upstream response payload");

        assert!(!detail.contains("arbitrary upstream response payload"));
    }
}
