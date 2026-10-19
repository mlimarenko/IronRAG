//! Loopback HTTP tests for the SystemOne client transport.
//!
//! The mock server is an in-process `TcpListener` so the tests exercise the real
//! reqwest transport, retry classifier and redaction without a live endpoint.
//! The API key is a dummy value: the tests assert it is sent as a bearer token
//! and that it never appears in an error message.

use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use reqwest::Url;
use tokio::{io::AsyncWriteExt, net::TcpStream};
use zeroize::Zeroizing;

use crate::{
    domains::runtime_ingestion::RuntimeProviderFailureClass,
    integrations::{
        provider_budget::{
            ProviderBudgetConfig, ProviderBudgetRegistry, ProviderBudgetRegistryOptions,
            ProviderIdentity,
        },
        retry::{ProviderCallError, RetryPolicy, with_retry},
    },
    services::ops::provider_failure::ProviderFailureClassificationService,
    shared::{
        outbound_http::PublicHttpUrlError,
        provider_http::{ProviderHttpError, ProviderHttpTransport, ProviderHttpTransportConfig},
    },
};

use super::{
    MockResponse, MockServer,
    client::{SystemOneBudget, SystemOneClient, SystemOneClientConfig},
    protocol::{SystemOneDecisionRequest, SystemOneQuestion, SystemOneUsage},
    validation::SystemOneError,
};

/// Dummy credential used by every test. It is deliberately not a real secret;
/// the tests prove it is sent and never leaked, not that it authenticates.
const DUMMY_API_KEY: &str = "test-dummy-api-key-not-a-secret";

/// End-to-end retry deadline for tests that only exercise bounded retry, not
/// the deadline itself: long enough that millisecond backoffs always fit.
const DEFAULT_EXECUTION_DEADLINE: Duration = Duration::from_secs(5);

fn noul_question() -> SystemOneQuestion {
    SystemOneQuestion::Noul {
        instructions: "Decide whether the state is healthy.".to_string(),
        criteria: None,
    }
}

fn single_noul_request() -> SystemOneDecisionRequest {
    let mut questions = BTreeMap::new();
    questions.insert("n".to_string(), noul_question());
    SystemOneDecisionRequest {
        state: "state text".to_string(),
        model: "caller-model".to_string(),
        questions,
    }
}

fn noul_response_body() -> String {
    serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {"n": {"type": "noul", "noul": 0.5}},
        "usage": {"input_tokens": 12, "output_tokens": 3},
    })
    .to_string()
}

fn test_client(
    server: &MockServer,
    max_attempts: u32,
    request_timeout: Duration,
) -> SystemOneClient {
    test_client_with_base_url(
        Url::parse(&server.base_url).expect("parse mock base url"),
        max_attempts,
        request_timeout,
        None,
    )
}

fn test_client_with_base_url(
    base_url: Url,
    max_attempts: u32,
    request_timeout: Duration,
    budget: Option<SystemOneBudget>,
) -> SystemOneClient {
    SystemOneClient::try_new(SystemOneClientConfig {
        base_url,
        api_key: Zeroizing::new(DUMMY_API_KEY.to_string()),
        request_timeout,
        allow_private_network: true,
        max_attempts,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
        execution_deadline: DEFAULT_EXECUTION_DEADLINE,
        budget,
    })
    .expect("build systemone client")
}

/// Builds a client with an explicit end-to-end retry deadline so a test can
/// prove the deadline bounds the whole retry sequence.
fn test_client_with_deadline(
    server: &MockServer,
    max_attempts: u32,
    request_timeout: Duration,
    execution_deadline: Duration,
) -> SystemOneClient {
    SystemOneClient::try_new(SystemOneClientConfig {
        base_url: Url::parse(&server.base_url).expect("parse mock base url"),
        api_key: Zeroizing::new(DUMMY_API_KEY.to_string()),
        request_timeout,
        allow_private_network: true,
        max_attempts,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
        execution_deadline,
        budget: None,
    })
    .expect("build systemone client")
}

/// Builds a client whose transport is an injected one, so a test can drive
/// target preparation directly. It shares the standard config builder, so the
/// injected transport is the only difference from [`test_client_with_base_url`].
fn test_client_with_base_url_and_transport(
    base_url: Url,
    max_attempts: u32,
    execution_deadline: Duration,
    transport: ProviderHttpTransport,
) -> SystemOneClient {
    SystemOneClient::try_new(SystemOneClientConfig {
        base_url,
        api_key: Zeroizing::new(DUMMY_API_KEY.to_string()),
        request_timeout: Duration::from_secs(2),
        allow_private_network: true,
        max_attempts,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
        execution_deadline,
        budget: None,
    })
    .expect("build systemone client")
    .with_transport(transport)
}

/// Builds a client with an explicit API key so a test can exercise key handling
/// without routing through the shared dummy fixture.
fn test_client_with_api_key(
    base_url: Url,
    api_key: &str,
    max_attempts: u32,
    request_timeout: Duration,
) -> Result<SystemOneClient, SystemOneError> {
    SystemOneClient::try_new(SystemOneClientConfig {
        base_url,
        api_key: Zeroizing::new(api_key.to_string()),
        request_timeout,
        allow_private_network: true,
        max_attempts,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
        execution_deadline: DEFAULT_EXECUTION_DEADLINE,
        budget: None,
    })
}

fn provider_error(error: SystemOneError) -> ProviderCallError {
    match error {
        SystemOneError::Provider { source } => source,
        other => panic!("expected a provider error, got {other}"),
    }
}

#[tokio::test]
async fn evaluate_sends_one_authenticated_request_and_returns_usage() {
    let server = MockServer::start(vec![MockResponse {
        status: 200,
        body: noul_response_body(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let response = client.evaluate(single_noul_request()).await.expect("evaluate succeeds");
    assert_eq!(response.usage, SystemOneUsage { input_tokens: 12, output_tokens: 3 });

    server.wait_for_requests(1).await;
    let requests = server.requests();
    server.shutdown().await;
    assert_eq!(requests.len(), 1, "a successful call must send exactly one request");
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/systemone");
    assert_eq!(request.header("content-type"), Some("application/json"));
    let expected_authorization = format!("Bearer {DUMMY_API_KEY}");
    assert_eq!(request.header("authorization"), Some(expected_authorization.as_str()));
    let body: serde_json::Value =
        serde_json::from_slice(&request.body).expect("request body is JSON");
    assert_eq!(body["model"], "caller-model");
    assert_eq!(body["questions"]["n"]["type"], "noul");
}

#[tokio::test]
async fn evaluate_aborts_without_retry_on_unauthorized() {
    let server = MockServer::start(vec![MockResponse {
        status: 401,
        body: r#"{"error":{"message":"unauthorized"}}"#.to_string(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let error =
        provider_error(client.evaluate(single_noul_request()).await.expect_err("401 fails"));
    assert!(matches!(
        &error,
        ProviderCallError::HttpStatus { status, .. } if status.as_u16() == 401
    ));
    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "401 must abort without a retry");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_aborts_without_retry_on_unprocessable_entity() {
    let server = MockServer::start(vec![MockResponse {
        status: 422,
        body: r#"{"error":{"message":"invalid question"}}"#.to_string(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let error =
        provider_error(client.evaluate(single_noul_request()).await.expect_err("422 fails"));
    assert!(matches!(
        &error,
        ProviderCallError::HttpStatus { status, .. } if status.as_u16() == 422
    ));
    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "422 must abort without a retry");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_retries_bounded_on_rate_limit_then_succeeds() {
    let server = MockServer::start(vec![
        MockResponse {
            status: 429,
            body: "{}".to_string(),
            delay: None,
            advertised_content_length: None,
        },
        MockResponse {
            status: 429,
            body: "{}".to_string(),
            delay: None,
            advertised_content_length: None,
        },
        MockResponse {
            status: 200,
            body: noul_response_body(),
            delay: None,
            advertised_content_length: None,
        },
    ])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let response = client.evaluate(single_noul_request()).await.expect("retry recovers");
    assert_eq!(response.usage.output_tokens, 3);
    server.wait_for_requests(3).await;
    assert_eq!(server.request_count(), 3, "two 429s then success is three attempts");
    server.shutdown().await;
}

#[tokio::test]
async fn evaluate_exhausts_bounded_retries_on_persistent_rate_limit() {
    let server = MockServer::start(vec![MockResponse {
        status: 429,
        body: "{}".to_string(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 2, Duration::from_secs(2));

    let error =
        provider_error(client.evaluate(single_noul_request()).await.expect_err("429 fails"));
    assert!(matches!(
        &error,
        ProviderCallError::HttpStatus { status, .. } if status.as_u16() == 429
    ));
    server.wait_for_requests(2).await;
    assert_eq!(server.request_count(), 2, "max_attempts bounds the retry count");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_retries_one_empty_404_then_succeeds() {
    // A 404 with a structurally empty upstream body is a transient blank, so
    // the real transport retries it exactly once and recovers.
    let server = MockServer::start(vec![
        MockResponse {
            status: 404,
            body: String::new(),
            delay: None,
            advertised_content_length: None,
        },
        MockResponse {
            status: 200,
            body: noul_response_body(),
            delay: None,
            advertised_content_length: None,
        },
    ])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let response = client.evaluate(single_noul_request()).await.expect("a blank 404 recovers");
    assert_eq!(response.usage.output_tokens, 3);
    server.wait_for_requests(2).await;
    assert_eq!(server.request_count(), 2, "one blank 404 then success is exactly two attempts");
    server.shutdown().await;
}

#[tokio::test]
async fn evaluate_stops_after_one_empty_404_retry() {
    // Even with a three-attempt budget a blank 404 retries only once: the
    // second blank 404 must abort instead of consuming the remaining attempt.
    let server = MockServer::start(vec![MockResponse {
        status: 404,
        body: String::new(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let error = provider_error(
        client.evaluate(single_noul_request()).await.expect_err("a lasting blank 404 fails"),
    );
    assert!(matches!(
        &error,
        ProviderCallError::HttpStatus { status, .. } if status.as_u16() == 404
    ));
    server.wait_for_requests(2).await;
    assert_eq!(
        server.request_count(),
        2,
        "the second blank 404 must stop despite the remaining attempt budget"
    );
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_aborts_nonempty_404_without_retry() {
    // A 404 that carries a body is a permanent resource error, never a blank.
    let server = MockServer::start(vec![MockResponse {
        status: 404,
        body: r#"{"error":{"message":"no such model"}}"#.to_string(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let error = provider_error(
        client.evaluate(single_noul_request()).await.expect_err("a populated 404 fails"),
    );
    assert!(matches!(
        &error,
        ProviderCallError::HttpStatus { status, .. } if status.as_u16() == 404
    ));
    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "a 404 carrying a body must not retry");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_aborts_oversized_404_without_retry() {
    // A 404 whose error body cannot be read because it is oversize is NOT a
    // blank: the read outcome must keep it terminal, not retry-once.
    let server = MockServer::start(vec![MockResponse {
        status: 404,
        body: String::new(),
        delay: None,
        advertised_content_length: Some(1 << 40),
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let error = provider_error(
        client.evaluate(single_noul_request()).await.expect_err("an oversize 404 fails"),
    );
    assert!(matches!(
        &error,
        ProviderCallError::HttpStatus { status, .. } if status.as_u16() == 404
    ));
    assert!(!error.is_empty_body(), "an unread oversize body is not a proven blank");
    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "an oversize 404 must not retry as blank");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_aborts_truncated_404_without_retry() {
    // A truncated error body (advertised length exceeds the bytes written) is
    // an unread body, not an empty one.
    let server = MockServer::start(vec![MockResponse {
        status: 404,
        body: String::new(),
        delay: None,
        advertised_content_length: Some(4096),
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let error = provider_error(
        client.evaluate(single_noul_request()).await.expect_err("a truncated 404 fails"),
    );
    assert!(matches!(
        &error,
        ProviderCallError::HttpStatus { status, .. } if status.as_u16() == 404
    ));
    assert!(!error.is_empty_body(), "an unread truncated body is not a proven blank");
    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "a truncated 404 must not retry as blank");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_bounds_an_oversized_retry_after_to_the_execution_deadline() {
    // A provider may advertise a Retry-After of a day. The shared retry loop
    // would sleep it in full and hold the caller far past any budget, so the
    // client bounds the whole retry group with its own wall-clock timeout: the
    // call returns at the deadline with a typed timeout error, and the sleep
    // for the advertised Retry-After is cancelled instead of honored.
    //
    // The budget must comfortably exceed one loopback round trip so the first
    // attempt always reaches the mock server before the timeout fires — a 50 ms
    // budget let a loaded host cancel the call before the first request was
    // sent, failing the request-count assertion below — while staying tiny
    // next to the one-day Retry-After this test proves is not honored.
    let server = MockServer::start_with_headers(
        vec![MockResponse {
            status: 429,
            body: "{}".to_string(),
            delay: None,
            advertised_content_length: None,
        }],
        vec![("Retry-After".to_string(), "86400".to_string())],
    )
    .await;
    let client =
        test_client_with_deadline(&server, 5, Duration::from_secs(2), Duration::from_secs(1));

    let bounded =
        tokio::time::timeout(Duration::from_secs(5), client.evaluate(single_noul_request()))
            .await
            .expect(
                "evaluate must return within its execution deadline, not sleep for Retry-After",
            );

    let error = provider_error(
        bounded.expect_err("a 429 with an oversized Retry-After hits the execution deadline"),
    );
    assert!(
        matches!(&error, ProviderCallError::Timeout { .. }),
        "the execution deadline must surface as a typed timeout error, got {error}"
    );
    assert_eq!(error.redacted_detail(), "timeout");
    server.wait_for_requests(1).await;
    assert_eq!(
        server.request_count(),
        1,
        "a Retry-After past the deadline must not start another attempt"
    );
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_deadline_cancels_pending_dns_before_any_http_request() {
    // A hostname is not a literal IP, so target preparation goes through the
    // injected resolver. That resolver never completes, which exercises the DNS
    // leg without touching real DNS. The execution deadline must cancel
    // preparation and return a typed timeout before any HTTP request can exist:
    // no resolved target means no attempt is ever built, so no socket is opened.
    const PENDING_HOST: &str = "pending.typesafe.invalid";
    let (transport, resolve_calls) = ProviderHttpTransport::try_new_with_pending_resolver(
        ProviderHttpTransportConfig::llm(Duration::from_secs(2)),
        PENDING_HOST,
    )
    .expect("pending-resolver transport builds");
    let base_url =
        Url::parse(&format!("http://{PENDING_HOST}:8080")).expect("parse pending base url");
    let client =
        test_client_with_base_url_and_transport(base_url, 5, Duration::from_millis(100), transport);

    let started = Instant::now();
    let bounded =
        tokio::time::timeout(Duration::from_secs(2), client.evaluate(single_noul_request()))
            .await
            .expect("the execution deadline must cancel pending DNS, not hang");
    let elapsed = started.elapsed();

    let error = provider_error(bounded.expect_err("pending DNS must hit the execution deadline"));
    assert!(
        matches!(&error, ProviderCallError::Timeout { .. }),
        "pending DNS must surface as a typed timeout error, got {error}"
    );
    assert_eq!(error.redacted_detail(), "timeout");
    assert_eq!(
        resolve_calls.load(Ordering::SeqCst),
        1,
        "the DNS leg must be entered exactly once, proving the deadline bounded preparation \
         before any request could be built"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "the deadline must fire promptly instead of waiting on DNS, took {elapsed:?}"
    );
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_retries_on_service_unavailable() {
    let server = MockServer::start(vec![
        MockResponse {
            status: 529,
            body: "{}".to_string(),
            delay: None,
            advertised_content_length: None,
        },
        MockResponse {
            status: 529,
            body: "{}".to_string(),
            delay: None,
            advertised_content_length: None,
        },
        MockResponse {
            status: 200,
            body: noul_response_body(),
            delay: None,
            advertised_content_length: None,
        },
    ])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    client.evaluate(single_noul_request()).await.expect("529 is retryable");
    server.wait_for_requests(3).await;
    assert_eq!(server.request_count(), 3, "two 529s then success is three attempts");
    server.shutdown().await;
}

#[tokio::test]
async fn evaluate_retries_cut_success_body_then_succeeds() {
    // A 200 whose advertised Content-Length exceeds the bytes the server writes
    // is a transport-level body-read failure, not a status failure. The buffered
    // LLM chat path reads the success body inside the retryable attempt, so the
    // SystemOne client must do the same: the read has to be retried under the
    // shared provider policy, and never turn a recoverable cut body into a
    // terminal failure.
    let healthy = noul_response_body();
    let truncated = healthy.clone();
    let server = MockServer::start(vec![
        MockResponse {
            status: 200,
            body: truncated,
            delay: None,
            advertised_content_length: Some(healthy.len() + 64),
        },
        MockResponse { status: 200, body: healthy, delay: None, advertised_content_length: None },
    ])
    .await;
    let client = test_client(&server, 2, Duration::from_secs(2));

    let response =
        client.evaluate(single_noul_request()).await.expect("a cut body must be retried");
    assert_eq!(response.usage, SystemOneUsage { input_tokens: 12, output_tokens: 3 });
    server.wait_for_requests(2).await;
    assert_eq!(server.request_count(), 2, "a cut 200 body must be retried exactly once");
    server.shutdown().await;
}

#[tokio::test]
async fn try_new_rejects_blank_api_key() {
    let base_url = Url::parse("http://127.0.0.1:9").expect("parse base url");
    for blank in ["", "   ", "\t\n"] {
        let error = test_client_with_api_key(base_url.clone(), blank, 1, Duration::from_millis(10))
            .err()
            .expect("a blank api key must fail closed at construction");
        assert_eq!(
            error.to_string(),
            "typesafe systemone client configuration is invalid: \
             typesafe systemone api key must not be empty or whitespace",
            "a blank api key must be a fixed config error that never echoes the value"
        );
    }
}

#[tokio::test]
async fn try_new_trims_api_key_whitespace() {
    let server = MockServer::start(vec![MockResponse {
        status: 200,
        body: noul_response_body(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let base_url = Url::parse(&server.base_url).expect("parse mock base url");
    let padded = format!("  {DUMMY_API_KEY}\t");
    let client = test_client_with_api_key(base_url, &padded, 3, Duration::from_secs(2))
        .expect("a padded api key is accepted once trimmed");

    client.evaluate(single_noul_request()).await.expect("evaluate succeeds");
    server.wait_for_requests(1).await;
    let requests = server.requests();
    server.shutdown().await;
    let expected = format!("Bearer {DUMMY_API_KEY}");
    assert_eq!(
        requests[0].header("authorization"),
        Some(expected.as_str()),
        "the bearer token must carry the trimmed key, not the raw padded input"
    );
}

#[tokio::test]
async fn evaluate_retries_on_timeout_then_fails() {
    let server = MockServer::start(vec![MockResponse {
        status: 200,
        body: noul_response_body(),
        delay: Some(Duration::from_millis(200)),
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 2, Duration::from_millis(40));

    let error =
        provider_error(client.evaluate(single_noul_request()).await.expect_err("times out"));
    assert!(
        matches!(&error, ProviderCallError::Transport { .. }),
        "a request timeout is a transport error, got {error}"
    );
    assert_eq!(error.redacted_detail(), "transport_timeout");
    // A timed-out attempt is abandoned client-side, so the server may finish
    // reading it with or without a complete request; the retry contract is two
    // attempts, which is exactly two connections.
    server.wait_for_connections(2).await;
    assert_eq!(server.connection_count(), 2, "a timeout retries up to max_attempts");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_rejects_malformed_success_body() {
    let server = MockServer::start(vec![MockResponse {
        status: 200,
        body: "not json".to_string(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let error = client.evaluate(single_noul_request()).await.expect_err("malformed body fails");
    assert!(
        matches!(
            &error,
            SystemOneError::Response {
                source: super::validation::SystemOneResponseError::Decode(_),
                usage: None,
            }
        ),
        "a malformed body must be a decode error, got {error}"
    );
    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "a decode failure is terminal");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_rejects_response_validation_failure() {
    let server = MockServer::start(vec![MockResponse {
        status: 200,
        body: serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {},
            "usage": {"input_tokens": 1, "output_tokens": 1},
        })
        .to_string(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let error = client.evaluate(single_noul_request()).await.expect_err("missing answer fails");
    assert!(
        matches!(
            &error,
            SystemOneError::Response {
                source: super::validation::SystemOneResponseError::MissingAnswer { .. },
                usage: Some(_),
            }
        ),
        "a structurally invalid response must fail validation and keep its usage, got {error}"
    );
    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "a validation failure is terminal");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_rejects_invalid_request_before_any_http() {
    let server = MockServer::start(Vec::new()).await;
    let client = test_client(&server, 3, Duration::from_secs(2));

    let mut request = single_noul_request();
    request.model = String::new();
    let error = client.evaluate(request).await.expect_err("empty model fails");
    assert!(
        matches!(&error, SystemOneError::Request(_)),
        "request validation must run before dispatch, got {error}"
    );
    assert_eq!(server.request_count(), 0, "an invalid request must not reach the network");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_preserves_base_url_path_prefix() {
    let server = MockServer::start(vec![MockResponse {
        status: 200,
        body: noul_response_body(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    let base_url =
        Url::parse(&format!("{}/proxy", server.base_url)).expect("parse prefixed base url");
    let client = test_client_with_base_url(base_url, 3, Duration::from_secs(2), None);

    client.evaluate(single_noul_request()).await.expect("evaluate succeeds");
    server.wait_for_requests(1).await;
    let requests = server.requests();
    server.shutdown().await;
    assert_eq!(requests.len(), 1, "a successful call must send exactly one request");
    assert_eq!(
        requests[0].path, "/proxy/v1/systemone",
        "the configured base URL path prefix must be preserved"
    );
}

#[tokio::test]
async fn evaluate_rejects_credential_base_url_through_shared_policy() {
    // URL policy is owned by the shared provider transport: construction accepts
    // the URL and the first call fails closed on the credential component, so
    // the client does not re-implement the same rule.
    let server = MockServer::start(Vec::new()).await;
    let mut base_url = Url::parse(&server.base_url).expect("parse mock base url");
    base_url.set_username("user").expect("set username");
    base_url.set_password(Some("pass")).expect("set password");
    let client = test_client_with_base_url(base_url, 3, Duration::from_secs(2), None);

    let error = provider_error(
        client.evaluate(single_noul_request()).await.expect_err("credentials must fail"),
    );
    assert!(
        matches!(&error, ProviderCallError::Protocol { .. }),
        "a credential base URL must fail provider URL policy, got {error}"
    );
    assert_eq!(server.request_count(), 0, "a rejected base URL must not reach the network");
    server.shutdown().await;
    assert_no_secret(&error.to_string());
    assert_no_secret(&error.redacted_detail());
}

#[tokio::test]
async fn evaluate_acquires_and_releases_budgeted_permit() {
    let server = MockServer::start(vec![MockResponse {
        status: 200,
        body: noul_response_body(),
        delay: None,
        advertised_content_length: None,
    }])
    .await;
    // max_outbound = 1: the second call can proceed only if the first call
    // released its permit when `evaluate` returned.
    let registry = Arc::new(
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: 1,
                query_reserved: 0,
                requests_per_second: 0,
                burst: 0,
            },
            ProviderBudgetRegistryOptions::default(),
        )
        .expect("build budget registry"),
    );
    let identity = ProviderIdentity::new("typesafe-test", server.base_url.clone());
    let base_url = Url::parse(&server.base_url).expect("parse mock base url");
    let client = test_client_with_base_url(
        base_url,
        3,
        Duration::from_secs(2),
        Some(SystemOneBudget { registry, identity }),
    );

    client.evaluate(single_noul_request()).await.expect("first budgeted call succeeds");
    client.evaluate(single_noul_request()).await.expect("permit released for the second call");
    server.wait_for_requests(2).await;
    assert_eq!(server.request_count(), 2, "both budgeted calls must reach the endpoint");
    server.shutdown().await;
}

#[tokio::test]
async fn evaluate_releases_budgeted_permit_when_the_execution_deadline_cancels_the_attempt() {
    // The mock holds each response past the execution deadline, so the first
    // call is cancelled by the client's wall-clock timeout while its attempt
    // still holds the single budget permit. With max_outbound = 1, the second
    // call can reach the endpoint only if that cancellation dropped the
    // in-flight attempt's permit. A short acquire_timeout turns a leaked permit
    // into a fast typed refusal instead of a 30-second wait.
    let server = MockServer::start(vec![MockResponse {
        status: 200,
        body: noul_response_body(),
        delay: Some(Duration::from_millis(150)),
        advertised_content_length: None,
    }])
    .await;
    let registry = Arc::new(
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: 1,
                query_reserved: 0,
                requests_per_second: 0,
                burst: 0,
            },
            ProviderBudgetRegistryOptions {
                acquire_timeout: Duration::from_millis(200),
                ..ProviderBudgetRegistryOptions::default()
            },
        )
        .expect("build budget registry"),
    );
    let identity = ProviderIdentity::new("typesafe-timeout", server.base_url.clone());
    let base_url = Url::parse(&server.base_url).expect("parse mock base url");

    let budgeted_client = |execution_deadline: Duration| {
        SystemOneClient::try_new(SystemOneClientConfig {
            base_url: base_url.clone(),
            api_key: Zeroizing::new(DUMMY_API_KEY.to_string()),
            request_timeout: Duration::from_secs(2),
            allow_private_network: true,
            max_attempts: 1,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
            execution_deadline,
            budget: Some(SystemOneBudget {
                registry: Arc::clone(&registry),
                identity: identity.clone(),
            }),
        })
        .expect("build budgeted systemone client")
    };

    let cancelled = provider_error(
        budgeted_client(Duration::from_millis(40))
            .evaluate(single_noul_request())
            .await
            .expect_err("the execution deadline cancels the delayed attempt"),
    );
    assert!(
        matches!(&cancelled, ProviderCallError::Timeout { .. }),
        "a cancelled attempt must surface as a typed timeout error, got {cancelled}"
    );

    budgeted_client(Duration::from_secs(5))
        .evaluate(single_noul_request())
        .await
        .expect("the cancelled attempt must release its budget permit for the next call");

    server.wait_for_requests(2).await;
    assert_eq!(server.request_count(), 2, "both budgeted calls must reach the endpoint");
    server.shutdown().await;
    assert_no_secret(&cancelled.to_string());
    assert_no_secret(&cancelled.redacted_detail());
}

#[tokio::test]
async fn mock_server_ignores_connection_without_http_request() {
    // A readiness probe, preconnect or reset that reaches the listener without a
    // complete HTTP request must not be counted as a decision request, or the
    // "exactly one request" assertions on terminal statuses become load-flaky.
    let server = MockServer::start(Vec::new()).await;
    let address = server.base_url.trim_start_matches("http://").to_string();
    let mut socket = TcpStream::connect(address).await.expect("connect to mock server");
    socket.shutdown().await.expect("close without sending");

    server.wait_for_connections(1).await;
    assert_eq!(server.request_count(), 0, "a request-less connection is not a decision request");
    server.shutdown().await;
}

#[test]
fn target_preparation_dns_failures_are_upstream_typed_and_outer_retryable() {
    // DNS resolution failures are transient upstream failures. Each must map to
    // an upstream-shaped error carrying a retryable resolution source, so
    // operator failure classification reports an upstream class instead of
    // InternalRequestInvalid and the outer graph-extraction recovery schedules a
    // bounded whole-call retry.
    //
    // The local `with_retry` loop inside `SystemOneClient::evaluate` does NOT
    // retry this: `prepare` runs once before that loop, and its classifier
    // aborts a resolution ResponsePolicy. The retry a DNS failure actually
    // reaches is the outer, whole-call one, which
    // `target_preparation_dns_failures_are_not_retried_by_the_local_policy` pins.
    let service = ProviderFailureClassificationService::default();
    for (error, source) in [
        (ProviderHttpError::ResolveTimedOut, PublicHttpUrlError::ResolveTimedOut),
        (ProviderHttpError::ResolveFailed, PublicHttpUrlError::ResolveFailed),
        (ProviderHttpError::NoAddresses, PublicHttpUrlError::NoAddresses),
    ] {
        let mapped = ProviderCallError::target_preparation("systemone provider target", &error);
        let actual = match &mapped {
            ProviderCallError::ResponsePolicy { source, .. } => source,
            other => panic!("a DNS failure must map to an upstream-shaped error, got {other}"),
        };
        assert_eq!(actual, &source, "DNS failure must keep its typed source: {error}");
        assert!(actual.is_retryable_resolution_failure());
        assert_eq!(
            service.classify_provider_call_error(&mapped),
            Some(expected_dns_failure_class(&source)),
            "operator classification for {error}"
        );
        assert!(
            service.is_transient_retryable_error(&anyhow::Error::new(mapped)),
            "the outer classifier must see a DNS failure as transient-retryable: {error}"
        );
    }
}

#[tokio::test]
async fn target_preparation_dns_failures_are_not_retried_by_the_local_policy() {
    // `prepare` runs once before `with_retry`, and the local
    // `classify_provider_call_error` aborts a resolution ResponsePolicy — only a
    // `BodyReadFailed` response policy is retried there. The local policy must
    // therefore run its closure exactly once for a target-preparation DNS
    // failure; the retry it reaches is the outer, whole-call one.
    for error in [
        ProviderHttpError::ResolveTimedOut,
        ProviderHttpError::ResolveFailed,
        ProviderHttpError::NoAddresses,
    ] {
        let attempts = std::cell::Cell::new(0_u32);
        let local = with_retry(
            || {
                attempts.set(attempts.get() + 1);
                async {
                    Err::<(), _>(ProviderCallError::target_preparation(
                        "systemone provider target",
                        &error,
                    ))
                }
            },
            RetryPolicy::idempotent_provider_call(3, Duration::ZERO, Duration::ZERO),
        )
        .await;
        assert!(local.is_err(), "the local with_retry must return the DNS failure: {error}");
        assert_eq!(
            attempts.get(),
            1,
            "the local with_retry must not retry a target-preparation DNS failure: {error}"
        );
    }
}

#[test]
fn target_preparation_policy_rejections_stay_protocol_errors() {
    // URL, address and origin policy rejections describe a caller-supplied
    // target that no retry can fix, so they stay terminal protocol errors.
    let service = ProviderFailureClassificationService::default();
    for error in [
        ProviderHttpError::ForbiddenScheme,
        ProviderHttpError::CredentialsForbidden,
        ProviderHttpError::QueryOrFragmentForbidden,
        ProviderHttpError::MissingHost,
        ProviderHttpError::MissingPort,
        ProviderHttpError::NonPublicAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        ProviderHttpError::OriginMismatch,
    ] {
        let mapped = ProviderCallError::target_preparation("systemone provider target", &error);
        assert!(
            matches!(&mapped, ProviderCallError::Protocol { .. }),
            "a policy rejection must stay a protocol error: {error}"
        );
        assert_eq!(
            service.classify_provider_call_error(&mapped),
            Some(RuntimeProviderFailureClass::InternalRequestInvalid),
            "a policy rejection must classify as an internal request error: {error}"
        );
        assert!(
            !service.is_transient_retryable_error(&anyhow::Error::new(mapped)),
            "a policy rejection must not be retryable: {error}"
        );
    }
}

fn expected_dns_failure_class(error: &PublicHttpUrlError) -> RuntimeProviderFailureClass {
    match error {
        PublicHttpUrlError::ResolveTimedOut => RuntimeProviderFailureClass::UpstreamTimeout,
        PublicHttpUrlError::ResolveFailed | PublicHttpUrlError::NoAddresses => {
            RuntimeProviderFailureClass::UpstreamProtocolFailure
        }
        other => panic!("not a DNS resolution failure: {other}"),
    }
}

fn assert_no_secret(rendered: &str) {
    assert!(!rendered.contains(DUMMY_API_KEY), "leaked the api key: {rendered}");
}
