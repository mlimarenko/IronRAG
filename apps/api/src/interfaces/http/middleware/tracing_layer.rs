use std::time::Duration;

use ::http::Response;
use axum::{
    Router,
    body::Body,
    extract::MatchedPath,
    http::{Request, header},
    middleware,
};
use tower_http::{classify::ServerErrorsFailureClass, trace::TraceLayer};
use tracing::{Span, error, field, info, warn};

use crate::{app::state::AppState, interfaces::http::router_support, observability::Tracer};

pub fn apply(router: Router<AppState>) -> Router<AppState> {
    router
        .layer(middleware::map_request(inject_request_id))
        .layer(middleware::map_response(propagate_request_id))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let matched_path = request.extensions().get::<MatchedPath>().map_or_else(
                        || "<unmatched>".to_string(),
                        |path| path.as_str().to_string(),
                    );
                    let request_id = request
                        .extensions()
                        .get::<router_support::RequestId>()
                        .map_or_else(|| "-".to_string(), |request_id| request_id.0.clone());
                    let span = tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        matched_path,
                        uri = %request.uri(),
                        request_id,
                        error.kind = field::Empty,
                    );
                    Tracer::set_span_parent_from_headers(&span, request.headers());
                    span
                })
                .on_request(|request: &Request<_>, _span: &Span| {
                    let matched_path = request.extensions().get::<MatchedPath>().map_or_else(
                        || "<unmatched>".to_string(),
                        |path| path.as_str().to_string(),
                    );
                    let user_agent = request
                        .headers()
                        .get(header::USER_AGENT)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("-");
                    let request_id = request
                        .extensions()
                        .get::<router_support::RequestId>()
                        .map_or("-", |request_id| request_id.0.as_str());
                    info!(
                        method = %request.method(),
                        matched_path,
                        uri = %request.uri(),
                        user_agent,
                        request_id,
                        "http request started",
                    );
                })
                .on_response(|response: &Response<_>, latency: Duration, _span: &Span| {
                    let latency_ms = latency.as_millis();
                    let status = response.status();
                    let request_id = response
                        .headers()
                        .get(router_support::REQUEST_ID_HEADER)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("-");
                    if status.is_server_error() {
                        error!(%status, latency_ms, request_id, "http request completed with server error");
                    } else if status.is_client_error() {
                        warn!(%status, latency_ms, request_id, "http request completed with client error");
                    } else {
                        info!(%status, latency_ms, request_id, "http request completed");
                    }
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
}

async fn inject_request_id(mut request: Request<Body>) -> Request<Body> {
    let request_id = router_support::ensure_or_generate_request_id(request.headers());
    router_support::attach_request_id_header(request.headers_mut(), &request_id);
    request.extensions_mut().insert(router_support::RequestId(request_id));
    request
}

async fn propagate_request_id(response: Response<Body>) -> Response<Body> {
    response
}
