use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unavailable,
    Misconfigured,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsAsyncOperation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub operation_kind: String,
    pub status: String,
    pub surface_kind: Option<String>,
    pub subject_kind: Option<String>,
    pub subject_id: Option<Uuid>,
    pub failure_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpsLibraryWarning {
    pub id: Uuid,
    pub library_id: Uuid,
    pub warning_kind: String,
    pub severity: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}
