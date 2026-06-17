use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::diagnostics::OperatorWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantTurnRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantVerificationState {
    NotRun,
    Verified,
    PartiallySupported,
    Conflicting,
    InsufficientEvidence,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantConversation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
    pub conversation_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantTurn {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub turn_index: i32,
    pub turn_kind: AssistantTurnRole,
    pub author_principal_id: Option<Uuid>,
    pub content_text: String,
    pub execution_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantExecution {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub context_bundle_id: Uuid,
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub binding_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub lifecycle_state: String,
    pub active_stage: Option<String>,
    pub query_text: String,
    pub failure_code: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantChunkReference {
    pub execution_id: Uuid,
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantContentSourceAccess {
    pub kind: String,
    pub href: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantPreparedSegmentReference {
    pub execution_id: Uuid,
    pub segment_id: Uuid,
    pub revision_id: Uuid,
    pub block_kind: String,
    pub rank: i32,
    pub score: f64,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
    pub document_id: Option<Uuid>,
    pub document_title: Option<String>,
    pub document_hint: Option<String>,
    pub source_access: Option<AssistantContentSourceAccess>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantTechnicalFactReference {
    pub execution_id: Uuid,
    pub fact_id: Uuid,
    pub revision_id: Uuid,
    pub fact_kind: String,
    pub canonical_value: String,
    pub display_value: String,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantEntityReference {
    pub execution_id: Uuid,
    pub node_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub label: String,
    pub entity_type: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRelationReference {
    pub execution_id: Uuid,
    pub edge_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub predicate: String,
    pub normalized_assertion: Option<String>,
}

/// Structured provenance for a single answer candidate.
///
/// Every field is optional because the clarify branches surface candidates
/// derived from different evidence shapes: a graph-entity probe carries an
/// `entity_id`, a document-derived variant carries a `document_id`, and a
/// label-only variant (e.g. a grouped-reference title) carries none.
/// Callers treat a present id as a stable handle for a follow-up tool call
/// and a missing id as "label only".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantAnswerCandidateProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<Uuid>,
}

/// One typed disambiguation choice the answer pipeline surfaced alongside a
/// clarifying answer.
///
/// `kind` reuses the existing typed vocabulary where it applies — a graph
/// `node_type` for entity probes, `"document"` for a document-derived variant
/// — so agents can route a follow-up without parsing the prose answer body.
/// `confidence` is `None` when the branch has no ranking signal to expose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantAnswerCandidate {
    pub label: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub provenance: AssistantAnswerCandidateProvenance,
}

/// Typed clarification metadata attached to a grounded-answer turn.
///
/// The prose answer body stays authoritative for humans; this object is
/// additive machine-readable metadata for agent callers. `required` is
/// `true` only when the turn returned a clarifying answer; otherwise the
/// candidate list is empty and `question` is `None`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantClarification {
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default)]
    pub answer_candidates: Vec<AssistantAnswerCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantVerificationWarning {
    pub code: String,
    pub message: String,
    pub related_segment_id: Option<Uuid>,
    pub related_fact_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRuntimeStageSummary {
    pub stage_kind: String,
    pub stage_label: String,
    /// Total wall-clock spent in this pipeline stage across its attempts.
    /// `None` for executions recorded before stage timing existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantPolicyDecisionSummary {
    pub target_kind: String,
    pub decision_kind: String,
    pub reason_code: String,
    pub target_id: String,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantPolicySummary {
    pub allow_count: i32,
    pub reject_count: i32,
    pub terminate_count: i32,
    pub recent_decisions: Vec<AssistantPolicyDecisionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRuntimeSummary {
    pub runtime_execution_id: Uuid,
    pub lifecycle_state: String,
    pub active_stage: Option<String>,
    pub turn_budget: i32,
    pub turn_count: i32,
    pub parallel_action_limit: i32,
    pub failure_code: Option<String>,
    pub failure_summary_redacted: Option<String>,
    pub policy_summary: AssistantPolicySummary,
    pub accepted_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantEvidenceBundle {
    pub chunk_references: Vec<AssistantChunkReference>,
    pub prepared_segment_references: Vec<AssistantPreparedSegmentReference>,
    pub technical_fact_references: Vec<AssistantTechnicalFactReference>,
    pub entity_references: Vec<AssistantEntityReference>,
    pub relation_references: Vec<AssistantRelationReference>,
    pub verification_state: AssistantVerificationState,
    pub verification_warnings: Vec<AssistantVerificationWarning>,
    pub runtime_summary: AssistantRuntimeSummary,
    pub runtime_stage_summaries: Vec<AssistantRuntimeStageSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantConversationMessage {
    pub id: Uuid,
    pub role: AssistantTurnRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub execution_id: Option<Uuid>,
    pub evidence: Option<AssistantEvidenceBundle>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSessionListItem {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub title: String,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub conversation_state: String,
    pub turn_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantConversationDetail {
    pub session: AssistantConversation,
    pub turns: Vec<AssistantTurn>,
    pub executions: Vec<AssistantExecution>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantExecutionDetail {
    pub context_bundle_id: Uuid,
    pub execution: AssistantExecution,
    pub runtime_summary: AssistantRuntimeSummary,
    pub runtime_stage_summaries: Vec<AssistantRuntimeStageSummary>,
    pub request_turn: Option<AssistantTurn>,
    pub response_turn: Option<AssistantTurn>,
    pub chunk_references: Vec<AssistantChunkReference>,
    pub prepared_segment_references: Vec<AssistantPreparedSegmentReference>,
    pub technical_fact_references: Vec<AssistantTechnicalFactReference>,
    pub entity_references: Vec<AssistantEntityReference>,
    pub relation_references: Vec<AssistantRelationReference>,
    pub verification_state: AssistantVerificationState,
    pub verification_warnings: Vec<AssistantVerificationWarning>,
    /// Typed clarification metadata for this turn. Defaults to
    /// `required: false` with no candidates for ordinary answers and for
    /// historical-replay reads (the replay surface carries no clarification
    /// metadata); a clarify turn populates `question` and `answer_candidates`.
    #[serde(default)]
    pub clarification: AssistantClarification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantHydratedConversation {
    pub session: AssistantSessionListItem,
    pub messages: Vec<AssistantConversationMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantComposerState {
    pub can_submit: bool,
    pub draft: Option<String>,
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantEvidenceItem {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub score: Option<f64>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantEvidenceGroup {
    pub key: String,
    pub label: String,
    pub items: Vec<AssistantEvidenceItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantStageItem {
    pub stage_kind: String,
    pub stage_label: String,
    pub active: bool,
    pub completed: bool,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantWorkspaceSurface {
    pub sessions: Vec<AssistantSessionListItem>,
    pub active_session_id: Option<Uuid>,
    pub messages: Vec<AssistantConversationMessage>,
    pub stages: Vec<AssistantStageItem>,
    pub composer: AssistantComposerState,
    pub evidence_groups: Vec<AssistantEvidenceGroup>,
    pub warnings: Vec<OperatorWarning>,
}
