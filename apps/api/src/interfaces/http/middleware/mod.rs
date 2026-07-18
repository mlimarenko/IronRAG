//! Canonical HTTP middleware stack.
//!
//! The request path is ordered outer-most first:
//! tracing -> MCP Origin guard -> CORS -> compression -> auth -> handler.
//!
//! Prometheus metrics, request body limits, and request-id propagation are part of this
//! same canonical stack; they are kept here so router construction has one middleware path.
//! The concrete stack places body-limit and Prometheus inside compression, before auth.

pub mod auth;
pub mod compression;
pub mod cors;
mod mcp_origin;
pub mod prometheus;
pub mod tracing_layer;

use axum::{Router, extract::DefaultBodyLimit};

use crate::app::{
    config::{McpHttpOriginPolicy, PublicOriginSettings},
    state::AppState,
};

fn apply_browser_transport_security<S>(
    router: Router<S>,
    public_origins: &PublicOriginSettings,
    mcp_origin_policy: McpHttpOriginPolicy,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let router = cors::apply(router, public_origins);
    // Origin remains outside CORS so untrusted browser traffic receives the
    // transport-mandated bare 403 before CORS, credentials, or sessions.
    mcp_origin::apply(router, mcp_origin_policy)
}

pub fn apply_canonical_middleware(router: Router<AppState>, state: AppState) -> Router {
    let public_origin_settings = state.settings.public_origin_settings();
    let max_request_body_bytes = state.mcp_memory.max_request_body_bytes();

    let router = auth::apply(router, state.clone());
    let router = prometheus::apply(router);
    let router = router.layer(DefaultBodyLimit::max(max_request_body_bytes));
    let router = compression::apply(router);
    let router = apply_browser_transport_security(
        router,
        &public_origin_settings,
        state.mcp_http_origin_policy.clone(),
    );
    let router = tracing_layer::apply(router);

    router.with_state(state)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use axum::{
        Router,
        body::Body,
        http::{HeaderName, HeaderValue, Method, Request, StatusCode, header},
        response::{IntoResponse as _, Response},
        routing::post,
    };
    use tower::ServiceExt as _;

    use crate::{
        app::config::{McpHttpOriginPolicy, PublicOriginSettings},
        interfaces::http::{
            mcp::{
                MCP_PROTOCOL_HEADER, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE,
                MCP_PUBLIC_JSONRPC_ROUTE, MCP_SESSION_HEADER,
            },
            router_support::REQUEST_ID_HEADER,
        },
    };

    use super::apply_browser_transport_security;

    const CONFIGURED_BROWSER_ORIGIN: &str = "https://console.example";
    const GENERAL_ROUTE: &str = "/v1/health";
    const SYNTHETIC_SESSION_ID: &str = "browser-session-contract";

    fn public_origins() -> PublicOriginSettings {
        PublicOriginSettings {
            raw_frontend_origin: CONFIGURED_BROWSER_ORIGIN.to_string(),
            allowed_origins: vec![CONFIGURED_BROWSER_ORIGIN.to_string()],
            session_cookie_secure: true,
        }
    }

    fn origin_policy() -> McpHttpOriginPolicy {
        McpHttpOriginPolicy::try_from_origins([CONFIGURED_BROWSER_ORIGIN], None)
            .expect("synthetic browser origin must be canonical")
    }

    async fn initialize_response() -> Response {
        let mut response = StatusCode::OK.into_response();
        response.headers_mut().insert(
            HeaderName::from_static(MCP_SESSION_HEADER),
            HeaderValue::from_static(SYNTHETIC_SESSION_ID),
        );
        response
    }

    fn browser_transport_router() -> Router {
        let router = Router::new()
            .route(MCP_PUBLIC_JSONRPC_ROUTE, post(initialize_response))
            .route(MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE, post(initialize_response))
            .route(GENERAL_ROUTE, post(initialize_response));
        apply_browser_transport_security(router, &public_origins(), origin_policy())
    }

    fn preflight_request(path: &str, origin: &str) -> Request<Body> {
        Request::builder()
            .method(Method::OPTIONS)
            .uri(path)
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, Method::POST.as_str())
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                [
                    header::ACCEPT.as_str(),
                    header::AUTHORIZATION.as_str(),
                    header::CONTENT_TYPE.as_str(),
                    "traceparent",
                    "tracestate",
                    REQUEST_ID_HEADER,
                    MCP_PROTOCOL_HEADER,
                    MCP_SESSION_HEADER,
                ]
                .join(","),
            )
            .body(Body::empty())
            .expect("synthetic preflight request must be valid")
    }

    fn response_header_tokens(response: &Response, name: HeaderName) -> BTreeSet<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .into_iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .collect()
    }

    #[tokio::test]
    async fn configured_browser_preflight_accepts_canonical_and_mcp_session_headers() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            let response = browser_transport_router()
                .oneshot(preflight_request(path, CONFIGURED_BROWSER_ORIGIN))
                .await
                .expect("browser preflight must complete");

            assert!(response.status().is_success());
            assert_eq!(
                response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
                Some(&HeaderValue::from_static(CONFIGURED_BROWSER_ORIGIN))
            );
            let allowed = response_header_tokens(&response, header::ACCESS_CONTROL_ALLOW_HEADERS);
            for required in [
                header::ACCEPT.as_str(),
                header::AUTHORIZATION.as_str(),
                header::CONTENT_TYPE.as_str(),
                "traceparent",
                "tracestate",
                REQUEST_ID_HEADER,
                MCP_PROTOCOL_HEADER,
                MCP_SESSION_HEADER,
            ] {
                assert!(allowed.contains(required), "missing allowed request header {required}");
            }
        }
    }

    #[tokio::test]
    async fn initialize_session_header_is_exposed_to_configured_browsers() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            let request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .header(header::ORIGIN, CONFIGURED_BROWSER_ORIGIN)
                .body(Body::empty())
                .expect("synthetic initialize request must be valid");
            let response = browser_transport_router()
                .oneshot(request)
                .await
                .expect("browser initialize request must complete");

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get(MCP_SESSION_HEADER),
                Some(&HeaderValue::from_static(SYNTHETIC_SESSION_ID))
            );
            assert_eq!(
                response_header_tokens(&response, header::ACCESS_CONTROL_EXPOSE_HEADERS),
                BTreeSet::from([MCP_SESSION_HEADER.to_string()])
            );
        }
    }

    #[tokio::test]
    async fn untrusted_browser_preflight_is_rejected_before_cors() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            let response = browser_transport_router()
                .oneshot(preflight_request(path, "https://untrusted.example"))
                .await
                .expect("browser preflight must complete");

            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert!(response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
        }
    }

    #[tokio::test]
    async fn configured_browser_preflight_remains_available_for_non_mcp_routes() {
        let response = browser_transport_router()
            .oneshot(preflight_request(GENERAL_ROUTE, CONFIGURED_BROWSER_ORIGIN))
            .await
            .expect("general browser preflight must complete");

        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static(CONFIGURED_BROWSER_ORIGIN))
        );
    }
}
