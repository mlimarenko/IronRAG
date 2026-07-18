use anyhow::Error as AnyhowError;
use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sqlx::Error as SqlxError;
use thiserror::Error;
use tracing::{Span, error, warn};
use uuid::Uuid;

use crate::{
    agent_runtime::trace::{RuntimeExecutionTraceView, build_trace_view},
    domains::agent_runtime::{
        RuntimeActionRecord, RuntimeExecution, RuntimePolicyDecision, RuntimeStageRecord,
    },
    infra::repositories::runtime_repository,
    services::{
        content::error::ContentServiceError, graph::error::GraphServiceError,
        ingest::error::IngestServiceError, knowledge::error::KnowledgeServiceError,
        query::error::QueryServiceError, webhook::error::WebhookServiceError,
    },
    shared::extraction::file_extract::{UploadAdmissionError, UploadRejectionDetails},
    shared::secret_encryption::SecretEncryptionError,
};

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

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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
    IdempotencyConflict(String),
    #[error("{message}")]
    DuplicateContent { message: String, existing_document_id: Uuid },
    #[error("conflict: {0}")]
    MissingPrice(String),
    #[error("conflict: {0}")]
    KnowledgeNotReady(String),
    #[error("service unavailable: {message}")]
    ServiceUnavailable { message: String, kind: &'static str },
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
        details: Box<UploadRejectionDetails>,
    },
    #[error("internal server error")]
    Internal,
    #[error("internal server error: {0}")]
    InternalMessage(String),
}

impl ApiError {
    #[must_use]
    pub fn credential_encryption_writes_disabled() -> Self {
        Self::service_unavailable(
            "credential writes are disabled until the encrypted-storage rollout is activated",
            "credential_encryption_writes_disabled",
        )
    }

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

    const fn status_code(&self) -> StatusCode {
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
            | Self::IdempotencyConflict(_)
            | Self::DuplicateContent { .. }
            | Self::MissingPrice(_)
            | Self::KnowledgeNotReady(_)
            | Self::GraphWriteContention(_)
            | Self::GraphPersistenceIntegrity(_)
            | Self::SettlementRefreshFailed(_) => StatusCode::CONFLICT,
            Self::ProviderFailure(_) => StatusCode::BAD_GATEWAY,
            Self::ServiceUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
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
            Self::IdempotencyConflict(_) => "idempotency_conflict",
            Self::DuplicateContent { .. } => "duplicate_content",
            Self::MissingPrice(_) => "missing_price",
            Self::KnowledgeNotReady(_) => "knowledge_not_ready",
            Self::ServiceUnavailable { kind, .. } => kind,
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
    fn extensions(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let value = match self {
            Self::UploadRejected { details, .. } => serde_json::to_value(details.as_ref()),
            Self::DuplicateContent { existing_document_id, .. } => {
                serde_json::to_value(serde_json::json!({
                    "existingDocumentId": existing_document_id,
                }))
            }
            _ => return None,
        };
        value.ok().and_then(|value| value.as_object().cloned())
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
            details: Box::new(error.details().clone()),
        }
    }
}

pub(crate) fn record_error_kind(error_kind: &'static str) {
    Span::current().record("error.kind", error_kind);
}

impl From<ContentServiceError> for ApiError {
    fn from(error: ContentServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            ContentServiceError::NotFound { message } => Self::NotFound(message),
            ContentServiceError::InvalidRequest { message } => Self::BadRequest(message),
            ContentServiceError::StateConflict { message } => Self::Conflict(message),
            ContentServiceError::StorageUnavailable { message } => {
                Self::ServiceUnavailable { message, kind: "content_storage_unavailable" }
            }
            ContentServiceError::ProviderUnavailable { message } => Self::ProviderFailure(message),
            ContentServiceError::Cancelled => {
                Self::Conflict("content operation cancelled".to_string())
            }
            ContentServiceError::Repository(error) => {
                Self::internal_with_log(error, "content service repository failure")
            }
            ContentServiceError::Internal(error) => {
                Self::internal_with_log(error, "content service internal failure")
            }
        }
    }
}

impl From<GraphServiceError> for ApiError {
    fn from(error: GraphServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            GraphServiceError::LibraryNotFound { library_id } => {
                Self::resource_not_found("library", library_id)
            }
            GraphServiceError::NotFound { message } => Self::NotFound(message),
            GraphServiceError::StateConflict { message } => Self::Conflict(message),
            GraphServiceError::WriteContention { message } => Self::GraphWriteContention(message),
            GraphServiceError::PersistenceIntegrity { message } => {
                Self::GraphPersistenceIntegrity(message)
            }
            GraphServiceError::ProviderUnavailable { message } => Self::ProviderFailure(message),
            GraphServiceError::Cancelled => Self::Conflict("graph operation cancelled".to_string()),
            GraphServiceError::Repository(error) => {
                Self::internal_with_log(error, "graph service repository failure")
            }
            GraphServiceError::Internal(error) => {
                Self::internal_with_log(error, "graph service internal failure")
            }
        }
    }
}

impl From<IngestServiceError> for ApiError {
    fn from(error: IngestServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            IngestServiceError::LibraryNotFound { library_id } => {
                Self::resource_not_found("library", library_id)
            }
            IngestServiceError::BindingNotConfigured { message }
            | IngestServiceError::StateConflict { message } => Self::Conflict(message),
            IngestServiceError::ProviderUnavailable { message } => Self::ProviderFailure(message),
            IngestServiceError::Cancelled => {
                Self::Conflict("ingest operation cancelled".to_string())
            }
            IngestServiceError::Repository(error) => {
                Self::internal_with_log(error, "ingest service repository failure")
            }
            IngestServiceError::Internal(error) => {
                Self::internal_with_log(error, "ingest service internal failure")
            }
        }
    }
}

impl From<KnowledgeServiceError> for ApiError {
    fn from(error: KnowledgeServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            KnowledgeServiceError::LibraryNotFound { library_id } => {
                Self::resource_not_found("library", library_id)
            }
            KnowledgeServiceError::NotFound { message } => Self::NotFound(message),
            KnowledgeServiceError::GraphNotReady { message } => Self::KnowledgeNotReady(message),
            KnowledgeServiceError::CacheUnavailable { message } => {
                Self::ServiceUnavailable { message, kind: "cache_unavailable" }
            }
            KnowledgeServiceError::Repository(error) => {
                Self::internal_with_log(error, "knowledge service repository failure")
            }
            KnowledgeServiceError::Internal(error) => {
                Self::internal_with_log(error, "knowledge service internal failure")
            }
        }
    }
}

impl From<QueryServiceError> for ApiError {
    fn from(error: QueryServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            QueryServiceError::LibraryNotFound { library_id } => {
                Self::resource_not_found("library", library_id)
            }
            QueryServiceError::NotFound { message } => Self::NotFound(message),
            QueryServiceError::BindingNotConfigured { message }
            | QueryServiceError::StateConflict { message } => Self::Conflict(message),
            QueryServiceError::ProviderUnavailable { message } => Self::ProviderFailure(message),
            QueryServiceError::CacheUnavailable { message } => {
                Self::ServiceUnavailable { message, kind: "cache_unavailable" }
            }
            QueryServiceError::Cancelled => Self::Conflict("query operation cancelled".to_string()),
            QueryServiceError::DeadlineExceeded => Self::query_deadline_exceeded(),
            QueryServiceError::Repository(error) => {
                Self::internal_with_log(error, "query service repository failure")
            }
            QueryServiceError::Internal(error) => {
                Self::internal_with_log(error, "query service internal failure")
            }
        }
    }
}

impl From<WebhookServiceError> for ApiError {
    fn from(error: WebhookServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            WebhookServiceError::DeliveryAttemptNotFound { job_id } => {
                Self::resource_not_found("webhook_delivery_attempt_for_job", job_id)
            }
            WebhookServiceError::SubscriptionNotFound { subscription_id } => {
                Self::resource_not_found("webhook_subscription", subscription_id)
            }
            WebhookServiceError::DeliveryLeaseInFlight { retry_at, .. } => Self::Conflict(format!(
                "webhook delivery lease is still in flight until {retry_at}"
            )),
            WebhookServiceError::DeliveryCanceled { .. } => {
                Self::Conflict("webhook delivery operation cancelled".to_string())
            }
            WebhookServiceError::StateConflict { message } => Self::Conflict(message),
            WebhookServiceError::Repository(_) => {
                tracing::error!(
                    error_kind = "WebhookServiceError::Repository",
                    "webhook service repository failure (detail redacted)"
                );
                Self::Internal
            }
            WebhookServiceError::CredentialProtection(error) => Self::from_secret_encryption(error),
            WebhookServiceError::Internal(_) => {
                tracing::error!(
                    error_kind = "WebhookServiceError::Internal",
                    "webhook service internal failure (detail redacted)"
                );
                Self::Internal
            }
        }
    }
}

pub fn map_runtime_lifecycle_error(error: AnyhowError) -> ApiError {
    let error = match error.downcast::<ApiError>() {
        Ok(error) => return error,
        Err(error) => error,
    };
    let error = match error.downcast::<ContentServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<GraphServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<IngestServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<KnowledgeServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<QueryServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<WebhookServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    error!(error = ?error, "runtime lifecycle handler failed with unexpected internal error");
    ApiError::Internal
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

#[must_use]
pub fn map_runtime_write_error(error: AnyhowError) -> ApiError {
    match error.downcast::<UploadAdmissionError>() {
        Ok(upload_error) => ApiError::from_upload_admission(upload_error),
        Err(error) => map_runtime_lifecycle_error(error),
    }
}

#[must_use]
pub fn map_workspace_create_error(error: SqlxError, slug: &str) -> ApiError {
    match error {
        SqlxError::Database(database_error) if database_error.is_unique_violation() => {
            ApiError::Conflict(format!("workspace slug '{slug}' already exists"))
        }
        _ => ApiError::Internal,
    }
}

#[must_use]
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

pub fn map_runtime_execution_row(
    row: runtime_repository::RuntimeExecutionRow,
) -> Result<RuntimeExecution, ApiError> {
    Ok(RuntimeExecution {
        id: row.id,
        owner_kind: row.owner_kind,
        owner_id: row.owner_id,
        task_kind: row.task_kind,
        surface_kind: row.surface_kind,
        contract_name: row.contract_name,
        contract_version: row.contract_version,
        lifecycle_state: row.lifecycle_state,
        active_stage: row.active_stage,
        turn_budget: row.turn_budget,
        turn_count: row.turn_count,
        parallel_action_limit: row.parallel_action_limit,
        failure_code: row.failure_code,
        failure_summary_redacted: row.failure_summary_redacted,
        accepted_at: row.accepted_at,
        completed_at: row.completed_at,
    })
}

pub fn map_runtime_stage_record_row(
    row: runtime_repository::RuntimeStageRecordRow,
) -> Result<RuntimeStageRecord, ApiError> {
    Ok(RuntimeStageRecord {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_kind: row.stage_kind,
        stage_ordinal: row.stage_ordinal,
        attempt_no: row.attempt_no,
        stage_state: row.stage_state,
        deterministic: row.deterministic,
        started_at: row.started_at,
        completed_at: row.completed_at,
        input_summary_json: row.input_summary_json,
        output_summary_json: row.output_summary_json,
        failure_code: row.failure_code,
        failure_summary_redacted: row.failure_summary_redacted,
    })
}

pub fn map_runtime_action_record_row(
    row: runtime_repository::RuntimeActionRecordRow,
) -> Result<RuntimeActionRecord, ApiError> {
    Ok(RuntimeActionRecord {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_record_id: row.stage_record_id,
        action_kind: row.action_kind,
        action_ordinal: row.action_ordinal,
        action_state: row.action_state,
        provider_binding_id: row.provider_binding_id,
        tool_name: row.tool_name,
        usage_json: row.usage_json,
        summary_json: row.summary_json,
        created_at: row.created_at,
    })
}

pub fn map_runtime_policy_decision_row(
    row: runtime_repository::RuntimePolicyDecisionRow,
) -> Result<RuntimePolicyDecision, ApiError> {
    Ok(RuntimePolicyDecision {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_record_id: row.stage_record_id,
        action_record_id: row.action_record_id,
        target_kind: row.target_kind,
        decision_kind: row.decision_kind,
        reason_code: row.reason_code,
        reason_summary_redacted: row.reason_summary_redacted,
        created_at: row.created_at,
    })
}

pub fn map_runtime_trace_view(
    execution: runtime_repository::RuntimeExecutionRow,
    stages: Vec<runtime_repository::RuntimeStageRecordRow>,
    actions: Vec<runtime_repository::RuntimeActionRecordRow>,
    policy_decisions: Vec<runtime_repository::RuntimePolicyDecisionRow>,
) -> Result<RuntimeExecutionTraceView, ApiError> {
    Ok(build_trace_view(
        map_runtime_execution_row(execution)?,
        stages.into_iter().map(map_runtime_stage_record_row).collect::<Result<Vec<_>, _>>()?,
        actions.into_iter().map(map_runtime_action_record_row).collect::<Result<Vec<_>, _>>()?,
        policy_decisions
            .into_iter()
            .map(map_runtime_policy_decision_row)
            .collect::<Result<Vec<_>, _>>()?,
    ))
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
pub fn graph_refresh_fallback_warning(message: impl Into<String>) -> ApiWarningBody {
    ApiWarningBody { warning: message.into(), warning_kind: "graph_refresh_fallback" }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let code = self.kind();
        let message = self.to_string();
        let request_id = None::<String>;
        let extensions = self.extensions();

        if status.is_server_error() {
            error!(
                %status,
                error_kind = code,
                error_message = %message,
                request_id = request_id.as_deref().unwrap_or("-"),
                "http request failed in handler",
            );
        } else {
            warn!(
                %status,
                error_kind = code,
                error_message = %message,
                request_id = request_id.as_deref().unwrap_or("-"),
                "http request rejected in handler",
            );
        }

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
    use std::{borrow::Cow, error::Error as StdError, fmt};

    use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
    use chrono::Utc;
    use sqlx::error::{DatabaseError, ErrorKind};
    use uuid::Uuid;

    use super::{
        ApiError, extraction_recovery_warning, graph_refresh_fallback_warning,
        map_library_create_error, map_runtime_lifecycle_error, map_runtime_upload_error,
        map_workspace_create_error, query_intent_degradation_warning, rerank_failure_warning,
    };
    use crate::{
        services::{graph::error::GraphServiceError, webhook::error::WebhookServiceError},
        shared::extraction::file_extract::UploadAdmissionError,
    };

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
    fn maps_typed_graph_write_contention_to_specific_kind() {
        let error =
            map_runtime_lifecycle_error(anyhow::Error::new(GraphServiceError::WriteContention {
                message: "opaque".to_string(),
            }));
        assert!(matches!(error, ApiError::GraphWriteContention(_)));
    }

    #[test]
    fn unknown_runtime_lifecycle_message_is_always_internal() {
        let error = map_runtime_lifecycle_error(anyhow::anyhow!(
            "stale revision conflict provider failure upstream timeout missing price deadlock"
        ));

        assert!(matches!(error, ApiError::Internal));
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
    fn internal_message_preserves_internal_kind_and_explicit_text() {
        let error = ApiError::InternalMessage("knowledge mirror sync failed".to_string());
        assert_eq!(error.kind(), "internal");
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.to_string(), "internal server error: knowledge mirror sync failed");
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
            Some(super::PROBLEM_JSON_CONTENT_TYPE),
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
        assert_eq!(super::humanize_problem_title("bad_request"), "Bad Request");
        assert_eq!(super::humanize_problem_title("duplicate_content"), "Duplicate Content");
        assert_eq!(super::humanize_problem_title("internal"), "Internal");
    }

    #[test]
    fn maps_webhook_delivery_lease_contention_to_conflict_without_internal_ids() {
        let retry_at = Utc::now();
        let attempt_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let error = ApiError::from(WebhookServiceError::DeliveryLeaseInFlight {
            attempt_id,
            job_id,
            retry_at,
        });

        assert_eq!(error.kind(), "conflict");
        assert_eq!(error.status_code(), StatusCode::CONFLICT);
        assert!(error.to_string().contains(&retry_at.to_string()));
        assert!(!error.to_string().contains(&attempt_id.to_string()));
        assert!(!error.to_string().contains(&job_id.to_string()));
    }

    #[test]
    fn maps_webhook_delivery_cancellation_to_conflict_without_internal_ids() {
        let attempt_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let error = ApiError::from(WebhookServiceError::DeliveryCanceled { attempt_id, job_id });

        assert_eq!(error.kind(), "conflict");
        assert_eq!(error.status_code(), StatusCode::CONFLICT);
        assert_eq!(error.to_string(), "conflict: webhook delivery operation cancelled");
        assert!(!error.to_string().contains(&attempt_id.to_string()));
        assert!(!error.to_string().contains(&job_id.to_string()));
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
    fn builds_graph_refresh_fallback_warning() {
        let warning = graph_refresh_fallback_warning("targeted refresh fell back to broad rebuild");
        assert_eq!(warning.warning_kind, "graph_refresh_fallback");
    }

    #[test]
    fn builds_typed_not_found_error_messages() {
        let error = ApiError::resource_not_found("workspace", Uuid::nil());
        assert_eq!(error.kind(), "not_found");
        assert!(error.to_string().contains("workspace"));
    }
}
