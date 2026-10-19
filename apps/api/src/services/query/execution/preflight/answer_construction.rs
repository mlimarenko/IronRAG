#[cfg(test)]
use super::document_scope::{
    build_preflight_answer_chunks_for_scope, preflight_allows_empty_scope_fallback,
    preflight_exact_literal_document_scope,
};
use super::document_scope::{
    current_question_has_exact_technical_surface,
    query_ir_requests_broad_procedure_variant_coverage,
    question_prefers_single_exact_literal_scope, select_preflight_literal_document_id,
    select_preflight_literal_document_id_from_preferred,
};
use super::setup_literals::{
    append_setup_literal_chunks, select_setup_literal_document_id, setup_literal_chunk_score,
};
use super::{
    AnswerKind, AuthoritativeAnswerEvidence, DeterministicAnswerOverride,
    DocumentBalancedChunkSelection, HashMap, HashSet, KnowledgeDocumentRow, QueryAct, QueryIR,
    QueryIntentProfile, QueryScope, QueryTargetKind, QuestionIntent, RuntimeChunkScoreKind,
    RuntimeMatchedChunk, TechnicalLiteralIntent, Uuid, build_deterministic_grounded_answer,
    build_setup_configuration_anchor_candidate, classify_query_ir_intents,
    current_question_segment, has_question_intent, label_terms, select_document_balanced_chunks,
    structured_current_question_segment, technical_literal_candidate_limit,
};
use crate::services::query::execution::source_context::SOURCE_UNIT_CHUNK_KIND;

pub(super) fn broad_procedure_preflight_requires_synthesis(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    if !query_ir.requests_broad_procedure_variant_coverage() {
        return false;
    }
    build_setup_configuration_anchor_candidate(question, query_ir, chunks)
        .is_some_and(|candidate| candidate.is_multi_variant())
}

pub(super) fn build_setup_configuration_anchor_answer_override(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return None;
    }
    let answer = build_setup_configuration_anchor_candidate(question, query_ir, chunks)?;
    if answer.should_use_as_preflight_answer(query_ir, chunks) {
        Some(answer.into_answer())
    } else {
        None
    }
}

pub(in super::super) fn build_primary_preflight_answer_override(
    missing_explicit_document_override: Option<DeterministicAnswerOverride>,
    direct_targeted_table_answer: Option<&str>,
) -> Option<DeterministicAnswerOverride> {
    missing_explicit_document_override.or_else(|| {
        direct_targeted_table_answer.map(|answer| DeterministicAnswerOverride {
            answer: answer.to_string(),
            answer_kind: AnswerKind::TargetedTableAnswer,
        })
    })
}

pub(super) fn prepend_preflight_source_title_inventory(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    prompt_context: String,
) -> String {
    let mut seen = HashSet::<String>::new();
    let mut titles = Vec::<String>::new();
    for chunk in chunks {
        push_preflight_source_title(&mut titles, &mut seen, chunk.document_label.trim());
    }
    for block in &evidence.structured_blocks {
        if let Some(document) = document_index.get(&block.document_id) {
            push_preflight_source_title(
                &mut titles,
                &mut seen,
                preflight_document_title(document).as_deref().unwrap_or_default(),
            );
        }
    }
    for fact in &evidence.technical_facts {
        if let Some(document) = document_index.get(&fact.document_id) {
            push_preflight_source_title(
                &mut titles,
                &mut seen,
                preflight_document_title(document).as_deref().unwrap_or_default(),
            );
        }
    }
    if titles.len() < 2 {
        return prompt_context;
    }
    titles.truncate(24);
    let inventory = format!("Source title inventory\n- {}", titles.join("\n- "));
    if prompt_context.trim().is_empty() {
        inventory
    } else {
        format!("{inventory}\n\n{prompt_context}")
    }
}

fn preflight_document_title(document: &KnowledgeDocumentRow) -> Option<String> {
    document
        .title
        .as_deref()
        .or(document.file_name.as_deref())
        .or(document.document_hint.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
}

fn push_preflight_source_title(titles: &mut Vec<String>, seen: &mut HashSet<String>, title: &str) {
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > 240
        || !title.chars().any(|ch| ch.is_alphanumeric())
    {
        return;
    }
    if seen.insert(title.to_lowercase()) {
        titles.push(title.to_string());
    }
}

pub(in super::super) fn build_evidence_preflight_answer(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    intent_profile: &QueryIntentProfile,
    direct_targeted_table_answer: Option<String>,
    authoritative_evidence: &AuthoritativeAnswerEvidence,
    primary_answer_chunks: &[RuntimeMatchedChunk],
) -> Option<DeterministicAnswerOverride> {
    let requires_synthesis = preflight_requires_synthesis(question, query_ir, intent_profile);
    let deterministic_grounded_answer = if requires_synthesis {
        None
    } else {
        build_deterministic_grounded_answer(
            question,
            query_ir,
            authoritative_evidence,
            primary_answer_chunks,
        )
    };

    if intent_profile.exact_literal_technical {
        // Telemetry stays content-free: emit shapes and counts only, never the
        // verbatim question text or document/chunk previews. Diagnostics export
        // (traces especially, which are on by default) must not carry user
        // content; the persisted query turn in Postgres holds it for operator
        // debugging.
        tracing::info!(
            question_len = question.chars().count(),
            chunk_count = primary_answer_chunks.len(),
            chunk_document_count = primary_answer_chunks
                .iter()
                .map(|chunk| chunk.document_id)
                .collect::<HashSet<_>>()
                .len(),
            technical_fact_count = authoritative_evidence.technical_facts.len(),
            structured_block_count = authoritative_evidence.structured_blocks.len(),
            has_direct_targeted_table_answer = direct_targeted_table_answer.is_some(),
            has_deterministic_grounded_answer = deterministic_grounded_answer.is_some(),
            requires_synthesis,
            "exact technical preflight decision"
        );
    }

    if let Some(answer) = direct_targeted_table_answer {
        return Some(DeterministicAnswerOverride {
            answer,
            answer_kind: AnswerKind::TargetedTableAnswer,
        });
    }
    deterministic_grounded_answer.map(|answer| DeterministicAnswerOverride {
        answer,
        answer_kind: AnswerKind::DeterministicGroundedAnswer,
    })
}

pub(super) fn preflight_requires_synthesis(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
) -> bool {
    scoped_setup_literal_inventory_requires_synthesis(question, query_ir)
        || open_enumeration_preflight_requires_synthesis(question, query_ir, intent_profile)
        || multi_document_comparison_requires_synthesis(query_ir)
}

/// A cross-document comparison is a synthesis task by definition: collated
/// per-document excerpts are evidence, not an answer, so no deterministic
/// builder may finalize it.
fn multi_document_comparison_requires_synthesis(query_ir: &QueryIR) -> bool {
    matches!(query_ir.act, QueryAct::Compare) && matches!(query_ir.scope, QueryScope::MultiDocument)
}

fn open_enumeration_preflight_requires_synthesis(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
) -> bool {
    matches!(query_ir.act, QueryAct::Enumerate)
        && !intent_profile.exact_literal_technical
        && !query_ir_requests_setup_literal_context(query_ir)
        && !current_question_has_exact_technical_surface(question)
}

fn scoped_setup_literal_inventory_requires_synthesis(question: &str, query_ir: &QueryIR) -> bool {
    query_ir_requests_setup_literal_context(query_ir)
        && !current_question_has_exact_technical_surface(question)
        && (structured_current_question_segment(question).is_some()
            || !query_ir.conversation_refs.is_empty())
}

pub(in super::super) fn build_preflight_graph_evidence_context_lines(
    graph_evidence_context_lines: &[String],
) -> Vec<String> {
    graph_evidence_context_lines.to_vec()
}

#[cfg(test)]
pub(in super::super) fn build_preflight_answer_chunks(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    primary_answer_chunks: &[RuntimeMatchedChunk],
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        query_ir,
        intent_profile,
        technical_literal_chunks,
    );
    build_preflight_answer_chunks_for_scope(
        primary_answer_chunks,
        technical_literal_chunks,
        scoped_document_ids.as_ref(),
        preflight_allows_empty_scope_fallback(question, query_ir),
    )
}

/// Inputs of the technical-literal chunk selection.
pub(in super::super) struct TechnicalLiteralSelection<'a> {
    pub(in super::super) question: &'a str,
    pub(in super::super) query_ir: &'a QueryIR,
    pub(in super::super) chunks: &'a [RuntimeMatchedChunk],
    pub(in super::super) technical_literal_intent: TechnicalLiteralIntent,
    pub(in super::super) top_k: usize,
    pub(in super::super) literal_focus_keywords: &'a [String],
    pub(in super::super) preferred_document_ids: &'a [Uuid],
    pub(in super::super) pagination_requested: bool,
}

pub(in super::super) fn select_technical_literal_chunks(
    input: TechnicalLiteralSelection<'_>,
) -> Vec<RuntimeMatchedChunk> {
    let TechnicalLiteralSelection {
        question,
        query_ir,
        chunks,
        technical_literal_intent,
        top_k,
        literal_focus_keywords,
        preferred_document_ids,
        pagination_requested,
    } = input;
    let setup_literal_context = query_ir_requests_setup_literal_context(query_ir);
    let max_total_chunks = if setup_literal_context {
        top_k.saturating_mul(4).clamp(24, 64)
    } else if technical_literal_intent.any() {
        technical_literal_candidate_limit(technical_literal_intent, top_k)
    } else {
        12
    };
    let max_chunks_per_document = if setup_literal_context {
        24
    } else if technical_literal_intent.any() {
        4
    } else {
        3
    };
    let focused_chunks = if technical_literal_intent.any()
        && question_prefers_single_exact_literal_scope(question, query_ir)
        && !query_ir_requests_broad_procedure_variant_coverage(query_ir, chunks)
    {
        let focused_document_id = if setup_literal_context {
            select_setup_literal_document_id(question, query_ir, chunks)
                .or_else(|| select_preflight_literal_document_id(question, query_ir, chunks))
                .or_else(|| {
                    select_preflight_literal_document_id_from_preferred(
                        question,
                        query_ir,
                        chunks,
                        preferred_document_ids,
                    )
                })
        } else {
            select_preflight_literal_document_id_from_preferred(
                question,
                query_ir,
                chunks,
                preferred_document_ids,
            )
            .or_else(|| select_preflight_literal_document_id(question, query_ir, chunks))
        };
        focused_document_id.map(|document_id| {
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
    let mut selected = select_document_balanced_chunks(DocumentBalancedChunkSelection {
        question,
        ir: Some(query_ir),
        chunks: candidate_chunks,
        keywords: literal_focus_keywords,
        pagination_requested,
        max_total_chunks,
        max_chunks_per_document,
    })
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    if setup_literal_context {
        append_setup_literal_chunks(&mut selected, candidate_chunks, max_total_chunks);
    }
    selected
}

pub(in super::super) fn query_ir_requests_setup_literal_context(query_ir: &QueryIR) -> bool {
    if !matches!(
        query_ir.act,
        QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::RetrieveValue
    ) {
        return false;
    }
    let has_focus_signal = query_ir.document_focus.is_some()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty();
    let requests_configuration =
        query_ir.targets_any(&[QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey]);
    let requests_module_or_parameter =
        query_ir.targets_any(&[QueryTargetKind::Package, QueryTargetKind::Parameter]);
    if requests_configuration && requests_module_or_parameter {
        return true;
    }
    if requests_configuration && has_focus_signal {
        return true;
    }
    matches!(query_ir.act, QueryAct::ConfigureHow)
        && (requests_configuration || requests_module_or_parameter)
        && has_focus_signal
}

pub(in super::super) fn query_ir_requests_low_confidence_setup_preflight(
    query_ir: &QueryIR,
    context_chunks: &[RuntimeMatchedChunk],
) -> bool {
    (query_ir_low_confidence_unfocused_descriptive_setup(query_ir)
        || query_ir_low_confidence_structural_descriptive_setup(query_ir))
        && context_chunks.iter().any(low_confidence_context_chunk_requests_setup_bridge)
}

pub(super) fn query_ir_requests_structured_inventory_preflight(
    question: &str,
    query_ir: &QueryIR,
    context_chunks: &[RuntimeMatchedChunk],
) -> bool {
    if query_ir.source_slice.is_some()
        || !matches!(
            query_ir.act,
            QueryAct::Compare | QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
        || !context_chunks.iter().any(|chunk| {
            chunk.score_kind == RuntimeChunkScoreKind::SourceContext
                || chunk.chunk_kind.as_deref() == Some(SOURCE_UNIT_CHUNK_KIND)
        })
    {
        return false;
    }
    let intents = classify_query_ir_intents(query_ir);
    if has_question_intent(&intents, QuestionIntent::Port)
        || has_question_intent(&intents, QuestionIntent::Protocol)
        || has_question_intent(&intents, QuestionIntent::ConfigKey)
        || has_question_intent(&intents, QuestionIntent::Parameter)
        || has_question_intent(&intents, QuestionIntent::ErrorCode)
        || !query_ir.target_types.is_empty()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
    {
        return true;
    }

    let focus_terms = structured_inventory_preflight_focus_terms(question, query_ir);
    !focus_terms.is_empty()
        && context_chunks
            .iter()
            .filter(|chunk| {
                chunk.score_kind == RuntimeChunkScoreKind::SourceContext
                    || chunk.chunk_kind.as_deref() == Some(SOURCE_UNIT_CHUNK_KIND)
            })
            .any(|chunk| structured_inventory_preflight_overlap_score(chunk, &focus_terms) >= 2)
}

fn structured_inventory_preflight_focus_terms(
    question: &str,
    query_ir: &QueryIR,
) -> HashSet<String> {
    let mut terms =
        label_terms(current_question_segment(question), 3).into_iter().collect::<HashSet<_>>();
    for entity in &query_ir.target_entities {
        terms.extend(label_terms(&entity.label, 2));
    }
    for literal in &query_ir.literal_constraints {
        terms.extend(label_terms(&literal.text, 2));
    }
    terms
}

fn structured_inventory_preflight_overlap_score(
    chunk: &RuntimeMatchedChunk,
    focus_terms: &HashSet<String>,
) -> usize {
    label_terms(&format!("{}\n{}\n{}", chunk.document_label, chunk.excerpt, chunk.source_text), 2)
        .into_iter()
        .filter(|term| focus_terms.contains(term))
        .count()
}

pub(super) fn query_ir_low_confidence_unfocused_descriptive_setup(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.3
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
}

fn query_ir_low_confidence_structural_descriptive_setup(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.35
        && matches!(query_ir.scope, QueryScope::SingleDocument | QueryScope::MultiDocument)
        && matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::ConfigureHow | QueryAct::RetrieveValue
        )
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.comparison.is_none()
        && query_ir.temporal_constraints.is_empty()
        && query_ir.conversation_refs.is_empty()
        && (!query_ir.target_entities.is_empty() || !query_ir.literal_constraints.is_empty())
}

fn low_confidence_context_chunk_requests_setup_bridge(chunk: &RuntimeMatchedChunk) -> bool {
    let text = format!("{}\n{}", chunk.excerpt, chunk.source_text);
    let setup_score = setup_literal_chunk_score(&text);
    setup_score.anchor_score > 0
        || (setup_score.total_score >= 3
            && chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
}
