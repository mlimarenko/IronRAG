use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    extract::MatchedPath,
    http::{HeaderName, HeaderValue, Request, Response, StatusCode, header},
    middleware::{self, Next},
};
use tower_http::{classify::ServerErrorsFailureClass, trace::TraceLayer};
use tracing::{Span, error, field, info, warn};

use crate::{app::state::AppState, interfaces::http::router_support, observability::Tracer};

/// Carries the W3C trace id of the request's OpenTelemetry span back to the
/// caller, so a reported error can be joined to its trace and log lines.
pub const TRACE_ID_HEADER: &str = "x-trace-id";

/// Layer order is load-bearing: `inject_request_id` is outermost so the
/// `TraceLayer` span and its start event already carry the real request id;
/// `log_completed_request` is innermost so it runs inside the span (trace id is
/// current) and still sees the matched route from the request extensions.
pub fn apply(router: Router<AppState>) -> Router<AppState> {
    router
        .layer(middleware::from_fn(log_completed_request))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let span = tracing::info_span!(
                        "http_request",
                        otel.kind = "server",
                        method = %request.method(),
                        matched_path = matched_path_of(request),
                        uri = %request.uri().path(),
                        request_id = request_id_of(request),
                        error.kind = field::Empty,
                    );
                    Tracer::set_span_parent_from_headers(&span, request.headers());
                    span
                })
                .on_request(|request: &Request<_>, _span: &Span| {
                    let user_agent = request
                        .headers()
                        .get(header::USER_AGENT)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("-");
                    info!(
                        method = %request.method(),
                        matched_path = matched_path_of(request),
                        uri = %request.uri().path(),
                        user_agent,
                        request_id = request_id_of(request),
                        "http request started",
                    );
                })
                .on_failure(
                    |failure_class: ServerErrorsFailureClass, latency: Duration, _span: &Span| {
                        error!(
                            %failure_class,
                            latency_ms = latency.as_millis(),
                            "http request failed before response",
                        );
                    },
                ),
        )
        .layer(middleware::map_request(inject_request_id))
}

fn matched_path_of<B>(request: &Request<B>) -> String {
    request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "<unmatched>".to_string(), |path| path.as_str().to_string())
}

fn request_id_of<B>(request: &Request<B>) -> String {
    request
        .extensions()
        .get::<router_support::RequestId>()
        .map_or_else(|| "-".to_string(), |request_id| request_id.0.clone())
}

/// Logs the completed response and copies the correlation headers onto it.
/// Runs inside the `http_request` span, so the event also carries the trace id.
async fn log_completed_request(request: Request<Body>, next: Next) -> Response<Body> {
    let started = Instant::now();
    let request_id = request_id_of(&request);
    let matched_path = matched_path_of(&request);
    let mut response = next.run(request).await;
    attach_response_headers(response.headers_mut(), &request_id);
    log_completed_response(
        response.status(),
        started.elapsed().as_millis(),
        &request_id,
        &matched_path,
    );
    response
}

/// Writes the request id and the current trace id onto the response. The
/// request id was attached to the request headers by `inject_request_id`;
/// a response is a separate object, so it must be copied explicitly.
fn attach_response_headers(headers: &mut axum::http::HeaderMap, request_id: &str) {
    router_support::attach_request_id_header(headers, request_id);
    if let Some(trace_id) = crate::observability::current_trace_id()
        && let Ok(value) = HeaderValue::from_str(&trace_id)
    {
        headers.insert(HeaderName::from_static(TRACE_ID_HEADER), value);
    }
}

/// Routes the completed-request log line to its level. Kept as separate
/// single-macro helpers because a tracing macro's expansion is itself counted
/// as cognitive complexity: one macro per leaf keeps every function below the
/// threshold while preserving the exact level, fields and message.
fn log_completed_response(
    status: StatusCode,
    latency_ms: u128,
    request_id: &str,
    matched_path: &str,
) {
    if status.is_server_error() {
        log_response_server_error(status, latency_ms, request_id, matched_path);
    } else if status.is_client_error() {
        log_response_client_error(status, latency_ms, request_id, matched_path);
    } else {
        log_response_success(status, latency_ms, request_id, matched_path);
    }
}

fn log_response_server_error(
    status: StatusCode,
    latency_ms: u128,
    request_id: &str,
    matched_path: &str,
) {
    // The status lives in the fingerprint kind, not the message: message
    // normalisation folds numbers, and a 500 must not group with a 502.
    let fingerprint = crate::observability::error_tracking::fingerprint(
        &format!("http.server_error.{}", status.as_u16()),
        matched_path,
    );
    error!(
        %status,
        latency_ms,
        request_id,
        matched_path,
        "error.type" = "http.server_error",
        "error.fingerprint" = %fingerprint,
        "http request completed with server error",
    );
}

fn log_response_client_error(
    status: StatusCode,
    latency_ms: u128,
    request_id: &str,
    matched_path: &str,
) {
    warn!(%status, latency_ms, request_id, matched_path, "http request completed with client error");
}

fn log_response_success(
    status: StatusCode,
    latency_ms: u128,
    request_id: &str,
    matched_path: &str,
) {
    info!(%status, latency_ms, request_id, matched_path, "http request completed");
}

async fn inject_request_id(mut request: Request<Body>) -> Request<Body> {
    let request_id = router_support::ensure_or_generate_request_id(request.headers());
    router_support::attach_request_id_header(request.headers_mut(), &request_id);
    request.extensions_mut().insert(router_support::RequestId(request_id));
    request
}
