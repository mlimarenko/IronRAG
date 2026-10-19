use super::{
    BTreeMap, BTreeSet, DocumentAggregate, FocusReason, QueryAct, QueryIR, QueryTargetKind,
    RetrievalBundle, RuntimeChunkScoreKind, RuntimeMatchedChunk, Uuid,
    focus::{
        aggregate_by_document, evidence_dominance_budget, explicit_focus_budget,
        score_dominance_budget, winner_from_evidence_dominance, winner_from_hint,
        winner_from_query_focus_evidence, winner_from_score_dominance, winner_from_subject,
    },
    is_source_profile_chunk_row, query_ir_allows_document_focus_scope, source_anchor_window,
};
use std::collections::HashMap;

fn winner_from_versioned_procedure_runbook(
    query_ir: &QueryIR,
    question: &str,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &[DocumentAggregate],
) -> Option<DocumentAggregate> {
    let aggregates_by_document =
        aggregates.iter().map(|agg| (agg.document_id, agg)).collect::<HashMap<_, _>>();
    let mut scored_by_document = HashMap::<Uuid, (usize, f32, Vec<i32>)>::new();
    for chunk in chunks {
        let Some(runbook_score) = super::retrieve::versioned_update_procedure_runbook_anchor_score(
            question, query_ir, chunk,
        ) else {
            continue;
        };
        let retrieval_score = chunk.score.unwrap_or(0.0);
        scored_by_document
            .entry(chunk.document_id)
            .and_modify(|(best_runbook_score, best_retrieval_score, anchors)| {
                *best_runbook_score = (*best_runbook_score).max(runbook_score);
                *best_retrieval_score = best_retrieval_score.max(retrieval_score);
                if !anchors.contains(&chunk.chunk_index) {
                    anchors.push(chunk.chunk_index);
                }
            })
            .or_insert_with(|| (runbook_score, retrieval_score, vec![chunk.chunk_index]));
    }
    let mut scored = scored_by_document
        .into_iter()
        .filter_map(|(document_id, (runbook_score, retrieval_score, mut anchors))| {
            let aggregate = aggregates_by_document.get(&document_id)?;
            anchors.sort_unstable();
            Some((runbook_score, retrieval_score, document_id, anchors, (*aggregate).clone()))
        })
        .collect::<Vec<_>>();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| right.4.evidence_count.cmp(&left.4.evidence_count))
            .then_with(|| left.2.cmp(&right.2))
    });
    let (_, _, _, runbook_anchors, mut winner) = scored.into_iter().next()?;
    let mut ranked = runbook_anchors;
    for anchor in &winner.ranked_content_anchors {
        if !ranked.contains(anchor) {
            ranked.push(*anchor);
        }
    }
    winner.ranked_content_anchors = ranked;
    Some(winner)
}

fn winner_anchor_expansion_forward(winner: &DocumentAggregate, budget: usize) -> i32 {
    let materialized_anchor_count = winner.ranked_content_anchors.len().min(budget);
    budget.saturating_sub(materialized_anchor_count).max(1) as i32
}

pub(super) fn winner_anchor_windows(winner: &DocumentAggregate, budget: usize) -> Vec<(i32, i32)> {
    let expansion_forward = winner_anchor_expansion_forward(winner, budget);
    let mut windows = winner
        .ranked_content_anchors
        .iter()
        .take(budget)
        .map(|anchor| source_anchor_window(*anchor, 1, expansion_forward.saturating_add(1)))
        .collect::<Vec<_>>();
    windows.sort_unstable();

    let mut merged = Vec::<(i32, i32)>::new();
    for (min_index, max_index) in windows {
        match merged.last_mut() {
            Some((_, last_max)) if min_index <= last_max.saturating_add(1) => {
                *last_max = (*last_max).max(max_index);
            }
            _ => merged.push((min_index, max_index)),
        }
    }
    merged
}

fn push_winner_row_by_index(
    selected: &mut Vec<crate::infra::knowledge_rows::KnowledgeChunkRow>,
    selected_indices: &mut BTreeSet<i32>,
    rows_by_index: &BTreeMap<i32, crate::infra::knowledge_rows::KnowledgeChunkRow>,
    chunk_index: i32,
    budget: usize,
) {
    if selected.len() >= budget || selected_indices.contains(&chunk_index) {
        return;
    }
    let Some(row) = rows_by_index.get(&chunk_index) else {
        return;
    };
    selected_indices.insert(chunk_index);
    selected.push(row.clone());
}

pub(super) fn select_winner_rows(
    winner: &DocumentAggregate,
    budget: usize,
    fetched_rows: Vec<crate::infra::knowledge_rows::KnowledgeChunkRow>,
) -> Vec<crate::infra::knowledge_rows::KnowledgeChunkRow> {
    if budget == 0 || winner.ranked_content_anchors.is_empty() {
        return Vec::new();
    }

    let mut rows_by_index = BTreeMap::<i32, crate::infra::knowledge_rows::KnowledgeChunkRow>::new();
    for row in fetched_rows {
        if row.revision_id != winner.revision_id || is_source_profile_chunk_row(&row) {
            continue;
        }
        rows_by_index.entry(row.chunk_index).or_insert(row);
    }
    if rows_by_index.is_empty() {
        return Vec::new();
    }

    let ranked_anchors =
        winner.ranked_content_anchors.iter().copied().take(budget).collect::<Vec<_>>();
    let mut selected = Vec::with_capacity(budget.min(rows_by_index.len()));
    let mut selected_indices = BTreeSet::<i32>::new();
    for anchor in &ranked_anchors {
        push_winner_row_by_index(
            &mut selected,
            &mut selected_indices,
            &rows_by_index,
            *anchor,
            budget,
        );
    }

    for anchor in &ranked_anchors {
        push_winner_row_by_index(
            &mut selected,
            &mut selected_indices,
            &rows_by_index,
            anchor.saturating_sub(1),
            budget,
        );
    }

    let expansion_forward = winner_anchor_expansion_forward(winner, budget);
    for step in 1..=expansion_forward {
        for anchor in &ranked_anchors {
            push_winner_row_by_index(
                &mut selected,
                &mut selected_indices,
                &rows_by_index,
                anchor.saturating_add(step),
                budget,
            );
        }
    }

    if selected.len() < budget {
        for chunk_index in rows_by_index.keys().copied().collect::<Vec<_>>() {
            push_winner_row_by_index(
                &mut selected,
                &mut selected_indices,
                &rows_by_index,
                chunk_index,
                budget,
            );
        }
    }

    selected
}

/// Pure decision stage — picks a winner document (or not) given the
/// current reranked bundle and the compiled IR. Split from the
/// orchestrator so unit tests can cover the decision without building an
/// `AppState`.
///
/// Returns `Some((winner, reason, budget))` iff consolidation should
/// proceed; otherwise `None`. `budget` is the number of top_k slots
/// allocated to the winner document (rounded per-reason).
pub(crate) fn decide_focus(
    bundle: &RetrievalBundle,
    query_ir: &QueryIR,
    question: &str,
    top_k: usize,
) -> Option<(DocumentAggregate, FocusReason, usize)> {
    if top_k < 2 || bundle.chunks.is_empty() {
        return None;
    }
    if !query_ir_allows_document_focus_scope(query_ir) {
        return None;
    }
    let aggregates = aggregate_by_document(&bundle.chunks, question);
    log_decide_focus_inputs(&aggregates, top_k);

    // Priority order: explicit hint → structural runbook evidence →
    // single-doc subject → only retrieved document → evidence
    // dominance. The first match wins; later branches do not run.
    if let Some(winner) = winner_from_explicit_hint(query_ir, &bundle.chunks, &aggregates) {
        return Some((winner, FocusReason::DocumentFocusHint, explicit_focus_budget(top_k)));
    }
    if query_has_multi_document_setup_anchors(query_ir, &bundle.chunks) {
        return None;
    }
    if let Some(winner) =
        winner_from_versioned_procedure_runbook(query_ir, question, &bundle.chunks, &aggregates)
    {
        return Some((
            winner,
            FocusReason::VersionedProcedureRunbook,
            explicit_focus_budget(top_k),
        ));
    }
    if versioned_runbook_candidates_are_ambiguous(query_ir, question, &bundle.chunks, &aggregates) {
        return None;
    }
    winner_from_focus_cascade(query_ir, &bundle.chunks, &aggregates, top_k)
}

fn log_decide_focus_inputs(aggregates: &[DocumentAggregate], top_k: usize) {
    for (i, a) in aggregates.iter().take(3).enumerate() {
        tracing::info!(
            stage = "consolidation.decide_focus_inputs",
            rank = i,
            document_id = %a.document_id,
            best_score = a.best_score,
            evidence_count = a.evidence_count,
            aggregate_count = aggregates.len(),
            top_k,
            "decide_focus aggregate snapshot"
        );
    }
}

fn winner_from_explicit_hint(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &[DocumentAggregate],
) -> Option<DocumentAggregate> {
    let hint = query_ir.document_focus.as_ref()?;
    winner_from_hint(&hint.hint, chunks, aggregates).cloned()
}

/// Guards that prove the versioned-runbook retrieval set spans several
/// documents, so no single runbook document may be focused.
fn versioned_runbook_candidates_are_ambiguous(
    query_ir: &QueryIR,
    question: &str,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &[DocumentAggregate],
) -> bool {
    query_has_multi_document_versioned_procedure_anchors(query_ir, question, chunks)
        || super::retrieve::query_ir_requests_versioned_update_procedure_context(question, query_ir)
        || broad_unfocused_procedure_has_multiple_documents(query_ir, aggregates)
}

fn winner_from_focus_cascade(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    aggregates: &[DocumentAggregate],
    top_k: usize,
) -> Option<(DocumentAggregate, FocusReason, usize)> {
    if let Some(winner) = winner_from_subject(query_ir, chunks, aggregates) {
        return Some((
            winner.clone(),
            FocusReason::SingleDocumentSubject,
            explicit_focus_budget(top_k),
        ));
    }
    if let Some(winner) = winner_from_query_focus_evidence(query_ir, chunks, aggregates) {
        return Some((
            winner.clone(),
            FocusReason::QueryFocusEvidence,
            explicit_focus_budget(top_k),
        ));
    }
    if aggregates.len() == 1 {
        return aggregates.first().cloned().map(|winner| {
            (winner, FocusReason::OnlyRetrievedDocument, explicit_focus_budget(top_k))
        });
    }
    if let Some(winner) = winner_from_score_dominance(aggregates) {
        return Some((winner.clone(), FocusReason::ScoreDominance, score_dominance_budget(top_k)));
    }
    if let Some(winner) = winner_from_evidence_dominance(aggregates) {
        return Some((
            winner.clone(),
            FocusReason::EvidenceDominance,
            evidence_dominance_budget(top_k),
        ));
    }
    None
}

fn broad_unfocused_procedure_has_multiple_documents(
    query_ir: &QueryIR,
    aggregates: &[DocumentAggregate],
) -> bool {
    query_ir.requests_broad_procedure_variant_coverage() && aggregates.len() >= 2
}

pub(crate) fn query_has_multi_document_setup_anchors(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        || query_ir.document_focus.is_some()
        || query_ir.source_slice.is_some()
        || !query_ir.literal_constraints.is_empty()
        || super::retrieve::query_ir_requests_versioned_update_procedure_context("", query_ir)
        || query_ir.targets(QueryTargetKind::Document)
    {
        return false;
    }
    let anchored_documents = chunks
        .iter()
        .filter(|chunk| super::retrieve::chunk_is_setup_focus_command_path_anchor(chunk))
        .map(|chunk| chunk.document_id)
        .collect::<BTreeSet<_>>();
    anchored_documents.len() >= 2
}

fn query_has_multi_document_versioned_procedure_anchors(
    query_ir: &QueryIR,
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    if !super::retrieve::query_ir_requests_versioned_update_procedure_context(question, query_ir) {
        return false;
    }
    chunks
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument)
        .map(|chunk| chunk.document_id)
        .collect::<BTreeSet<_>>()
        .len()
        >= 2
}
