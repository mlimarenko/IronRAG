use std::collections::{HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{QueryAct, QueryIR, literal_text_is_identifier_shaped},
    infra::arangodb::document_store::{KnowledgeDocumentRow, KnowledgeStructuredBlockRow},
    services::query::{effective_query::current_question_segment, planner::QueryIntentProfile},
};

use super::{
    CanonicalAnswerEvidence, PreparedAnswerQueryResult, RuntimeMatchedChunk,
    build_canonical_answer_context, build_deterministic_grounded_answer,
    build_missing_explicit_document_answer, load_canonical_answer_chunks,
    load_canonical_answer_evidence, load_direct_targeted_table_answer, load_document_index,
    question_intent::{QuestionIntent, classify_query_ir_intents, has_question_intent},
    question_intent::{canonical_target_type_tag, query_ir_has_focused_document_answer_intent},
    question_requests_multi_document_scope,
    retrieve::{canonical_document_revision_id, merge_chunks, score_value},
    technical_literals::{
        TechnicalLiteralIntent, document_local_focus_keywords, extract_explicit_path_literals,
        extract_package_command_literals, extract_parameter_literals,
        select_document_balanced_chunks, technical_chunk_selection_score, technical_keyword_weight,
        technical_literal_candidate_limit, technical_literal_focus_keywords,
    },
};

const SETUP_PREFLIGHT_STRUCTURED_BLOCK_LIMIT: usize = 96;

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
    let allow_empty_scope_fallback =
        preflight_allows_empty_scope_fallback(question, &prepared.query_ir);
    let mut preflight_answer_chunks = build_preflight_answer_chunks_for_scope(
        &canonical_answer_chunks,
        &prepared.structured.technical_literal_chunks,
        scoped_document_ids.as_ref(),
        allow_empty_scope_fallback,
    );
    if query_ir_requests_setup_literal_context(&prepared.query_ir) {
        extend_setup_preflight_chunks_from_structured_context(
            &mut preflight_answer_chunks,
            &prepared.structured.context_chunks,
            scoped_document_ids.as_ref(),
        );
    }
    let mut preflight_evidence = build_preflight_canonical_evidence_for_scope(
        &canonical_evidence,
        scoped_document_ids.as_ref(),
        allow_empty_scope_fallback,
    );
    augment_setup_preflight_structured_blocks(
        state,
        question,
        &prepared.query_ir,
        &document_index,
        &preflight_answer_chunks,
        scoped_document_ids.as_ref(),
        &mut preflight_evidence,
    )
    .await?;
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
    let requires_synthesis = query_ir.is_follow_up();
    let deterministic_grounded_answer = if requires_synthesis {
        None
    } else {
        build_deterministic_grounded_answer(
            question,
            query_ir,
            canonical_evidence,
            canonical_answer_chunks,
        )
    };

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
            requires_synthesis,
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
        preflight_allows_empty_scope_fallback(question, query_ir),
    )
}

pub(super) fn select_technical_literal_chunks(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    technical_literal_intent: TechnicalLiteralIntent,
    top_k: usize,
    literal_focus_keywords: &[String],
    preferred_document_ids: &[Uuid],
    pagination_requested: bool,
) -> Vec<RuntimeMatchedChunk> {
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
    let mut selected = select_document_balanced_chunks(
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
    .collect::<Vec<_>>();
    if setup_literal_context {
        append_setup_literal_chunks(&mut selected, candidate_chunks, max_total_chunks);
    }
    selected
}

fn query_ir_requests_setup_literal_context(query_ir: &QueryIR) -> bool {
    if !matches!(
        query_ir.act,
        QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::RetrieveValue
    ) {
        return false;
    }
    let requests_configuration = query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "configuration_file" | "config_key"
        )
    });
    let requests_module_or_parameter = query_ir.target_types.iter().any(|target_type| {
        matches!(canonical_target_type_tag(target_type).as_str(), "package" | "parameter")
    });
    if requests_configuration && requests_module_or_parameter {
        return true;
    }
    matches!(query_ir.act, QueryAct::ConfigureHow)
        && (requests_configuration || requests_module_or_parameter)
        && (query_ir.document_focus.is_some()
            || !query_ir.target_entities.is_empty()
            || !query_ir.literal_constraints.is_empty()
            || !query_ir.conversation_refs.is_empty())
}

fn select_setup_literal_document_id(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if chunks.is_empty() {
        return None;
    }

    #[derive(Debug)]
    struct SetupLiteralDocumentCandidate {
        document_id: Uuid,
        label_score: usize,
        setup_anchor_score: usize,
        setup_score: usize,
        best_chunk_signal: isize,
        retrieval_score_sum: f32,
        first_rank: usize,
    }

    let label_keywords = preflight_target_label_keywords(query_ir);
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let pagination_requested = false;
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
            let document_label = document_chunks.first()?.document_label.to_lowercase();
            let label_score = label_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&document_label, keyword))
                .sum::<usize>();
            let local_keywords = document_local_focus_keywords(
                question,
                Some(query_ir),
                document_chunks,
                &question_keywords,
            );
            let mut setup_anchor_score = 0usize;
            let mut setup_score = 0usize;
            let mut best_chunk_signal = isize::MIN;
            let mut retrieval_score_sum = 0.0f32;
            for chunk in document_chunks {
                let text = format!("{} {}", chunk.excerpt, chunk.source_text);
                let chunk_setup_score = setup_literal_chunk_score(&text);
                setup_anchor_score =
                    setup_anchor_score.saturating_add(chunk_setup_score.anchor_score);
                setup_score = setup_score.saturating_add(chunk_setup_score.total_score);
                best_chunk_signal = best_chunk_signal.max(technical_chunk_selection_score(
                    &text,
                    &local_keywords,
                    pagination_requested,
                ));
                retrieval_score_sum += score_value(chunk.score);
            }
            (label_score > 0 || setup_score > 0).then_some(SetupLiteralDocumentCandidate {
                document_id: *document_id,
                label_score,
                setup_anchor_score,
                setup_score,
                best_chunk_signal,
                retrieval_score_sum,
                first_rank,
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    if candidates.iter().any(|candidate| candidate.setup_anchor_score > 0) {
        candidates.retain(|candidate| candidate.setup_anchor_score > 0);
    } else if candidates.iter().any(|candidate| candidate.setup_score > 0) {
        candidates.retain(|candidate| candidate.setup_score > 0);
    }

    candidates.sort_by(|left, right| {
        right
            .label_score
            .cmp(&left.label_score)
            .then_with(|| right.setup_anchor_score.cmp(&left.setup_anchor_score))
            .then_with(|| right.setup_score.cmp(&left.setup_score))
            .then_with(|| right.best_chunk_signal.cmp(&left.best_chunk_signal))
            .then_with(|| right.retrieval_score_sum.total_cmp(&left.retrieval_score_sum))
            .then_with(|| left.first_rank.cmp(&right.first_rank))
            .then_with(|| left.document_id.cmp(&right.document_id))
    });

    Some(candidates[0].document_id)
}

#[derive(Debug, Clone, Copy, Default)]
struct SetupLiteralChunkScore {
    anchor_score: usize,
    total_score: usize,
}

fn setup_literal_chunk_score(text: &str) -> SetupLiteralChunkScore {
    let package_score = extract_package_command_literals(text, 4).len().saturating_mul(16);
    let path_score = setup_literal_configuration_path_count(text).saturating_mul(24);
    let assignment_score = setup_literal_assignment_count(text).saturating_mul(10);
    let section_score = setup_literal_section_count(text).saturating_mul(8);
    let parameter_score = extract_parameter_literals(text, 32).len().saturating_mul(3);
    let anchor_score = package_score
        .saturating_add(path_score)
        .saturating_add(assignment_score)
        .saturating_add(section_score);
    SetupLiteralChunkScore {
        anchor_score,
        total_score: anchor_score.saturating_add(parameter_score),
    }
}

fn setup_literal_configuration_path_count(text: &str) -> usize {
    extract_explicit_path_literals(text, 16)
        .into_iter()
        .filter(|path| setup_literal_path_has_configuration_extension(path))
        .count()
}

fn setup_literal_path_has_configuration_extension(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
        .iter()
        .any(|extension| lowered.ends_with(extension))
}

fn setup_literal_assignment_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let Some((name, _)) = token.split_once('=') else {
                return false;
            };
            let name = name.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}')
            });
            let Some(first) = name.chars().next() else {
                return false;
            };
            first.is_ascii_alphabetic()
                && name.chars().any(|ch| ch.is_ascii_alphabetic())
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        })
        .take(16)
        .count()
}

fn setup_literal_section_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let cleaned = token.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.' | '(' | ')' | '{' | '}')
            });
            cleaned.len() > 2 && cleaned.starts_with('[') && cleaned.ends_with(']')
        })
        .take(16)
        .count()
}

fn append_setup_literal_chunks(
    selected: &mut Vec<RuntimeMatchedChunk>,
    candidate_chunks: &[RuntimeMatchedChunk],
    max_total_chunks: usize,
) {
    if selected.len() >= max_total_chunks {
        return;
    }
    let selected_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut candidates = candidate_chunks
        .iter()
        .filter(|chunk| !selected_ids.contains(&chunk.chunk_id))
        .filter_map(|chunk| {
            let package_count = extract_package_command_literals(&chunk.source_text, 2).len();
            let config_path_count = extract_explicit_path_literals(&chunk.source_text, 4)
                .into_iter()
                .filter(|path| {
                    let lowered = path.to_ascii_lowercase();
                    lowered.ends_with(".conf") || lowered.ends_with(".ini")
                })
                .count();
            (package_count > 0 && config_path_count > 0).then_some((
                package_count,
                config_path_count,
                chunk,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_packages, left_paths, left), (right_packages, right_paths, right)| {
            right_packages
                .cmp(left_packages)
                .then_with(|| right_paths.cmp(left_paths))
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
                .then_with(|| left.document_id.cmp(&right.document_id))
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        },
    );
    for (_, _, chunk) in candidates {
        if selected.len() >= max_total_chunks {
            break;
        }
        selected.push(chunk.clone());
    }
    if selected.len() >= max_total_chunks {
        return;
    }

    let selected_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let selected_documents = selected.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>();
    let mut parameter_candidates = candidate_chunks
        .iter()
        .filter(|chunk| {
            selected_documents.is_empty() || selected_documents.contains(&chunk.document_id)
        })
        .filter(|chunk| !selected_ids.contains(&chunk.chunk_id))
        .filter_map(|chunk| {
            let parameter_count = extract_parameter_literals(&chunk.source_text, 16).len();
            (parameter_count > 0).then_some((parameter_count, chunk))
        })
        .collect::<Vec<_>>();
    parameter_candidates.sort_by(|(left_count, left), (right_count, right)| {
        right_count
            .cmp(left_count)
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    for (_, chunk) in parameter_candidates {
        if selected.len() >= max_total_chunks {
            break;
        }
        selected.push(chunk.clone());
    }
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
    build_preflight_canonical_evidence_for_scope(
        canonical_evidence,
        scoped_document_ids.as_ref(),
        preflight_allows_empty_scope_fallback(question, query_ir),
    )
}

fn preflight_allows_empty_scope_fallback(_question: &str, query_ir: &QueryIR) -> bool {
    query_ir.is_follow_up()
}

pub(super) fn preflight_exact_literal_document_scope(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> Option<HashSet<Uuid>> {
    if query_ir_has_focused_document_answer_intent(query_ir) {
        return None;
    }
    if has_question_intent(&classify_query_ir_intents(query_ir), QuestionIntent::ErrorCode) {
        return None;
    }
    if query_ir_requests_transport_inventory_scope(query_ir) {
        return None;
    }
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

fn query_ir_requests_transport_inventory_scope(query_ir: &QueryIR) -> bool {
    if !query_ir.literal_constraints.is_empty() || query_ir.source_slice.is_some() {
        return false;
    }
    let intents = classify_query_ir_intents(query_ir);
    (has_question_intent(&intents, QuestionIntent::Port)
        && has_question_intent(&intents, QuestionIntent::Protocol))
        || query_ir
            .target_types
            .iter()
            .any(|target_type| target_type.trim().eq_ignore_ascii_case("connection"))
}

pub(super) fn question_prefers_single_exact_literal_scope(
    question: &str,
    query_ir: &QueryIR,
) -> bool {
    if question_requests_multi_document_scope(question, Some(query_ir)) {
        return false;
    }
    if query_ir.is_follow_up() && !current_question_has_exact_technical_surface(question) {
        return false;
    }
    if query_ir_requests_setup_literal_context(query_ir) {
        return true;
    }
    if query_ir_targets_multiple_technical_literal_families(query_ir) {
        return false;
    }
    !matches!(query_ir.act, crate::domains::query_ir::QueryAct::Enumerate)
}

fn current_question_has_exact_technical_surface(question: &str) -> bool {
    let current = current_question_segment(question);
    current.contains("http://")
        || current.contains("https://")
        || current.contains('/')
        || current
            .split_whitespace()
            .map(|token| {
                token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '.')
            })
            .any(literal_text_is_identifier_shaped)
}

fn query_ir_targets_multiple_technical_literal_families(query_ir: &QueryIR) -> bool {
    let mut families = HashSet::new();
    for target_type in query_ir.target_types.iter().map(|value| value.trim().to_ascii_lowercase()) {
        let Some(family) = technical_literal_target_family(&target_type) else {
            continue;
        };
        families.insert(family);
        if families.len() > 1 {
            return true;
        }
    }
    false
}

fn technical_literal_target_family(target_type: &str) -> Option<&'static str> {
    match target_type {
        "endpoint" | "url" | "path" | "wsdl" | "base_url" | "http_method" | "protocol" => {
            Some("interface")
        }
        "configuration_file" | "filesystem_path" | "config_key" | "parameter" | "env_var" => {
            Some("configuration")
        }
        "software_module" | "package" => Some("module"),
        _ => None,
    }
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
        focus_label_score: usize,
        target_label_score: usize,
        label_score: usize,
        best_chunk_signal: isize,
        chunk_signal_sum: isize,
        retrieval_score_sum: f32,
        first_rank: usize,
    }

    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let focus_label_keywords = preflight_document_focus_label_keywords(query_ir);
    let target_label_keywords = preflight_target_label_keywords(query_ir);
    let pagination_requested = false;
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
            let focus_label_score = focus_label_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&lowered_label, keyword))
                .sum::<usize>();
            let target_label_score = target_label_keywords
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
                focus_label_score,
                target_label_score,
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
            .focus_label_score
            .cmp(&left.focus_label_score)
            .then_with(|| right.best_chunk_signal.cmp(&left.best_chunk_signal))
            .then_with(|| right.chunk_signal_sum.cmp(&left.chunk_signal_sum))
            .then_with(|| right.target_label_score.cmp(&left.target_label_score))
            .then_with(|| right.label_score.cmp(&left.label_score))
            .then_with(|| right.retrieval_score_sum.total_cmp(&left.retrieval_score_sum))
            .then_with(|| left.first_rank.cmp(&right.first_rank))
            .then_with(|| left.document_label.cmp(right.document_label))
    });

    Some(candidates[0].document_id)
}

fn select_preflight_literal_document_id_from_preferred(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    preferred_document_ids: &[Uuid],
) -> Option<Uuid> {
    if chunks.is_empty() || preferred_document_ids.is_empty() {
        return None;
    }
    let preferred = preferred_document_ids.iter().copied().collect::<HashSet<_>>();
    let preferred_chunks = chunks
        .iter()
        .filter(|chunk| preferred.contains(&chunk.document_id))
        .cloned()
        .collect::<Vec<_>>();
    select_preflight_literal_document_id(question, query_ir, &preferred_chunks)
}

fn preflight_document_focus_label_keywords(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut keywords = Vec::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_preflight_label_keywords(&document_focus.hint, &mut seen, &mut keywords);
    }
    keywords
}

fn preflight_target_label_keywords(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut keywords = Vec::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_preflight_label_keywords(&document_focus.hint, &mut seen, &mut keywords);
    }
    for entity in &query_ir.target_entities {
        push_preflight_label_keywords(&entity.label, &mut seen, &mut keywords);
    }
    keywords
}

fn push_preflight_label_keywords(value: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    for token in value
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 4)
        .map(str::to_lowercase)
    {
        if seen.insert(token.clone()) {
            out.push(token);
        }
    }
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
    allow_empty_scope_fallback: bool,
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
        Some(document_ids) => {
            let filtered = filter_runtime_chunks_to_documents(&merged, document_ids);
            if filtered.is_empty()
                && allow_empty_scope_fallback
                && !canonical_answer_chunks.is_empty()
            {
                canonical_answer_chunks.to_vec()
            } else {
                filtered
            }
        }
        None => merged,
    }
}

pub(super) fn extend_setup_preflight_chunks_from_structured_context(
    preflight_answer_chunks: &mut Vec<RuntimeMatchedChunk>,
    structured_context_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
) {
    if structured_context_chunks.is_empty() {
        return;
    }
    let mut seen_chunk_ids =
        preflight_answer_chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut context_chunks = structured_context_chunks
        .iter()
        .filter(|chunk| {
            scoped_document_ids
                .map(|document_ids| document_ids.contains(&chunk.document_id))
                .unwrap_or(true)
        })
        .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    context_chunks.sort_by(|left, right| {
        left.document_label
            .cmp(&right.document_label)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    preflight_answer_chunks.extend(context_chunks);
}

async fn augment_setup_preflight_structured_blocks(
    state: &AppState,
    question: &str,
    query_ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
    preflight_evidence: &mut CanonicalAnswerEvidence,
) -> anyhow::Result<()> {
    if !query_ir_requests_setup_literal_context(query_ir) {
        return Ok(());
    }
    let Some(document_id) = setup_preflight_focused_document_id(
        question,
        query_ir,
        preflight_answer_chunks,
        scoped_document_ids,
    ) else {
        return Ok(());
    };
    let Some(revision_id) =
        setup_preflight_revision_id(document_id, preflight_answer_chunks, document_index)
    else {
        return Ok(());
    };

    let revision_blocks = state
        .arango_document_store
        .list_structured_blocks_by_revision(revision_id)
        .await
        .context("failed to load focused setup structured blocks for canonical preflight")?;
    let loaded_block_count = revision_blocks.len();
    let added_block_count = merge_setup_preflight_structured_blocks(
        preflight_evidence,
        document_id,
        revision_blocks,
        SETUP_PREFLIGHT_STRUCTURED_BLOCK_LIMIT,
    );
    if added_block_count > 0 {
        tracing::info!(
            stage = "answer.preflight.setup_structured_blocks",
            %document_id,
            %revision_id,
            loaded_block_count,
            added_block_count,
            structured_block_count = preflight_evidence.structured_blocks.len(),
            "focused setup structured blocks added to canonical preflight evidence"
        );
    }
    Ok(())
}

fn setup_preflight_focused_document_id(
    question: &str,
    query_ir: &QueryIR,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
) -> Option<Uuid> {
    if let Some(document_ids) = scoped_document_ids
        && document_ids.len() == 1
    {
        return document_ids.iter().next().copied();
    }
    select_setup_literal_document_id(question, query_ir, preflight_answer_chunks)
}

fn setup_preflight_revision_id(
    document_id: Uuid,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<Uuid> {
    preflight_answer_chunks
        .iter()
        .find(|chunk| chunk.document_id == document_id)
        .map(|chunk| chunk.revision_id)
        .or_else(|| document_index.get(&document_id).and_then(canonical_document_revision_id))
}

pub(super) fn merge_setup_preflight_structured_blocks(
    preflight_evidence: &mut CanonicalAnswerEvidence,
    document_id: Uuid,
    revision_blocks: Vec<KnowledgeStructuredBlockRow>,
    limit: usize,
) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut selected = revision_blocks
        .into_iter()
        .filter(|block| block.document_id == document_id)
        .filter_map(|block| {
            let score = setup_preflight_structured_block_score(&block);
            (score > 0).then_some((score, block.ordinal, block.block_id, block))
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)).then_with(|| left.2.cmp(&right.2))
    });
    selected.truncate(limit);
    selected.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)));

    let mut seen_block_ids = preflight_evidence
        .structured_blocks
        .iter()
        .map(|block| block.block_id)
        .collect::<HashSet<_>>();
    let before = preflight_evidence.structured_blocks.len();
    preflight_evidence.structured_blocks.extend(
        selected
            .into_iter()
            .map(|(_, _, _, block)| block)
            .filter(|block| seen_block_ids.insert(block.block_id)),
    );
    preflight_evidence.structured_blocks.len().saturating_sub(before)
}

fn setup_preflight_structured_block_score(block: &KnowledgeStructuredBlockRow) -> usize {
    let text = if block.normalized_text == block.text {
        block.text.clone()
    } else {
        format!("{}\n{}", block.text, block.normalized_text)
    };
    let package_count = extract_package_command_literals(&text, 4).len();
    let path_count = setup_literal_configuration_path_count(&text);
    let assignment_count = setup_literal_assignment_count(&text);
    let section_count = setup_literal_section_count(&text);
    let parameter_count = extract_parameter_literals(&text, 32).len();
    let block_kind = block.block_kind.as_str();
    let kind_score: usize = if block_kind.contains("table_row") {
        32
    } else if block_kind.contains("table") {
        18
    } else if block_kind.contains("code") {
        24
    } else {
        0
    };
    let has_structured_parameter = parameter_count > 0 && kind_score > 0;
    let has_setup_signal =
        package_count > 0 || path_count > 0 || assignment_count > 0 || section_count > 0;
    if !has_setup_signal && !has_structured_parameter {
        return 0;
    }
    kind_score
        .saturating_add(package_count.saturating_mul(16))
        .saturating_add(path_count.saturating_mul(24))
        .saturating_add(assignment_count.saturating_mul(10))
        .saturating_add(section_count.saturating_mul(8))
        .saturating_add(parameter_count.saturating_mul(3))
}

fn build_preflight_canonical_evidence_for_scope(
    canonical_evidence: &CanonicalAnswerEvidence,
    scoped_document_ids: Option<&HashSet<Uuid>>,
    allow_empty_scope_fallback: bool,
) -> CanonicalAnswerEvidence {
    match scoped_document_ids {
        Some(document_ids) => {
            let filtered = filter_canonical_evidence_to_documents(canonical_evidence, document_ids);
            if allow_empty_scope_fallback
                && canonical_evidence_has_rows(canonical_evidence)
                && !canonical_evidence_has_rows(&filtered)
            {
                canonical_evidence.clone()
            } else {
                filtered
            }
        }
        None => canonical_evidence.clone(),
    }
}

fn canonical_evidence_has_rows(canonical_evidence: &CanonicalAnswerEvidence) -> bool {
    !canonical_evidence.chunk_rows.is_empty()
        || !canonical_evidence.structured_blocks.is_empty()
        || !canonical_evidence.technical_facts.is_empty()
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
