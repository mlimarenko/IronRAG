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
    /// Omitted from the `initialize` response because agents never
    /// need the raw token UUID — it would only bloat the LLM context.
    /// Still populated for the HTTP capabilities endpoint used by
    /// admin dashboards.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<Uuid>,
    pub token_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_scope: Option<Uuid>,
    pub visible_workspace_count: usize,
    pub visible_library_count: usize,
    /// The full tool name list is already in the `tools/list` response.
    /// Repeating it in `initialize` doubles the context cost for zero
    /// information gain. Skipped when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<DateTime<Utc>>,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListLibrariesRequest {
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSearchDocumentsRequest {
    pub query: String,
    pub library_ids: Option<Vec<Uuid>>,
    pub limit: Option<usize>,
    pub include_references: Option<bool>,
}

/// Hard cap on library IDs per MCP request. Prevents an unbounded
/// clone + a fan-out that would turn one search call into an O(N)
/// database scatter. Agents rarely reference more than 5 libraries
/// in a single tool call; 50 gives headroom for batch-style scripts.
const MCP_MAX_LIBRARY_IDS: usize = 50;

impl McpSearchDocumentsRequest {
    #[must_use]
    pub fn requested_library_ids(&self) -> Option<Vec<Uuid>> {
        self.library_ids.as_ref().map(|ids| {
            if ids.len() > MCP_MAX_LIBRARY_IDS {
                ids[..MCP_MAX_LIBRARY_IDS].to_vec()
            } else {
                ids.clone()
            }
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCreateWorkspaceRequest {
    pub slug: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCreateLibraryRequest {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpContentSourceAccess {
    pub kind: String,
    pub href: String,
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

/// One search hit returned to an agent. Every optional/empty field is
/// elided from the serialized JSON so the agent's context window stays
/// tight — on a typical 3-hit response the bloated shape used to carry
/// ~1 KB of `null` / `[]` padding per hit. `logicalDocumentId` has been
/// dropped because it has always equaled `documentId`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDocumentHit {
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub document_title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_revision_id: Option<Uuid>,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt_start_offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt_end_offset: Option<usize>,
    /// Character offset of the best-matching chunk inside the full
    /// normalized document. Callers should pass this back to
    /// `read_document` as `startOffset` so the very first read window
    /// already lands on real content instead of the document's table
    /// of contents / front matter. `None` means the source chunks
    /// lack span info (legacy data) or no chunk matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_start_offset: Option<usize>,
    pub readability_state: McpReadabilityState,
    pub readiness_kind: String,
    pub graph_coverage_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunk_references: Vec<McpChunkReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub technical_fact_references: Vec<McpTechnicalFactReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_references: Vec<McpEntityReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relation_references: Vec<McpRelationReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpReadDocumentRequest {
    pub document_id: Option<Uuid>,
    pub mode: Option<McpReadMode>,
    pub start_offset: Option<usize>,
    pub length: Option<usize>,
    pub continuation_token: Option<String>,
    pub include_references: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpUploadDocumentInput {
    pub file_name: Option<String>,
    pub content_base64: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    pub source_type: Option<String>,
    pub source_uri: Option<String>,
    pub mime_type: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpUploadDocumentsRequest {
    pub library_id: Uuid,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpUpdateDocumentRequest {
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub operation_kind: McpDocumentMutationKind,
    pub idempotency_key: Option<String>,
    pub appended_text: Option<String>,
    pub replacement_file_name: Option<String>,
    pub replacement_content_base64: Option<String>,
    pub replacement_mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetMutationStatusRequest {
    pub receipt_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetRuntimeExecutionRequest {
    pub runtime_execution_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetRuntimeExecutionTraceRequest {
    pub runtime_execution_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSubmitWebIngestRunRequest {
    pub library_id: Uuid,
    pub seed_url: String,
    pub mode: String,
    pub boundary_policy: Option<String>,
    pub max_depth: Option<i32>,
    pub max_pages: Option<i32>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetWebIngestRunRequest {
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListWebIngestRunPagesRequest {
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCancelWebIngestRunRequest {
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSearchEntitiesRequest {
    pub library_id: Uuid,
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListDocumentsRequest {
    /// When omitted, auto-filled from the token's sole library grant.
    pub library_id: Option<Uuid>,
    pub limit: Option<usize>,
    pub status_filter: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpDeleteDocumentRequest {
    pub document_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetGraphTopologyRequest {
    pub library_id: Uuid,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListRelationsRequest {
    pub library_id: Uuid,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetCommunitiesRequest {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_revision_id: Option<Uuid>,
    pub read_mode: McpReadMode,
    pub readability_state: McpReadabilityState,
    pub readiness_kind: String,
    pub graph_coverage_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_access: Option<McpContentSourceAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub slice_start_offset: usize,
    pub slice_end_offset: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_content_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_token: Option<String>,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunk_references: Vec<McpChunkReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub technical_fact_references: Vec<McpTechnicalFactReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_references: Vec<McpEntityReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relation_references: Vec<McpRelationReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    fn search_documents_request_accepts_canonical_library_ids() {
        let request: McpSearchDocumentsRequest = serde_json::from_value(json!({
            "query": "alpha",
            "libraryIds": [Uuid::nil()],
            "limit": 3
        }))
        .expect("request should deserialize");

        assert_eq!(request.requested_library_ids(), Some(vec![Uuid::nil()]));
    }

    #[test]
    fn read_document_request_accepts_canonical_fields() {
        let document_id = Uuid::now_v7();
        let request: McpReadDocumentRequest = serde_json::from_value(json!({
            "documentId": document_id,
            "startOffset": 12,
            "continuationToken": "token"
        }))
        .expect("request should deserialize");

        assert_eq!(request.document_id, Some(document_id));
        assert_eq!(request.start_offset, Some(12));
        assert_eq!(request.continuation_token.as_deref(), Some("token"));
    }

    #[test]
    fn upload_documents_request_accepts_canonical_fields() {
        let library_id = Uuid::now_v7();
        let request: McpUploadDocumentsRequest = serde_json::from_value(json!({
            "libraryId": library_id,
            "idempotencyKey": "idem",
            "documents": [{
                "fileName": "demo.txt",
                "contentBase64": "ZGVtbw==",
                "mimeType": "text/plain"
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
            "libraryId": library_id,
            "documents": [{
                "body": "hello world",
                "sourceType": "inline",
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
    fn update_document_request_accepts_canonical_fields() {
        let request: McpUpdateDocumentRequest = serde_json::from_value(json!({
            "libraryId": Uuid::nil(),
            "documentId": Uuid::now_v7(),
            "operationKind": "append",
            "appendedText": "hello"
        }))
        .expect("request should deserialize");

        assert_eq!(request.appended_text.as_deref(), Some("hello"));
    }

    #[test]
    fn mutation_status_request_accepts_canonical_field() {
        let receipt_id = Uuid::now_v7();
        let request: McpGetMutationStatusRequest = serde_json::from_value(json!({
            "receiptId": receipt_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.receipt_id, receipt_id);
    }

    #[test]
    fn runtime_execution_request_accepts_canonical_field() {
        let runtime_execution_id = Uuid::now_v7();
        let request: McpGetRuntimeExecutionRequest = serde_json::from_value(json!({
            "runtimeExecutionId": runtime_execution_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.runtime_execution_id, runtime_execution_id);
    }

    #[test]
    fn runtime_execution_trace_request_accepts_canonical_field() {
        let runtime_execution_id = Uuid::now_v7();
        let request: McpGetRuntimeExecutionTraceRequest = serde_json::from_value(json!({
            "runtimeExecutionId": runtime_execution_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.runtime_execution_id, runtime_execution_id);
    }

    #[test]
    fn submit_web_ingest_run_request_accepts_canonical_fields() {
        let library_id = Uuid::now_v7();
        let request: McpSubmitWebIngestRunRequest = serde_json::from_value(json!({
            "libraryId": library_id,
            "seedUrl": "https://example.com/docs",
            "mode": "single_page",
            "boundaryPolicy": "same_host",
            "maxDepth": 0,
            "maxPages": 1,
            "idempotencyKey": "web-seed-1"
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
    fn get_web_ingest_run_request_accepts_canonical_field() {
        let run_id = Uuid::now_v7();
        let request: McpGetWebIngestRunRequest = serde_json::from_value(json!({
            "runId": run_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.run_id, run_id);
    }

    #[test]
    fn list_web_ingest_run_pages_request_accepts_canonical_field() {
        let run_id = Uuid::now_v7();
        let request: McpListWebIngestRunPagesRequest = serde_json::from_value(json!({
            "runId": run_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.run_id, run_id);
    }

    #[test]
    fn cancel_web_ingest_run_request_accepts_canonical_field() {
        let run_id = Uuid::now_v7();
        let request: McpCancelWebIngestRunRequest = serde_json::from_value(json!({
            "runId": run_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.run_id, run_id);
    }

    #[test]
    fn upload_document_input_accepts_canonical_fields() {
        let input: McpUploadDocumentInput = serde_json::from_value(json!({
            "fileName": "demo.txt",
            "contentBase64": "ZGVtbw==",
            "mimeType": "text/plain"
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
            "sourceUri": "memory://demo.txt",
            "mimeType": "text/plain"
        }))
        .expect("input should deserialize");

        assert_eq!(input.body.as_deref(), Some("demo"));
        assert_eq!(input.source_uri.as_deref(), Some("memory://demo.txt"));
        assert_eq!(input.mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn search_documents_request_rejects_legacy_aliases() {
        let error = serde_json::from_value::<McpSearchDocumentsRequest>(json!({
            "query": "alpha",
            "library_id": Uuid::nil()
        }))
        .expect_err("legacy alias must be rejected");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn read_document_request_rejects_legacy_aliases() {
        let error = serde_json::from_value::<McpReadDocumentRequest>(json!({
            "documentId": Uuid::now_v7(),
            "start_offset": 12
        }))
        .expect_err("legacy alias must be rejected");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn upload_documents_request_rejects_legacy_aliases() {
        let error = serde_json::from_value::<McpUploadDocumentsRequest>(json!({
            "libraryId": Uuid::now_v7(),
            "documents": [{
                "file_name": "demo.txt",
                "contentBase64": "ZGVtbw=="
            }]
        }))
        .expect_err("legacy alias must be rejected");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn update_document_request_rejects_legacy_aliases() {
        let error = serde_json::from_value::<McpUpdateDocumentRequest>(json!({
            "libraryId": Uuid::nil(),
            "documentId": Uuid::now_v7(),
            "operationKind": "append",
            "appended_text": "hello"
        }))
        .expect_err("legacy alias must be rejected");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn submit_web_ingest_run_request_rejects_legacy_aliases() {
        let error = serde_json::from_value::<McpSubmitWebIngestRunRequest>(json!({
            "libraryId": Uuid::now_v7(),
            "seedUrl": "https://example.com/docs",
            "mode": "single_page",
            "max_depth": 0
        }))
        .expect_err("legacy alias must be rejected");

        assert!(error.to_string().contains("unknown field"));
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
