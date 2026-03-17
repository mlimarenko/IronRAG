use anyhow::Error as AnyhowError;
use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;
use tracing::{error, warn};
use uuid::Uuid;

use crate::shared::file_extract::{UploadAdmissionError, UploadRejectionDetails};

pub const REQUEST_ID_HEADER: &str = "x-request-id";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<UploadRejectionDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiWarningBody {
    pub warning: String,
    pub warning_kind: &'static str,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("conflict: {0}")]
    StaleRevision(String),
    #[error("conflict: {0}")]
    ConflictingMutation(String),
    #[error("conflict: {0}")]
    MissingPrice(String),
    #[error("{message}")]
    UploadRejected { message: String, error_kind: &'static str, details: UploadRejectionDetails },
    #[error("internal server error")]
    Internal,
}

impl ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_)
            | Self::StaleRevision(_)
            | Self::ConflictingMutation(_)
            | Self::MissingPrice(_) => StatusCode::CONFLICT,
            Self::UploadRejected { .. } => StatusCode::BAD_REQUEST,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::StaleRevision(_) => "stale_revision",
            Self::ConflictingMutation(_) => "conflicting_mutation",
            Self::MissingPrice(_) => "missing_price",
            Self::UploadRejected { error_kind, .. } => error_kind,
            Self::Internal => "internal",
        }
    }

    fn details(&self) -> Option<UploadRejectionDetails> {
        match self {
            Self::UploadRejected { details, .. } => Some(details.clone()),
            _ => None,
        }
    }

    #[must_use]
    pub fn from_upload_admission(error: UploadAdmissionError) -> Self {
        Self::UploadRejected {
            message: error.message().to_string(),
            error_kind: error.error_kind(),
            details: error.details().clone(),
        }
    }
}

pub fn map_runtime_lifecycle_error(error: AnyhowError) -> ApiError {
    map_runtime_lifecycle_error_message(error.to_string())
}

pub fn map_runtime_upload_error(error: AnyhowError) -> ApiError {
    match error.downcast::<UploadAdmissionError>() {
        Ok(upload_error) => ApiError::from_upload_admission(upload_error),
        Err(error) => {
            error!(error = ?error, "runtime upload handler failed with unexpected internal error");
            ApiError::Internal
        }
    }
}

pub fn map_runtime_write_error(error: AnyhowError) -> ApiError {
    match error.downcast::<UploadAdmissionError>() {
        Ok(upload_error) => ApiError::from_upload_admission(upload_error),
        Err(error) => map_runtime_lifecycle_error(error),
    }
}

pub fn map_runtime_lifecycle_error_message(message: String) -> ApiError {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("stale revision") {
        return ApiError::StaleRevision(message);
    }
    if normalized.contains("missing price") || normalized.contains("unpriced") {
        return ApiError::MissingPrice(message);
    }
    if normalized.contains("document mutation conflict")
        || normalized.contains("another mutation is already active")
        || normalized.contains("logical document has been deleted")
        || normalized.contains("still processing")
    {
        return ApiError::ConflictingMutation(message);
    }
    if normalized.contains("conflict") {
        return ApiError::Conflict(message);
    }
    ApiError::BadRequest(message)
}

#[must_use]
pub fn blocked_activity_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "blocked_activity" }
}

#[must_use]
pub fn stalled_activity_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "stalled_activity" }
}

#[must_use]
pub fn partial_accounting_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "partial_accounting" }
}

#[must_use]
pub fn partial_convergence_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "partial_convergence" }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let error_kind = self.kind();
        let message = self.to_string();
        let request_id = None::<String>;
        let details = self.details();

        if status.is_server_error() {
            error!(
                %status,
                error_kind,
                error_message = %message,
                request_id = request_id.as_deref().unwrap_or("-"),
                "http request failed in handler",
            );
        } else {
            warn!(
                %status,
                error_kind,
                error_message = %message,
                request_id = request_id.as_deref().unwrap_or("-"),
                "http request rejected in handler",
            );
        }

        let mut response = (
            status,
            Json(ApiErrorBody {
                error: message,
                error_kind: Some(error_kind),
                details,
                request_id: request_id.clone(),
            }),
        )
            .into_response();

        if let Some(request_id) = request_id {
            attach_request_id_header(response.headers_mut(), &request_id);
        }

        response
    }
}

#[must_use]
pub fn ensure_or_generate_request_id(headers: &HeaderMap) -> String {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| Uuid::now_v7().to_string())
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
    use super::{ApiError, map_runtime_lifecycle_error_message, map_runtime_upload_error};
    use crate::shared::file_extract::UploadAdmissionError;

    #[test]
    fn maps_stale_revision_errors_to_specific_kind() {
        let error = map_runtime_lifecycle_error_message(
            "stale revision attempt rejected: expected active revision 2, found 3".to_string(),
        );
        assert!(matches!(error, ApiError::StaleRevision(_)));
    }

    #[test]
    fn maps_conflicting_mutation_errors_to_specific_kind() {
        let error = map_runtime_lifecycle_error_message(
            "document mutation conflict: another mutation is already active".to_string(),
        );
        assert!(matches!(error, ApiError::ConflictingMutation(_)));
    }

    #[test]
    fn maps_missing_price_errors_to_specific_kind() {
        let error = map_runtime_lifecycle_error_message(
            "missing price for provider/model/capability".to_string(),
        );
        assert!(matches!(error, ApiError::MissingPrice(_)));
    }

    #[test]
    fn maps_upload_admission_errors_to_structured_upload_rejections() {
        let error = map_runtime_upload_error(anyhow::Error::new(
            UploadAdmissionError::invalid_file_body(Some("report.pdf"), Some("application/pdf")),
        ));
        match error {
            ApiError::UploadRejected { error_kind, details, .. } => {
                assert_eq!(error_kind, "invalid_file_body");
                assert_eq!(details.file_name.as_deref(), Some("report.pdf"));
                assert_eq!(details.detected_format.as_deref(), Some("PDF"));
            }
            other => panic!("expected upload rejection, got {other:?}"),
        }
    }
}
