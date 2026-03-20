use anyhow::Error as AnyhowError;
use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sqlx::Error as SqlxError;
use thiserror::Error;
use tracing::{error, warn};
use uuid::Uuid;

use crate::shared::file_extract::{UploadAdmissionError, UploadRejectionDetails};

pub const REQUEST_ID_HEADER: &str = "x-request-id";
pub const FORBIDDEN_VOCABULARY_TOKENS: [(&str, &str); 6] = [
    ("project", "library"),
    ("projects", "libraries"),
    ("collection", "library"),
    ("collections", "libraries"),
    ("provider_account", "provider credential"),
    ("model_profile", "model preset"),
];

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
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("bad request: {0}")]
    InvalidMcpToolCall(String),
    #[error("bad request: {0}")]
    InvalidContinuationToken(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("unauthorized: {0}")]
    InaccessibleMemoryScope(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    BootstrapAlreadyClaimed(String),
    #[error(
        "bad request: legacy vocabulary '{legacy}' is not allowed for {field}; use '{canonical}'"
    )]
    ForbiddenVocabulary { field: &'static str, legacy: &'static str, canonical: &'static str },
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("conflict: {0}")]
    UnreadableDocument(String),
    #[error("conflict: {0}")]
    StaleRevision(String),
    #[error("conflict: {0}")]
    ConflictingMutation(String),
    #[error("conflict: {0}")]
    IdempotencyConflict(String),
    #[error("conflict: {0}")]
    MissingPrice(String),
    #[error("conflict: {0}")]
    ProjectionContention(String),
    #[error("conflict: {0}")]
    GraphPersistenceIntegrity(String),
    #[error("conflict: {0}")]
    SettlementRefreshFailed(String),
    #[error("conflict: {0}")]
    ProviderFailure(String),
    #[error("{message}")]
    UploadRejected { message: String, error_kind: &'static str, details: UploadRejectionDetails },
    #[error("internal server error")]
    Internal,
}

impl ApiError {
    #[must_use]
    pub fn invalid_mcp_tool_call(message: impl Into<String>) -> Self {
        Self::InvalidMcpToolCall(message.into())
    }

    #[must_use]
    pub fn invalid_continuation_token(message: impl Into<String>) -> Self {
        Self::InvalidContinuationToken(message.into())
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
    pub fn bootstrap_already_claimed(message: impl Into<String>) -> Self {
        Self::BootstrapAlreadyClaimed(message.into())
    }

    #[must_use]
    pub fn resource_not_found(resource_kind: &'static str, id: impl std::fmt::Display) -> Self {
        Self::NotFound(format!("{resource_kind} {id} not found"))
    }

    #[must_use]
    pub fn forbidden_vocabulary(
        field: &'static str,
        legacy: &'static str,
        canonical: &'static str,
    ) -> Self {
        Self::ForbiddenVocabulary { field, legacy, canonical }
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_)
            | Self::ForbiddenVocabulary { .. }
            | Self::InvalidMcpToolCall(_)
            | Self::InvalidContinuationToken(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized | Self::InaccessibleMemoryScope(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BootstrapAlreadyClaimed(_) => StatusCode::CONFLICT,
            Self::Conflict(_)
            | Self::UnreadableDocument(_)
            | Self::StaleRevision(_)
            | Self::ConflictingMutation(_)
            | Self::IdempotencyConflict(_)
            | Self::MissingPrice(_)
            | Self::ProjectionContention(_)
            | Self::GraphPersistenceIntegrity(_)
            | Self::SettlementRefreshFailed(_)
            | Self::ProviderFailure(_) => StatusCode::CONFLICT,
            Self::UploadRejected { .. } => StatusCode::BAD_REQUEST,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Forbidden(_) => "forbidden",
            Self::InvalidMcpToolCall(_) => "invalid_mcp_tool_call",
            Self::InvalidContinuationToken(_) => "invalid_continuation_token",
            Self::Unauthorized => "unauthorized",
            Self::InaccessibleMemoryScope(_) => "inaccessible_memory_scope",
            Self::NotFound(_) => "not_found",
            Self::BootstrapAlreadyClaimed(_) => "bootstrap_already_claimed",
            Self::ForbiddenVocabulary { .. } => "forbidden_vocabulary",
            Self::Conflict(_) => "conflict",
            Self::UnreadableDocument(_) => "unreadable_document",
            Self::StaleRevision(_) => "stale_revision",
            Self::ConflictingMutation(_) => "conflicting_mutation",
            Self::IdempotencyConflict(_) => "idempotency_conflict",
            Self::MissingPrice(_) => "missing_price",
            Self::ProjectionContention(_) => "projection_contention",
            Self::GraphPersistenceIntegrity(_) => "graph_persistence_integrity",
            Self::SettlementRefreshFailed(_) => "settlement_refresh_failed",
            Self::ProviderFailure(_) => "provider_failure",
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
    if normalized.contains("projection contention")
        || normalized.contains("deadlock")
        || normalized.contains("lock timeout")
    {
        return ApiError::ProjectionContention(message);
    }
    if normalized.contains("graph persistence integrity")
        || normalized.contains("foreign key violation")
        || normalized.contains("edge persistence skipped because node")
    {
        return ApiError::GraphPersistenceIntegrity(message);
    }
    if normalized.contains("settlement refresh failed")
        || normalized.contains("failed to persist collection settlement")
        || normalized.contains("failed to persist collection terminal outcome")
    {
        return ApiError::SettlementRefreshFailed(message);
    }
    if normalized.contains("provider failure")
        || normalized.contains("upstream timeout")
        || normalized.contains("upstream rejection")
        || normalized.contains("invalid model output")
        || normalized.contains("invalid_request")
        || normalized.contains("invalid request")
    {
        return ApiError::ProviderFailure(message);
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

pub fn map_workspace_create_error(error: SqlxError, slug: &str) -> ApiError {
    match error {
        SqlxError::Database(database_error) if database_error.is_unique_violation() => {
            ApiError::Conflict(format!("workspace slug '{slug}' already exists"))
        }
        _ => ApiError::Internal,
    }
}

pub fn map_library_create_error(error: SqlxError, workspace_id: Uuid, slug: &str) -> ApiError {
    match error {
        SqlxError::Database(database_error) if database_error.is_unique_violation() => {
            ApiError::Conflict(format!("library slug '{slug}' already exists in this workspace"))
        }
        SqlxError::Database(database_error) if database_error.is_foreign_key_violation() => {
            ApiError::NotFound(format!("workspace {workspace_id} not found"))
        }
        _ => ApiError::Internal,
    }
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

#[must_use]
pub fn query_intent_degradation_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "query_intent_degradation" }
}

#[must_use]
pub fn rerank_failure_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "rerank_failure" }
}

#[must_use]
pub fn extraction_recovery_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "extraction_recovery" }
}

#[must_use]
pub fn reconciliation_fallback_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "reconciliation_fallback" }
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

#[must_use]
pub fn detect_forbidden_vocabulary(value: &str) -> Option<(&'static str, &'static str)> {
    let normalized = value.to_ascii_lowercase();
    FORBIDDEN_VOCABULARY_TOKENS
        .iter()
        .copied()
        .find(|(legacy, _canonical)| normalized.contains(legacy))
}

pub fn ensure_canonical_vocabulary(field: &'static str, value: &str) -> Result<(), ApiError> {
    if let Some((legacy, canonical)) = detect_forbidden_vocabulary(value) {
        return Err(ApiError::forbidden_vocabulary(field, legacy, canonical));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, error::Error as StdError, fmt};

    use sqlx::error::{DatabaseError, ErrorKind};
    use uuid::Uuid;

    use super::{
        ApiError, detect_forbidden_vocabulary, ensure_canonical_vocabulary,
        extraction_recovery_warning, map_library_create_error, map_runtime_lifecycle_error_message,
        map_runtime_upload_error, map_workspace_create_error, query_intent_degradation_warning,
        reconciliation_fallback_warning, rerank_failure_warning,
    };
    use crate::shared::file_extract::UploadAdmissionError;

    #[derive(Debug)]
    struct FakeDatabaseError {
        message: &'static str,
        code: &'static str,
        constraint: Option<&'static str>,
    }

    impl fmt::Display for FakeDatabaseError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl StdError for FakeDatabaseError {}

    impl DatabaseError for FakeDatabaseError {
        fn message(&self) -> &str {
            self.message
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
        }

        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }

        fn constraint(&self) -> Option<&str> {
            self.constraint
        }

        fn kind(&self) -> ErrorKind {
            match self.code {
                "23505" => ErrorKind::UniqueViolation,
                "23503" => ErrorKind::ForeignKeyViolation,
                "23502" => ErrorKind::NotNullViolation,
                "23514" => ErrorKind::CheckViolation,
                _ => ErrorKind::Other,
            }
        }
    }

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
    fn maps_projection_contention_errors_to_specific_kind() {
        let error = map_runtime_lifecycle_error_message(
            "projection contention: Neo4j deadlock detected during graph projection".to_string(),
        );
        assert!(matches!(error, ApiError::ProjectionContention(_)));
    }

    #[test]
    fn maps_graph_integrity_errors_to_specific_kind() {
        let error = map_runtime_lifecycle_error_message(
            "graph persistence integrity failure: foreign key violation on runtime_graph_edge"
                .to_string(),
        );
        assert!(matches!(error, ApiError::GraphPersistenceIntegrity(_)));
    }

    #[test]
    fn maps_settlement_refresh_errors_to_specific_kind() {
        let error = map_runtime_lifecycle_error_message(
            "failed to persist collection settlement snapshot".to_string(),
        );
        assert!(matches!(error, ApiError::SettlementRefreshFailed(_)));
    }

    #[test]
    fn maps_provider_failures_to_specific_kind() {
        let error = map_runtime_lifecycle_error_message(
            "provider failure: upstream timeout while extracting graph".to_string(),
        );
        assert!(matches!(error, ApiError::ProviderFailure(_)));
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
                assert_eq!(details.rejection_kind.as_deref(), Some("invalid_file_body"));
                assert_eq!(details.detected_format.as_deref(), Some("PDF"));
            }
            other => panic!("expected upload rejection, got {other:?}"),
        }
    }

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
    fn maps_workspace_unique_violations_to_conflict() {
        let error = map_workspace_create_error(
            sqlx::Error::Database(Box::new(FakeDatabaseError {
                message: "duplicate key value violates unique constraint",
                code: "23505",
                constraint: Some("workspace_slug_key"),
            })),
            "agent-workspace",
        );

        assert!(matches!(error, ApiError::Conflict(_)));
        assert_eq!(error.to_string(), "conflict: workspace slug 'agent-workspace' already exists");
    }

    #[test]
    fn maps_library_unique_violations_to_conflict() {
        let error = map_library_create_error(
            sqlx::Error::Database(Box::new(FakeDatabaseError {
                message: "duplicate key value violates unique constraint",
                code: "23505",
                constraint: Some("project_workspace_id_slug_key"),
            })),
            Uuid::nil(),
            "agent-library",
        );

        assert!(matches!(error, ApiError::Conflict(_)));
        assert_eq!(
            error.to_string(),
            "conflict: library slug 'agent-library' already exists in this workspace"
        );
    }

    #[test]
    fn maps_library_foreign_key_violations_to_not_found() {
        let workspace_id = Uuid::now_v7();
        let error = map_library_create_error(
            sqlx::Error::Database(Box::new(FakeDatabaseError {
                message: "insert or update on table project violates foreign key constraint",
                code: "23503",
                constraint: Some("project_workspace_id_fkey"),
            })),
            workspace_id,
            "agent-library",
        );

        assert!(matches!(error, ApiError::NotFound(_)));
        assert_eq!(error.to_string(), format!("not found: workspace {workspace_id} not found"));
    }

    #[test]
    fn builds_query_intent_degradation_warning() {
        let warning = query_intent_degradation_warning("intent fell back to literal keywords");
        assert_eq!(warning.warning_kind, "query_intent_degradation");
    }

    #[test]
    fn builds_rerank_failure_warning() {
        let warning = rerank_failure_warning("rerank provider unavailable");
        assert_eq!(warning.warning_kind, "rerank_failure");
    }

    #[test]
    fn builds_extraction_recovery_warning() {
        let warning =
            extraction_recovery_warning("partial recovery preserved only part of the graph");
        assert_eq!(warning.warning_kind, "extraction_recovery");
    }

    #[test]
    fn builds_reconciliation_fallback_warning() {
        let warning =
            reconciliation_fallback_warning("targeted refresh fell back to broad rebuild");
        assert_eq!(warning.warning_kind, "reconciliation_fallback");
    }

    #[test]
    fn detects_forbidden_vocabulary_tokens() {
        assert_eq!(detect_forbidden_vocabulary("projectSlug"), Some(("project", "library")));
        assert_eq!(detect_forbidden_vocabulary("collection_name"), Some(("collection", "library")));
        assert_eq!(detect_forbidden_vocabulary("librarySlug"), None);
    }

    #[test]
    fn rejects_forbidden_vocabulary_in_canonical_fields() {
        let error = ensure_canonical_vocabulary("path", "/v1/projects")
            .expect_err("legacy vocabulary should be rejected");

        assert!(matches!(error, ApiError::ForbiddenVocabulary { .. }));
        assert_eq!(error.kind(), "forbidden_vocabulary");
    }

    #[test]
    fn builds_typed_not_found_error_messages() {
        let error = ApiError::resource_not_found("workspace", Uuid::nil());
        assert_eq!(error.kind(), "not_found");
        assert!(error.to_string().contains("workspace"));
    }
}
