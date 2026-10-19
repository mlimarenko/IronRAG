use axum::http::StatusCode;
use thiserror::Error;
use uuid::Uuid;

use crate::domains::ops::OpsFailureDisposition;
use crate::services::iam::login_rate_limit::LoginRateLimitError;
use crate::shared::{
    extraction::file_extract::{UploadAdmissionError, UploadRejectionDetails},
    secret_encryption::SecretEncryptionError,
};

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("bad request: {0}")]
    InvalidMcpToolCall(String),
    #[error("bad request: {0}")]
    InvalidContinuationToken(String),
    #[error("unsupported media type: {0}")]
    UnsupportedMediaType(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("unauthorized: {0}")]
    InaccessibleMemoryScope(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    BootstrapAlreadyClaimed(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("conflict: {0}")]
    UnreadableDocument(String),
    #[error("conflict: {0}")]
    StaleRevision(String),
    #[error("conflict: {0}")]
    ConflictingMutation(String),
    #[error("conflict: {0}")]
    DocumentAdmissionIncomplete(String),
    #[error("conflict: {0}")]
    IdempotencyConflict(String),
    #[error("conflict: {0}")]
    VisibilityIntentRequired(String),
    #[error("conflict: {0}")]
    VisibilityIntentSuperseded(String),
    #[error("conflict: {0}")]
    VisibilityBindingConflict(String),
    #[error("{message}")]
    DuplicateContent { message: String, existing_document_id: Uuid },
    #[error("conflict: {0}")]
    MissingPrice(String),
    #[error("conflict: {0}")]
    KnowledgeNotReady(String),
    #[error("service unavailable: {message}")]
    ServiceUnavailable { message: String, kind: &'static str },
    #[error("too many requests: {message}")]
    RateLimited { message: String, kind: &'static str, retry_after_seconds: u64 },
    #[error("gateway timeout: {message}")]
    GatewayTimeout { message: String, kind: &'static str },
    #[error("conflict: {0}")]
    GraphWriteContention(String),
    #[error("conflict: {0}")]
    GraphPersistenceIntegrity(String),
    #[error("conflict: {0}")]
    SettlementRefreshFailed(String),
    #[error("conflict: {0}")]
    ProviderFailure(String),
    #[error("{message}")]
    UploadRejected {
        message: String,
        error_kind: &'static str,
        /// Server-authoritative disposition of this synchronous rejection,
        /// published as the `failureDisposition` problem extension. Only
        /// `content_unsupported` is a proven, permanent content rejection.
        failure_disposition: OpsFailureDisposition,
        details: Box<UploadRejectionDetails>,
    },
    #[error("internal server error")]
    Internal,
    #[error("internal server error: {0}")]
    InternalMessage(String),
}

impl ApiError {
    #[must_use]
    pub fn from_secret_encryption(error: SecretEncryptionError) -> Self {
        match error {
            SecretEncryptionError::InvalidPlaintext => {
                Self::BadRequest("secret must be non-empty and at most 4096 bytes".to_string())
            }
            SecretEncryptionError::MasterKeyNotConfigured
            | SecretEncryptionError::InvalidMasterKey
            | SecretEncryptionError::InvalidKeyId
            | SecretEncryptionError::InvalidPreviousKeyMap => Self::service_unavailable(
                "credential encryption is not configured",
                "credential_encryption_unavailable",
            ),
            SecretEncryptionError::InvalidEnvelope
            | SecretEncryptionError::UnsupportedEnvelope
            | SecretEncryptionError::UnknownKeyId
            | SecretEncryptionError::DecryptionFailed => Self::service_unavailable(
                "stored credential cannot be decrypted",
                "credential_decryption_unavailable",
            ),
            SecretEncryptionError::EncryptionFailed => {
                Self::internal_with_log(error, "credential encryption failed")
            }
        }
    }

    pub fn internal_with_log(error: impl std::fmt::Debug, context: &str) -> Self {
        tracing::error!(?error, "{context}");
        Self::Internal
    }

    #[must_use]
    pub fn invalid_mcp_tool_call(message: impl Into<String>) -> Self {
        Self::InvalidMcpToolCall(message.into())
    }

    #[must_use]
    pub fn invalid_continuation_token(message: impl Into<String>) -> Self {
        Self::InvalidContinuationToken(message.into())
    }

    #[must_use]
    pub fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::UnsupportedMediaType(message.into())
    }

    #[must_use]
    pub fn inaccessible_memory_scope(message: impl Into<String>) -> Self {
        Self::InaccessibleMemoryScope(message.into())
    }

    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    #[must_use]
    pub fn unreadable_document(message: impl Into<String>) -> Self {
        Self::UnreadableDocument(message.into())
    }

    #[must_use]
    pub fn idempotency_conflict(message: impl Into<String>) -> Self {
        Self::IdempotencyConflict(message.into())
    }

    #[must_use]
    pub fn knowledge_not_ready(message: impl Into<String>) -> Self {
        Self::KnowledgeNotReady(message.into())
    }

    #[must_use]
    pub fn service_unavailable(message: impl Into<String>, kind: &'static str) -> Self {
        Self::ServiceUnavailable { message: message.into(), kind }
    }

    #[must_use]
    pub fn rate_limited(
        message: impl Into<String>,
        kind: &'static str,
        retry_after_seconds: u64,
    ) -> Self {
        Self::RateLimited { message: message.into(), kind, retry_after_seconds }
    }

    /// Maps a login-rate-limit rejection to its public HTTP shape. Every
    /// variant either surfaces as a `429` with a `Retry-After` hint, or --
    /// for the two variants that should never happen once configuration is
    /// validated at startup -- fails closed as a logged `500` rather than
    /// silently admitting the request.
    #[must_use]
    pub fn from_login_rate_limit(error: LoginRateLimitError) -> Self {
        match error {
            LoginRateLimitError::LoginRateLimited { retry_after_seconds } => Self::rate_limited(
                "too many login attempts for this account; retry later",
                error.redacted_detail(),
                retry_after_seconds,
            ),
            LoginRateLimitError::SourceRateLimited { retry_after_seconds } => Self::rate_limited(
                "too many login attempts from this source; retry later",
                error.redacted_detail(),
                retry_after_seconds,
            ),
            LoginRateLimitError::VerificationAdmissionRejected { retry_after_seconds } => {
                Self::rate_limited(
                    "too many concurrent login attempts; retry shortly",
                    error.redacted_detail(),
                    retry_after_seconds,
                )
            }
            LoginRateLimitError::RegistryPoisoned => {
                Self::internal_with_log(error, "login rate limiter registry lock poisoned")
            }
        }
    }

    #[must_use]
    pub fn database_contention() -> Self {
        Self::service_unavailable(
            "database transaction contention; retry the request",
            "database_contention",
        )
    }

    #[must_use]
    pub fn query_deadline_exceeded() -> Self {
        Self::GatewayTimeout {
            message: "query answer exceeded its execution deadline".to_string(),
            kind: "query_deadline_exceeded",
        }
    }

    #[must_use]
    pub fn bootstrap_already_claimed(message: impl Into<String>) -> Self {
        Self::BootstrapAlreadyClaimed(message.into())
    }

    #[must_use]
    pub fn resource_not_found(resource_kind: &'static str, id: impl std::fmt::Display) -> Self {
        Self::NotFound(format!("{resource_kind} {id} not found"))
    }

    #[must_use]
    pub fn context_bundle_not_found(id: impl std::fmt::Display) -> Self {
        Self::NotFound(format!("knowledge_bundle {id} not found"))
    }

    pub(super) const fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_)
            | Self::InvalidMcpToolCall(_)
            | Self::InvalidContinuationToken(_)
            | Self::UploadRejected { .. } => StatusCode::BAD_REQUEST,
            Self::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Unauthorized | Self::InaccessibleMemoryScope(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BootstrapAlreadyClaimed(_)
            | Self::Conflict(_)
            | Self::UnreadableDocument(_)
            | Self::StaleRevision(_)
            | Self::ConflictingMutation(_)
            | Self::DocumentAdmissionIncomplete(_)
            | Self::IdempotencyConflict(_)
            | Self::VisibilityIntentRequired(_)
            | Self::VisibilityIntentSuperseded(_)
            | Self::VisibilityBindingConflict(_)
            | Self::DuplicateContent { .. }
            | Self::MissingPrice(_)
            | Self::KnowledgeNotReady(_)
            | Self::GraphWriteContention(_)
            | Self::GraphPersistenceIntegrity(_)
            | Self::SettlementRefreshFailed(_) => StatusCode::CONFLICT,
            Self::ProviderFailure(_) => StatusCode::BAD_GATEWAY,
            Self::ServiceUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::GatewayTimeout { .. } => StatusCode::GATEWAY_TIMEOUT,
            Self::Internal | Self::InternalMessage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Forbidden(_) => "forbidden",
            Self::InvalidMcpToolCall(_) => "invalid_mcp_tool_call",
            Self::InvalidContinuationToken(_) => "invalid_continuation_token",
            Self::UnsupportedMediaType(_) => "unsupported_media_type",
            Self::Unauthorized => "unauthorized",
            Self::InaccessibleMemoryScope(_) => "inaccessible_memory_scope",
            Self::NotFound(_) => "not_found",
            Self::BootstrapAlreadyClaimed(_) => "bootstrap_already_claimed",
            Self::Conflict(_) => "conflict",
            Self::UnreadableDocument(_) => "unreadable_document",
            Self::StaleRevision(_) => "stale_revision",
            Self::ConflictingMutation(_) => "conflicting_mutation",
            Self::DocumentAdmissionIncomplete(_) => "document_admission_incomplete",
            Self::IdempotencyConflict(_) => "idempotency_conflict",
            Self::VisibilityIntentRequired(_) => "visibility_intent_required",
            Self::VisibilityIntentSuperseded(_) => "visibility_intent_superseded",
            Self::VisibilityBindingConflict(_) => "visibility_binding_conflict",
            Self::DuplicateContent { .. } => "duplicate_content",
            Self::MissingPrice(_) => "missing_price",
            Self::KnowledgeNotReady(_) => "knowledge_not_ready",
            Self::ServiceUnavailable { kind, .. } => kind,
            Self::RateLimited { kind, .. } => kind,
            Self::GatewayTimeout { kind, .. } => kind,
            Self::GraphWriteContention(_) => "graph_write_contention",
            Self::GraphPersistenceIntegrity(_) => "graph_persistence_integrity",
            Self::SettlementRefreshFailed(_) => "graph_state_refresh_failed",
            Self::ProviderFailure(_) => "provider_failure",
            Self::UploadRejected { error_kind, .. } => error_kind,
            Self::Internal | Self::InternalMessage(_) => "internal",
        }
    }

    /// RFC 9457 extension members for this error occurrence, flattened into
    /// the top level of the response body. Empty `Map`/`None` for variants
    /// with no error-specific structured payload.
    pub(super) fn extensions(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let value = match self {
            Self::UploadRejected { details, failure_disposition, .. } => {
                let mut extensions = match serde_json::to_value(details.as_ref()) {
                    Ok(serde_json::Value::Object(map)) => map,
                    _ => serde_json::Map::new(),
                };
                extensions.insert(
                    "failureDisposition".to_string(),
                    serde_json::Value::from(failure_disposition.as_str()),
                );
                return Some(extensions);
            }
            Self::DuplicateContent { existing_document_id, .. } => {
                serde_json::to_value(serde_json::json!({
                    "existingDocumentId": existing_document_id,
                }))
            }
            Self::RateLimited { retry_after_seconds, .. } => {
                serde_json::to_value(serde_json::json!({
                    "retryAfterSeconds": retry_after_seconds,
                }))
            }
            _ => return None,
        };
        value.ok().and_then(|value| value.as_object().cloned())
    }

    /// Seconds a rate-limited caller should wait before retrying, used to set
    /// the `Retry-After` response header. `None` for every other variant.
    #[must_use]
    pub(super) const fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_seconds, .. } => Some(*retry_after_seconds),
            _ => None,
        }
    }

    #[must_use]
    pub fn duplicate_content(message: impl Into<String>, existing_document_id: Uuid) -> Self {
        Self::DuplicateContent { message: message.into(), existing_document_id }
    }

    #[must_use]
    pub fn from_upload_admission(error: UploadAdmissionError) -> Self {
        Self::UploadRejected {
            message: error.message().to_string(),
            error_kind: error.error_kind(),
            failure_disposition: OpsFailureDisposition::from_extraction_failure_class(
                error.extraction_failure_class(),
            ),
            details: Box::new(error.details().clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use uuid::Uuid;

    use super::ApiError;

    #[test]
    fn exposes_mcp_specific_error_kinds() {
        assert_eq!(
            ApiError::invalid_mcp_tool_call("unsupported tool").kind(),
            "invalid_mcp_tool_call"
        );
        assert_eq!(
            ApiError::invalid_continuation_token("tampered token").kind(),
            "invalid_continuation_token"
        );
        assert_eq!(
            ApiError::inaccessible_memory_scope("library not visible").kind(),
            "inaccessible_memory_scope"
        );
        assert_eq!(
            ApiError::idempotency_conflict("payload changed").kind(),
            "idempotency_conflict"
        );
        assert_eq!(
            ApiError::bootstrap_already_claimed("already claimed").kind(),
            "bootstrap_already_claimed"
        );
    }

    #[test]
    fn internal_message_preserves_internal_kind_and_explicit_text() {
        let error = ApiError::InternalMessage("knowledge mirror sync failed".to_string());
        assert_eq!(error.kind(), "internal");
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.to_string(), "internal server error: knowledge mirror sync failed");
    }

    #[test]
    fn synchronous_upload_rejection_publishes_the_server_disposition() {
        use crate::shared::extraction::file_extract::{FileExtractError, UploadAdmissionError};

        let content =
            ApiError::from_upload_admission(UploadAdmissionError::from_file_extract_error(
                "notes.txt",
                Some("text/plain"),
                2,
                &FileExtractError::InvalidUtf8,
            ));
        assert_eq!(content.status_code(), StatusCode::BAD_REQUEST);
        let extensions = content.extensions().expect("upload rejection extensions");
        assert_eq!(
            extensions.get("failureDisposition"),
            Some(&serde_json::json!("content_unsupported")),
        );
        assert_eq!(
            extensions.get("rejectionKind"),
            Some(&serde_json::json!("invalid_text_encoding"))
        );

        let request = ApiError::from_upload_admission(UploadAdmissionError::file_too_large(
            "big.pdf",
            Some("application/pdf"),
            10,
            1,
        ));
        assert_eq!(
            request.extensions().expect("upload rejection extensions").get("failureDisposition"),
            Some(&serde_json::json!("operational_terminal")),
            "a request-level rejection carries no content verdict",
        );
    }

    #[test]
    fn builds_typed_not_found_error_messages() {
        let error = ApiError::resource_not_found("workspace", Uuid::nil());
        assert_eq!(error.kind(), "not_found");
        assert!(error.to_string().contains("workspace"));
    }
}
