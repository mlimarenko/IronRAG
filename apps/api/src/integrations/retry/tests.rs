use super::provider::{classify_provider_call_error, retry_after_from_headers, retry_delay};
use super::*;
use crate::integrations::provider_budget::{
    ProviderBudgetConfig, ProviderBudgetError, ProviderBudgetGuard, ProviderBudgetRegistry,
    ProviderBudgetRegistryOptions, ProviderIdentity, ProviderLane, with_lane,
};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::time::Instant;

struct DropSignal {
    _guard: ProviderBudgetGuard,
    dropped: Arc<tokio::sync::Notify>,
}

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.dropped.notify_one();
    }
}

#[tokio::test]
async fn fake_provider_429s_until_it_succeeds_within_the_execution_deadline() {
    const RATE_LIMITS_BEFORE_SUCCESS: u32 = 5;
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_call = Arc::clone(&attempts);
    let headers = HeaderMap::new();

    let (value, telemetry) = with_provider_retry_execution(Duration::from_secs(1), async {
        let value = with_retry(
            || {
                let attempts_for_call = Arc::clone(&attempts_for_call);
                let headers = headers.clone();
                async move {
                    let attempt = attempts_for_call.fetch_add(1, Ordering::SeqCst);
                    if attempt < RATE_LIMITS_BEFORE_SUCCESS {
                        Err(ProviderCallError::http_status(
                            "provider-alpha",
                            StatusCode::TOO_MANY_REQUESTS,
                            &headers,
                            "rate limited",
                        ))
                    } else {
                        Ok("clean answer")
                    }
                }
            },
            RetryPolicy {
                max_attempts: 1,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::ZERO,
                jitter: false,
                classify: classify_provider_call_error,
            },
        )
        .await;
        (value, current_provider_retry_telemetry())
    })
    .await;

    assert_eq!(value.expect("provider should recover before the deadline"), "clean answer");
    assert_eq!(attempts.load(Ordering::SeqCst), RATE_LIMITS_BEFORE_SUCCESS + 1);
    assert_eq!(telemetry.provider_429_count, u64::from(RATE_LIMITS_BEFORE_SUCCESS));
    assert_eq!(telemetry.provider_retry_count, u64::from(RATE_LIMITS_BEFORE_SUCCESS));
    assert_eq!(telemetry.degraded_completion_count, 0);
}

#[tokio::test]
async fn retry_after_past_the_execution_deadline_does_not_start_another_attempt() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_call = Arc::clone(&attempts);
    let mut headers = HeaderMap::new();
    headers.insert(RETRY_AFTER, "60".parse().expect("retry-after header should parse"));

    let (error, telemetry) = with_provider_retry_execution(Duration::from_millis(10), async {
        let error = with_retry(
            || {
                attempts_for_call.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err::<(), _>(ProviderCallError::http_status(
                    "provider-alpha",
                    StatusCode::TOO_MANY_REQUESTS,
                    &headers,
                    "rate limited",
                )))
            },
            RetryPolicy {
                max_attempts: u32::MAX,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::from_millis(1),
                jitter: false,
                classify: classify_provider_call_error,
            },
        )
        .await
        .expect_err("retry-after cannot fit inside the execution deadline");
        (error, current_provider_retry_telemetry())
    })
    .await;

    assert!(matches!(
        error,
        ProviderCallError::HttpStatus { status: StatusCode::TOO_MANY_REQUESTS, .. }
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(telemetry.provider_429_count, 1);
    assert_eq!(telemetry.provider_retry_count, 0);
}

#[tokio::test]
async fn throttled_query_releases_the_shared_permit_before_retry_backoff() {
    let registry = Arc::new(
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: 2,
                query_reserved: 1,
                ..ProviderBudgetConfig::unlimited()
            },
            ProviderBudgetRegistryOptions::default(),
        )
        .expect("test provider budget should be valid"),
    );
    let identity = ProviderIdentity::new("provider-alpha", "https://endpoint.example/v1");
    let attempts = Arc::new(AtomicU32::new(0));
    let first_guard_dropped = Arc::new(tokio::sync::Notify::new());
    let retry_registry = Arc::clone(&registry);
    let retry_identity = identity.clone();
    let retry_attempts = Arc::clone(&attempts);
    let retry_drop_signal = Arc::clone(&first_guard_dropped);
    let headers = HeaderMap::new();

    let retrying_query = tokio::spawn(with_lane(ProviderLane::Query, async move {
        with_retry(
            || {
                let retry_registry = Arc::clone(&retry_registry);
                let retry_identity = retry_identity.clone();
                let retry_attempts = Arc::clone(&retry_attempts);
                let retry_drop_signal = Arc::clone(&retry_drop_signal);
                let headers = headers.clone();
                async move {
                    let guard = retry_registry
                        .acquire(&retry_identity, ProviderLane::Query)
                        .await
                        .expect("query attempt should acquire a lane permit");
                    let _drop_signal = DropSignal { _guard: guard, dropped: retry_drop_signal };
                    if retry_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(ProviderCallError::http_status(
                            "provider-alpha",
                            StatusCode::TOO_MANY_REQUESTS,
                            &headers,
                            "rate limited",
                        ))
                    } else {
                        Ok(())
                    }
                }
            },
            RetryPolicy {
                max_attempts: 2,
                initial_backoff: Duration::from_millis(100),
                max_backoff: Duration::from_millis(100),
                jitter: false,
                classify: classify_provider_call_error,
            },
        )
        .await
    }));

    first_guard_dropped.notified().await;
    let ingest_guard = tokio::time::timeout(
        Duration::from_millis(25),
        registry.acquire(&identity, ProviderLane::Ingest),
    )
    .await
    .expect("ingest must not wait behind a query lane in retry backoff")
    .expect("ingest lane permit should remain available");
    drop(ingest_guard);

    retrying_query
        .await
        .expect("retrying query task should join")
        .expect("retrying query should eventually succeed");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

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
        RetryPolicy::no_retry(),
    )
    .await
    .expect("first call should succeed");

    assert_eq!(value, "ok");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn non_idempotent_policy_never_retries() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_call = Arc::clone(&attempts);

    let error = with_retry(
        || {
            attempts_for_call.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err::<(), _>("failed"))
        },
        RetryPolicy::no_retry(),
    )
    .await
    .expect_err("a failed non-idempotent call should return its first error");

    assert_eq!(error, "failed");
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

#[test]
fn retry_after_http_date_is_parsed() {
    let retry_at = std::time::SystemTime::now() + Duration::from_secs(2);
    let mut headers = HeaderMap::new();
    headers.insert(
        RETRY_AFTER,
        httpdate::fmt_http_date(retry_at)
            .parse()
            .expect("retry-after HTTP date should parse as a header"),
    );

    let delay = retry_after_from_headers(&headers)
        .expect("a valid retry-after HTTP date should produce a delay");

    assert!(delay <= Duration::from_secs(2));
    assert!(delay >= Duration::from_millis(500));
}

#[test]
fn exponential_backoff_is_capped() {
    const MAX_BACKOFF: Duration = Duration::from_millis(250);
    let policy = RetryPolicy {
        max_attempts: 10,
        initial_backoff: Duration::from_secs(2),
        max_backoff: MAX_BACKOFF,
        jitter: false,
        classify: classify_provider_call_error,
    };

    assert_eq!(retry_delay(&policy, 9), MAX_BACKOFF);
}

#[test]
fn service_unavailable_honors_retry_after() {
    const RETRY_AFTER_SECONDS: u64 = 7;
    let mut headers = HeaderMap::new();
    headers.insert(
        RETRY_AFTER,
        RETRY_AFTER_SECONDS.to_string().parse().expect("retry-after header should parse"),
    );
    let error = ProviderCallError::http_status(
        "provider-alpha",
        StatusCode::SERVICE_UNAVAILABLE,
        &headers,
        "unavailable",
    );

    assert_eq!(
        classify_provider_call_error(&error),
        RetryDecision::RetryAfter(Duration::from_secs(RETRY_AFTER_SECONDS))
    );
    let policy = RetryPolicy {
        max_attempts: 2,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(5),
        jitter: false,
        classify: classify_provider_call_error,
    };
    assert_eq!(
        retry_delay_for_decision(
            &policy,
            1,
            RetryDecision::RetryAfter(Duration::from_secs(RETRY_AFTER_SECONDS)),
        ),
        Duration::from_secs(RETRY_AFTER_SECONDS),
        "Retry-After must not be capped by the exponential-backoff ceiling",
    );
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
async fn aborts_non_429_4xx_without_retry() {
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
                    StatusCode::REQUEST_TIMEOUT,
                    &headers,
                    "request timeout",
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
    .expect_err("a non-429 client error should not retry");

    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(matches!(
        error,
        ProviderCallError::HttpStatus { status: StatusCode::REQUEST_TIMEOUT, .. }
    ));
}

#[test]
fn single_retry_budget_survives_other_retry_classes() {
    let (decision, used) = apply_single_retry_budget(RetryDecision::RetryOnce, false);
    assert_eq!((decision, used), (RetryDecision::Retry, true));

    for decision in [
        RetryDecision::Retry,
        RetryDecision::RetryAfter(Duration::from_secs(2)),
        RetryDecision::Abort,
    ] {
        assert_eq!(apply_single_retry_budget(decision, false), (decision, false));
        assert_eq!(apply_single_retry_budget(decision, used), (decision, true));
    }
    assert_eq!(
        apply_single_retry_budget(RetryDecision::RetryOnce, used),
        (RetryDecision::Abort, true)
    );
}

#[test]
fn empty_404_retries_once_while_nonempty_404_and_auth_errors_abort() {
    let headers = HeaderMap::new();
    let empty_404 =
        provider_http_status_error("provider-alpha", StatusCode::NOT_FOUND, &headers, "");
    assert_eq!(classify_provider_call_error(&empty_404), RetryDecision::RetryOnce);

    let whitespace_404 =
        provider_http_status_error("provider-alpha", StatusCode::NOT_FOUND, &headers, "  \r\n");
    assert_eq!(
        classify_provider_call_error(&whitespace_404),
        RetryDecision::RetryOnce,
        "a whitespace-only body is structurally empty"
    );

    let json_404 = provider_http_status_error(
        "provider-alpha",
        StatusCode::NOT_FOUND,
        &headers,
        r#"{"error":{"message":"no such model"}}"#,
    );
    assert_eq!(
        classify_provider_call_error(&json_404),
        RetryDecision::Abort,
        "a 404 carrying any body is a permanent resource error"
    );

    for status in
        [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN, StatusCode::UNPROCESSABLE_ENTITY]
    {
        let error = provider_http_status_error("provider-alpha", status, &headers, "");
        assert_eq!(
            classify_provider_call_error(&error),
            RetryDecision::Abort,
            "{} must abort even with an empty body",
            status.as_u16()
        );
    }
}

/// Drives `with_retry` through a persistent 404 that always carries the same
/// upstream body, under a three-attempt policy, and returns the number of
/// attempts observed. One helper so the two 404 outcomes below cannot drift
/// into a duplicated body the repository-wide clone gate rejects.
async fn attempts_for_persistent_404(body: &str) -> u32 {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_call = Arc::clone(&attempts);
    let headers = HeaderMap::new();
    let body = body.to_string();

    with_retry(
        || {
            let attempts_for_call = Arc::clone(&attempts_for_call);
            let headers = headers.clone();
            let body = body.clone();
            async move {
                attempts_for_call.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(provider_http_status_error(
                    "provider-alpha",
                    StatusCode::NOT_FOUND,
                    &headers,
                    &body,
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
    .expect_err("a persistent 404 must surface an error");

    attempts.load(Ordering::SeqCst)
}

#[tokio::test]
async fn nonempty_404_never_retries() {
    assert_eq!(
        attempts_for_persistent_404(r#"{"error":{"message":"no such model"}}"#).await,
        1,
        "a 404 that carries a body is a permanent resource error"
    );
}

#[tokio::test]
async fn empty_404_retries_exactly_once_then_succeeds() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_call = Arc::clone(&attempts);
    let headers = HeaderMap::new();

    let value = with_retry(
        || {
            let attempts_for_call = Arc::clone(&attempts_for_call);
            let headers = headers.clone();
            async move {
                if attempts_for_call.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(provider_http_status_error(
                        "provider-alpha",
                        StatusCode::NOT_FOUND,
                        &headers,
                        "",
                    ))
                } else {
                    Ok("recovered")
                }
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
    .expect("a blank 404 should recover on the single bounded retry");

    assert_eq!(value, "recovered");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn second_empty_404_aborts_even_with_attempt_budget_left() {
    assert_eq!(
        attempts_for_persistent_404("").await,
        2,
        "the second blank 404 must stop, not consume the third attempt"
    );
}

#[test]
fn empty_body_flag_separates_blank_from_unread_404() {
    let headers = HeaderMap::new();
    let blank = provider_http_status_error("provider-alpha", StatusCode::NOT_FOUND, &headers, "");
    assert!(blank.is_empty_body());
    assert_eq!(classify_provider_call_error(&blank), RetryDecision::RetryOnce);

    let unread =
        provider_http_status_error_unread("provider-alpha", StatusCode::NOT_FOUND, &headers);
    assert!(!unread.is_empty_body(), "an unread body is never a proven blank");
    assert_eq!(classify_provider_call_error(&unread), RetryDecision::Abort);
    assert_eq!(unread.redacted_detail(), "provider=provider-alpha http_status=404");
    assert!(!unread.redacted_detail().contains("empty"));
}

#[test]
fn protocol_message_text_never_controls_retryability() {
    let error = ProviderCallError::protocol(
        "connection reset unexpected eof http2 sendrequest error sending request",
    );

    assert_eq!(classify_provider_call_error(&error), RetryDecision::Abort);
}

#[test]
fn quota_exhausted_is_extracted_from_structured_code_and_aborts_retry() {
    let headers = HeaderMap::new();
    let body = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;

    let error =
        provider_http_status_error("provider-alpha", StatusCode::TOO_MANY_REQUESTS, &headers, body);

    assert!(error.is_quota_exhausted());
    assert_eq!(classify_provider_call_error(&error), RetryDecision::Abort);
    let rendered = error.to_string();
    assert!(rendered.contains("quota exhausted"), "{rendered}");
    assert!(!rendered.contains("You exceeded"), "provider text must stay redacted: {rendered}");
}

#[test]
fn plain_rate_limit_without_quota_code_stays_retryable() {
    let headers = HeaderMap::new();
    let body = r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#;

    let error =
        provider_http_status_error("provider-alpha", StatusCode::TOO_MANY_REQUESTS, &headers, body);

    assert!(!error.is_quota_exhausted());
    assert_eq!(classify_provider_call_error(&error), RetryDecision::Retry);
}

#[test]
fn quota_code_in_free_text_only_is_ignored() {
    let headers = HeaderMap::new();
    let body = r#"{"error":{"message":"mentions insufficient_quota in prose","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#;

    let error =
        provider_http_status_error("provider-alpha", StatusCode::TOO_MANY_REQUESTS, &headers, body);

    assert!(!error.is_quota_exhausted());
}

#[test]
fn provider_error_detail_is_redacted_without_a_marker_dictionary() {
    let detail = sanitize_provider_error_detail("arbitrary upstream response payload");

    assert!(!detail.contains("arbitrary upstream response payload"));
}

#[test]
fn redacted_detail_reports_status_and_quota_without_response_text() {
    let headers = HeaderMap::new();
    let body = r#"{"error":{"message":"secret-body-diagnostic","type":"insufficient_quota","code":"insufficient_quota"}}"#;
    let error = provider_http_status_error("OpenAI", StatusCode::TOO_MANY_REQUESTS, &headers, body);

    let detail = error.redacted_detail();

    assert_eq!(detail, "provider=openai http_status=429 quota_exhausted");
    assert!(!detail.contains("secret-body-diagnostic"));
}

#[test]
fn redacted_detail_classifies_non_http_variants_by_shape() {
    let json_source = serde_json::from_str::<serde_json::Value>("{").expect_err("malformed JSON");
    assert_eq!(
        ProviderCallError::json("secret-context", json_source).redacted_detail(),
        "json_decode"
    );
    assert_eq!(
        ProviderCallError::protocol("secret-protocol-message").redacted_detail(),
        "protocol"
    );
    assert_eq!(
        ProviderCallError::admission(ProviderBudgetError::ThrottleTimeout { timeout_ms: 1 })
            .redacted_detail(),
        "admission_throttle_timeout"
    );
    assert_eq!(ProviderCallError::timeout("secret-timeout-context").redacted_detail(), "timeout");
}

#[test]
fn execution_deadline_timeout_is_terminal_and_redacted() {
    let error = ProviderCallError::timeout("secret-timeout-context");

    assert_eq!(classify_provider_call_error(&error), RetryDecision::Abort);
    assert_eq!(error.redacted_detail(), "timeout");
    assert!(!error.redacted_detail().contains("secret-timeout-context"));
}

#[test]
fn redacted_detail_sanitizes_untrusted_provider_kind() {
    let headers = HeaderMap::new();
    let hostile =
        provider_http_status_error("Weird Provider!!", StatusCode::BAD_GATEWAY, &headers, "");
    assert_eq!(hostile.redacted_detail(), "provider=weirdprovider http_status=502");

    let empty = provider_http_status_error("!!!", StatusCode::BAD_GATEWAY, &headers, "");
    assert_eq!(empty.redacted_detail(), "provider=unknown http_status=502");
}

#[tokio::test(start_paused = true)]
async fn rejected_admission_refunds_the_rate_token_it_consumed() {
    // burst = 2 lets the second attempt consume a rate token while
    // max_outbound = 1 refuses its concurrency permit. The refunded token must
    // let the next attempt throttle without waiting a full refill interval, so
    // this pins the throttle/acquire/refund ordering of
    // `acquire_provider_budget` against a regression that drops the refund.
    let registry = Arc::new(
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: 1,
                query_reserved: 0,
                requests_per_second: 1,
                burst: 2,
            },
            ProviderBudgetRegistryOptions {
                acquire_timeout: Duration::from_millis(50),
                ..ProviderBudgetRegistryOptions::default()
            },
        )
        .expect("build budget registry"),
    );
    let identity = ProviderIdentity::new("provider-refund", "https://endpoint.example/v1");

    let first =
        acquire_provider_budget(&registry, &identity).await.expect("first attempt admitted");

    let refused = acquire_provider_budget(&registry, &identity).await;
    assert!(
        matches!(
            refused,
            Err(ProviderCallError::Admission {
                source: ProviderBudgetError::AcquireTimeout { .. }
            })
        ),
        "the held concurrency permit must refuse the second attempt, got {refused:?}"
    );

    drop(first);

    let next = acquire_provider_budget(&registry, &identity).await;
    assert!(
        next.is_ok(),
        "a rate token consumed by a rejected admission must be refunded, got {next:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn cancelled_concurrency_admission_refunds_the_rate_token() {
    // A saturated concurrency permit forces `acquire_provider_budget` to wait
    // AFTER it has reserved the only burst token. An outer deadline that drops
    // that future must return the token: otherwise the next attempt is starved
    // for a full refill interval, and the leak stays invisible until the
    // provider goes quiet. This is the cancellation path the explicit refund
    // call in the old implementation could never observe.
    let registry = Arc::new(
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: 1,
                query_reserved: 0,
                requests_per_second: 1,
                burst: 1,
            },
            ProviderBudgetRegistryOptions {
                acquire_timeout: Duration::from_secs(30),
                ..ProviderBudgetRegistryOptions::default()
            },
        )
        .expect("build budget registry"),
    );
    let identity = ProviderIdentity::new("provider-cancelled", "https://endpoint.example/v1");

    // Hold the single concurrency permit so admission cannot complete.
    let held = registry.acquire(&identity, ProviderLane::Ingest).await.expect("hold permit");

    // Consume the only burst token, then drop the admission future while it is
    // still waiting on concurrency.
    let cancelled = tokio::time::timeout(
        Duration::from_millis(10),
        acquire_provider_budget(&registry, &identity),
    )
    .await;
    assert!(cancelled.is_err(), "a saturated concurrency permit must keep admission pending");

    // Release the permit and admit again. A leaked token would force this
    // throttle to wait a full one-second refill, so the 10 ms bound would
    // elapse instead of admitting.
    let admitted = tokio::time::timeout(Duration::from_millis(10), async {
        drop(held);
        acquire_provider_budget(&registry, &identity).await
    })
    .await
    .expect("a cancelled admission must refund its rate token");
    assert!(admitted.is_ok(), "the next attempt must be admitted, got {admitted:?}");
}
