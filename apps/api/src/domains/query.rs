use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::agent_runtime::{RuntimeExecutionSummary, RuntimeLifecycleState, RuntimeStageKind},
    domains::content::ContentSourceAccess,
    shared::extraction::{
        structured_document::StructuredBlockKind, technical_facts::TechnicalFactKind,
    },
};

pub const DEFAULT_TOP_K: usize = 24;
pub const MAX_TOP_K: usize = 32;
/// Minimum retrieval breadth for a grounded answer on a contextual follow-up
/// turn. Pinned to [`DEFAULT_TOP_K`] so a follow-up question retrieves as
/// widely as a fresh one: an earlier lower floor let the UI agent lowball
/// `topK` on follow-ups, which starved recall on how-to / configuration
/// questions (the concrete parameter chunks never reached the answer model,
/// so it hedged instead of listing the steps). The agent may still request a
/// *wider* breadth up to [`MAX_TOP_K`]; it can no longer request a narrower one.
pub const CONTEXTUAL_GROUNDED_ANSWER_MIN_TOP_K: usize = DEFAULT_TOP_K;

#[must_use]
pub fn resolve_top_k(requested_top_k: Option<usize>) -> usize {
    requested_top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K)
}

#[must_use]
pub fn resolve_contextual_grounded_answer_top_k(
    requested_top_k: Option<usize>,
    has_contextual_turns: bool,
    max_top_k: usize,
) -> usize {
    let bounded_max = max_top_k.clamp(1, MAX_TOP_K);
    let mut effective_top_k = requested_top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, bounded_max);
    if has_contextual_turns {
        effective_top_k =
            effective_top_k.max(CONTEXTUAL_GROUNDED_ANSWER_MIN_TOP_K.min(bounded_max));
    }
    effective_top_k
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeQueryMode {
    Document,
    Local,
    Global,
    Hybrid,
    Mix,
}

impl RuntimeQueryMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Local => "local",
            Self::Global => "global",
            Self::Hybrid => "hybrid",
            Self::Mix => "mix",
        }
    }
}

impl std::str::FromStr for RuntimeQueryMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "document" => Ok(Self::Document),
            "local" => Ok(Self::Local),
            "global" => Ok(Self::Global),
            "hybrid" => Ok(Self::Hybrid),
            "mix" => Ok(Self::Mix),
            other => Err(format!("unsupported query mode: {other}")),
        }
    }
}

/// Controls the optional provider-backed semantic reranker.
///
/// The deterministic structural ranker remains the fail-safe path in
/// every mode. `Off` is deliberately the default so an upgrade cannot add a
/// provider round-trip or change answer ordering without operator intent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRerankMode {
    #[default]
    Off,
    Shadow,
    Active,
}

impl SemanticRerankMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryIntentCacheStatus {
    Miss,
    HitFresh,
    HitStaleRecomputed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IntentKeywords {
    pub high_level: Vec<String>,
    pub low_level: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanningMetadata {
    pub requested_mode: RuntimeQueryMode,
    pub planned_mode: RuntimeQueryMode,
    pub intent_cache_status: QueryIntentCacheStatus,
    pub keywords: IntentKeywords,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RerankStatus {
    NotApplicable,
    Applied,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRerankStrategy {
    ProviderSemantic,
    LexicalHeuristicWithProviderShadow,
    LexicalHeuristicFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRerankOutcome {
    ShadowScheduled,
    ShadowCapacitySkipped,
    Applied,
    MissingBinding,
    TimedOut,
    ProviderFailure,
    InvalidResponse,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SemanticRerankMetadata {
    pub mode: SemanticRerankMode,
    pub strategy: SemanticRerankStrategy,
    pub outcome: SemanticRerankOutcome,
    /// Candidates prepared within the configured text budgets. This is not a
    /// claim that a provider request started: shadow scheduling can be skipped
    /// and active mode can fail while resolving its optional binding.
    pub prepared_candidate_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RerankMetadata {
    pub status: RerankStatus,
    pub candidate_count: usize,
    pub reordered_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_rerank: Option<SemanticRerankMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextAssemblyStatus {
    DocumentOnly,
    GraphOnly,
    BalancedMixed,
    MixedSkewed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAssemblyMetadata {
    pub status: ContextAssemblyStatus,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupedReferenceKind {
    Document,
    Relationship,
    Entity,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupedReference {
    pub id: String,
    pub kind: GroupedReferenceKind,
    pub rank: usize,
    pub title: String,
    pub excerpt: Option<String>,
    pub evidence_count: usize,
    pub support_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryConversationState {
    Active,
    Archived,
}

impl QueryConversationState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

impl std::str::FromStr for QueryConversationState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            other => Err(format!("unsupported query conversation state: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryTurnKind {
    User,
    Assistant,
    System,
    Tool,
}

impl QueryTurnKind {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

impl std::str::FromStr for QueryTurnKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            "tool" => Ok(Self::Tool),
            other => Err(format!("unsupported query turn kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryRuntimeStageSummary {
    pub stage_kind: RuntimeStageKind,
    pub stage_label: String,
    /// Total wall-clock spent in this stage across its attempts, derived
    /// from runtime stage records. `None` when no completed timing exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueryConversation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
    pub conversation_state: QueryConversationState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueryTurn {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub turn_index: i32,
    pub turn_kind: QueryTurnKind,
    pub author_principal_id: Option<Uuid>,
    pub content_text: String,
    pub execution_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueryExecution {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub context_bundle_id: Uuid,
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub binding_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub lifecycle_state: RuntimeLifecycleState,
    pub active_stage: Option<RuntimeStageKind>,
    pub query_text: String,
    pub failure_code: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueryChunkReference {
    pub execution_id: Uuid,
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueryGraphNodeReference {
    pub execution_id: Uuid,
    pub node_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub label: String,
    pub entity_type: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueryGraphEdgeReference {
    pub execution_id: Uuid,
    pub edge_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub relation_type: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSegmentReference {
    pub execution_id: Uuid,
    pub segment_id: Uuid,
    pub revision_id: Uuid,
    pub block_kind: StructuredBlockKind,
    pub rank: i32,
    pub score: f64,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
    pub document_id: Option<Uuid>,
    pub document_title: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub source_access: Option<ContentSourceAccess>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactReference {
    pub execution_id: Uuid,
    pub fact_id: Uuid,
    pub revision_id: Uuid,
    pub fact_kind: TechnicalFactKind,
    pub canonical_value: String,
    pub display_value: String,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryVerificationState {
    #[default]
    NotRun,
    Verified,
    PartiallySupported,
    Conflicting,
    InsufficientEvidence,
    Failed,
}

/// Finalizer-owned public disposition for an answer body.
///
/// Verification state describes evidence analysis; disposition describes what
/// callers may do with the body after visibility policy has run. Keeping these
/// concepts separate prevents transports from treating an inapplicable prose
/// verifier as a failure or a strict safe fallback as a factual answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryAnswerDisposition {
    #[default]
    NonTerminal,
    FactualReady,
    SafeFallback,
    Clarification,
}

impl QueryAnswerDisposition {
    #[must_use]
    pub const fn is_factual_ready(self) -> bool {
        matches!(self, Self::FactualReady)
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::FactualReady | Self::SafeFallback | Self::Clarification)
    }

    #[must_use]
    pub const fn requires_repair(self) -> bool {
        matches!(self, Self::NonTerminal)
    }

    #[must_use]
    pub const fn storage_label(self) -> &'static str {
        match self {
            Self::NonTerminal => "non_terminal",
            Self::FactualReady => "factual_ready",
            Self::SafeFallback => "safe_fallback",
            Self::Clarification => "clarification",
        }
    }
}

/// Structured provenance for one typed clarification candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnswerCandidateProvenance {
    pub entity_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
}

/// One typed disambiguation choice surfaced by the answer pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnswerCandidate {
    pub label: String,
    pub kind: String,
    pub confidence: Option<f64>,
    pub provenance: QueryAnswerCandidateProvenance,
}

/// Typed clarification metadata persisted with a query answer outcome.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryClarification {
    pub required: bool,
    pub question: Option<String>,
    pub answer_candidates: Vec<QueryAnswerCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryVerificationWarning {
    pub code: String,
    pub message: String,
    pub related_segment_id: Option<Uuid>,
    pub related_fact_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueryConversationDetail {
    pub conversation: QueryConversation,
    pub turns: Vec<QueryTurn>,
    pub executions: Vec<QueryExecution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueryExecutionDetail {
    pub execution: QueryExecution,
    pub runtime_summary: RuntimeExecutionSummary,
    pub runtime_stage_summaries: Vec<QueryRuntimeStageSummary>,
    pub request_turn: Option<QueryTurn>,
    pub response_turn: Option<QueryTurn>,
    pub chunk_references: Vec<QueryChunkReference>,
    pub prepared_segment_references: Vec<PreparedSegmentReference>,
    pub technical_fact_references: Vec<TechnicalFactReference>,
    pub graph_node_references: Vec<QueryGraphNodeReference>,
    pub graph_edge_references: Vec<QueryGraphEdgeReference>,
    pub verification_state: QueryVerificationState,
    pub verification_warnings: Vec<QueryVerificationWarning>,
    /// Persisted finalizer-owned disposition of the public response body.
    #[serde(default)]
    pub answer_disposition: QueryAnswerDisposition,
    /// Persisted typed clarification, when the disposition is
    /// [`QueryAnswerDisposition::Clarification`].
    #[serde(default)]
    pub clarification: QueryClarification,
    /// Canonical compiler output used by the execution. This is an internal
    /// runtime hand-off for typed completion policy and cache replay; it is
    /// intentionally absent from public HTTP/OpenAPI payloads.
    #[serde(skip)]
    #[schema(ignore)]
    pub query_ir: Option<crate::domains::query_ir::QueryIR>,
}

#[cfg(test)]
mod tests {
    use super::{RerankMetadata, RerankStatus};

    #[test]
    fn semantic_rerank_metadata_is_absent_for_legacy_off_path() {
        let metadata = RerankMetadata {
            status: RerankStatus::Applied,
            candidate_count: 4,
            reordered_count: Some(2),
            semantic_rerank: None,
        };

        let value = serde_json::to_value(metadata).expect("metadata should serialize");

        assert!(value.get("semanticRerank").is_none());
    }
}
