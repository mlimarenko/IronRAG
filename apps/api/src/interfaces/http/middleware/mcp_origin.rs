use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};

use crate::{
    app::config::McpHttpOriginPolicy,
    interfaces::http::mcp::{MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE, MCP_PUBLIC_JSONRPC_ROUTE},
};

pub(super) fn apply<S>(router: Router<S>, policy: McpHttpOriginPolicy) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(middleware::from_fn_with_state(policy, enforce_origin))
}

async fn enforce_origin(
    State(policy): State<McpHttpOriginPolicy>,
    request: Request,
    next: Next,
) -> Response {
    if !is_mcp_transport_path(request.uri().path()) {
        return next.run(request).await;
    }

    let mut origins = request.headers().get_all(header::ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return next.run(request).await;
    };
    if origins.next().is_some() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(origin) = origin.to_str() else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if !policy.allows(origin) {
        return StatusCode::FORBIDDEN.into_response();
    }

    next.run(request).await
}

fn is_mcp_transport_path(path: &str) -> bool {
    matches!(path, MCP_PUBLIC_JSONRPC_ROUTE | MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE)
}

#[cfg(test)]
mod tests {
    use axum::{
        Json, Router,
        body::Body,
        http::{HeaderValue, Method, Request, StatusCode, header},
        middleware,
        response::{IntoResponse as _, Response},
        routing::{any, post},
    };
    use serde_json::Value;
    use tower::ServiceExt as _;

    use crate::{
        app::config::McpHttpOriginPolicy,
        interfaces::http::mcp::{MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE, MCP_PUBLIC_JSONRPC_ROUTE},
    };

    use super::apply;

    const CONFIGURED_ORIGIN: &str = "https://console.example";
    const CANONICAL_PUBLIC_ORIGIN: &str = "https://api.example";

    fn policy() -> McpHttpOriginPolicy {
        McpHttpOriginPolicy::try_from_origins([CONFIGURED_ORIGIN], Some(CANONICAL_PUBLIC_ORIGIN))
            .expect("synthetic origin policy must be valid")
    }

    async fn reached() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    async fn parsed(Json(_payload): Json<Value>) -> StatusCode {
        StatusCode::NO_CONTENT
    }

    fn transport_router() -> Router {
        apply(
            Router::new()
                .route(MCP_PUBLIC_JSONRPC_ROUTE, any(reached))
                .route(MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE, any(reached))
                .route("/v1/health", any(reached)),
            policy(),
        )
    }

    fn parsing_transport_router() -> Router {
        apply(
            Router::new()
                .route(MCP_PUBLIC_JSONRPC_ROUTE, post(parsed))
                .route(MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE, post(parsed)),
            policy(),
        )
    }

    fn request_with_method(
        method: Method,
        path: &str,
        origins: &[&str],
        body: &'static str,
    ) -> Request<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("synthetic request must be valid");
        for origin in origins {
            request.headers_mut().append(
                header::ORIGIN,
                origin.parse().expect("synthetic origin header must parse"),
            );
        }
        request
    }

    fn post_request(path: &str, origins: &[&str], body: &'static str) -> Request<Body> {
        request_with_method(Method::POST, path, origins, body)
    }

    #[tokio::test]
    async fn absent_origin_is_allowed_for_native_clients_on_both_surfaces() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            let response = transport_router()
                .oneshot(post_request(path, &[], "{}"))
                .await
                .expect("transport request must complete");
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }
    }

    #[tokio::test]
    async fn exact_configured_and_canonical_public_origins_are_allowed_on_both_surfaces() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            for origin in [CONFIGURED_ORIGIN, CANONICAL_PUBLIC_ORIGIN] {
                let response = transport_router()
                    .oneshot(post_request(path, &[origin], "{}"))
                    .await
                    .expect("transport request must complete");
                assert_eq!(response.status(), StatusCode::NO_CONTENT);
            }
        }
    }

    #[tokio::test]
    async fn untrusted_origin_is_rejected_before_initialize_or_subsequent_json_parsing() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            let response = parsing_transport_router()
                .oneshot(post_request(path, &["https://untrusted.example"], "not-json"))
                .await
                .expect("transport request must complete");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn untrusted_origin_is_rejected_for_every_streamable_http_method() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            for method in [Method::POST, Method::GET, Method::DELETE, Method::OPTIONS] {
                let response = transport_router()
                    .oneshot(request_with_method(
                        method,
                        path,
                        &["https://untrusted.example"],
                        "{}",
                    ))
                    .await
                    .expect("transport request must complete");
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
        }
    }

    #[tokio::test]
    async fn duplicate_origin_headers_are_rejected_on_both_surfaces() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            let response = transport_router()
                .oneshot(post_request(path, &[CONFIGURED_ORIGIN, CONFIGURED_ORIGIN], "{}"))
                .await
                .expect("transport request must complete");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn malformed_or_noncanonical_origins_are_rejected_on_both_surfaces() {
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            for origin in ["not-an-origin", "https://console.example/", "null"] {
                let response = transport_router()
                    .oneshot(post_request(path, &[origin], "{}"))
                    .await
                    .expect("transport request must complete");
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
        }
    }

    #[tokio::test]
    async fn non_utf8_origin_is_rejected_on_both_surfaces() {
        let malformed = HeaderValue::from_bytes(&[0xff]).expect("opaque header byte must parse");
        for path in [MCP_PUBLIC_JSONRPC_ROUTE, MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE] {
            let mut request = post_request(path, &[], "{}");
            request.headers_mut().append(header::ORIGIN, malformed.clone());
            let response =
                transport_router().oneshot(request).await.expect("transport request must complete");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }

    async fn reject_as_unauthenticated(
        _request: Request<Body>,
        _next: middleware::Next,
    ) -> Response {
        StatusCode::UNAUTHORIZED.into_response()
    }

    #[tokio::test]
    async fn origin_guard_runs_before_inner_authentication() {
        let router = Router::new()
            .route(MCP_PUBLIC_JSONRPC_ROUTE, post(reached))
            .layer(middleware::from_fn(reject_as_unauthenticated));
        let router = apply(router, policy());

        let response = router
            .oneshot(post_request(MCP_PUBLIC_JSONRPC_ROUTE, &["https://untrusted.example"], "{}"))
            .await
            .expect("transport request must complete");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn non_mcp_routes_are_not_restricted_by_the_mcp_origin_policy() {
        let response = transport_router()
            .oneshot(post_request("/v1/health", &["https://untrusted.example"], "{}"))
            .await
            .expect("transport request must complete");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
