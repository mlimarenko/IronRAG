use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::{
    agent_runtime::{
        RuntimeActionKind, RuntimeActionState, RuntimeDecisionKind, RuntimeDecisionTargetKind,
        RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimePolicySummary, RuntimeStageKind,
        RuntimeStageState, RuntimeSurfaceKind, RuntimeTaskKind,
    },
    ai::AiBindingPurpose,
};

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
pub struct McpLibraryIngestionReadiness {
    pub ready: bool,
    pub missing_binding_purposes: Vec<AiBindingPurpose>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpLibraryDescriptor {
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub ingestion_readiness: McpLibraryIngestionReadiness,
    pub document_count: usize,
    pub readable_document_count: usize,
    pub processing_document_count: usize,
    pub failed_document_count: usize,
    pub document_counts_by_readiness: BTreeMap<String, usize>,
    pub graph_ready_document_count: usize,
    pub graph_sparse_document_count: usize,
    pub typed_fact_document_count: usize,
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
    #[serde(default, alias = "include_references")]
    pub include_references: Option<bool>,
}

impl McpSearchDocumentsRequest {
    #[must_use]
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
pub struct McpTechnicalFactReference {
    pub fact_id: Uuid,
    pub fact_kind: String,
    pub canonical_value: String,
    pub display_value: String,
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
    pub readiness_kind: String,
    pub graph_coverage_kind: String,
    pub status_reason: Option<String>,
    pub chunk_references: Vec<McpChunkReference>,
    pub technical_fact_references: Vec<McpTechnicalFactReference>,
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
    #[serde(default, alias = "include_references")]
    pub include_references: Option<bool>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGetRuntimeExecutionRequest {
    #[serde(alias = "execution_id")]
    pub execution_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGetRuntimeExecutionTraceRequest {
    #[serde(alias = "execution_id")]
    pub execution_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSubmitWebIngestRunRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    #[serde(alias = "seed_url")]
    pub seed_url: String,
    pub mode: String,
    #[serde(default, alias = "boundary_policy")]
    pub boundary_policy: Option<String>,
    #[serde(default, alias = "max_depth")]
    pub max_depth: Option<i32>,
    #[serde(default, alias = "max_pages")]
    pub max_pages: Option<i32>,
    #[serde(default, alias = "idempotency_key")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGetWebIngestRunRequest {
    #[serde(alias = "run_id")]
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpListWebIngestRunPagesRequest {
    #[serde(alias = "run_id")]
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCancelWebIngestRunRequest {
    #[serde(alias = "run_id")]
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAskRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    pub question: String,
    #[serde(default, alias = "top_k")]
    pub top_k: Option<usize>,
    #[serde(default, alias = "include_evidence")]
    pub include_evidence: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAskResponse {
    pub answer: String,
    pub verification_state: Option<String>,
    pub source_count: usize,
    pub entity_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSearchEntitiesRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpListDocumentsRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    pub limit: Option<usize>,
    #[serde(default, alias = "status_filter")]
    pub status_filter: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDeleteDocumentRequest {
    #[serde(alias = "document_id")]
    pub document_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGetGraphTopologyRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpListRelationsRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGetCommunitiesRequest {
    #[serde(alias = "library_id")]
    pub library_id: Uuid,
    pub limit: Option<usize>,
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
    ListDocuments,
    DeleteDocument,
    Ask,
    CreateWorkspace,
    CreateLibrary,
    UploadDocuments,
    UpdateDocument,
    GetMutationStatus,
    GetRuntimeExecution,
    GetRuntimeExecutionTrace,
    SubmitWebIngestRun,
    GetWebIngestRun,
    ListWebIngestRunPages,
    CancelWebIngestRun,
    SearchEntities,
    GetGraphTopology,
    ListRelations,
    GetCommunities,
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
    pub readiness_kind: String,
    pub graph_coverage_kind: String,
    pub status_reason: Option<String>,
    pub content: Option<String>,
    pub slice_start_offset: usize,
    pub slice_end_offset: usize,
    pub total_content_length: Option<usize>,
    pub continuation_token: Option<String>,
    pub has_more: bool,
    pub chunk_references: Vec<McpChunkReference>,
    pub technical_fact_references: Vec<McpTechnicalFactReference>,
    pub entity_references: Vec<McpEntityReference>,
    pub relation_references: Vec<McpRelationReference>,
    pub evidence_references: Vec<McpEvidenceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRuntimeExecutionSummary {
    pub runtime_execution_id: Uuid,
    pub owner_kind: RuntimeExecutionOwnerKind,
    pub owner_id: Uuid,
    pub task_kind: RuntimeTaskKind,
    pub surface_kind: RuntimeSurfaceKind,
    pub contract_name: String,
    pub contract_version: String,
    pub lifecycle_state: RuntimeLifecycleState,
    pub active_stage: Option<RuntimeStageKind>,
    pub turn_budget: i32,
    pub turn_count: i32,
    pub parallel_action_limit: i32,
    pub failure_code: Option<String>,
    pub failure_summary: Option<String>,
    pub policy_summary: RuntimePolicySummary,
    pub accepted_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRuntimeStageSummary {
    pub stage_record_id: Uuid,
    pub stage_kind: RuntimeStageKind,
    pub stage_ordinal: i32,
    pub attempt_no: i32,
    pub stage_state: RuntimeStageState,
    pub deterministic: bool,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub input_summary: serde_json::Value,
    pub output_summary: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRuntimeActionSummary {
    pub action_id: Uuid,
    pub stage_record_id: Uuid,
    pub action_kind: RuntimeActionKind,
    pub action_ordinal: i32,
    pub action_state: RuntimeActionState,
    pub provider_binding_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub usage: Option<serde_json::Value>,
    pub summary: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRuntimePolicySummary {
    pub decision_id: Uuid,
    pub stage_record_id: Option<Uuid>,
    pub action_record_id: Option<Uuid>,
    pub target_kind: RuntimeDecisionTargetKind,
    pub decision_kind: RuntimeDecisionKind,
    pub reason_code: String,
    pub reason_summary: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRuntimeExecutionTrace {
    pub execution: McpRuntimeExecutionSummary,
    pub stages: Vec<McpRuntimeStageSummary>,
    pub actions: Vec<McpRuntimeActionSummary>,
    pub policy_decisions: Vec<McpRuntimePolicySummary>,
}

#[cfg(test)]
mod tests {
    use super::{
        McpCancelWebIngestRunRequest, McpGetMutationStatusRequest, McpGetRuntimeExecutionRequest,
        McpGetRuntimeExecutionTraceRequest, McpGetWebIngestRunRequest,
        McpListWebIngestRunPagesRequest, McpReadDocumentRequest, McpSearchDocumentsRequest,
        McpSubmitWebIngestRunRequest, McpUpdateDocumentRequest, McpUploadDocumentInput,
        McpUploadDocumentsRequest,
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
    fn runtime_execution_request_accepts_snake_case_field() {
        let execution_id = Uuid::now_v7();
        let request: McpGetRuntimeExecutionRequest = serde_json::from_value(json!({
            "execution_id": execution_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.execution_id, execution_id);
    }

    #[test]
    fn runtime_execution_trace_request_accepts_snake_case_field() {
        let execution_id = Uuid::now_v7();
        let request: McpGetRuntimeExecutionTraceRequest = serde_json::from_value(json!({
            "execution_id": execution_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.execution_id, execution_id);
    }

    #[test]
    fn submit_web_ingest_run_request_accepts_snake_case_fields() {
        let library_id = Uuid::now_v7();
        let request: McpSubmitWebIngestRunRequest = serde_json::from_value(json!({
            "library_id": library_id,
            "seed_url": "https://example.com/docs",
            "mode": "single_page",
            "boundary_policy": "same_host",
            "max_depth": 0,
            "max_pages": 1,
            "idempotency_key": "web-seed-1"
        }))
        .expect("request should deserialize");

        assert_eq!(request.library_id, library_id);
        assert_eq!(request.seed_url, "https://example.com/docs");
        assert_eq!(request.mode, "single_page");
        assert_eq!(request.boundary_policy.as_deref(), Some("same_host"));
        assert_eq!(request.max_depth, Some(0));
        assert_eq!(request.max_pages, Some(1));
        assert_eq!(request.idempotency_key.as_deref(), Some("web-seed-1"));
    }

    #[test]
    fn get_web_ingest_run_request_accepts_snake_case_field() {
        let run_id = Uuid::now_v7();
        let request: McpGetWebIngestRunRequest = serde_json::from_value(json!({
            "run_id": run_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.run_id, run_id);
    }

    #[test]
    fn list_web_ingest_run_pages_request_accepts_snake_case_field() {
        let run_id = Uuid::now_v7();
        let request: McpListWebIngestRunPagesRequest = serde_json::from_value(json!({
            "run_id": run_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.run_id, run_id);
    }

    #[test]
    fn cancel_web_ingest_run_request_accepts_snake_case_field() {
        let run_id = Uuid::now_v7();
        let request: McpCancelWebIngestRunRequest = serde_json::from_value(json!({
            "run_id": run_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.run_id, run_id);
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
    pub accepted_at: DateTime<Utc>,
    pub last_status_at: DateTime<Utc>,
    pub failure_kind: Option<String>,
}
