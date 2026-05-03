use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{
    app::state::AppState, domains::query_ir::QueryIR,
    infra::arangodb::document_store::KnowledgeDocumentRow,
    services::query::planner::QueryIntentProfile,
};

use super::{
    CanonicalAnswerEvidence, PreparedAnswerQueryResult, RuntimeMatchedChunk,
    build_canonical_answer_context, build_deterministic_grounded_answer,
    build_missing_explicit_document_answer, load_canonical_answer_chunks,
    load_canonical_answer_evidence, load_direct_targeted_table_answer, load_document_index,
    question_requests_multi_document_scope,
    retrieve::{merge_chunks, score_value},
    technical_literals::{
        TechnicalLiteralIntent, document_local_focus_keywords, question_mentions_pagination,
        select_document_balanced_chunks, technical_chunk_selection_score, technical_keyword_weight,
        technical_literal_candidate_limit, technical_literal_focus_keywords,
    },
};

#[derive(Debug, Clone)]
pub(super) struct CanonicalAnswerPreflight {
    pub(super) canonical_answer_chunks: Vec<RuntimeMatchedChunk>,
    pub(super) canonical_evidence: CanonicalAnswerEvidence,
    pub(super) prompt_context: String,
    pub(super) answer_override: Option<String>,
}

pub(super) async fn prepare_canonical_answer_preflight(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    question: &str,
    prepared: &PreparedAnswerQueryResult,
) -> anyhow::Result<CanonicalAnswerPreflight> {
    let document_index = load_document_index(state, library_id).await?;
    let direct_targeted_table_answer = load_direct_targeted_table_answer(
        state,
        question,
        Some(&prepared.query_ir),
        &document_index,
    )
    .await?;
    let canonical_answer_chunks = load_canonical_answer_chunks(
        state,
        execution_id,
        question,
        &prepared.query_ir,
        &prepared.structured.context_chunks,
        &document_index,
    )
    .await?;
    let canonical_evidence = load_canonical_answer_evidence(state, execution_id).await?;
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        &prepared.query_ir,
        &prepared.structured.intent_profile,
        &prepared.structured.technical_literal_chunks,
    );
    let preflight_answer_chunks = build_preflight_answer_chunks_for_scope(
        &canonical_answer_chunks,
        &prepared.structured.technical_literal_chunks,
        scoped_document_ids.as_ref(),
    );
    let preflight_evidence = build_preflight_canonical_evidence_for_scope(
        &canonical_evidence,
        scoped_document_ids.as_ref(),
    );
    let graph_evidence_context_lines = build_preflight_graph_evidence_context_lines(
        &prepared.structured.graph_evidence_context_lines,
    );
    let prompt_context = build_canonical_answer_context(
        question,
        &prepared.query_ir,
        prepared.structured.technical_literals_text.as_deref(),
        &preflight_evidence,
        &preflight_answer_chunks,
        &graph_evidence_context_lines,
    );
    let answer_override = build_canonical_preflight_answer(
        question,
        &prepared.query_ir,
        &prepared.structured.intent_profile,
        &document_index,
        direct_targeted_table_answer,
        &preflight_evidence,
        &preflight_answer_chunks,
    );
    Ok(CanonicalAnswerPreflight {
        canonical_answer_chunks: preflight_answer_chunks,
        canonical_evidence: preflight_evidence,
        prompt_context,
        answer_override,
    })
}

pub(super) fn build_canonical_preflight_answer(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    intent_profile: &QueryIntentProfile,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    direct_targeted_table_answer: Option<String>,
    canonical_evidence: &CanonicalAnswerEvidence,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let missing_explicit_document_answer =
        build_missing_explicit_document_answer(question, document_index);
    let deterministic_grounded_answer = build_deterministic_grounded_answer(
        question,
        query_ir,
        canonical_evidence,
        canonical_answer_chunks,
    );

    if intent_profile.exact_literal_technical {
        let top_documents = canonical_answer_chunks
            .iter()
            .map(|chunk| chunk.document_label.as_str())
            .collect::<Vec<_>>();
        let top_chunk_previews = canonical_answer_chunks
            .iter()
            .take(3)
            .map(|chunk| {
                let text = if chunk.excerpt.trim().is_empty() {
                    chunk.source_text.trim()
                } else {
                    chunk.excerpt.trim()
                };
                text.chars().take(120).collect::<String>()
            })
            .collect::<Vec<_>>();
        tracing::info!(
            question = question,
            chunk_count = canonical_answer_chunks.len(),
            chunk_document_count = canonical_answer_chunks
                .iter()
                .map(|chunk| chunk.document_id)
                .collect::<HashSet<_>>()
                .len(),
            technical_fact_count = canonical_evidence.technical_facts.len(),
            structured_block_count = canonical_evidence.structured_blocks.len(),
            has_missing_explicit_document_answer = missing_explicit_document_answer.is_some(),
            has_direct_targeted_table_answer = direct_targeted_table_answer.is_some(),
            has_deterministic_grounded_answer = deterministic_grounded_answer.is_some(),
            top_documents = ?top_documents,
            top_chunk_previews = ?top_chunk_previews,
            "exact technical preflight decision"
        );
    }

    missing_explicit_document_answer
        .or(direct_targeted_table_answer)
        .or(deterministic_grounded_answer)
}

pub(super) fn build_preflight_graph_evidence_context_lines(
    graph_evidence_context_lines: &[String],
) -> Vec<String> {
    graph_evidence_context_lines.to_vec()
}

#[cfg(test)]
pub(super) fn build_preflight_answer_chunks(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        query_ir,
        intent_profile,
        technical_literal_chunks,
    );
    build_preflight_answer_chunks_for_scope(
        canonical_answer_chunks,
        technical_literal_chunks,
        scoped_document_ids.as_ref(),
    )
}

pub(super) fn select_technical_literal_chunks(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    technical_literal_intent: TechnicalLiteralIntent,
    top_k: usize,
    literal_focus_keywords: &[String],
    pagination_requested: bool,
) -> Vec<RuntimeMatchedChunk> {
    let max_total_chunks = if technical_literal_intent.any() {
        technical_literal_candidate_limit(technical_literal_intent, top_k)
    } else {
        12
    };
    let max_chunks_per_document = if technical_literal_intent.any() { 4 } else { 3 };
    let focused_chunks = if technical_literal_intent.any()
        && question_prefers_single_exact_literal_scope(question, query_ir)
    {
        select_preflight_literal_document_id(question, query_ir, chunks).map(|document_id| {
            chunks
                .iter()
                .filter(|chunk| chunk.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        })
    } else {
        None
    };
    let candidate_chunks = focused_chunks.as_deref().unwrap_or(chunks);
    select_document_balanced_chunks(
        question,
        Some(query_ir),
        candidate_chunks,
        literal_focus_keywords,
        pagination_requested,
        max_total_chunks,
        max_chunks_per_document,
    )
    .into_iter()
    .cloned()
    .collect()
}

#[cfg(test)]
pub(super) fn build_preflight_canonical_evidence(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    canonical_evidence: &CanonicalAnswerEvidence,
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> CanonicalAnswerEvidence {
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        query_ir,
        intent_profile,
        technical_literal_chunks,
    );
    build_preflight_canonical_evidence_for_scope(canonical_evidence, scoped_document_ids.as_ref())
}

pub(super) fn preflight_exact_literal_document_scope(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> Option<HashSet<Uuid>> {
    if !intent_profile.exact_literal_technical || technical_literal_chunks.is_empty() {
        return None;
    }

    if !question_prefers_single_exact_literal_scope(question, query_ir) {
        return Some(
            technical_literal_chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>(),
        );
    }

    select_preflight_literal_document_id(question, query_ir, technical_literal_chunks)
        .map(|document_id| HashSet::from([document_id]))
        .or_else(|| {
            Some(
                technical_literal_chunks
                    .iter()
                    .map(|chunk| chunk.document_id)
                    .collect::<HashSet<_>>(),
            )
        })
}

pub(super) fn question_prefers_single_exact_literal_scope(
    question: &str,
    query_ir: &QueryIR,
) -> bool {
    let _ = question;
    if question_requests_multi_document_scope(question, Some(query_ir)) {
        return false;
    }
    !matches!(query_ir.act, crate::domains::query_ir::QueryAct::Enumerate)
}

pub(super) fn select_preflight_literal_document_id(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if chunks.is_empty() {
        return None;
    }

    #[derive(Debug)]
    struct ExactLiteralDocumentCandidate<'a> {
        document_id: Uuid,
        document_label: &'a str,
        label_score: usize,
        best_chunk_signal: isize,
        chunk_signal_sum: isize,
        retrieval_score_sum: f32,
        first_rank: usize,
    }

    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let pagination_requested = question_mentions_pagination(question);
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    let mut candidates = ordered_document_ids
        .iter()
        .enumerate()
        .filter_map(|(first_rank, document_id)| {
            let document_chunks = per_document_chunks.get(document_id)?;
            let local_keywords = document_local_focus_keywords(
                question,
                Some(query_ir),
                document_chunks,
                &question_keywords,
            );
            let document_label = document_chunks.first()?.document_label.as_str();
            let lowered_label = document_label.to_lowercase();
            let label_score = question_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&lowered_label, keyword))
                .sum::<usize>();
            let (best_chunk_signal, chunk_signal_sum, retrieval_score_sum) =
                document_chunks.iter().fold(
                    (isize::MIN, 0isize, 0.0f32),
                    |(best_chunk_signal, chunk_signal_sum, retrieval_score_sum), chunk| {
                        let chunk_signal = technical_chunk_selection_score(
                            &format!("{} {}", chunk.excerpt, chunk.source_text),
                            &local_keywords,
                            pagination_requested,
                        );
                        (
                            best_chunk_signal.max(chunk_signal),
                            chunk_signal_sum + chunk_signal,
                            retrieval_score_sum + score_value(chunk.score),
                        )
                    },
                );
            Some(ExactLiteralDocumentCandidate {
                document_id: *document_id,
                document_label,
                label_score,
                best_chunk_signal,
                chunk_signal_sum,
                retrieval_score_sum,
                first_rank,
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by(|left, right| {
        right
            .best_chunk_signal
            .cmp(&left.best_chunk_signal)
            .then_with(|| right.chunk_signal_sum.cmp(&left.chunk_signal_sum))
            .then_with(|| right.label_score.cmp(&left.label_score))
            .then_with(|| right.retrieval_score_sum.total_cmp(&left.retrieval_score_sum))
            .then_with(|| left.first_rank.cmp(&right.first_rank))
            .then_with(|| left.document_label.cmp(right.document_label))
    });

    Some(candidates[0].document_id)
}

fn filter_runtime_chunks_to_documents(
    chunks: &[RuntimeMatchedChunk],
    document_ids: &HashSet<Uuid>,
) -> Vec<RuntimeMatchedChunk> {
    chunks.iter().filter(|chunk| document_ids.contains(&chunk.document_id)).cloned().collect()
}

fn build_preflight_answer_chunks_for_scope(
    canonical_answer_chunks: &[RuntimeMatchedChunk],
    technical_literal_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
) -> Vec<RuntimeMatchedChunk> {
    let merged = if technical_literal_chunks.is_empty() {
        canonical_answer_chunks.to_vec()
    } else if canonical_answer_chunks.is_empty() {
        technical_literal_chunks.to_vec()
    } else {
        merge_chunks(
            technical_literal_chunks.to_vec(),
            canonical_answer_chunks.to_vec(),
            canonical_answer_chunks.len().max(technical_literal_chunks.len()).max(12),
        )
    };

    match scoped_document_ids {
        Some(document_ids) => filter_runtime_chunks_to_documents(&merged, document_ids),
        None => merged,
    }
}

fn build_preflight_canonical_evidence_for_scope(
    canonical_evidence: &CanonicalAnswerEvidence,
    scoped_document_ids: Option<&HashSet<Uuid>>,
) -> CanonicalAnswerEvidence {
    match scoped_document_ids {
        Some(document_ids) => {
            filter_canonical_evidence_to_documents(canonical_evidence, document_ids)
        }
        None => canonical_evidence.clone(),
    }
}

fn filter_canonical_evidence_to_documents(
    canonical_evidence: &CanonicalAnswerEvidence,
    document_ids: &HashSet<Uuid>,
) -> CanonicalAnswerEvidence {
    CanonicalAnswerEvidence {
        bundle: canonical_evidence.bundle.clone(),
        chunk_rows: canonical_evidence
            .chunk_rows
            .iter()
            .filter(|row| document_ids.contains(&row.document_id))
            .cloned()
            .collect(),
        structured_blocks: canonical_evidence
            .structured_blocks
            .iter()
            .filter(|block| document_ids.contains(&block.document_id))
            .cloned()
            .collect(),
        technical_facts: canonical_evidence
            .technical_facts
            .iter()
            .filter(|fact| document_ids.contains(&fact.document_id))
            .cloned()
            .collect(),
    }
}
