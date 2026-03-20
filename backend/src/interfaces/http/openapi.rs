use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, header},
    routing::get,
};

use crate::app::state::AppState;

const OPENAPI_SPEC: &str = include_str!("../../../contracts/rustrag.openapi.yaml");
const RELATIVE_API_ROOT: &str = "/v1";
const PRIMARY_SERVER_DESCRIPTION: &str = "Primary public entrypoint";
const ALLOWED_SERVER_DESCRIPTION: &str = "Allowed configured origin";
const RELATIVE_SERVER_DESCRIPTION: &str = "Relative API root";

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new().route("/openapi/rustrag.openapi.yaml", get(get_openapi_spec))
}

async fn get_openapi_spec(
    State(state): State<AppState>,
    request_headers: HeaderMap,
) -> (HeaderMap, String) {
    let mut response_headers = HeaderMap::new();
    response_headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/yaml; charset=utf-8"));
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, max-age=0"));
    (
        response_headers,
        render_openapi_spec(OPENAPI_SPEC, &state.ui_runtime.frontend_origin, &request_headers),
    )
}

fn render_openapi_spec(spec: &str, frontend_origins: &str, request_headers: &HeaderMap) -> String {
    let servers = collect_openapi_server_urls(frontend_origins, request_headers);
    let servers_block = build_servers_block(&servers);

    replace_servers_block(spec, &servers_block)
}

fn collect_openapi_server_urls(frontend_origins: &str, request_headers: &HeaderMap) -> Vec<String> {
    let mut urls = Vec::new();

    if let Some(origin) = infer_request_origin(request_headers) {
        push_unique_server_url(&mut urls, &origin);
    }

    for origin in frontend_origins.split(',').map(str::trim).filter(|value| !value.is_empty()) {
        push_unique_server_url(&mut urls, origin);
    }

    if urls.is_empty() {
        urls.push(RELATIVE_API_ROOT.to_string());
    }

    urls
}

fn push_unique_server_url(urls: &mut Vec<String>, origin: &str) {
    let normalized_origin = origin.trim().trim_end_matches('/');
    if normalized_origin.is_empty() {
        return;
    }

    let candidate = if normalized_origin.ends_with("/v1") {
        normalized_origin.to_string()
    } else {
        format!("{normalized_origin}/v1")
    };

    if !urls.iter().any(|item| item == &candidate) {
        urls.push(candidate);
    }
}

fn infer_request_origin(headers: &HeaderMap) -> Option<String> {
    let forwarded = header_value(headers, "forwarded");
    let forwarded_proto = parse_forwarded_token(forwarded.as_deref(), "proto");
    let forwarded_host = parse_forwarded_token(forwarded.as_deref(), "host");

    let proto = header_value(headers, "x-forwarded-proto")
        .or(forwarded_proto)
        .unwrap_or_else(|| "http".to_string());
    let host = header_value(headers, "x-forwarded-host")
        .or(forwarded_host)
        .or_else(|| header_value(headers, header::HOST.as_str()))?;

    Some(format!("{}://{}", proto.trim(), host.trim()))
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.split(',').next().unwrap_or(value).trim().to_string())
}

fn parse_forwarded_token(value: Option<&str>, token: &str) -> Option<String> {
    let value = value?;
    for entry in value.split(',') {
        for pair in entry.split(';') {
            let (key, raw_value) = pair.split_once('=')?;
            if key.trim().eq_ignore_ascii_case(token) {
                return Some(raw_value.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn build_servers_block(urls: &[String]) -> String {
    let mut block = String::from("servers:\n");
    for (index, url) in urls.iter().enumerate() {
        let description = if url.starts_with('/') {
            RELATIVE_SERVER_DESCRIPTION
        } else if index == 0 {
            PRIMARY_SERVER_DESCRIPTION
        } else {
            ALLOWED_SERVER_DESCRIPTION
        };
        block.push_str(&format!("  - url: {url}\n    description: {description}\n"));
    }
    block
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
    use super::{collect_openapi_server_urls, infer_request_origin, render_openapi_spec};
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn prefers_forwarded_request_origin_for_primary_server() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert("x-forwarded-host", HeaderValue::from_static("docs.example.com"));

        let origin = infer_request_origin(&headers).expect("origin inferred");
        assert_eq!(origin, "https://docs.example.com");
    }

    #[test]
    fn includes_frontend_origins_after_request_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:19000"));

        let urls =
            collect_openapi_server_urls("http://localhost:19000,https://app.example.com", &headers);

        assert_eq!(urls[0], "http://127.0.0.1:19000/v1");
        assert!(urls.iter().any(|url| url == "http://localhost:19000/v1"));
        assert!(urls.iter().any(|url| url == "https://app.example.com/v1"));
    }

    #[test]
    fn rewrites_openapi_servers_block() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert("x-forwarded-host", HeaderValue::from_static("api.example.com"));

        let rendered = render_openapi_spec(
            "openapi: 3.1.0\nservers:\n  - url: http://localhost:8095\n    description: Local default\nsecurity:\n  - bearerAuth: []\n",
            "",
            &headers,
        );

        assert!(rendered.contains("url: https://api.example.com/v1"));
        assert!(!rendered.contains("http://localhost:8095"));
    }

    #[test]
    fn falls_back_to_relative_api_root_when_no_origin_is_available() {
        let rendered = render_openapi_spec(
            "openapi: 3.1.0\nservers:\n  - url: http://localhost:8095\n    description: Local default\nsecurity:\n  - bearerAuth: []\n",
            "",
            &HeaderMap::new(),
        );

        assert!(rendered.contains("url: /v1"));
        assert!(rendered.contains("description: Relative API root"));
    }
}
