use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// Canonical status values for `OpsAsyncOperation`. These match the
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

// Canonical operation_kind values for `content_mutation`.
pub const MUTATION_KIND_DELETE: &str = "delete";
pub const MUTATION_KIND_EDIT: &str = "edit";
pub const MUTATION_KIND_REPLACE: &str = "replace";

// Graph projection status values.
pub const GRAPH_STATUS_READY: &str = "ready";
pub const GRAPH_STATUS_EMPTY: &str = "empty";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unavailable,
    Misconfigured,
    Blocked,
}

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
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
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
