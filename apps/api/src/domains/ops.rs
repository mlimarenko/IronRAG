use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// Supported status values for `OpsAsyncOperation`. These match the
// Postgres TEXT values stored in `ops_async_operation.status`; using
// the constants instead of bare string literals makes typos a compile
// error and lets `grep` surface every comparison site.
pub const ASYNC_OP_STATUS_PROCESSING: &str = "processing";
pub const ASYNC_OP_STATUS_READY: &str = "ready";
pub const ASYNC_OP_STATUS_FAILED: &str = "failed";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpsAsyncOperationStatus {
    Accepted,
    Processing,
    Ready,
    Failed,
    Superseded,
    Canceled,
}

#[derive(Debug, Error)]
#[error("unknown ops_async_operation.status `{value}`")]
pub struct UnknownOpsAsyncOperationStatus {
    value: String,
}

impl UnknownOpsAsyncOperationStatus {
    fn new(value: &str) -> Self {
        Self { value: value.to_string() }
    }
}

impl OpsAsyncOperationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Processing => ASYNC_OP_STATUS_PROCESSING,
            Self::Ready => ASYNC_OP_STATUS_READY,
            Self::Failed => ASYNC_OP_STATUS_FAILED,
            Self::Superseded => "superseded",
            Self::Canceled => "canceled",
        }
    }

    pub fn from_db(value: &str) -> Result<Self, UnknownOpsAsyncOperationStatus> {
        match value {
            "accepted" => Ok(Self::Accepted),
            ASYNC_OP_STATUS_PROCESSING => Ok(Self::Processing),
            ASYNC_OP_STATUS_READY => Ok(Self::Ready),
            ASYNC_OP_STATUS_FAILED => Ok(Self::Failed),
            "superseded" => Ok(Self::Superseded),
            "canceled" => Ok(Self::Canceled),
            _ => Err(UnknownOpsAsyncOperationStatus::new(value)),
        }
    }
}

/// Server-authoritative terminal disposition of a failed operation.
///
/// Only the operation finalisation that owns an authoritative ingest failure
/// classification writes this: `fail_content_ingest_attempt` maps the
/// already-computed `ContentIngestFailureClassification` into it. `null` on the
/// row is the honest answer whenever the server holds no terminal proof — an
/// older row created before the column existed, an operation that has not
/// settled, or an operation kind whose failure path is out of scope (web runs,
/// deletes, query executions).
///
/// `content_unsupported` is the only value a connector may treat as
/// `skipped_unsupported`: it marks a deterministic content admission/rejection
/// (a byte-level `UnsupportedBinary`, `InvalidUtf8` or malformed local content
/// — never the `upload_extraction_failed` code as a whole, which now also
/// carries operational failures). A retryable failure that exhausted its
/// attempt budget is `retry_exhausted`; any other terminal storage,
/// configuration or worker failure is `operational_terminal`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpsFailureDisposition {
    ContentUnsupported,
    RetryExhausted,
    OperationalTerminal,
}

#[derive(Debug, Error)]
#[error("unknown ops_async_operation.failure_disposition `{value}`")]
pub struct UnknownOpsFailureDisposition {
    value: String,
}

impl OpsFailureDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContentUnsupported => "content_unsupported",
            Self::RetryExhausted => "retry_exhausted",
            Self::OperationalTerminal => "operational_terminal",
        }
    }

    /// The one mapping from a producer-assigned extraction class to the public
    /// disposition, shared by the asynchronous operation finalisation and the
    /// synchronous admission problem. Only a deterministic content rejection is
    /// `content_unsupported`; a request-level rejection (`None`: malformed,
    /// truncated or oversized body) carries no content verdict and is
    /// operational-terminal.
    #[must_use]
    pub const fn from_extraction_failure_class(
        class: Option<crate::shared::extraction::file_extract::ExtractionFailureClass>,
    ) -> Self {
        use crate::shared::extraction::file_extract::ExtractionFailureClass;
        match class {
            Some(ExtractionFailureClass::ContentInvalid) => Self::ContentUnsupported,
            Some(ExtractionFailureClass::OperationalRetryable) => Self::RetryExhausted,
            Some(ExtractionFailureClass::OperationalTerminal) | None => Self::OperationalTerminal,
        }
    }

    pub fn from_db(value: &str) -> Result<Self, UnknownOpsFailureDisposition> {
        match value {
            "content_unsupported" => Ok(Self::ContentUnsupported),
            "retry_exhausted" => Ok(Self::RetryExhausted),
            "operational_terminal" => Ok(Self::OperationalTerminal),
            _ => Err(UnknownOpsFailureDisposition { value: value.to_string() }),
        }
    }
}

// `content_mutation` operation_kind value that branches deletion handling.
pub const MUTATION_KIND_DELETE: &str = "delete";

// Graph projection status values.
pub const GRAPH_STATUS_READY: &str = "ready";
pub const GRAPH_STATUS_EMPTY: &str = "empty";

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpsAsyncOperation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub operation_kind: String,
    pub status: OpsAsyncOperationStatus,
    pub surface_kind: Option<String>,
    pub subject_kind: Option<String>,
    pub subject_id: Option<Uuid>,
    pub parent_async_operation_id: Option<Uuid>,
    pub failure_code: Option<String>,
    /// Server-authoritative terminal disposition, written only by the ingest
    /// failure finalisation that owns the classification. `null` means the
    /// server holds no terminal proof for this operation.
    pub failure_disposition: Option<OpsFailureDisposition>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// What the operation produced, for the kinds that produce something a
    /// caller cannot reconstruct afterwards.
    ///
    /// A poller that is told `ready` otherwise learns only that the work
    /// stopped. For a snapshot import that is not enough: a restore that moved
    /// every row and one that silently moved none settle identically. The shape
    /// is per `operation_kind` and deliberately untyped here — the kind that
    /// writes it owns the schema — and `null` is the honest answer for every
    /// kind that publishes nothing and for every operation still in flight.
    #[schema(value_type = Option<Object>)]
    pub result_json: Option<serde_json::Value>,
}

/// Aggregated child-operation counts for a parent batch `OpsAsyncOperation`.
/// Returned alongside the parent row so polling clients can render
/// "completed / total" progress in a single response.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpsAsyncOperationProgress {
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
    pub in_flight: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OpsLibraryState {
    pub library_id: Uuid,
    pub queue_depth: i64,
    pub running_attempts: i64,
    pub readable_document_count: i64,
    pub failed_document_count: i64,
    pub degraded_state: String,
    pub latest_knowledge_generation_id: Option<Uuid>,
    pub knowledge_generation_state: Option<String>,
    pub last_recomputed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OpsLibraryWarning {
    pub id: Uuid,
    pub library_id: Uuid,
    pub warning_kind: String,
    pub severity: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}
