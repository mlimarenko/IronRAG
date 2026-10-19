use super::{
    DOCUMENT_IDENTITY_DOMINANCE_RATIO, DOCUMENT_IDENTITY_SCORE_FLOOR, EntityRole,
    FOCUSED_WINNER_MAX_CHUNKS, QueryAct, QueryIR, QueryTargetKind, RetrievalBundle,
    RuntimeMatchedChunk, TopicalPruneDiagnostics, Uuid, add_label_terms_with_acronyms,
    extract_keywords, is_source_profile_runtime_chunk, near_token_overlap_count,
    normalized_alnum_tokens,
};
use std::collections::{BTreeSet, HashMap};

/// When the soft "evidence dominance" branch fires, how much of the
/// `top_k` budget the winner is allowed to occupy. Smaller than the
/// explicit-IR share because the signal is weaker.
pub(super) fn evidence_dominance_budget(top_k: usize) -> usize {
    let cap = ((top_k as f32) * 0.6).round() as usize;
    cap.clamp(2, top_k.saturating_sub(1).max(2))
}

pub(super) fn score_dominance_budget(_top_k: usize) -> usize {
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
pub(super) fn explicit_focus_budget(_top_k: usize) -> usize {
    FOCUSED_WINNER_MAX_CHUNKS
}

/// Extract significant lowercase tokens (≥3 chars, not pure stopwords).
/// Used by the hint-matcher to compare `document_focus.hint` against
/// retrieved document titles in a language-agnostic way (no hardcoded
/// domain words — just "skip tokens shorter than 3 chars").
pub(super) fn significant_tokens(text: &str) -> BTreeSet<String> {
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

#[derive(Debug, Default)]
struct LabelOverlapTerms {
    tokens: BTreeSet<String>,
    acronym_terms: BTreeSet<String>,
    primary_term_count: usize,
}

impl LabelOverlapTerms {
    fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.acronym_terms.is_empty()
    }

    fn total_term_count(&self) -> usize {
        self.primary_term_count
    }
}

fn label_overlap_terms(label: &str, min_token_chars: usize) -> LabelOverlapTerms {
    let mut terms = LabelOverlapTerms {
        primary_term_count: normalized_alnum_tokens(label, min_token_chars).len(),
        ..Default::default()
    };
    add_label_terms_with_acronyms(
        &mut terms.tokens,
        &mut terms.acronym_terms,
        label,
        min_token_chars,
    );
    terms
}

fn subject_title_tokens_for_document(
    document_id: Uuid,
    chunks: &[RuntimeMatchedChunk],
) -> BTreeSet<String> {
    chunks
        .iter()
        .find(|chunk| chunk.document_id == document_id)
        .map(|chunk| normalized_alnum_tokens(&chunk.document_label, 2))
        .unwrap_or_default()
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

// RRF-scored ordinary lexical/vector hits are around 0.01. Additive
// evidence lanes (entity bio, graph evidence, query-IR focus) preserve
// source scores near 1.0+ so downstream pruning can recognise them as
// intentional retrieval anchors even when the document title is generic.
const ADDITIVE_EVIDENCE_PRUNE_SCORE_FLOOR: f32 = 0.9;

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
    query_ir: Option<&QueryIR>,
    skip_prune: bool,
) -> TopicalPruneDiagnostics {
    if skip_prune {
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
                || is_additive_evidence_chunk(chunk)
                || query_ir.is_some_and(|ir| {
                    super::retrieve::chunk_is_versioned_update_instruction_title_anchor(
                        question, ir, chunk,
                    )
                })
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

fn is_additive_evidence_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    chunk.score.unwrap_or(0.0) >= ADDITIVE_EVIDENCE_PRUNE_SCORE_FLOOR
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
    pub(crate) content_anchors: BTreeSet<i32>,
    pub(crate) ranked_content_anchors: Vec<i32>,
}

#[derive(Debug, Clone, Copy)]
struct AnchorRank {
    chunk_index: i32,
    focus_score: usize,
    retrieval_score: f32,
    first_rank: usize,
}

pub(super) fn aggregate_by_document(
    chunks: &[RuntimeMatchedChunk],
    question: &str,
) -> Vec<DocumentAggregate> {
    let question_keywords = extract_keywords(question);
    let mut map: HashMap<Uuid, DocumentAggregate> = HashMap::new();
    let mut anchor_ranks = HashMap::<Uuid, HashMap<i32, AnchorRank>>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        if is_source_profile_runtime_chunk(chunk) {
            continue;
        }
        let entry = map.entry(chunk.document_id).or_insert_with(|| DocumentAggregate {
            document_id: chunk.document_id,
            revision_id: chunk.revision_id,
            evidence_count: 0,
            best_score: f32::MIN,
            content_anchors: BTreeSet::new(),
            ranked_content_anchors: Vec::new(),
        });
        entry.evidence_count += 1;
        if entry.content_anchors.insert(chunk.chunk_index) {
            entry.ranked_content_anchors.push(chunk.chunk_index);
        }
        let score = chunk.score.unwrap_or(0.0);
        if score > entry.best_score {
            entry.best_score = score;
            // Keep the most recently-seen revision_id for this doc —
            // under normal retrieval this is stable, but aggregate
            // stays defensive if a doc ever spans revisions mid-swap.
            entry.revision_id = chunk.revision_id;
        }
        let focus_score = anchor_focus_score(chunk, &question_keywords);
        anchor_ranks
            .entry(chunk.document_id)
            .or_default()
            .entry(chunk.chunk_index)
            .and_modify(|existing| {
                if anchor_rank_is_better(focus_score, score, rank, existing) {
                    *existing = AnchorRank {
                        chunk_index: chunk.chunk_index,
                        focus_score,
                        retrieval_score: score,
                        first_rank: rank,
                    };
                }
            })
            .or_insert(AnchorRank {
                chunk_index: chunk.chunk_index,
                focus_score,
                retrieval_score: score,
                first_rank: rank,
            });
    }
    let mut agg: Vec<_> = map
        .into_values()
        .map(|mut aggregate| {
            if let Some(anchor_rank_map) = anchor_ranks.remove(&aggregate.document_id) {
                let mut ranks = anchor_rank_map.into_values().collect::<Vec<_>>();
                ranks.sort_by(|left, right| {
                    right
                        .focus_score
                        .cmp(&left.focus_score)
                        .then_with(|| right.retrieval_score.total_cmp(&left.retrieval_score))
                        .then_with(|| left.first_rank.cmp(&right.first_rank))
                        .then_with(|| left.chunk_index.cmp(&right.chunk_index))
                });
                aggregate.ranked_content_anchors =
                    ranks.into_iter().map(|rank| rank.chunk_index).collect();
            }
            aggregate
        })
        .collect();
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

fn anchor_rank_is_better(
    focus_score: usize,
    retrieval_score: f32,
    rank: usize,
    existing: &AnchorRank,
) -> bool {
    focus_score > existing.focus_score
        || (focus_score == existing.focus_score && retrieval_score > existing.retrieval_score)
        || (focus_score == existing.focus_score
            && retrieval_score == existing.retrieval_score
            && rank < existing.first_rank)
}

fn anchor_focus_score(chunk: &RuntimeMatchedChunk, question_keywords: &[String]) -> usize {
    if question_keywords.is_empty() {
        return 0;
    }
    let haystack = format!("{} {}", chunk.excerpt, chunk.source_text).to_lowercase();
    question_keywords
        .iter()
        .filter(|keyword| haystack.contains(keyword.as_str()))
        .map(|keyword| keyword.chars().count().max(1))
        .sum()
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
            is_selective_title_overlap(overlap, reference_tokens.len(), title_tokens.len())
                .then_some((overlap, agg))
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

fn pick_by_subject_overlap<'agg>(
    reference_terms: &LabelOverlapTerms,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    if reference_terms.is_empty() {
        return None;
    }
    let mut scored: Vec<(usize, usize, usize, &DocumentAggregate)> = aggregates
        .iter()
        .filter_map(|agg| {
            let title_tokens = subject_title_tokens_for_document(agg.document_id, chunks);
            let token_overlap = near_token_overlap_count(&reference_terms.tokens, &title_tokens);
            let acronym_overlap =
                near_token_overlap_count(&reference_terms.acronym_terms, &title_tokens);
            let score = token_overlap.saturating_add(acronym_overlap.saturating_mul(3));
            subject_overlap_is_selective(
                token_overlap,
                acronym_overlap,
                reference_terms.total_term_count(),
            )
            .then_some((score, token_overlap, acronym_overlap, agg))
        })
        .collect();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| b.3.evidence_count.cmp(&a.3.evidence_count))
            .then_with(|| {
                b.3.best_score.partial_cmp(&a.3.best_score).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.3.document_id.cmp(&b.3.document_id))
    });
    let top = scored[0];
    if let Some(runner) = scored.get(1)
        && top.0 <= runner.0
    {
        return None;
    }
    Some(top.3)
}

fn is_selective_title_overlap(
    overlap: usize,
    reference_token_count: usize,
    _title_token_count: usize,
) -> bool {
    overlap >= 2 || (overlap == 1 && reference_token_count == 1)
}

fn subject_overlap_is_selective(
    token_overlap: usize,
    acronym_overlap: usize,
    reference_term_count: usize,
) -> bool {
    acronym_overlap >= 1 || token_overlap >= 2 || (token_overlap == 1 && reference_term_count == 1)
}

/// Pick a winner from IR `document_focus.hint` by title-overlap size.
pub(super) fn winner_from_hint<'agg>(
    hint: &str,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    pick_by_overlap(&significant_tokens(hint), chunks, aggregates)
}

/// Pick a winner from IR subject-role entities when scope is
/// `SingleDocument` by title-overlap size.
pub(super) fn winner_from_subject<'agg>(
    ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    if !query_ir_subject_can_pin_document(ir) {
        return None;
    }
    let subject_tokens: LabelOverlapTerms = ir
        .target_entities
        .iter()
        .filter(|entity| entity.role == EntityRole::Subject)
        .fold(LabelOverlapTerms::default(), |mut terms, entity| {
            let entity_terms = label_overlap_terms(&entity.label, 2);
            terms.tokens.extend(entity_terms.tokens);
            terms.acronym_terms.extend(entity_terms.acronym_terms);
            terms.primary_term_count += entity_terms.primary_term_count;
            terms
        });
    pick_by_subject_overlap(&subject_tokens, chunks, aggregates)
}

pub(super) fn winner_from_query_focus_evidence<'agg>(
    ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &'agg [DocumentAggregate],
) -> Option<&'agg DocumentAggregate> {
    if !matches!(ir.scope, crate::domains::query_ir::QueryScope::SingleDocument)
        || ir.source_slice.is_some()
        || ir.comparison.is_some()
        || !matches!(ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
    {
        return None;
    }
    let focus = QueryFocusEvidenceTerms::new(ir);
    if focus.is_empty() {
        return None;
    }
    let mut scored = aggregates
        .iter()
        .filter_map(|aggregate| {
            let score = query_focus_evidence_document_score(aggregate.document_id, chunks, &focus);
            (score > 0).then_some((score, aggregate))
        })
        .collect::<Vec<_>>();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.evidence_count.cmp(&left.1.evidence_count))
            .then_with(|| right.1.best_score.total_cmp(&left.1.best_score))
            .then_with(|| left.1.document_id.cmp(&right.1.document_id))
    });
    let (top_score, top) = scored[0];
    if let Some((runner_score, _)) = scored.get(1)
        && top_score <= *runner_score
    {
        return None;
    }
    Some(top)
}

#[derive(Debug, Default)]
struct QueryFocusEvidenceTerms {
    subject_tokens: BTreeSet<String>,
    literal_tokens: BTreeSet<String>,
    document_tokens: BTreeSet<String>,
}

impl QueryFocusEvidenceTerms {
    fn new(ir: &QueryIR) -> Self {
        let mut terms = Self::default();
        for entity in ir.target_entities.iter().filter(|entity| entity.role == EntityRole::Subject)
        {
            terms.subject_tokens.extend(normalized_alnum_tokens(&entity.label, 2));
        }
        if let Some(document_focus) = ir.document_focus.as_ref() {
            terms.document_tokens.extend(normalized_alnum_tokens(&document_focus.hint, 2));
        }
        for literal in &ir.literal_constraints {
            terms.literal_tokens.extend(normalized_alnum_tokens(&literal.text, 2));
        }
        terms
    }

    fn is_empty(&self) -> bool {
        self.subject_tokens.is_empty()
            && self.literal_tokens.is_empty()
            && self.document_tokens.is_empty()
    }
}

fn exact_token_overlap_count(expected: &BTreeSet<String>, observed: &BTreeSet<String>) -> usize {
    expected.intersection(observed).count()
}

fn query_focus_evidence_document_score(
    document_id: Uuid,
    chunks: &[RuntimeMatchedChunk],
    focus: &QueryFocusEvidenceTerms,
) -> usize {
    let mut title_tokens = BTreeSet::<String>::new();
    let mut evidence_tokens = BTreeSet::<String>::new();
    for chunk in chunks.iter().filter(|chunk| chunk.document_id == document_id) {
        title_tokens.extend(normalized_alnum_tokens(&chunk.document_label, 2));
        evidence_tokens.extend(normalized_alnum_tokens(
            &format!("{} {}", chunk.excerpt, chunk.source_text),
            2,
        ));
    }
    if title_tokens.is_empty() && evidence_tokens.is_empty() {
        return 0;
    }
    let subject_title_overlap = exact_token_overlap_count(&focus.subject_tokens, &title_tokens);
    let subject_evidence_overlap =
        exact_token_overlap_count(&focus.subject_tokens, &evidence_tokens);
    let literal_overlap = exact_token_overlap_count(&focus.literal_tokens, &evidence_tokens);
    let document_overlap = exact_token_overlap_count(&focus.document_tokens, &title_tokens)
        + exact_token_overlap_count(&focus.document_tokens, &evidence_tokens);

    if !focus.subject_tokens.is_empty()
        && subject_title_overlap == 0
        && subject_evidence_overlap == 0
    {
        return 0;
    }
    if !focus.literal_tokens.is_empty()
        && literal_overlap == 0
        && subject_evidence_overlap <= subject_title_overlap
    {
        return 0;
    }

    subject_title_overlap
        .saturating_mul(96)
        .saturating_add(subject_evidence_overlap.saturating_mul(32))
        .saturating_add(literal_overlap.saturating_mul(40))
        .saturating_add(document_overlap.saturating_mul(64))
}

fn query_ir_subject_can_pin_document(ir: &QueryIR) -> bool {
    if ir.targets(QueryTargetKind::Document) {
        return true;
    }
    ir.target_entities.iter().filter(|entity| entity.role == EntityRole::Subject).any(|entity| {
        let terms = label_overlap_terms(&entity.label, 2);
        !terms.is_empty() && terms.total_term_count() <= 3
    })
}

/// Soft signal: one document clearly dominates by evidence count and
/// also has the best score. Only fires when there is no explicit IR
/// pin (hint / subject), so it does not override an explicit decision.
pub(super) fn winner_from_evidence_dominance(
    aggregates: &[DocumentAggregate],
) -> Option<&DocumentAggregate> {
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

pub(super) fn winner_from_score_dominance(
    aggregates: &[DocumentAggregate],
) -> Option<&DocumentAggregate> {
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
