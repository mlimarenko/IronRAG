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
//! then reallocates the top-k budget to pack contiguous neighbours of
//! that winner.

use std::collections::BTreeSet;

use uuid::Uuid;

use crate::app::state::AppState;
use crate::{
    domains::query_ir::{EntityRole, QueryIR, QueryScope},
    services::query::{
        latest_versions::question_requests_latest_versions,
        text_match::{near_token_overlap_count, normalized_alnum_tokens},
    },
};

use super::{RetrievalBundle, RuntimeMatchedChunk};

/// Why the consolidation stage picked a particular winner (or no
/// winner). Carried in the answer-pipeline diagnostics so
/// `prod` logs can tell apart "IR explicitly forced this" vs. "soft
/// evidence-dominance signal" vs. "nothing to do".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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
    /// candidate by orders of magnitude. This captures canonical
    /// document-identity lanes whose score scale is intentionally much
    /// larger than generic lexical/vector scores.
    ScoreDominance,
    /// Retrieval returned exactly one document, so fetching contiguous
    /// neighbours cannot crowd out a competing document.
    OnlyRetrievedDocument,
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
        }
    }
}

/// Structured summary of the consolidation decision, suitable for
/// logging into `stage = "answer.consolidation"` records.
#[derive(Debug, Clone, serde::Serialize)]
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

#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct TopicalPruneDiagnostics {
    pub(crate) removed_chunk_count: usize,
    pub(crate) kept_chunk_count: usize,
    pub(crate) topical_token_count: usize,
}

use super::tuning::{
    DOCUMENT_IDENTITY_DOMINANCE_RATIO, DOCUMENT_IDENTITY_SCORE_FLOOR, FOCUSED_WINNER_EXCERPT_CHARS,
    FOCUSED_WINNER_MAX_CHUNKS,
};

/// When the soft "evidence dominance" branch fires, how much of the
/// `top_k` budget the winner is allowed to occupy. Smaller than the
/// explicit-IR share because the signal is weaker.
fn evidence_dominance_budget(top_k: usize) -> usize {
    let cap = ((top_k as f32) * 0.6).round() as usize;
    cap.clamp(2, top_k.saturating_sub(1).max(2))
}

fn score_dominance_budget(_top_k: usize) -> usize {
    FOCUSED_WINNER_MAX_CHUNKS
}

/// When IR pins the winner explicitly — either by hint or by
/// subject-role entity — the winner is allowed to exceed `top_k` and
/// pull as many as `FOCUSED_WINNER_MAX_CHUNKS` anchor-window chunks.
/// Tangentials are NOT given any bonus room above `top_k`: they still
/// fill only the `top_k.saturating_sub(winner_chunk_count)` residue,
/// which collapses to zero when the winner pack widens past `top_k`.
/// That keeps the "explicit focus = pack one document" invariant the
/// compiler's hint is asking for while preserving a real winner cap.
fn explicit_focus_budget(_top_k: usize) -> usize {
    FOCUSED_WINNER_MAX_CHUNKS
}

/// Extract significant lowercase tokens (≥3 chars, not pure stopwords).
/// Used by the hint-matcher to compare `document_focus.hint` against
/// retrieved document titles in a language-agnostic way (no hardcoded
/// domain words — just "skip tokens shorter than 3 chars").
fn significant_tokens(text: &str) -> BTreeSet<String> {
    normalized_alnum_tokens(text, 3)
}

fn title_tokens_for_document(
    document_id: Uuid,
    chunks: &[RuntimeMatchedChunk],
) -> BTreeSet<String> {
    chunks
        .iter()
        .find(|chunk| chunk.document_id == document_id)
        .map(|chunk| significant_tokens(&chunk.document_label))
        .unwrap_or_default()
}

fn title_tokens_by_document(
    chunks: &[RuntimeMatchedChunk],
) -> std::collections::HashMap<Uuid, BTreeSet<String>> {
    let mut map = std::collections::HashMap::new();
    for chunk in chunks {
        map.entry(chunk.document_id).or_insert_with(|| significant_tokens(&chunk.document_label));
    }
    map
}

fn topical_title_tokens(question: &str, chunks: &[RuntimeMatchedChunk]) -> BTreeSet<String> {
    let query_tokens = significant_tokens(question);
    if query_tokens.is_empty() {
        return BTreeSet::new();
    }
    let titles = title_tokens_by_document(chunks);
    let document_count = titles.len();
    if document_count < 3 {
        return BTreeSet::new();
    }

    query_tokens
        .into_iter()
        .filter(|query_token| {
            let matching_document_count = titles
                .values()
                .filter(|title_tokens| {
                    title_tokens.iter().any(|title_token| {
                        crate::services::query::text_match::near_token_match(
                            query_token,
                            title_token,
                        )
                    })
                })
                .count();

            // A token that matches several titles is a real topic
            // family. A token that matches every title is usually a
            // library-wide word, so it should not prune anything.
            matching_document_count >= 2 && matching_document_count < document_count
        })
        .collect()
}

/// Drop the ranked tail when it is outside the user's explicit topic.
///
/// Broad questions often retrieve a set of sibling documents plus a few
/// generic how-to/manual pages. If a query token identifies a multi-doc
/// title family in the current bundle, keep that family and remove the
/// unrelated tail before answer-context assembly. This is intentionally
/// data-driven: no domain words, library names, or fixture values are
/// baked into the code.
pub(crate) fn prune_non_topical_document_tail(
    bundle: &mut RetrievalBundle,
    question: &str,
) -> TopicalPruneDiagnostics {
    if question_requests_latest_versions(question) {
        return TopicalPruneDiagnostics::default();
    }
    let topical_tokens = topical_title_tokens(question, &bundle.chunks);
    if topical_tokens.is_empty() {
        return TopicalPruneDiagnostics::default();
    }

    let before = bundle.chunks.len();
    let retained = bundle
        .chunks
        .iter()
        .filter(|chunk| {
            let title_tokens = significant_tokens(&chunk.document_label);
            near_token_overlap_count(&topical_tokens, &title_tokens) > 0
        })
        .cloned()
        .collect::<Vec<_>>();
    let kept = retained.len();
    if kept == 0 {
        return TopicalPruneDiagnostics::default();
    }
    bundle.chunks = retained;

    TopicalPruneDiagnostics {
        removed_chunk_count: before.saturating_sub(kept),
        kept_chunk_count: kept,
        topical_token_count: topical_tokens.len(),
    }
}

/// Document-level aggregate over the rerank bundle. Captures both the
/// "this doc has many hits" signal (for evidence dominance) and the
/// "best-scored chunk so far" signal (so ties on evidence fall back
/// to score).
#[derive(Debug, Clone)]
pub(crate) struct DocumentAggregate {
    pub(crate) document_id: Uuid,
    pub(crate) revision_id: Uuid,
    pub(crate) evidence_count: usize,
    pub(crate) best_score: f32,
    pub(crate) anchors: BTreeSet<i32>,
}

fn aggregate_by_document(chunks: &[RuntimeMatchedChunk]) -> Vec<DocumentAggregate> {
    let mut map: std::collections::HashMap<Uuid, DocumentAggregate> =
        std::collections::HashMap::new();
    for chunk in chunks {
        let entry = map.entry(chunk.document_id).or_insert_with(|| DocumentAggregate {
            document_id: chunk.document_id,
            revision_id: chunk.revision_id,
            evidence_count: 0,
            best_score: f32::MIN,
            anchors: BTreeSet::new(),
        });
        entry.evidence_count += 1;
        entry.anchors.insert(chunk.chunk_index);
        let score = chunk.score.unwrap_or(0.0);
        if score > entry.best_score {
            entry.best_score = score;
            // Keep the most recently-seen revision_id for this doc —
            // under normal retrieval this is stable, but aggregate
            // stays defensive if a doc ever spans revisions mid-swap.
            entry.revision_id = chunk.revision_id;
        }
    }
    let mut agg: Vec<_> = map.into_values().collect();
    // Stable order: evidence desc, then score desc, then document_id for
    // determinism under ties.
    agg.sort_by(|a, b| {
        b.evidence_count
            .cmp(&a.evidence_count)
            .then_with(|| {
                b.best_score.partial_cmp(&a.best_score).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.document_id.cmp(&b.document_id))
    });
    agg
}

/// Rank candidate documents by how strongly their title overlaps the
/// reference token set. Generic brand/role tokens ("platform", "guide",
/// "manual") typically appear in every doc title of a library, so
/// "at least one overlap" is not selective — the winner has to have a
/// STRICTLY larger overlap than the runner-up to count as a match.
fn pick_by_overlap<'agg>(
    reference_tokens: &BTreeSet<String>,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    if reference_tokens.is_empty() {
        return None;
    }
    let mut scored: Vec<(usize, &DocumentAggregate)> = aggregates
        .iter()
        .filter_map(|agg| {
            let title_tokens = title_tokens_for_document(agg.document_id, chunks);
            let overlap = near_token_overlap_count(reference_tokens, &title_tokens);
            (overlap > 0).then_some((overlap, agg))
        })
        .collect();
    if scored.is_empty() {
        return None;
    }
    // Overlap desc → evidence desc → score desc → document_id for
    // determinism.
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.evidence_count.cmp(&a.1.evidence_count))
            .then_with(|| {
                b.1.best_score.partial_cmp(&a.1.best_score).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.1.document_id.cmp(&b.1.document_id))
    });
    // Require strict dominance on overlap-size to avoid promoting a doc
    // that only shares generic brand tokens with the reference.
    let top = scored[0];
    if let Some(runner) = scored.get(1)
        && top.0 <= runner.0
    {
        return None;
    }
    Some(top.1)
}

/// Pick a winner from IR `document_focus.hint` by title-overlap size.
fn winner_from_hint<'agg>(
    hint: &str,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    pick_by_overlap(&significant_tokens(hint), chunks, aggregates)
}

/// Pick a winner from IR subject-role entities when scope is
/// `SingleDocument` by title-overlap size.
fn winner_from_subject<'agg>(
    ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    let subject_tokens: BTreeSet<String> = ir
        .target_entities
        .iter()
        .filter(|entity| entity.role == EntityRole::Subject)
        .flat_map(|entity| significant_tokens(&entity.label).into_iter())
        .collect();
    pick_by_overlap(&subject_tokens, chunks, aggregates)
}

/// Soft signal: one document clearly dominates by evidence count and
/// also has the best score. Only fires when there is no explicit IR
/// pin (hint / subject), so it does not override an explicit decision.
fn winner_from_evidence_dominance<'agg>(
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    let top = aggregates.first()?;
    let runner_up = aggregates.get(1);
    if top.evidence_count < 2 {
        return None;
    }
    let Some(runner) = runner_up else {
        // Exactly one document in the bundle — dominance is trivial
        // and nothing would change by "packing" the only doc.
        return None;
    };
    if top.evidence_count < runner.evidence_count.saturating_mul(2) {
        return None;
    }
    // Top also has best score — otherwise a strongly-scored minority
    // doc is more informative than the spammy majority one.
    let top_score = top.best_score;
    let best_other = aggregates.iter().skip(1).map(|a| a.best_score).fold(f32::MIN, f32::max);
    if top_score < best_other {
        return None;
    }
    Some(top)
}

fn winner_from_score_dominance<'agg>(
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    let mut by_score: Vec<&DocumentAggregate> = aggregates
        .iter()
        .filter(|aggregate| aggregate.best_score.is_finite() && aggregate.best_score > 0.0)
        .collect();
    by_score.sort_by(|a, b| {
        b.best_score
            .partial_cmp(&a.best_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.evidence_count.cmp(&a.evidence_count))
            .then_with(|| a.document_id.cmp(&b.document_id))
    });

    let top = by_score.first().copied()?;
    let runner = by_score.get(1).copied()?;
    if top.best_score < DOCUMENT_IDENTITY_SCORE_FLOOR {
        return None;
    }
    if top.best_score >= runner.best_score * DOCUMENT_IDENTITY_DOMINANCE_RATIO {
        return Some(top);
    }
    None
}

/// Expand an anchor set into a contiguous `[min, max]` range to fetch
/// via `list_chunks_by_revision_range`. Budget controls how many
/// additional chunks can be appended forward/backward from the outer
/// anchors. "Contiguous around anchors (±1)" comes for free from the
/// outer min/max span; the extra budget is applied first forward
/// then backward so intro-chunk retrievals (chunk_index=0) grow in
/// reading order rather than padding a non-existent prefix.
fn anchor_expansion_window(
    anchors: &BTreeSet<i32>,
    budget_expand_forward: i32,
    budget_expand_backward: i32,
) -> Option<(i32, i32)> {
    let &first = anchors.iter().next()?;
    let &last = anchors.iter().next_back()?;
    let expanded_min = (first - 1 - budget_expand_backward).max(0);
    let expanded_max = last + 1 + budget_expand_forward;
    Some((expanded_min, expanded_max))
}

/// Pure decision stage — picks a winner document (or not) given the
/// current reranked bundle and the compiled IR. Split from the
/// orchestrator so unit tests can cover the decision without building
/// an AppState / Arango stub.
///
/// Returns `Some((winner, reason, budget))` iff consolidation should
/// proceed; otherwise `None`. `budget` is the number of top_k slots
/// allocated to the winner document (rounded per-reason).
pub(crate) fn decide_focus(
    bundle: &RetrievalBundle,
    query_ir: &QueryIR,
    top_k: usize,
) -> Option<(DocumentAggregate, FocusReason, usize)> {
    if top_k < 2 || bundle.chunks.is_empty() {
        return None;
    }
    if matches!(
        query_ir.scope,
        QueryScope::LibraryMeta | QueryScope::MultiDocument | QueryScope::CrossLibrary
    ) {
        return None;
    }
    let aggregates = aggregate_by_document(&bundle.chunks);

    // Priority order: explicit hint → single-doc subject → only
    // retrieved document → evidence dominance. The first match wins;
    // later branches do not run.
    if let Some(hint) = query_ir.document_focus.as_ref()
        && let Some(winner) = winner_from_hint(&hint.hint, &bundle.chunks, &aggregates)
    {
        return Some((
            winner.clone(),
            FocusReason::DocumentFocusHint,
            explicit_focus_budget(top_k),
        ));
    }
    if matches!(query_ir.scope, QueryScope::SingleDocument)
        && let Some(winner) = winner_from_subject(query_ir, &bundle.chunks, &aggregates)
    {
        return Some((
            winner.clone(),
            FocusReason::SingleDocumentSubject,
            explicit_focus_budget(top_k),
        ));
    }
    if aggregates.len() == 1 {
        return aggregates.first().cloned().map(|winner| {
            (winner, FocusReason::OnlyRetrievedDocument, explicit_focus_budget(top_k))
        });
    }
    if let Some(winner) = winner_from_score_dominance(&aggregates) {
        return Some((winner.clone(), FocusReason::ScoreDominance, score_dominance_budget(top_k)));
    }
    if let Some(winner) = winner_from_evidence_dominance(&aggregates) {
        return Some((
            winner.clone(),
            FocusReason::EvidenceDominance,
            evidence_dominance_budget(top_k),
        ));
    }
    None
}

/// Materialize `fetched_rows` into winner-chunk `RuntimeMatchedChunk`s,
/// combine with the existing tangential chunks in `bundle`, and write
/// the result back. Pure (no I/O) so it can be unit-tested directly
/// against hand-built `KnowledgeChunkRow` fixtures.
///
/// Returns the consolidation diagnostics. If the fetched set contains
/// no rows for the winner revision (or zero rows total), returns
/// `ConsolidationDiagnostics::noop()` and leaves the bundle untouched.
pub(crate) fn apply_winner_chunks(
    bundle: &mut RetrievalBundle,
    winner: &DocumentAggregate,
    focus_reason: FocusReason,
    budget: usize,
    top_k: usize,
    fetched_rows: Vec<crate::infra::arangodb::document_store::KnowledgeChunkRow>,
) -> ConsolidationDiagnostics {
    let (winner_label, winner_score_seed) = bundle
        .chunks
        .iter()
        .find(|chunk| chunk.document_id == winner.document_id)
        .map(|chunk| (chunk.document_label.clone(), chunk.score.unwrap_or(0.0)))
        .unwrap_or_else(|| (String::new(), 0.0));

    let mut winner_chunks: Vec<RuntimeMatchedChunk> = fetched_rows
        .into_iter()
        .filter(|row| row.revision_id == winner.revision_id)
        .take(budget)
        .map(|row| {
            let source_text = chunk_source_text(&row);
            let excerpt = super::retrieve::focused_excerpt_for(
                &source_text,
                &[],
                FOCUSED_WINNER_EXCERPT_CHARS,
            );
            // Bias winner scores above any tangential so the downstream
            // global sort cannot re-interleave the pack back into the
            // original fragmentation.
            let biased = winner_score_seed.max(0.0) + 10_000.0 - (row.chunk_index as f32 * 0.001);
            RuntimeMatchedChunk {
                chunk_id: row.chunk_id,
                document_id: row.document_id,
                revision_id: row.revision_id,
                chunk_index: row.chunk_index,
                document_label: winner_label.clone(),
                excerpt,
                score: Some(biased),
                source_text,
            }
        })
        .collect();
    winner_chunks.sort_by_key(|chunk| chunk.chunk_index);

    let winner_chunk_count = winner_chunks.len();
    if winner_chunk_count == 0 {
        return ConsolidationDiagnostics::noop();
    }

    let mut tangential: Vec<RuntimeMatchedChunk> = std::mem::take(&mut bundle.chunks)
        .into_iter()
        .filter(|chunk| chunk.document_id != winner.document_id)
        .collect();
    tangential.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .partial_cmp(&a.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Tangentials only fill the leftover slots *below* top_k — they are
    // a fallback for when the winner revision doesn't have enough
    // chunks to fill its share, not a bonus pad on top of an
    // already-packed winner. When `explicit_focus_budget` lets the
    // winner pack past top_k, `top_k.saturating_sub(winner_chunk_count)`
    // collapses to 0 and the bundle is all winner chunks — which is
    // exactly the "the compiler pinned ONE document, pack it" invariant.
    let tangential_slots = top_k.saturating_sub(winner_chunk_count);
    tangential.truncate(tangential_slots);
    let tangential_chunk_count = tangential.len();

    bundle.chunks = winner_chunks;
    bundle.chunks.extend(tangential);

    ConsolidationDiagnostics {
        focused_document_id: Some(winner.document_id),
        focus_reason,
        winner_chunk_count,
        tangential_chunk_count,
    }
}

/// Run the consolidation stage in-place on a reranked bundle.
///
/// Mutation contract:
/// - On `FocusReason::None` the bundle is left untouched.
/// - On any other focus reason, `bundle.chunks` is rewritten to
///   `[winner_chunks_sorted_by_index..., tangential_chunks...]`
///   truncated to the allocated winner budget + remaining slots
///   for top-scored tangentials.
/// - Winner chunks are materialized from Arango via
///   `list_chunks_by_revision_range`; if that call fails, the bundle
///   is left untouched and `FocusReason::None` is returned (we'd
///   rather ship the original than panic the whole answer).
pub(crate) async fn focused_document_consolidation(
    state: &AppState,
    bundle: &mut RetrievalBundle,
    query_ir: &QueryIR,
    top_k: usize,
) -> ConsolidationDiagnostics {
    let Some((winner, focus_reason, budget)) = decide_focus(bundle, query_ir, top_k) else {
        return ConsolidationDiagnostics::noop();
    };

    let expansion_forward = budget.saturating_sub(winner.anchors.len()).max(1) as i32;
    let expansion_backward = 1_i32;
    let Some((min_index, max_index)) =
        anchor_expansion_window(&winner.anchors, expansion_forward, expansion_backward)
    else {
        return ConsolidationDiagnostics::noop();
    };

    let fetched = match state
        .arango_document_store
        .list_chunks_by_revision_range(winner.revision_id, min_index, max_index)
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(
                stage = "answer.consolidation",
                %error,
                document_id = %winner.document_id,
                revision_id = %winner.revision_id,
                "range fetch failed — consolidation skipped"
            );
            return ConsolidationDiagnostics::noop();
        }
    };

    let diagnostics = apply_winner_chunks(bundle, &winner, focus_reason, budget, top_k, fetched);

    if diagnostics.focused_document_id.is_some() {
        tracing::info!(
            stage = "answer.consolidation",
            focus_reason = diagnostics.focus_reason.as_str(),
            document_id = ?diagnostics.focused_document_id,
            revision_id = %winner.revision_id,
            winner_chunk_count = diagnostics.winner_chunk_count,
            tangential_chunk_count = diagnostics.tangential_chunk_count,
            top_k,
            budget,
            "consolidation reshaped retrieval bundle"
        );
    }

    diagnostics
}

fn chunk_source_text(chunk: &crate::infra::arangodb::document_store::KnowledgeChunkRow) -> String {
    use crate::shared::extraction::text_render::repair_technical_layout_noise;
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    if chunk.content_text.trim().is_empty() && !chunk.normalized_text.trim().is_empty() {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    repair_technical_layout_noise(&chunk.content_text)
}

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
            document_label: document_label.to_string(),
            excerpt: format!("chunk {chunk_index}"),
            score: Some(score),
            source_text: format!("source text for chunk {chunk_index}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::sample_chunk;
    use super::*;

    use crate::domains::query_ir::{
        DocumentHint, EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope,
    };
    use crate::infra::arangodb::document_store::KnowledgeChunkRow;

    fn ir(scope: QueryScope) -> QueryIR {
        QueryIR {
            act: QueryAct::ConfigureHow,
            scope,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            confidence: 0.9,
        }
    }

    fn chunk_row(
        document_id: Uuid,
        revision_id: Uuid,
        chunk_index: i32,
        content: &str,
    ) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id,
            revision_id,
            chunk_index,
            chunk_kind: Some("paragraph".to_string()),
            content_text: content.to_string(),
            normalized_text: content.to_string(),
            span_start: Some(0),
            span_end: Some(content.len() as i32),
            token_count: Some(1),
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: None,
        }
    }

    fn run_apply(
        bundle: &mut RetrievalBundle,
        query_ir: &QueryIR,
        top_k: usize,
        revision_rows: Vec<KnowledgeChunkRow>,
    ) -> ConsolidationDiagnostics {
        let Some((winner, reason, budget)) = decide_focus(bundle, query_ir, top_k) else {
            return ConsolidationDiagnostics::noop();
        };
        // Pure path also validates the anchor window is well-formed
        // for the chosen winner before we apply; mirrors the async
        // orchestrator's guard.
        let expansion_forward = budget.saturating_sub(winner.anchors.len()).max(1) as i32;
        let _ = anchor_expansion_window(&winner.anchors, expansion_forward, 1)
            .expect("anchors present imply window");
        apply_winner_chunks(bundle, &winner, reason, budget, top_k, revision_rows)
    }

    #[test]
    fn test_consolidation_explicit_single_doc() {
        let subject_doc = Uuid::now_v7();
        let subject_rev = Uuid::now_v7();
        let tangential_doc = Uuid::now_v7();
        let tangential_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(subject_doc, subject_rev, 0, "Provider Alpha Admin Guide", 0.9),
                sample_chunk(tangential_doc, tangential_rev, 0, "Provider B Manual", 0.8),
                sample_chunk(tangential_doc, tangential_rev, 1, "Provider B Manual", 0.7),
                sample_chunk(tangential_doc, tangential_rev, 2, "Provider B Manual", 0.6),
            ],
        };

        let mut query_ir = ir(QueryScope::SingleDocument);
        query_ir.target_entities =
            vec![EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject }];

        // Winner revision has 6 chunks (0..=5). Fetched range will
        // respect budget = 75% of 8 = 6 slots.
        let revision_rows: Vec<_> = (0..6)
            .map(|idx| chunk_row(subject_doc, subject_rev, idx, &format!("winner chunk {idx}")))
            .collect();

        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
        assert_eq!(diag.focus_reason, FocusReason::SingleDocumentSubject);
        assert_eq!(diag.focused_document_id, Some(subject_doc));
        assert!(diag.winner_chunk_count >= 6);

        // Winner chunks come first and are sorted by chunk_index ascending.
        let winner_slice: Vec<_> = bundle.chunks.iter().take(diag.winner_chunk_count).collect();
        for chunk in &winner_slice {
            assert_eq!(chunk.document_id, subject_doc);
            assert_eq!(chunk.revision_id, subject_rev);
        }
        let indices: Vec<i32> = winner_slice.iter().map(|c| c.chunk_index).collect();
        let expected: Vec<i32> = (*indices.first().unwrap()..=*indices.last().unwrap()).collect();
        assert_eq!(indices, expected, "winner chunks must be contiguous and sorted by chunk_index");
        // Budget = max(top_k, FOCUSED_WINNER_MAX_CHUNKS) = 16 for top_k=8,
        // but the revision only has 6 chunks; tangentials fill the
        // `top_k - winner_chunk_count` residue so the bundle never
        // exceeds top_k when the winner pack stays under it.
        assert!(bundle.chunks.len() <= 8);
    }

    #[test]
    fn test_consolidation_document_focus_hint() {
        let hint_doc = Uuid::now_v7();
        let hint_rev = Uuid::now_v7();
        let other_doc = Uuid::now_v7();
        let other_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(hint_doc, hint_rev, 0, "Payment Module Provider A Admin", 0.6),
                sample_chunk(other_doc, other_rev, 0, "Platform POS Manual", 0.9),
                sample_chunk(other_doc, other_rev, 1, "Platform POS Manual", 0.85),
            ],
        };

        let mut query_ir = ir(QueryScope::SingleDocument);
        query_ir.document_focus = Some(DocumentHint { hint: "provider a".to_string() });

        let revision_rows: Vec<_> = (0..8)
            .map(|idx| chunk_row(hint_doc, hint_rev, idx, &format!("hint chunk {idx}")))
            .collect();

        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
        assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
        assert_eq!(diag.focused_document_id, Some(hint_doc));
        assert_eq!(diag.winner_chunk_count, 8);
        assert_eq!(diag.tangential_chunk_count, 0);
        for chunk in &bundle.chunks {
            assert_eq!(chunk.document_id, hint_doc);
        }
    }

    #[test]
    fn test_consolidation_subject_match_tolerates_single_token_edit() {
        let subject_doc = Uuid::now_v7();
        let subject_rev = Uuid::now_v7();
        let other_doc = Uuid::now_v7();
        let other_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(subject_doc, subject_rev, 0, "Connector TargetName Guide", 0.6),
                sample_chunk(other_doc, other_rev, 0, "Connector Alpha Guide", 0.9),
                sample_chunk(other_doc, other_rev, 1, "Connector Alpha Guide", 0.8),
            ],
        };

        let mut query_ir = ir(QueryScope::SingleDocument);
        query_ir.target_entities =
            vec![EntityMention { label: "TargetNme".to_string(), role: EntityRole::Subject }];

        let revision_rows: Vec<_> = (0..8)
            .map(|idx| chunk_row(subject_doc, subject_rev, idx, &format!("subject chunk {idx}")))
            .collect();

        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);

        assert_eq!(diag.focus_reason, FocusReason::SingleDocumentSubject);
        assert_eq!(diag.focused_document_id, Some(subject_doc));
    }

    #[test]
    fn test_consolidation_multi_document_negative() {
        let doc_a = Uuid::now_v7();
        let doc_b = Uuid::now_v7();
        let rev_a = Uuid::now_v7();
        let rev_b = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(doc_a, rev_a, 0, "Doc A", 0.9),
                sample_chunk(doc_a, rev_a, 1, "Doc A", 0.8),
                sample_chunk(doc_b, rev_b, 0, "Doc B", 0.7),
                sample_chunk(doc_b, rev_b, 1, "Doc B", 0.6),
            ],
        };
        let original_ids: Vec<Uuid> = bundle.chunks.iter().map(|c| c.chunk_id).collect();

        let mut query_ir = ir(QueryScope::MultiDocument);
        query_ir.target_entities =
            vec![EntityMention { label: "Doc A".to_string(), role: EntityRole::Subject }];

        let diag = run_apply(&mut bundle, &query_ir, 8, Vec::new());
        assert_eq!(diag.focus_reason, FocusReason::None);
        assert_eq!(diag.focused_document_id, None);
        let after_ids: Vec<Uuid> = bundle.chunks.iter().map(|c| c.chunk_id).collect();
        assert_eq!(after_ids, original_ids, "bundle must be untouched on multi-document scope");
    }

    #[test]
    fn test_consolidation_evidence_dominance() {
        let winner_doc = Uuid::now_v7();
        let winner_rev = Uuid::now_v7();
        let other_doc = Uuid::now_v7();
        let other_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(winner_doc, winner_rev, 0, "Dominant doc", 0.9),
                sample_chunk(winner_doc, winner_rev, 1, "Dominant doc", 0.85),
                sample_chunk(winner_doc, winner_rev, 2, "Dominant doc", 0.8),
                sample_chunk(winner_doc, winner_rev, 3, "Dominant doc", 0.75),
                sample_chunk(other_doc, other_rev, 0, "Other doc", 0.6),
                sample_chunk(other_doc, other_rev, 1, "Other doc", 0.55),
            ],
        };

        // Plain descriptive IR with no hint / no subject — only the
        // structural evidence signal can fire.
        let query_ir = ir(QueryScope::SingleDocument);

        let revision_rows: Vec<_> = (0..6)
            .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner {idx}")))
            .collect();
        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
        assert_eq!(diag.focus_reason, FocusReason::EvidenceDominance);
        assert_eq!(diag.focused_document_id, Some(winner_doc));
        // Budget cap ≈ 60% of 8 = 5 → winner chunk count should be
        // at most 5 (not the full 6 available).
        assert!(diag.winner_chunk_count <= 5);
    }

    #[test]
    fn test_consolidation_score_dominance_packs_identity_lane_winner() {
        let winner_doc = Uuid::now_v7();
        let winner_rev = Uuid::now_v7();
        let other_doc = Uuid::now_v7();
        let other_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(winner_doc, winner_rev, 0, "Untitled external source", 1_000_000.0),
                sample_chunk(other_doc, other_rev, 0, "Other doc", 50.0),
                sample_chunk(other_doc, other_rev, 1, "Other doc", 49.0),
            ],
        };
        let query_ir = ir(QueryScope::SingleDocument);
        let revision_rows: Vec<_> = (0..24)
            .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner chunk {idx}")))
            .collect();

        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);

        assert_eq!(diag.focus_reason, FocusReason::ScoreDominance);
        assert_eq!(diag.focused_document_id, Some(winner_doc));
        assert!(diag.winner_chunk_count > 1);
        assert!(diag.winner_chunk_count <= 16);
        assert!(bundle.chunks.iter().all(|chunk| chunk.document_id == winner_doc));
    }

    #[test]
    fn test_consolidation_score_dominance_ignores_normal_score_spread() {
        let doc_a = Uuid::now_v7();
        let rev_a = Uuid::now_v7();
        let doc_b = Uuid::now_v7();
        let rev_b = Uuid::now_v7();

        let bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(doc_a, rev_a, 0, "Doc A", 1.0),
                sample_chunk(doc_b, rev_b, 0, "Doc B", 0.5),
            ],
        };
        let query_ir = ir(QueryScope::SingleDocument);

        assert!(decide_focus(&bundle, &query_ir, 8).is_none());
    }

    #[test]
    fn test_consolidation_score_dominance_requires_identity_scale_and_finite_scores() {
        let doc_a = Uuid::now_v7();
        let rev_a = Uuid::now_v7();
        let doc_b = Uuid::now_v7();
        let rev_b = Uuid::now_v7();

        let below_identity_scale = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(doc_a, rev_a, 0, "Doc A", 1_000.0),
                sample_chunk(doc_b, rev_b, 0, "Doc B", 0.5),
            ],
        };
        let query_ir = ir(QueryScope::SingleDocument);

        assert!(decide_focus(&below_identity_scale, &query_ir, 8).is_none());

        let non_finite = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(doc_a, rev_a, 0, "Doc A", f32::INFINITY),
                sample_chunk(doc_b, rev_b, 0, "Doc B", 1.0),
            ],
        };

        assert!(decide_focus(&non_finite, &query_ir, 8).is_none());
    }

    #[test]
    fn test_consolidation_only_retrieved_document_fetches_neighbours() {
        let winner_doc = Uuid::now_v7();
        let winner_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![sample_chunk(winner_doc, winner_rev, 0, "Only matched guide", 0.9)],
        };
        let query_ir = ir(QueryScope::SingleDocument);

        let revision_rows: Vec<_> = (0..24)
            .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner chunk {idx}")))
            .collect();
        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
        assert_eq!(diag.focus_reason, FocusReason::OnlyRetrievedDocument);
        assert_eq!(diag.focused_document_id, Some(winner_doc));
        assert!(
            diag.winner_chunk_count > 1,
            "single-document retrieval must fetch neighbouring chunks"
        );
        assert!(diag.winner_chunk_count <= 16);
        assert!(bundle.chunks.iter().all(|chunk| chunk.document_id == winner_doc));
    }

    #[test]
    fn test_consolidation_winner_excerpt_preserves_late_technical_literal() {
        let winner_doc = Uuid::now_v7();
        let winner_rev = Uuid::now_v7();
        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![sample_chunk(winner_doc, winner_rev, 0, "Only matched guide", 0.9)],
        };
        let query_ir = ir(QueryScope::SingleDocument);
        let late_literal = "callbackEndpoint=https://localhost.example/internal/api";
        let long_content = format!("{} {late_literal}", "context ".repeat(70));
        let revision_rows = vec![chunk_row(winner_doc, winner_rev, 0, &long_content)];

        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);

        assert_eq!(diag.focus_reason, FocusReason::OnlyRetrievedDocument);
        assert!(
            bundle.chunks[0].excerpt.contains(late_literal),
            "winner excerpts must not truncate late technical literals"
        );
    }

    #[test]
    fn test_consolidation_tie_no_action() {
        let doc_a = Uuid::now_v7();
        let doc_b = Uuid::now_v7();
        let rev_a = Uuid::now_v7();
        let rev_b = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(doc_a, rev_a, 0, "Alpha guide", 0.9),
                sample_chunk(doc_a, rev_a, 1, "Alpha guide", 0.85),
                sample_chunk(doc_a, rev_a, 2, "Alpha guide", 0.8),
                sample_chunk(doc_b, rev_b, 0, "Bravo guide", 0.88),
                sample_chunk(doc_b, rev_b, 1, "Bravo guide", 0.82),
            ],
        };
        let original_ids: Vec<Uuid> = bundle.chunks.iter().map(|c| c.chunk_id).collect();

        // No IR hint, no subject — evidence ratio 3/2 = 1.5 < 2.0 cutoff.
        let query_ir = ir(QueryScope::SingleDocument);
        let diag = run_apply(&mut bundle, &query_ir, 8, Vec::new());
        assert_eq!(diag.focus_reason, FocusReason::None);
        let after_ids: Vec<Uuid> = bundle.chunks.iter().map(|c| c.chunk_id).collect();
        assert_eq!(after_ids, original_ids);
    }

    #[test]
    fn test_consolidation_budget_cap() {
        let winner_doc = Uuid::now_v7();
        let winner_rev = Uuid::now_v7();
        let other_doc = Uuid::now_v7();
        let other_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(winner_doc, winner_rev, 0, "Provider A Admin Guide", 0.9),
                sample_chunk(other_doc, other_rev, 0, "Tangent 1", 0.85),
                sample_chunk(other_doc, other_rev, 1, "Tangent 1", 0.8),
                sample_chunk(other_doc, other_rev, 2, "Tangent 1", 0.75),
                sample_chunk(other_doc, other_rev, 3, "Tangent 1", 0.7),
                sample_chunk(other_doc, other_rev, 4, "Tangent 1", 0.65),
                sample_chunk(other_doc, other_rev, 5, "Tangent 1", 0.6),
            ],
        };

        let mut query_ir = ir(QueryScope::SingleDocument);
        query_ir.document_focus = Some(DocumentHint { hint: "provider a".to_string() });

        // Revision only has 3 chunks total — even with full budget,
        // winner_chunk_count must be capped at 3, remaining 5 slots
        // get top tangentials.
        let revision_rows = vec![
            chunk_row(winner_doc, winner_rev, 0, "intro"),
            chunk_row(winner_doc, winner_rev, 1, "config"),
            chunk_row(winner_doc, winner_rev, 2, "verify"),
        ];

        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
        assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
        assert_eq!(diag.winner_chunk_count, 3);
        assert_eq!(diag.tangential_chunk_count, 5);
        assert_eq!(bundle.chunks.len(), 8);

        // First three are winner chunks sorted by index; rest are tangentials.
        for (slot, chunk) in bundle.chunks.iter().take(3).enumerate() {
            assert_eq!(chunk.document_id, winner_doc, "slot {slot} must be winner");
            assert_eq!(chunk.chunk_index, slot as i32);
        }
        for chunk in bundle.chunks.iter().skip(3) {
            assert_eq!(chunk.document_id, other_doc);
        }
    }

    #[test]
    fn test_consolidation_explicit_single_doc_extended_budget() {
        // When the compiler explicitly pins one document via a hint
        // and that document has a long config / contract body, the
        // winner pack is allowed to exceed `top_k` so the LLM sees
        // the whole anchor-window instead of an arbitrary 8-chunk
        // slice truncated in the middle of a section.
        let winner_doc = Uuid::now_v7();
        let winner_rev = Uuid::now_v7();
        let other_doc = Uuid::now_v7();
        let other_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(winner_doc, winner_rev, 0, "Provider A Admin Guide", 0.9),
                sample_chunk(other_doc, other_rev, 0, "Tangent", 0.6),
                sample_chunk(other_doc, other_rev, 1, "Tangent", 0.55),
            ],
        };
        let mut query_ir = ir(QueryScope::SingleDocument);
        query_ir.document_focus = Some(DocumentHint { hint: "provider a".to_string() });

        // Revision has 24 chunks — strictly more than `top_k = 8` and
        // more than `FOCUSED_WINNER_MAX_CHUNKS = 16`.
        let revision_rows: Vec<_> = (0..24)
            .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner chunk {idx}")))
            .collect();

        let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
        assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
        assert_eq!(diag.focused_document_id, Some(winner_doc));
        assert!(
            diag.winner_chunk_count > 8,
            "winner must be allowed to exceed top_k = 8 under explicit focus, got {}",
            diag.winner_chunk_count
        );
        assert!(
            diag.winner_chunk_count <= 16,
            "winner budget must stay inside FOCUSED_WINNER_MAX_CHUNKS = 16"
        );
        // Bundle stays inside the explicit-focus cap.
        assert!(bundle.chunks.len() <= 16, "bundle must not exceed explicit focus cap");
    }

    #[test]
    fn test_consolidation_explicit_budget_is_hard_cap() {
        let winner_doc = Uuid::now_v7();
        let winner_rev = Uuid::now_v7();

        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![sample_chunk(winner_doc, winner_rev, 0, "Provider A Admin Guide", 0.9)],
        };
        let mut query_ir = ir(QueryScope::SingleDocument);
        query_ir.document_focus = Some(DocumentHint { hint: "provider a".to_string() });

        let revision_rows: Vec<_> = (0..32)
            .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner chunk {idx}")))
            .collect();

        let diag = run_apply(&mut bundle, &query_ir, 32, revision_rows);
        assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
        assert_eq!(diag.winner_chunk_count, 16);
        assert_eq!(bundle.chunks.len(), 16);
    }

    #[test]
    fn topical_prune_keeps_title_family_and_drops_generic_tail() {
        let payment_a = Uuid::now_v7();
        let payment_b = Uuid::now_v7();
        let generic = Uuid::now_v7();
        let rev = Uuid::now_v7();
        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(payment_a, rev, 0, "Payment Alpha Admin Guide", 0.9),
                sample_chunk(payment_b, rev, 0, "Payment Beta Admin Guide", 0.8),
                sample_chunk(generic, rev, 0, "Generic Installation HOWTO", 0.7),
            ],
        };

        let diagnostics = prune_non_topical_document_tail(&mut bundle, "how to configure payment");

        assert_eq!(diagnostics.removed_chunk_count, 1);
        assert_eq!(diagnostics.kept_chunk_count, 2);
        assert!(bundle.chunks.iter().all(|chunk| chunk.document_label.contains("Payment")));
    }

    #[test]
    fn topical_prune_skips_latest_version_multi_document_queries() {
        let release_a = Uuid::now_v7();
        let release_b = Uuid::now_v7();
        let generic = Uuid::now_v7();
        let rev = Uuid::now_v7();
        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![
                sample_chunk(release_a, rev, 0, "Version 9.8.765 Admin Guide", 0.9),
                sample_chunk(release_b, rev, 0, "Version 9.8.764 Admin Guide", 0.8),
                sample_chunk(generic, rev, 0, "Admin Guide", 0.7),
            ],
        };

        let diagnostics =
            prune_non_topical_document_tail(&mut bundle, "что нового в последних 5 релизах");

        assert_eq!(diagnostics.removed_chunk_count, 0);
        assert_eq!(bundle.chunks.len(), 3);
    }

    #[test]
    fn topical_prune_ignores_library_wide_title_tokens() {
        let doc_a = Uuid::now_v7();
        let doc_b = Uuid::now_v7();
        let doc_c = Uuid::now_v7();
        let rev = Uuid::now_v7();
        let original = vec![
            sample_chunk(doc_a, rev, 0, "Platform Alpha Guide", 0.9),
            sample_chunk(doc_b, rev, 0, "Platform Beta Guide", 0.8),
            sample_chunk(doc_c, rev, 0, "Platform Gamma Guide", 0.7),
        ];
        let mut bundle =
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks: original };

        let diagnostics = prune_non_topical_document_tail(&mut bundle, "platform setup");

        assert_eq!(diagnostics.removed_chunk_count, 0);
        assert_eq!(bundle.chunks.len(), 3);
    }

    #[test]
    fn significant_tokens_language_agnostic() {
        // Hint-matching must not rely on domain vocabulary — any
        // 3+ char alphanumeric token counts, regardless of language.
        let tokens = significant_tokens("ProviderA PaymentModule Guide v2.0");
        assert!(tokens.contains("providera"));
        assert!(tokens.contains("paymentmodule"));
    }
}
