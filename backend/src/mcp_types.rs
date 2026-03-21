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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpListLibrariesRequest {
    #[serde(default, alias = "workspace_id")]
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSearchDocumentsRequest {
    pub query: String,
    #[serde(default, alias = "library_ids")]
    pub library_ids: Option<Vec<Uuid>>,
    #[serde(default, alias = "library_id")]
    pub library_id: Option<Uuid>,
    pub limit: Option<usize>,
}

impl McpSearchDocumentsRequest {
    pub fn requested_library_ids(&self) -> Option<Vec<Uuid>> {
        match (&self.library_ids, self.library_id) {
            (Some(library_ids), Some(library_id)) => {
                let mut requested = library_ids.clone();
                if !requested.contains(&library_id) {
                    requested.push(library_id);
                }
                Some(requested)
            }
            (Some(library_ids), None) => Some(library_ids.clone()),
            (None, Some(library_id)) => Some(vec![library_id]),
            (None, None) => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCreateWorkspaceRequest {
    pub slug: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCreateLibraryRequest {
    #[serde(alias = "workspace_id")]
    pub workspace_id: Uuid,
    pub slug: Option<String>,
    pub name: String,
    pub description: Option<String>,
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
pub struct McpChunkReference {
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpEntityReference {
    pub entity_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRelationReference {
    pub relation_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpEvidenceReference {
    pub evidence_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
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
    pub chunk_references: Vec<McpChunkReference>,
    pub entity_references: Vec<McpEntityReference>,
    pub relation_references: Vec<McpRelationReference>,
    pub evidence_references: Vec<McpEvidenceReference>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpReadDocumentRequest {
    #[serde(default, alias = "document_id")]
    pub document_id: Option<Uuid>,
    pub mode: Option<McpReadMode>,
    #[serde(default, alias = "start_offset")]
    pub start_offset: Option<usize>,
    pub length: Option<usize>,
    #[serde(default, alias = "continuation_token")]
    pub continuation_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUploadDocumentInput {
    #[serde(default, alias = "file_name")]
    pub file_name: Option<String>,
    #[serde(default, alias = "content_base64")]
    pub content_base64: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default, alias = "source_type")]
    pub source_type: Option<String>,
    #[serde(default, alias = "source_uri")]
    pub source_uri: Option<String>,
    #[serde(default, alias = "mime_type")]
    pub mime_type: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUploadDocumentsRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    #[serde(default, alias = "idempotency_key")]
    pub idempotency_key: Option<String>,
    pub documents: Vec<McpUploadDocumentInput>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDocumentMutationKind {
    Append,
    Replace,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUpdateDocumentRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    #[serde(alias = "document_id")]
    pub document_id: Uuid,
    #[serde(alias = "operation_kind")]
    pub operation_kind: McpDocumentMutationKind,
    #[serde(default, alias = "idempotency_key")]
    pub idempotency_key: Option<String>,
    #[serde(default, alias = "appended_text")]
    pub appended_text: Option<String>,
    #[serde(default, alias = "replacement_file_name")]
    pub replacement_file_name: Option<String>,
    #[serde(default, alias = "replacement_content_base64")]
    pub replacement_content_base64: Option<String>,
    #[serde(default, alias = "replacement_mime_type")]
    pub replacement_mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGetMutationStatusRequest {
    #[serde(alias = "receipt_id")]
    pub receipt_id: Uuid,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuditStatus {
    Succeeded,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Default)]
pub struct McpAuditScope {
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpReadDocumentResponse {
    pub document_id: Uuid,
    pub document_title: String,
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
    pub chunk_references: Vec<McpChunkReference>,
    pub entity_references: Vec<McpEntityReference>,
    pub relation_references: Vec<McpRelationReference>,
    pub evidence_references: Vec<McpEvidenceReference>,
}

#[cfg(test)]
mod tests {
    use super::{
        McpGetMutationStatusRequest, McpReadDocumentRequest, McpSearchDocumentsRequest,
        McpUpdateDocumentRequest, McpUploadDocumentInput, McpUploadDocumentsRequest,
    };
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn search_documents_request_accepts_snake_case_library_id() {
        let request: McpSearchDocumentsRequest = serde_json::from_value(json!({
            "query": "alpha",
            "library_id": Uuid::nil(),
            "limit": 3
        }))
        .expect("request should deserialize");

        assert_eq!(request.requested_library_ids(), Some(vec![Uuid::nil()]));
    }

    #[test]
    fn read_document_request_accepts_snake_case_fields() {
        let document_id = Uuid::now_v7();
        let request: McpReadDocumentRequest = serde_json::from_value(json!({
            "document_id": document_id,
            "start_offset": 12,
            "continuation_token": "token"
        }))
        .expect("request should deserialize");

        assert_eq!(request.document_id, Some(document_id));
        assert_eq!(request.start_offset, Some(12));
        assert_eq!(request.continuation_token.as_deref(), Some("token"));
    }

    #[test]
    fn upload_documents_request_accepts_snake_case_fields() {
        let library_id = Uuid::now_v7();
        let request: McpUploadDocumentsRequest = serde_json::from_value(json!({
            "library_id": library_id,
            "idempotency_key": "idem",
            "documents": [{
                "file_name": "demo.txt",
                "content_base64": "ZGVtbw==",
                "mime_type": "text/plain"
            }]
        }))
        .expect("request should deserialize");

        assert_eq!(request.library_id, library_id);
        assert_eq!(request.idempotency_key.as_deref(), Some("idem"));
        assert_eq!(request.documents.len(), 1);
    }

    #[test]
    fn upload_documents_request_accepts_inline_body_fields() {
        let library_id = Uuid::now_v7();
        let request: McpUploadDocumentsRequest = serde_json::from_value(json!({
            "library_id": library_id,
            "documents": [{
                "body": "hello world",
                "source_type": "inline",
                "title": "Inline note"
            }]
        }))
        .expect("request should deserialize");

        assert_eq!(request.library_id, library_id);
        assert_eq!(request.documents.len(), 1);
        assert_eq!(request.documents[0].body.as_deref(), Some("hello world"));
        assert_eq!(request.documents[0].source_type.as_deref(), Some("inline"));
    }

    #[test]
    fn update_document_request_accepts_snake_case_fields() {
        let request: McpUpdateDocumentRequest = serde_json::from_value(json!({
            "library_id": Uuid::nil(),
            "document_id": Uuid::now_v7(),
            "operation_kind": "append",
            "appended_text": "hello"
        }))
        .expect("request should deserialize");

        assert_eq!(request.appended_text.as_deref(), Some("hello"));
    }

    #[test]
    fn mutation_status_request_accepts_snake_case_field() {
        let receipt_id = Uuid::now_v7();
        let request: McpGetMutationStatusRequest = serde_json::from_value(json!({
            "receipt_id": receipt_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.receipt_id, receipt_id);
    }

    #[test]
    fn upload_document_input_accepts_snake_case_fields() {
        let input: McpUploadDocumentInput = serde_json::from_value(json!({
            "file_name": "demo.txt",
            "content_base64": "ZGVtbw==",
            "mime_type": "text/plain"
        }))
        .expect("input should deserialize");

        assert_eq!(input.file_name.as_deref(), Some("demo.txt"));
        assert_eq!(input.content_base64.as_deref(), Some("ZGVtbw=="));
        assert_eq!(input.mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn upload_document_input_accepts_inline_body_fields() {
        let input: McpUploadDocumentInput = serde_json::from_value(json!({
            "body": "demo",
            "source_uri": "memory://demo.txt",
            "mime_type": "text/plain"
        }))
        .expect("input should deserialize");

        assert_eq!(input.body.as_deref(), Some("demo"));
        assert_eq!(input.source_uri.as_deref(), Some("memory://demo.txt"));
        assert_eq!(input.mime_type.as_deref(), Some("text/plain"));
    }
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
