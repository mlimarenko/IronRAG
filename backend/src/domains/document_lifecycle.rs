use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentRevisionKind {
    InitialUpload,
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentRevisionStatus {
    Pending,
    Active,
    Superseded,
    Deleted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentMutationKind {
    UpdateAppend,
    UpdateReplace,
    Retry,
    Reprocess,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentMutationStatus {
    Accepted,
    Reconciling,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentAttemptKind {
    InitialUpload,
    UpdateAppend,
    UpdateReplace,
    Retry,
    Reprocess,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRevision {
    pub id: Uuid,
    pub document_id: Uuid,
    pub revision_no: i32,
    pub revision_kind: DocumentRevisionKind,
    pub parent_revision_id: Option<Uuid>,
    pub status: DocumentRevisionStatus,
    pub source_file_name: String,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub content_hash: Option<String>,
    pub accepted_at: DateTime<Utc>,
    pub activated_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMutationWorkflow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub target_revision_id: Option<Uuid>,
    pub mutation_kind: DocumentMutationKind,
    pub status: DocumentMutationStatus,
    pub stale_guard_revision_no: Option<i32>,
    pub requested_by: Option<String>,
    pub accepted_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}
