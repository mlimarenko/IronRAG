use super::answer_construction::{
    query_ir_low_confidence_unfocused_descriptive_setup, query_ir_requests_setup_literal_context,
};
use super::{
    AuthoritativeAnswerEvidence, HashMap, HashSet, QueryAct, QueryIR, QueryIntentProfile,
    QueryTargetKind, QuestionIntent, RuntimeMatchedChunk, Uuid, classify_query_ir_intents,
    current_question_segment, document_local_focus_keywords, has_question_intent,
    literal_text_is_identifier_shaped, merge_chunks, query_ir_has_focused_document_answer_intent,
    query_ir_targets_graph_entities_or_relationships, question_requests_multi_document_scope,
    score_value, technical_chunk_selection_score, technical_keyword_weight,
    technical_literal_focus_keywords,
};

#[cfg(test)]
pub(in super::super) fn build_preflight_authoritative_evidence(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    authoritative_evidence: &AuthoritativeAnswerEvidence,
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> AuthoritativeAnswerEvidence {
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        query_ir,
        intent_profile,
        technical_literal_chunks,
    );
    build_preflight_authoritative_evidence_for_scope(
        authoritative_evidence,
        scoped_document_ids.as_ref(),
        preflight_allows_empty_scope_fallback(question, query_ir),
    )
}

pub(super) fn preflight_allows_empty_scope_fallback(_question: &str, query_ir: &QueryIR) -> bool {
    query_ir.is_follow_up()
}

pub(in super::super) fn query_ir_requests_broad_procedure_variant_coverage(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    query_ir.requests_broad_procedure_variant_coverage()
        && chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>().len() >= 2
}

pub(in super::super) fn preflight_exact_literal_document_scope(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> Option<HashSet<Uuid>> {
    if query_ir_requests_broad_procedure_variant_coverage(query_ir, technical_literal_chunks) {
        return None;
    }
    if query_ir_has_focused_document_answer_intent(query_ir) {
        return None;
    }
    if has_question_intent(&classify_query_ir_intents(query_ir), QuestionIntent::ErrorCode) {
        return None;
    }
    if query_ir_requests_open_descriptive_context(query_ir) {
        return None;
    }
    if query_ir_requests_transport_inventory_scope(query_ir) {
        return None;
    }
    if query_ir_low_confidence_unfocused_descriptive_setup(query_ir)
        && !current_question_has_exact_preflight_scope_surface(question)
    {
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
        || query_ir.targets(QueryTargetKind::Connection)
}

fn query_ir_requests_open_descriptive_context(query_ir: &QueryIR) -> bool {
    if !query_ir.literal_constraints.is_empty()
        || query_ir.source_slice.is_some()
        || query_ir.is_follow_up()
        || query_ir_has_focused_document_answer_intent(query_ir)
        || query_ir_requests_setup_literal_context(query_ir)
    {
        return false;
    }
    if !matches!(query_ir.act, QueryAct::Compare | QueryAct::Describe | QueryAct::RetrieveValue) {
        return false;
    }
    let intents = classify_query_ir_intents(query_ir);
    if !intents.is_empty() {
        return false;
    }
    !query_ir.target_types.is_empty() && query_ir_targets_graph_entities_or_relationships(query_ir)
}

pub(in super::super) fn question_prefers_single_exact_literal_scope(
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

pub(super) fn current_question_has_exact_technical_surface(question: &str) -> bool {
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

fn current_question_has_exact_preflight_scope_surface(question: &str) -> bool {
    let current = current_question_segment(question);
    current.contains("http://")
        || current.contains("https://")
        || current.contains('/')
        || current
            .split_whitespace()
            .map(|token| {
                token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '.')
            })
            .any(token_has_exact_preflight_scope_surface)
}

fn token_has_exact_preflight_scope_surface(token: &str) -> bool {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return false;
    }
    let mut has_numeric = false;
    let mut has_separator = false;
    let mut seen_lowercase_before = false;
    let mut has_uppercase_after_lowercase = false;

    for ch in trimmed.chars() {
        if ch.is_alphabetic() {
            if ch.is_uppercase() {
                has_uppercase_after_lowercase |= seen_lowercase_before;
            }
            if ch.is_lowercase() {
                seen_lowercase_before = true;
            }
        } else if ch.is_numeric() {
            has_numeric = true;
        } else if matches!(ch, '_' | '-' | '.') {
            has_separator = true;
        } else {
            return false;
        }
    }

    has_separator || has_numeric || has_uppercase_after_lowercase
}

fn query_ir_targets_multiple_technical_literal_families(query_ir: &QueryIR) -> bool {
    let mut families = HashSet::new();
    for target_type in &query_ir.target_types {
        let Some(family) = technical_literal_target_family(*target_type) else {
            continue;
        };
        families.insert(family);
        if families.len() > 1 {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TechnicalLiteralTargetFamily {
    Interface,
    Configuration,
    Module,
}

const fn technical_literal_target_family(
    target_type: QueryTargetKind,
) -> Option<TechnicalLiteralTargetFamily> {
    match target_type {
        QueryTargetKind::Endpoint
        | QueryTargetKind::Url
        | QueryTargetKind::Path
        | QueryTargetKind::Wsdl
        | QueryTargetKind::BaseUrl
        | QueryTargetKind::HttpMethod
        | QueryTargetKind::Protocol => Some(TechnicalLiteralTargetFamily::Interface),
        QueryTargetKind::ConfigurationFile
        | QueryTargetKind::FilesystemPath
        | QueryTargetKind::ConfigKey
        | QueryTargetKind::Parameter
        | QueryTargetKind::EnvVar => Some(TechnicalLiteralTargetFamily::Configuration),
        QueryTargetKind::SoftwareModule | QueryTargetKind::Package => {
            Some(TechnicalLiteralTargetFamily::Module)
        }
        _ => None,
    }
}

pub(in super::super) fn select_preflight_literal_document_id(
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

pub(super) fn select_preflight_literal_document_id_from_preferred(
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

pub(super) fn preflight_target_label_keywords(query_ir: &QueryIR) -> Vec<String> {
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

pub(super) fn build_preflight_answer_chunks_for_scope(
    primary_answer_chunks: &[RuntimeMatchedChunk],
    technical_literal_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
    allow_empty_scope_fallback: bool,
) -> Vec<RuntimeMatchedChunk> {
    let merged = if technical_literal_chunks.is_empty() {
        primary_answer_chunks.to_vec()
    } else if primary_answer_chunks.is_empty() {
        technical_literal_chunks.to_vec()
    } else {
        merge_chunks(
            technical_literal_chunks.to_vec(),
            primary_answer_chunks.to_vec(),
            primary_answer_chunks.len().max(technical_literal_chunks.len()).max(12),
        )
    };

    match scoped_document_ids {
        Some(document_ids) => {
            let filtered = filter_runtime_chunks_to_documents(&merged, document_ids);
            if filtered.is_empty()
                && allow_empty_scope_fallback
                && !primary_answer_chunks.is_empty()
            {
                primary_answer_chunks.to_vec()
            } else {
                filtered
            }
        }
        None => merged,
    }
}

pub(super) fn build_preflight_authoritative_evidence_for_scope(
    authoritative_evidence: &AuthoritativeAnswerEvidence,
    scoped_document_ids: Option<&HashSet<Uuid>>,
    allow_empty_scope_fallback: bool,
) -> AuthoritativeAnswerEvidence {
    match scoped_document_ids {
        Some(document_ids) => {
            let filtered =
                filter_authoritative_evidence_to_documents(authoritative_evidence, document_ids);
            if allow_empty_scope_fallback
                && authoritative_evidence_has_rows(authoritative_evidence)
                && !authoritative_evidence_has_rows(&filtered)
            {
                authoritative_evidence.clone()
            } else {
                filtered
            }
        }
        None => authoritative_evidence.clone(),
    }
}

fn authoritative_evidence_has_rows(authoritative_evidence: &AuthoritativeAnswerEvidence) -> bool {
    !authoritative_evidence.chunk_rows.is_empty()
        || !authoritative_evidence.structured_blocks.is_empty()
        || !authoritative_evidence.technical_facts.is_empty()
}

fn filter_authoritative_evidence_to_documents(
    authoritative_evidence: &AuthoritativeAnswerEvidence,
    document_ids: &HashSet<Uuid>,
) -> AuthoritativeAnswerEvidence {
    AuthoritativeAnswerEvidence {
        bundle: authoritative_evidence.bundle.clone(),
        chunk_rows: authoritative_evidence
            .chunk_rows
            .iter()
            .filter(|row| document_ids.contains(&row.document_id))
            .cloned()
            .collect(),
        structured_blocks: authoritative_evidence
            .structured_blocks
            .iter()
            .filter(|block| document_ids.contains(&block.document_id))
            .cloned()
            .collect(),
        technical_facts: authoritative_evidence
            .technical_facts
            .iter()
            .filter(|fact| document_ids.contains(&fact.document_id))
            .cloned()
            .collect(),
    }
}
