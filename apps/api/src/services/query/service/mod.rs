#![allow(
    clippy::all,
    clippy::expect_used,
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::result_large_err,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

mod context;
mod formatting;
mod session;
mod turn;

#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::{
    domains::agent_runtime::{RuntimeExecutionSummary, RuntimeSurfaceKind},
    domains::query::{
        PreparedSegmentReference, QueryChunkReference, QueryConversation, QueryExecution,
        QueryGraphEdgeReference, QueryGraphNodeReference, QueryRuntimeStageSummary, QueryTurn,
        QueryTurnKind, QueryVerificationState, RuntimeQueryMode, TechnicalFactReference,
    },
    infra::arangodb::context_store::KnowledgeContextBundleReferenceSetRow,
};

pub(crate) const MAX_LIBRARY_CONVERSATIONS: usize = 5;
pub(crate) const QUERY_CONVERSATION_TITLE_LIMIT: usize = 72;
pub(crate) const MAX_PROMPT_HISTORY_TURNS: usize = 6;
pub(crate) const MAX_PROMPT_HISTORY_TURN_CHARS: usize = 360;
pub(crate) const MAX_EFFECTIVE_QUERY_HISTORY_TURNS: usize = 3;
pub(crate) const MAX_EFFECTIVE_QUERY_TURN_CHARS: usize = 220;
pub(crate) const CANONICAL_QUERY_MODE: RuntimeQueryMode = RuntimeQueryMode::Mix;
pub(crate) const MAX_DETAIL_TECHNICAL_FACT_REFERENCES: usize = 24;
pub(crate) const MAX_DETAIL_PREPARED_SEGMENT_REFERENCES: usize = 48;
pub(crate) const MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION: usize = 8;
pub(crate) const MAX_ANSWER_SOURCE_LINKS: usize = 5;
/// Minimum characters a token must have to count as a focus signal for
/// prepared-segment ranking. Length cutoff is language-agnostic; mirrors
/// `planner.rs::TOKEN_MIN_LEN`.
pub(crate) const PREPARED_SEGMENT_FOCUS_MIN_TOKEN_LEN: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationRuntimeContext {
    pub(crate) effective_query_text: String,
    pub(crate) prompt_history_text: Option<String>,
    pub(crate) coreference_entities: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedSegmentRevisionInfo {
    pub(crate) document_title: Option<String>,
    pub(crate) source_uri: Option<String>,
    pub(crate) source_access: Option<crate::domains::content::ContentSourceAccess>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExecutionPreparedReferenceContext {
    pub(crate) bundle_refs: Option<KnowledgeContextBundleReferenceSetRow>,
    pub(crate) fact_rank_refs: HashMap<Uuid, RankedBundleReference>,
    pub(crate) technical_fact_rows:
        Vec<crate::infra::arangodb::document_store::KnowledgeTechnicalFactRow>,
    pub(crate) block_rank_refs: HashMap<Uuid, RankedBundleReference>,
    pub(crate) structured_block_rows:
        Vec<crate::infra::arangodb::document_store::KnowledgeStructuredBlockRow>,
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
