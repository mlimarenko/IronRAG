use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, header},
    routing::get,
};

use crate::app::state::AppState;

const OPENAPI_SPEC: &str = include_str!("../../../contracts/rustrag.openapi.yaml");
/// OpenAPI `paths` in the contract are absolute from the host (`/v1/...`). The server URL must be
/// the API origin **without** a `/v1` suffix, otherwise Swagger UI concatenates `/v1` + `/v1/...`.
const RELATIVE_SERVER_URL: &str = "/";
const CONFIGURED_SERVER_DESCRIPTION: &str = "Public API origin";
const RELATIVE_SERVER_DESCRIPTION: &str = "Same origin (paths include /v1)";

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new().route("/openapi/rustrag.openapi.yaml", get(get_openapi_spec))
}

async fn get_openapi_spec(State(state): State<AppState>) -> (HeaderMap, String) {
    let mut response_headers = HeaderMap::new();
    response_headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/yaml; charset=utf-8"));
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, max-age=0"));
    (
        response_headers,
        render_openapi_spec(OPENAPI_SPEC, state.settings.openapi_public_origin.as_deref()),
    )
}

fn render_openapi_spec(spec: &str, openapi_public_origin: Option<&str>) -> String {
    let (url, description) = match trimmed_non_empty(openapi_public_origin) {
        Some(origin) => (public_origin_to_server_url(origin), CONFIGURED_SERVER_DESCRIPTION),
        None => (RELATIVE_SERVER_URL.to_string(), RELATIVE_SERVER_DESCRIPTION),
    };
    let servers_block = format!("servers:\n  - url: {url}\n    description: {description}\n");
    replace_servers_block(spec, &servers_block)
}

fn trimmed_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|chunk| !chunk.is_empty())
}

fn public_origin_to_server_url(origin: &str) -> String {
    let base = origin.trim().trim_end_matches('/');
    if base.is_empty() {
        return RELATIVE_SERVER_URL.to_string();
    }
    // Paths in `rustrag.openapi.yaml` already start with `/v1/`; strip a redundant `/v1` suffix.
    if base.ends_with("/v1") {
        let trimmed = base[..base.len() - "/v1".len()].trim_end_matches('/');
        if trimmed.is_empty() { RELATIVE_SERVER_URL.to_string() } else { trimmed.to_string() }
    } else {
        base.to_string()
    }
}

fn replace_servers_block(spec: &str, servers_block: &str) -> String {
    let Some(servers_start) = spec.find("servers:\n") else {
        return spec.to_string();
    };
    let Some(security_start) = spec.find("\nsecurity:\n") else {
        return spec.to_string();
    };
    if servers_start >= security_start {
        return spec.to_string();
    }

    let mut rendered = String::with_capacity(spec.len() + servers_block.len());
    rendered.push_str(&spec[..servers_start]);
    rendered.push_str(servers_block);
    rendered.push_str(&spec[security_start + 1..]);
    rendered
}

#[cfg(test)]
mod tests {
    use super::render_openapi_spec;

    const SPEC_WITH_PLACEHOLDER_SERVER: &str = "openapi: 3.1.0\nservers:\n  - url: http://localhost:8095\n    description: Local default\nsecurity:\n  - bearerAuth: []\n";

    #[test]
    fn uses_configured_public_origin_as_single_server() {
        let rendered =
            render_openapi_spec(SPEC_WITH_PLACEHOLDER_SERVER, Some("https://api.example.com"));

        assert!(rendered.contains("url: https://api.example.com"));
        assert!(rendered.contains("description: Public API origin"));
        assert!(!rendered.contains("http://localhost:8095"));
        assert_eq!(rendered.matches("  - url:").count(), 1);
    }

    #[test]
    fn configured_origin_strips_redundant_trailing_v1() {
        let rendered =
            render_openapi_spec(SPEC_WITH_PLACEHOLDER_SERVER, Some("https://api.example.com/v1/"));

        assert!(rendered.contains("url: https://api.example.com"));
        assert_eq!(rendered.matches("  - url:").count(), 1);
    }

    #[test]
    fn configured_origin_strips_v1_from_host_with_port() {
        let rendered =
            render_openapi_spec(SPEC_WITH_PLACEHOLDER_SERVER, Some("http://127.0.0.1:8000/v1"));

        assert!(rendered.contains("url: http://127.0.0.1:8000"));
        assert_eq!(rendered.matches("  - url:").count(), 1);
    }

    #[test]
    fn falls_back_to_relative_api_root_when_origin_is_unset() {
        let rendered = render_openapi_spec(SPEC_WITH_PLACEHOLDER_SERVER, None);

        assert!(rendered.contains("url: /"));
        assert!(rendered.contains("description: Same origin (paths include /v1)"));
        assert_eq!(rendered.matches("  - url:").count(), 1);
    }

    #[test]
    fn falls_back_to_relative_when_origin_is_blank() {
        let rendered = render_openapi_spec(SPEC_WITH_PLACEHOLDER_SERVER, Some("   "));

        assert!(rendered.contains("url: /"));
        assert_eq!(rendered.matches("  - url:").count(), 1);
    }
}
