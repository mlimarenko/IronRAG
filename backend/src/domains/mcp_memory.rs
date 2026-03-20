use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilitySnapshot {
    pub token_id: Uuid,
    pub token_kind: String,
    pub workspace_scope: Option<Uuid>,
    pub visible_workspace_count: usize,
    pub visible_library_count: usize,
    pub tools: Vec<String>,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpWorkspaceDescriptor {
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub visible_library_count: usize,
    pub can_write_any_library: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpLibraryDescriptor {
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub document_count: usize,
    pub readable_document_count: usize,
    pub processing_document_count: usize,
    pub failed_document_count: usize,
    pub supports_search: bool,
    pub supports_read: bool,
    pub supports_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpReadabilityState {
    Readable,
    Processing,
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDocumentHit {
    pub document_id: Uuid,
    pub logical_document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub document_title: String,
    pub latest_revision_id: Option<Uuid>,
    pub score: f64,
    pub excerpt: Option<String>,
    pub excerpt_start_offset: Option<usize>,
    pub excerpt_end_offset: Option<usize>,
    pub readability_state: McpReadabilityState,
    pub status_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSearchDocumentsResponse {
    pub query: String,
    pub limit: usize,
    pub library_ids: Vec<Uuid>,
    pub hits: Vec<McpDocumentHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpReadMode {
    Full,
    Excerpt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpMutationOperationKind {
    Upload,
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpMutationReceiptStatus {
    Accepted,
    Processing,
    Ready,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuditActionKind {
    CapabilitySnapshot,
    ListWorkspaces,
    ListLibraries,
    SearchDocuments,
    ReadDocument,
    CreateWorkspace,
    CreateLibrary,
    UploadDocuments,
    UpdateDocument,
    GetMutationStatus,
    ReviewAudit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuditStatus {
    Pending,
    Succeeded,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpReadDocumentResponse {
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub latest_revision_id: Option<Uuid>,
    pub read_mode: McpReadMode,
    pub readability_state: McpReadabilityState,
    pub status_reason: Option<String>,
    pub content: Option<String>,
    pub slice_start_offset: usize,
    pub slice_end_offset: usize,
    pub total_content_length: Option<usize>,
    pub continuation_token: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMutationReceipt {
    pub receipt_id: Uuid,
    pub token_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Option<Uuid>,
    pub operation_kind: McpMutationOperationKind,
    pub idempotency_key: String,
    pub status: McpMutationReceiptStatus,
    pub runtime_tracking_id: Option<String>,
    pub accepted_at: DateTime<Utc>,
    pub last_status_at: DateTime<Utc>,
    pub failure_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAuditEvent {
    pub audit_event_id: Uuid,
    pub request_id: String,
    pub token_id: Uuid,
    pub token_kind: String,
    pub action_kind: McpAuditActionKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub status: McpAuditStatus,
    pub error_kind: Option<String>,
    pub created_at: DateTime<Utc>,
}
