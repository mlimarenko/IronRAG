use axum::{
    Json,
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tracing::{error, warn};
use uuid::Uuid;

use super::api_error::ApiError;

pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// URN prefix for the RFC 9457 `type` member. IronRAG does not host a
/// documentation page per error code, so `type` is a stable, dereferenceable-
/// in-name-only identifier rather than a browsable URL — RFC 9457 only
/// requires it to be a URI reference, not a live resource.
const PROBLEM_TYPE_PREFIX: &str = "urn:ironrag:error:";

pub const PROBLEM_JSON_CONTENT_TYPE: &str = "application/problem+json";

/// RFC 9457 `application/problem+json` response body. `title` is derived
/// mechanically from `code` (stable per code, satisfying RFC 9457's "SHOULD
/// NOT change from occurrence to occurrence" guidance) rather than hand
/// authored per variant. `extensions` carries error-specific structured
/// data (e.g. `existingDocumentId` on a duplicate-content conflict) as
/// flattened top-level members, per RFC 9457 §3.2.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ApiErrorBody {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Humanizes a stable machine `code` slug (e.g. `duplicate_content`) into an
/// RFC 9457 `title` (e.g. `"Duplicate Content"`). Deterministic and stable
/// per code, so it satisfies the "SHOULD NOT change" guidance without
/// requiring a hand-authored title string per error variant.
fn humanize_problem_title(code: &str) -> String {
    code.split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let code = self.kind();
        let message = self.to_string();
        let request_id = None::<String>;
        let extensions = self.extensions();
        let retry_after_seconds = self.retry_after_seconds();

        log_handler_failure(status, code, &message, request_id.as_deref().unwrap_or("-"));

        let body = ApiErrorBody {
            problem_type: format!("{PROBLEM_TYPE_PREFIX}{code}"),
            title: humanize_problem_title(code),
            status: status.as_u16(),
            detail: message,
            code,
            request_id: request_id.clone(),
            extensions,
        };

        let mut response = (status, Json(body)).into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON_CONTENT_TYPE));

        attach_optional_headers(response.headers_mut(), request_id.as_deref(), retry_after_seconds);

        response
    }
}

/// Routes the handler-failure log line to its level, preserving the fields and
/// message of the original branch. One tracing macro per leaf because the
/// macro's expansion is itself counted as cognitive complexity.
fn log_handler_failure(
    status: axum::http::StatusCode,
    code: &'static str,
    message: &str,
    request_id: &str,
) {
    if status.is_server_error() {
        log_handler_server_failure(status, code, message, request_id);
    } else {
        log_handler_client_failure(status, code, message, request_id);
    }
}

fn log_handler_server_failure(
    status: axum::http::StatusCode,
    code: &'static str,
    message: &str,
    request_id: &str,
) {
    error!(
        %status,
        error_kind = code,
        error_message = %message,
        request_id,
        "http request failed in handler",
    );
}

fn log_handler_client_failure(
    status: axum::http::StatusCode,
    code: &'static str,
    message: &str,
    request_id: &str,
) {
    warn!(
        %status,
        error_kind = code,
        error_message = %message,
        request_id,
        "http request rejected in handler",
    );
}

/// Attaches the request id and, when the error carries one, a `Retry-After`
/// header parsed from the seconds value.
fn attach_optional_headers(
    headers: &mut HeaderMap,
    request_id: Option<&str>,
    retry_after_seconds: Option<u64>,
) {
    if let Some(request_id) = request_id {
        attach_request_id_header(headers, request_id);
    }
    if let Some(value) =
        retry_after_seconds.and_then(|seconds| HeaderValue::from_str(&seconds.to_string()).ok())
    {
        headers.insert(header::RETRY_AFTER, value);
    }
}

#[must_use]
pub fn ensure_or_generate_request_id(headers: &HeaderMap) -> String {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| Uuid::now_v7().to_string(), std::string::ToString::to_string)
}

pub fn attach_request_id_header(headers: &mut HeaderMap, request_id: &str) {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert(header::HeaderName::from_static(REQUEST_ID_HEADER), value);
    }
}

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
    use uuid::Uuid;

    use super::{
        PROBLEM_JSON_CONTENT_TYPE, REQUEST_ID_HEADER, attach_optional_headers,
        humanize_problem_title,
    };
    use crate::interfaces::http::router_support::ApiError;

    #[test]
    fn attaches_request_id_and_retry_after_only_when_present() {
        let mut headers = axum::http::HeaderMap::new();
        attach_optional_headers(&mut headers, Some("req-1"), Some(3));
        assert_eq!(
            headers.get(REQUEST_ID_HEADER).and_then(|value| value.to_str().ok()),
            Some("req-1"),
        );
        assert_eq!(
            headers.get(axum::http::header::RETRY_AFTER).and_then(|value| value.to_str().ok()),
            Some("3"),
        );

        let mut empty = axum::http::HeaderMap::new();
        attach_optional_headers(&mut empty, None, None);
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn gateway_timeout_has_stable_safe_contract() -> Result<(), Box<dyn StdError>> {
        let error = ApiError::query_deadline_exceeded();

        assert_eq!(error.kind(), "query_deadline_exceeded");
        assert_eq!(error.status_code(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            error.to_string(),
            "gateway timeout: query answer exceeded its execution deadline"
        );

        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some(PROBLEM_JSON_CONTENT_TYPE),
        );
        let body = to_bytes(response.into_body(), 4096).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(
            json,
            serde_json::json!({
                "type": "urn:ironrag:error:query_deadline_exceeded",
                "title": "Query Deadline Exceeded",
                "status": 504,
                "detail": "gateway timeout: query answer exceeded its execution deadline",
                "code": "query_deadline_exceeded",
            })
        );

        Ok(())
    }

    #[tokio::test]
    async fn duplicate_content_conflict_carries_existing_document_id_extension()
    -> Result<(), Box<dyn StdError>> {
        let existing_document_id = Uuid::now_v7();
        let error = ApiError::duplicate_content(
            "an active document with this external key already exists",
            existing_document_id,
        );

        assert_eq!(error.kind(), "duplicate_content");
        assert_eq!(error.status_code(), StatusCode::CONFLICT);

        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), 4096).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(
            json.get("existingDocumentId").and_then(serde_json::Value::as_str),
            Some(existing_document_id.to_string().as_str()),
        );
        assert_eq!(json.get("code").and_then(serde_json::Value::as_str), Some("duplicate_content"));

        Ok(())
    }

    #[test]
    fn humanizes_problem_titles_from_stable_codes() {
        assert_eq!(humanize_problem_title("bad_request"), "Bad Request");
        assert_eq!(humanize_problem_title("duplicate_content"), "Duplicate Content");
        assert_eq!(humanize_problem_title("internal"), "Internal");
    }
}
