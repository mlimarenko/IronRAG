mod context;
mod formatting;
mod session;
mod turn;

pub(crate) use turn::ASSISTANT_AGENT_LOOP_DEADLINE_MS;

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions require descriptive failures")]
mod tests;

use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::{
    domains::agent_runtime::{RuntimeExecutionSummary, RuntimeSurfaceKind},
    domains::query::{
        PreparedSegmentReference, QueryChunkReference, QueryClarification, QueryConversation,
        QueryExecution, QueryGraphEdgeReference, QueryGraphNodeReference, QueryRuntimeStageSummary,
        QueryTurn, QueryTurnKind, QueryVerificationState, RuntimeQueryMode, TechnicalFactReference,
    },
    domains::query_ir::QueryIR,
    infra::knowledge_rows::KnowledgeContextBundleReferenceSetRow,
};

pub(crate) const MAX_LIBRARY_CONVERSATIONS: usize = 5;
pub(crate) const QUERY_CONVERSATION_TITLE_LIMIT: usize = 72;
pub(crate) const MAX_PROMPT_HISTORY_TURNS: usize = 12;
pub(crate) const MAX_PROMPT_HISTORY_TURN_CHARS: usize = 1_200;
pub(crate) const MAX_EFFECTIVE_QUERY_HISTORY_TURNS: usize = 3;
pub(crate) const MAX_EFFECTIVE_QUERY_TURN_CHARS: usize = 220;
pub(crate) const MAX_GROUNDED_ANSWER_TOOL_HISTORY_TURNS: usize = 6;
pub(crate) const MAX_GROUNDED_ANSWER_TOOL_HISTORY_CHARS: usize = 2_000;
pub(crate) const CANONICAL_QUERY_MODE: RuntimeQueryMode = RuntimeQueryMode::Mix;
pub(crate) const MAX_DETAIL_TECHNICAL_FACT_REFERENCES: usize = 24;
pub(crate) const MAX_DETAIL_PREPARED_SEGMENT_REFERENCES: usize = 48;
pub(crate) const MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION: usize = 8;
pub(crate) const MAX_DETAIL_GRAPH_NODE_REFERENCES: usize = 96;
pub(crate) const MAX_DETAIL_GRAPH_EDGE_REFERENCES: usize = 96;
/// Minimum characters a token must have to count as a focus signal for
/// prepared-segment ranking. Length cutoff is language-agnostic; mirrors
/// `planner.rs::TOKEN_MIN_LEN`.
pub(crate) const PREPARED_SEGMENT_FOCUS_MIN_TOKEN_LEN: usize = 4;
const MAX_RUNTIME_FAILURE_PUBLIC_SUMMARY_CHARS: usize = 120;

/// Returns a public-safe summary only for a canonical typed failure code.
///
/// Runtime diagnostics are intentionally not accepted here. Failure codes are
/// protocol identifiers and therefore must use bounded ASCII snake_case. This
/// keeps public and persisted summaries useful without treating arbitrary
/// provider or query text as redacted merely because it was truncated.
pub(crate) fn runtime_failure_summary_from_typed_code(code: &str) -> Option<String> {
    let bytes = code.as_bytes();
    let first = bytes.first().copied()?;
    let last = bytes.last().copied()?;
    let is_canonical = bytes.len() <= MAX_RUNTIME_FAILURE_PUBLIC_SUMMARY_CHARS
        && first.is_ascii_lowercase()
        && last.is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');
    is_canonical.then(|| code.to_string())
}

/// Identifies protocol-level failures whose accompanying summary originates
/// at the typed runtime-policy boundary and is already redacted.
pub(crate) fn is_runtime_policy_failure_code(code: &str) -> bool {
    matches!(
        code,
        "runtime_policy_rejected" | "runtime_policy_terminated" | "runtime_policy_blocked"
    )
}

/// Bounds a summary whose policy-domain type already guarantees redaction.
/// This must not be used for provider, repository, query, or user-supplied
/// diagnostics; those paths use [`runtime_failure_summary_from_typed_code`].
pub(crate) fn bounded_runtime_policy_summary(summary_redacted: &str) -> Option<String> {
    let summary_redacted = summary_redacted.trim();
    if summary_redacted.is_empty() {
        return None;
    }
    Some(summary_redacted.chars().take(MAX_RUNTIME_FAILURE_PUBLIC_SUMMARY_CHARS).collect())
}

#[derive(Debug, Clone)]
pub(crate) struct ConversationRuntimeContext {
    /// Verbatim current user turn. Prior conversation is carried separately
    /// so the typed QueryCompiler remains the only semantic boundary.
    pub(crate) current_question_text: String,
    /// Structural availability only; this does not classify the current turn.
    pub(crate) has_prior_conversation: bool,
    pub(crate) query_compiler_history: Vec<ExternalConversationTurn>,
    pub(crate) prompt_history_text: Option<String>,
    pub(crate) prompt_history_messages: Vec<crate::integrations::llm::ChatMessage>,
    pub(crate) grounded_answer_tool_history: Vec<ExternalConversationTurn>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedSegmentRevisionInfo {
    pub(crate) document_title: Option<String>,
    pub(crate) source_uri: Option<String>,
    pub(crate) document_hint: Option<String>,
    pub(crate) source_access: Option<crate::domains::content::ContentSourceAccess>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExecutionPreparedReferenceContext {
    pub(crate) bundle_refs: Option<KnowledgeContextBundleReferenceSetRow>,
    pub(crate) chunk_rows: Vec<crate::infra::knowledge_rows::KnowledgeChunkRow>,
    pub(crate) fact_rank_refs: HashMap<Uuid, RankedBundleReference>,
    pub(crate) technical_fact_rows: Vec<crate::infra::knowledge_rows::KnowledgeTechnicalFactRow>,
    pub(crate) block_rank_refs: HashMap<Uuid, RankedBundleReference>,
    pub(crate) structured_block_rows:
        Vec<crate::infra::knowledge_rows::KnowledgeStructuredBlockRow>,
    pub(crate) segment_revision_info: HashMap<Uuid, PreparedSegmentRevisionInfo>,
    pub(crate) assistant_document_references:
        Vec<crate::services::query::assistant_grounding::AssistantGroundingDocumentReference>,
}

#[derive(Debug, Clone)]
pub struct CreateConversationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
    /// Originating surface — `'ui'` for the web assistant, `'mcp'`
    /// for the grounded_answer tool. Drives the UI session-listing
    /// filter so MCP-born conversations never leak into the web
    /// assistant surface.
    pub request_surface: String,
}

#[derive(Debug, Clone)]
/// Authorized explicit-title mutation for one assistant conversation.
pub struct RenameConversationCommand {
    /// Conversation selected by the caller.
    pub conversation_id: Uuid,
    /// Principal performing the mutation.
    pub actor_principal_id: Uuid,
    /// Whether scoped library-management permission may override ownership.
    pub allow_manage_all: bool,
    /// Requested durable display title.
    pub title: String,
}

#[derive(Debug, Clone, Copy)]
/// Authorized deletion request for one assistant conversation.
pub struct DeleteConversationCommand {
    /// Conversation selected by the caller.
    pub conversation_id: Uuid,
    /// Principal performing the mutation.
    pub actor_principal_id: Uuid,
    /// Whether scoped library-management permission may override ownership.
    pub allow_manage_all: bool,
}

#[derive(Debug, Clone)]
pub struct ExternalConversationTurn {
    pub turn_kind: QueryTurnKind,
    pub content_text: String,
}

#[derive(Debug, Clone)]
pub struct ExecuteConversationTurnCommand {
    pub conversation_id: Uuid,
    pub author_principal_id: Option<Uuid>,
    pub surface_kind: RuntimeSurfaceKind,
    pub content_text: String,
    pub external_prior_turns: Vec<ExternalConversationTurn>,
    pub top_k: usize,
    pub include_debug: bool,
}

#[derive(Debug, Clone)]
pub struct QueryTurnExecutionResult {
    pub conversation: QueryConversation,
    pub request_turn: QueryTurn,
    pub response_turn: Option<QueryTurn>,
    pub execution: QueryExecution,
    pub runtime_summary: RuntimeExecutionSummary,
    pub runtime_stage_summaries: Vec<QueryRuntimeStageSummary>,
    pub context_bundle_id: Uuid,
    pub chunk_references: Vec<QueryChunkReference>,
    pub prepared_segment_references: Vec<PreparedSegmentReference>,
    pub technical_fact_references: Vec<TechnicalFactReference>,
    pub graph_node_references: Vec<QueryGraphNodeReference>,
    pub graph_edge_references: Vec<QueryGraphEdgeReference>,
    pub verification_state: QueryVerificationState,
    pub verification_warnings: Vec<crate::domains::query::QueryVerificationWarning>,
    /// Persisted finalizer-owned answer disposition. Live and result-cache
    /// replay paths both read this from the execution context bundle.
    pub answer_disposition: crate::domains::query::QueryAnswerDisposition,
    /// Canonical typed compiler output for the answer. MCP completion policy
    /// consumes this directly instead of reclassifying the raw question.
    pub query_ir: Option<QueryIR>,
    /// Typed clarification metadata persisted with the final answer outcome.
    pub clarification: QueryClarification,
}

#[derive(Clone, Default)]
pub struct QueryService;

impl QueryService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RankedBundleReference {
    pub(crate) rank: i32,
    pub(crate) score: f64,
    pub(crate) reasons: BTreeSet<String>,
}

pub(crate) fn runtime_mode_label(mode: RuntimeQueryMode) -> &'static str {
    match mode {
        RuntimeQueryMode::Document => "document",
        RuntimeQueryMode::Local => "local",
        RuntimeQueryMode::Global => "global",
        RuntimeQueryMode::Hybrid => "hybrid",
        RuntimeQueryMode::Mix => "mix",
    }
}

pub(crate) fn saturating_rank(index: usize) -> i32 {
    i32::try_from(index.saturating_add(1)).unwrap_or(i32::MAX)
}

pub(crate) fn merge_ranked_reference(
    refs: &mut HashMap<Uuid, RankedBundleReference>,
    target_id: Uuid,
    rank: i32,
    score: f64,
    reason: &str,
) {
    let entry = refs.entry(target_id).or_insert_with(|| RankedBundleReference {
        rank,
        score,
        reasons: BTreeSet::new(),
    });
    entry.rank = entry.rank.min(rank);
    if score > entry.score {
        entry.score = score;
    }
    entry.reasons.insert(reason.to_string());
}

pub(crate) fn top_ranked_ids(
    refs: &HashMap<Uuid, RankedBundleReference>,
    limit: usize,
) -> Vec<Uuid> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, rank)| (*id, rank)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items.into_iter().take(limit).map(|(id, _)| id).collect()
}
