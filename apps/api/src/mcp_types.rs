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
    recognition::LibraryRecognitionPolicy,
};
use crate::shared::web::ingest::{
    WebIngestPolicy, WebIngestUrlFilter, default_web_ingest_crawl_filter,
};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
    /// Version of the canonical hash framing used for the visible MCP
    /// tool contract. Consumers compare this together with
    /// `tool_contract_hash`; a future incompatible canonicalization can
    /// increment the version without making old hashes ambiguous.
    pub tool_contract_version: u32,
    /// SHA-256 over the ordered visible tool names, descriptions, and
    /// canonical input schemas. The in-process UI agent and external MCP
    /// transport derive this value from the same descriptors.
    pub tool_contract_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<DateTime<Utc>>,
}

/// Selects the amount of grounded-answer execution detail returned to an MCP
/// caller. Ordinary external and in-process UI agent calls use `Compact`;
/// `Full` preserves the debug-heavy contract for explicit diagnostic requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpGroundedAnswerResponseProfile {
    #[default]
    Full,
    Compact,
}

/// Default number of typed evidence references returned by the compact
/// grounded-answer response profile.
pub const MCP_COMPACT_DEFAULT_REFERENCES: usize = 8;

/// Hard upper bound for compact-profile references. The full execution trace
/// remains available through the returned execution identifiers.
pub const MCP_COMPACT_MAX_REFERENCES: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpWorkspaceDescriptor {
    pub workspace_id: Uuid,
    #[serde(rename = "ref")]
    pub catalog_ref: String,
    pub name: String,
    pub status: String,
    pub visible_library_count: usize,
    pub can_write_any_library: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpLibraryIngestionReadiness {
    pub ready: bool,
    pub missing_binding_purposes: Vec<AiBindingPurpose>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpLibraryDescriptor {
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    #[serde(rename = "ref")]
    pub catalog_ref: String,
    pub name: String,
    pub description: Option<String>,
    pub web_ingest_policy: WebIngestPolicy,
    pub recognition_policy: LibraryRecognitionPolicy,
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

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListLibrariesRequest {
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSearchDocumentsRequest {
    pub query: String,
    pub libraries: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub include_references: Option<bool>,
    pub include_unreadable: Option<bool>,
}

/// Hard cap on library IDs per MCP request. Prevents an unbounded
/// clone + a fan-out that would turn one search call into an O(N)
/// database scatter. Agents rarely reference more than 5 libraries
/// in a single tool call; 50 gives headroom for batch-style scripts.
const MCP_MAX_LIBRARY_REFS: usize = 50;

impl McpSearchDocumentsRequest {
    #[must_use]
    pub fn requested_library_refs(&self) -> Option<Vec<String>> {
        self.libraries.as_ref().map(|refs| {
            if refs.len() > MCP_MAX_LIBRARY_REFS {
                refs[..MCP_MAX_LIBRARY_REFS].to_vec()
            } else {
                refs.clone()
            }
        })
    }
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCreateWorkspaceRequest {
    pub workspace: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCreateLibraryRequest {
    pub library: String,
    pub title: Option<String>,
    pub description: Option<String>,
}

/// Fields are optional: any field left `None` keeps the workspace's
/// current value unchanged (load-current -> merge -> write), so a
/// caller renaming a workspace does not have to also restate its
/// lifecycle state.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpUpdateWorkspaceRequest {
    pub workspace: String,
    pub title: Option<String>,
    /// `"active"` or `"archived"`. Omit to leave lifecycle unchanged.
    pub lifecycle_state: Option<String>,
}

/// Fields are optional: any field left `None` keeps the library's
/// current value unchanged (load-current -> merge -> write). Knobs not
/// exposed here (extraction prompt, `includeDocumentHintInMcpAnswers`)
/// are always preserved as-is — this tool is agent-ergonomic, not a 1:1
/// mirror of the full REST library PATCH surface.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpUpdateLibraryRequest {
    pub library: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// `"active"` or `"archived"`. Omit to leave lifecycle unchanged.
    pub lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpReadabilityState {
    Readable,
    Processing,
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpChunkReference {
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpEntityReference {
    pub entity_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpRelationReference {
    pub relation_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpEvidenceReference {
    pub evidence_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpContentSourceAccess {
    pub kind: String,
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
/// elided from the serialized JSON to keep the agent's context window tight.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
    /// Character offset for the first `read_document` window that should
    /// contain the best-matching chunk inside the full normalized document.
    /// Callers should pass this back to `read_document` as `startOffset`
    /// so the very first read window already lands on real content instead
    /// of the document's table of contents / front matter. `None` means
    /// the source chunks lack span info (older data) or no chunk matched.
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpSearchDocumentsResponse {
    pub query: String,
    pub limit: usize,
    pub libraries: Vec<String>,
    pub hits: Vec<McpDocumentHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpReadMode {
    Full,
    Excerpt,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpReadDocumentRequest {
    pub document_id: Option<Uuid>,
    pub mode: Option<McpReadMode>,
    pub start_offset: Option<usize>,
    pub length: Option<usize>,
    pub continuation_token: Option<String>,
    pub include_references: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
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
    /// HTTP(S) URL the backend will fetch the file from. Preferred
    /// over `content_base64` for anything larger than a few kB — LLM
    /// tool-call outputs cap at a few thousand tokens, so a 22 kB
    /// file's ~30 kB base64 payload gets truncated inside the
    /// `tool_calls.arguments_json` the model emits and the upload
    /// fails. Passing a URL keeps the tool-call arguments tiny and
    /// moves the transfer into the backend where it only has to fit
    /// the normal upload-size limit.
    pub fetch_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpUploadDocumentsRequest {
    pub library: String,
    pub idempotency_key: Option<String>,
    pub documents: Vec<McpUploadDocumentInput>,
}

#[derive(Debug, Clone, Copy, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpDocumentMutationKind {
    Append,
    Replace,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpUpdateDocumentRequest {
    pub library: String,
    pub document_id: Uuid,
    /// Renamed from `operationKind` on the wire: the tool itself is
    /// `create_document_revision`, so the argument that used to repeat
    /// "operation" would be redundant.
    #[serde(rename = "mode")]
    pub operation_kind: McpDocumentMutationKind,
    pub idempotency_key: Option<String>,
    pub appended_text: Option<String>,
    pub replacement_file_name: Option<String>,
    pub replacement_content_base64: Option<String>,
    pub replacement_mime_type: Option<String>,
}

/// Request for the `get_operation` tool. Reads from the same canonical
/// `ops_async_operation` store as `GET /v1/ops/operations/{operationId}`
/// — superseding the old `get_mutation_status`/`receiptId` pair, which
/// polled a content-mutation-specific receipt instead of the canonical
/// operation record.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetOperationRequest {
    pub operation_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetRuntimeExecutionRequest {
    pub runtime_execution_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetRuntimeExecutionTraceRequest {
    pub runtime_execution_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSubmitWebIngestRunRequest {
    pub library: String,
    pub seed_url: String,
    pub mode: String,
    pub boundary_policy: Option<String>,
    pub max_depth: Option<i32>,
    pub max_pages: Option<i32>,
    #[serde(default = "default_web_ingest_crawl_filter")]
    pub crawl_filter: WebIngestUrlFilter,
    #[serde(default)]
    pub materialization_filter: WebIngestUrlFilter,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetWebIngestRunRequest {
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListWebIngestRunPagesRequest {
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCancelWebIngestRunRequest {
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSearchEntitiesRequest {
    pub library: String,
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListDocumentsRequest {
    /// When omitted, auto-filled from the token's sole library grant.
    pub library: Option<String>,
    pub limit: Option<usize>,
    pub status_filter: Option<String>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpDeleteDocumentRequest {
    pub document_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetGraphTopologyRequest {
    pub library: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListRelationsRequest {
    pub library: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetCommunitiesRequest {
    pub library: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpMutationOperationKind {
    Upload,
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpMutationReceiptStatus {
    Accepted,
    Processing,
    Ready,
    Failed,
    Superseded,
}

// `McpAuditActionKind`/`McpAuditStatus`/`McpAuditScope` (and the
// `record_success_audit`/`record_error_audit` no-ops that were their only
// callers) were removed here: they fed a per-tool audit path that never
// persisted anything (see `interfaces::http::mcp::audit`). Every
// `tools/call` dispatch now runs through one mandatory audit chokepoint in
// `interfaces::http::mcp::tools::handle_tools_call`, which persists via the
// canonical `audit_event` store instead.

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
        McpCancelWebIngestRunRequest, McpGetOperationRequest, McpGetRuntimeExecutionRequest,
        McpGetRuntimeExecutionTraceRequest, McpGetWebIngestRunRequest,
        McpListWebIngestRunPagesRequest, McpReadDocumentRequest, McpSearchDocumentsRequest,
        McpSubmitWebIngestRunRequest, McpUpdateDocumentRequest, McpUploadDocumentInput,
        McpUploadDocumentsRequest,
    };
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn search_documents_request_accepts_canonical_library_refs() {
        let request: McpSearchDocumentsRequest = serde_json::from_value(json!({
            "query": "alpha",
            "libraries": ["default/alpha"],
            "limit": 3
        }))
        .expect("request should deserialize");

        assert_eq!(request.requested_library_refs(), Some(vec!["default/alpha".to_string()]));
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
        let request: McpUploadDocumentsRequest = serde_json::from_value(json!({
            "library": "default/demo",
            "idempotencyKey": "idem",
            "documents": [{
                "fileName": "demo.txt",
                "contentBase64": "ZGVtbw==",
                "mimeType": "text/plain"
            }]
        }))
        .expect("request should deserialize");

        assert_eq!(request.library, "default/demo");
        assert_eq!(request.idempotency_key.as_deref(), Some("idem"));
        assert_eq!(request.documents.len(), 1);
    }

    #[test]
    fn upload_documents_request_accepts_inline_body_fields() {
        let request: McpUploadDocumentsRequest = serde_json::from_value(json!({
            "library": "default/demo",
            "documents": [{
                "body": "hello world",
                "sourceType": "inline",
                "title": "Inline note"
            }]
        }))
        .expect("request should deserialize");

        assert_eq!(request.library, "default/demo");
        assert_eq!(request.documents.len(), 1);
        assert_eq!(request.documents[0].body.as_deref(), Some("hello world"));
        assert_eq!(request.documents[0].source_type.as_deref(), Some("inline"));
    }

    #[test]
    fn update_document_request_accepts_canonical_fields() {
        let request: McpUpdateDocumentRequest = serde_json::from_value(json!({
            "library": "default/demo",
            "documentId": Uuid::now_v7(),
            "mode": "append",
            "appendedText": "hello"
        }))
        .expect("request should deserialize");

        assert_eq!(request.library, "default/demo");
        assert_eq!(request.appended_text.as_deref(), Some("hello"));
    }

    #[test]
    fn get_operation_request_accepts_canonical_field() {
        let operation_id = Uuid::now_v7();
        let request: McpGetOperationRequest = serde_json::from_value(json!({
            "operationId": operation_id
        }))
        .expect("request should deserialize");

        assert_eq!(request.operation_id, operation_id);
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
        let request: McpSubmitWebIngestRunRequest = serde_json::from_value(json!({
            "library": "default/demo",
            "seedUrl": "https://example.com/docs",
            "mode": "single_page",
            "boundaryPolicy": "same_host",
            "maxDepth": 0,
            "maxPages": 1,
            "crawlFilter": {
                "blockPatterns": [{"kind": "path_prefix", "value": "/labels/viewlabel.action"}],
                "allowPatterns": []
            },
            "materializationFilter": {
                "allowPatterns": [{"kind": "path_prefix", "value": "/docs/"}],
                "blockPatterns": []
            },
            "idempotencyKey": "web-seed-1"
        }))
        .expect("request should deserialize");

        assert_eq!(request.library, "default/demo");
        assert_eq!(request.seed_url, "https://example.com/docs");
        assert_eq!(request.mode, "single_page");
        assert_eq!(request.boundary_policy.as_deref(), Some("same_host"));
        assert_eq!(request.max_depth, Some(0));
        assert_eq!(request.max_pages, Some(1));
        assert_eq!(request.crawl_filter.block_patterns.len(), 1);
        assert_eq!(request.materialization_filter.allow_patterns.len(), 1);
        assert_eq!(request.idempotency_key.as_deref(), Some("web-seed-1"));
    }

    #[test]
    fn submit_web_ingest_run_request_defaults_optional_filters() {
        let request: McpSubmitWebIngestRunRequest = serde_json::from_value(json!({
            "library": "default/demo",
            "seedUrl": "https://example.com/docs",
            "mode": "single_page"
        }))
        .expect("request should deserialize");

        assert!(request.crawl_filter.block_patterns.is_empty());
        assert!(request.materialization_filter.allow_patterns.is_empty());
        assert!(request.materialization_filter.block_patterns.is_empty());
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
            "libraryIds": [Uuid::nil()]
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
            "library": "default/demo",
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
            "library": "default/demo",
            "documentId": Uuid::now_v7(),
            "mode": "append",
            "appended_text": "hello"
        }))
        .expect_err("legacy alias must be rejected");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn submit_web_ingest_run_request_rejects_legacy_aliases() {
        let error = serde_json::from_value::<McpSubmitWebIngestRunRequest>(json!({
            "library": "default/demo",
            "seedUrl": "https://example.com/docs",
            "mode": "single_page",
            "max_depth": 0
        }))
        .expect_err("legacy alias must be rejected");

        assert!(error.to_string().contains("unknown field"));
    }
}

/// Returned by `create_documents`/`create_document_revision`.
/// `operation_id` (wire: `operationId`, renamed from `receiptId`) is the
/// canonical async-operation identifier, pollable via the `get_operation`
/// tool and `GET /v1/ops/operations/{operationId}` — not a
/// content-mutation-specific receipt.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpMutationReceipt {
    pub operation_id: Uuid,
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

/// Canonical async-operation status returned by the `get_operation` MCP
/// tool. Mirrors `GET /v1/ops/operations/{operationId}` field-for-field
/// (the operation row flattened, plus aggregated child progress) instead
/// of introducing a second operation-status shape.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpOperationStatus {
    #[serde(flatten)]
    pub operation: crate::domains::ops::OpsAsyncOperation,
    pub progress: crate::domains::ops::OpsAsyncOperationProgress,
}
