use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, header},
    routing::get,
};

use crate::{app::state::AppState, openapi::ApiDoc};
use utoipa::OpenApi;
const RELATIVE_SERVER_URL: &str = "/";
const CONFIGURED_SERVER_DESCRIPTION: &str = "Public API origin";
const RELATIVE_SERVER_DESCRIPTION: &str = "Same origin (paths include /v1)";

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new().route("/openapi/ironrag.openapi.yaml", get(get_openapi_spec))
}

#[utoipa::path(
    get,
    path = "/v1/openapi/ironrag.openapi.yaml",
    tag = "system",
    operation_id = "getOpenApiContract",
    security(),
    responses(
        (status = 200, description = "OpenAPI 3.1 contract for the public IronRAG HTTP API", content_type = "application/yaml", body = String),
    ),
)]
pub async fn get_openapi_spec(State(state): State<AppState>) -> (HeaderMap, String) {
    let mut response_headers = HeaderMap::new();
    response_headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/yaml; charset=utf-8"));
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, max-age=0"));
    let spec = ApiDoc::openapi()
        .to_yaml()
        .unwrap_or_else(|e| format!("error: failed to serialize OpenAPI spec: {e}"));

    (response_headers, render_openapi_spec(&spec, state.settings.openapi_public_origin.as_deref()))
}

fn render_openapi_spec(spec: &str, openapi_public_origin: Option<&str>) -> String {
    let (url, description) = trimmed_non_empty(openapi_public_origin).map_or_else(
        || (RELATIVE_SERVER_URL.to_string(), RELATIVE_SERVER_DESCRIPTION),
        |origin| (public_origin_to_server_url(origin), CONFIGURED_SERVER_DESCRIPTION),
    );
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
    base.strip_suffix("/v1").map_or_else(
        || base.to_string(),
        |stripped| {
            let trimmed = stripped.trim_end_matches('/');
            if trimmed.is_empty() { RELATIVE_SERVER_URL.to_string() } else { trimmed.to_string() }
        },
    )
}

/// Replaces the `servers:` block in the YAML string.  The block runs from
/// the `servers:` key through the last line that starts with whitespace
/// (a continued list item).  The replacement block is injected in its place.
fn replace_servers_block(spec: &str, servers_block: &str) -> String {
    let Some(servers_start) = spec.find("servers:\n") else {
        return spec.to_string();
    };
    // Find the end of the servers block: scan forward from the
    // `servers:` key line, skipping continuation lines (those that
    // start with whitespace or are empty).  The first non-continuation
    // line marks the end of the block.
    let block_body_start = servers_start + "servers:\n".len();
    let after_servers = &spec[block_body_start..];
    let block_end = scan_yaml_block_end(after_servers, spec, block_body_start);

    let mut rendered = String::with_capacity(spec.len() + servers_block.len());
    rendered.push_str(&spec[..servers_start]);
    rendered.push_str(servers_block);
    if block_end < spec.len() {
        rendered.push_str(&spec[block_end..]);
    }
    rendered
}

/// Returns the absolute byte offset of the first line in `chunk` that
/// is NOT a YAML block continuation (indented / empty).  `full_spec`
/// and `chunk_start` are used to return an absolute offset into the
/// full document.
fn scan_yaml_block_end(chunk: &str, full_spec: &str, chunk_start: usize) -> usize {
    let mut pos = 0usize;
    while pos < chunk.len() {
        let line = &chunk[pos..];
        let Some(line_end) = line.find('\n') else {
            return full_spec.len();
        };
        let content = &line[..line_end];
        if content.is_empty()
            || content.starts_with(' ')
            || content.starts_with('\t')
            || content.starts_with("- ")
            || content == "-"
        {
            pos += line_end + 1;
            continue;
        }
        return chunk_start + pos;
    }
    full_spec.len()
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
