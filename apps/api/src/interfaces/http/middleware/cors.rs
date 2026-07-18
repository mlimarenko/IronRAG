use axum::{
    Router,
    http::{Method, header},
};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::{
    app::config::PublicOriginSettings,
    interfaces::http::{
        mcp::{MCP_PROTOCOL_HEADER, MCP_SESSION_HEADER},
        router_support::REQUEST_ID_HEADER,
    },
};

pub fn apply<S>(router: Router<S>, origins: &PublicOriginSettings) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(
        CorsLayer::new()
            .allow_origin(parse_allowed_origins(origins))
            .allow_credentials(true)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                header::ACCEPT,
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
                header::HeaderName::from_static("traceparent"),
                header::HeaderName::from_static("tracestate"),
                header::HeaderName::from_static(REQUEST_ID_HEADER),
                header::HeaderName::from_static(MCP_PROTOCOL_HEADER),
                header::HeaderName::from_static(MCP_SESSION_HEADER),
            ])
            .expose_headers([header::HeaderName::from_static(MCP_SESSION_HEADER)]),
    )
}

fn parse_allowed_origins(origins: &PublicOriginSettings) -> AllowOrigin {
    let parsed_origins = origins
        .allowed_origins
        .iter()
        .filter_map(|value| value.parse().ok())
        .collect::<Vec<header::HeaderValue>>();

    if parsed_origins.is_empty() {
        return AllowOrigin::list([
            header::HeaderValue::from_static("http://127.0.0.1:19000"),
            header::HeaderValue::from_static("http://localhost:19000"),
        ]);
    }

    AllowOrigin::list(parsed_origins)
}
