//! IR-aware consolidation stage that runs between rerank and context
//! assembly in the structured-query pipeline.
//!
//! Why this exists: the post-retrieval `truncate_bundle(bundle, top_k)`
//! keeps only the globally top-k scored chunks. When the winning
//! document has a high-scoring intro chunk but its configuration
//! details live in subsequent chunks, the truncation drops those
//! neighbours in favour of tangential documents — the LLM then sees
//! chunk 0 of the right document and 7 chunks from unrelated
//! documents, and hedges. Consolidation detects "this question is
//! clearly about ONE document" from the compiled `QueryIR`
//! (explicit hint, subject-role entity matching a title, or an
//! evidence-dominance / only-document signal on the retrieval itself),
//! then reallocates the top-k budget to pack ranked content anchors
//! and their local neighbours from that winner.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use crate::app::state::AppState;
use crate::{
    domains::query_ir::{EntityRole, QueryAct, QueryIR, QueryTargetKind},
    services::query::{
        planner::extract_keywords,
        text_match::{
            add_label_terms_with_acronyms, near_token_overlap_count, normalized_alnum_tokens,
        },
    },
};

use super::{RetrievalBundle, RuntimeMatchedChunk, retrieve};
use super::{
    RuntimeChunkScoreKind,
    document_target::query_ir_allows_document_focus_scope,
    source_anchor_window,
    source_profile::{is_source_profile_chunk_row, is_source_profile_runtime_chunk},
};

/// Why the consolidation stage picked a particular winner (or no
/// winner). Carried in the answer-pipeline diagnostics so
/// `prod` logs can tell apart "IR explicitly forced this" vs. "soft
/// evidence-dominance signal" vs. "nothing to do".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FocusReason {
    /// No consolidation applied — either the IR did not pin a single
    /// subject or the retrieval did not agree.
    None,
    /// `query_ir.document_focus.hint` matched a retrieved document.
    DocumentFocusHint,
    /// `query_ir.scope == SingleDocument` + a subject-role entity
    /// matched exactly one retrieved document title.
    SingleDocumentSubject,
    /// No IR hint, but one document dominates the retrieved evidence
    /// by both chunk count (≥ 2× runner-up) and best score.
    EvidenceDominance,
    /// No IR hint, but one document's best score dominates the next
    /// candidate by orders of magnitude. This captures normalized
    /// document-identity lanes whose score scale is intentionally much
    /// larger than generic lexical/vector scores.
    ScoreDominance,
    /// Retrieval returned exactly one document, so fetching contiguous
    /// neighbours cannot crowd out a competing document.
    OnlyRetrievedDocument,
    /// A lifecycle-procedure query has an exact-target document containing
    /// dense ordered evidence, so pack that runbook instead of letting
    /// generic notes crowd it out.
    VersionedProcedureRunbook,
    /// A single-document query carried explicit subject/literal focus, and one
    /// document's own evidence text matched that focus better than siblings
    /// even though the document titles alone were ambiguous.
    QueryFocusEvidence,
}

impl FocusReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::DocumentFocusHint => "document_focus_hint",
            Self::SingleDocumentSubject => "single_document_subject",
            Self::EvidenceDominance => "evidence_dominance",
            Self::ScoreDominance => "score_dominance",
            Self::OnlyRetrievedDocument => "only_retrieved_document",
            Self::VersionedProcedureRunbook => "versioned_procedure_runbook",
            Self::QueryFocusEvidence => "query_focus_evidence",
        }
    }
}

/// Structured summary of the consolidation decision, suitable for
/// logging into `stage = "answer.consolidation"` records.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ConsolidationDiagnostics {
    pub(crate) focused_document_id: Option<Uuid>,
    pub(crate) focus_reason: FocusReason,
    pub(crate) winner_chunk_count: usize,
    pub(crate) tangential_chunk_count: usize,
}

impl ConsolidationDiagnostics {
    pub(crate) const fn noop() -> Self {
        Self {
            focused_document_id: None,
            focus_reason: FocusReason::None,
            winner_chunk_count: 0,
            tangential_chunk_count: 0,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct TopicalPruneDiagnostics {
    pub(crate) removed_chunk_count: usize,
    pub(crate) kept_chunk_count: usize,
    pub(crate) topical_token_count: usize,
}

use super::tuning::{
    DOCUMENT_IDENTITY_DOMINANCE_RATIO, DOCUMENT_IDENTITY_SCORE_FLOOR, FOCUSED_WINNER_EXCERPT_CHARS,
    FOCUSED_WINNER_MAX_CHUNKS,
};

mod focus;
mod orchestration;
mod selection;
#[cfg(test)]
mod tests;

#[cfg(test)]
use focus::significant_tokens;
pub(crate) use focus::{DocumentAggregate, prune_non_topical_document_tail};
// Preserve the crate-visible function surface across the mechanical split.
#[allow(unused_imports)]
pub(crate) use orchestration::{apply_winner_chunks, focused_document_consolidation};
pub(crate) use selection::decide_focus;
pub(super) use selection::query_has_multi_document_setup_anchors;
use selection::{select_winner_rows, winner_anchor_windows};

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn sample_chunk(
        document_id: Uuid,
        revision_id: Uuid,
        chunk_index: i32,
        document_label: &str,
        score: f32,
    ) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id,
            chunk_index,
            chunk_kind: None,
            document_label: document_label.to_string(),
            excerpt: format!("chunk {chunk_index}"),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(score),
            source_text: format!("source text for chunk {chunk_index}"),
        }
    }
}
