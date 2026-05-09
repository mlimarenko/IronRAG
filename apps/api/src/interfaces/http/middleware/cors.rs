use axum::{
    Router,
    http::{Method, header},
};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::{app::config::PublicOriginSettings, app::state::AppState};

pub fn apply(router: Router<AppState>, origins: &PublicOriginSettings) -> Router<AppState> {
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
                header::HeaderName::from_static(
                    crate::interfaces::http::router_support::REQUEST_ID_HEADER,
                ),
            ]),
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
