use super::literal_coverage::{
    answer_has_partial_coverage_warning, answer_needs_literal_revision,
    answer_omits_expected_technical_literals,
};
use super::literal_revision::{
    extract_ranked_structural_literal_candidates, structural_literal_focus_keyword_is_eligible,
};
use super::output_boundary::source_slice_single_shot_min_chars;
use super::preflight::structural_question_tokens;
use super::verification::query_ir_is_low_confidence_unfocused_answer;
use super::{
    AnswerVerificationStage, BTreeSet, EntityRole, HashSet, PreparedAnswerQueryResult, QueryAct,
    QueryIR, QueryScope, QueryTargetKind, SINGLE_SHOT_CONFIDENT_ANSWER_CHARS,
    SINGLE_SHOT_MIN_ANSWER_CHARS, SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS,
    detect_explicit_technical_literal_intent_from_query_ir, extract_config_section_literals,
    extract_explicit_path_literals, extract_package_command_literals, extract_parameter_literals,
    query_ir_has_focused_document_answer_intent,
};

pub(super) fn should_use_single_shot_answer(
    question: &str,
    prepared: &PreparedAnswerQueryResult,
    conversation_history: Option<&str>,
) -> bool {
    let _ = question;
    if single_shot_blocked_by_structure(prepared) {
        return false;
    }
    let has_conversation_history =
        conversation_history.map(str::trim).is_some_and(|v| !v.is_empty());
    single_shot_coverage_verdict(prepared, has_conversation_history)
}

/// Structural gates that keep a question off the single-shot fast path
/// regardless of the evidence-coverage verdict.
fn single_shot_blocked_by_structure(prepared: &PreparedAnswerQueryResult) -> bool {
    if query_ir_has_focused_document_answer_intent(&prepared.query_ir) {
        return true;
    }
    if prepared.query_ir.requests_source_coverage_context() {
        return true;
    }
    // Only hard requirement: the prepared context must carry *something*
    // the model can ground an answer in. Even when structured retrieval
    // returned zero chunks, `answer_context` still packs the library
    // summary, recent documents, and selected graph context. That alone
    // is enough for the model to produce a grounded insufficiency answer
    // without spending another pass rediscovering the same empty result.
    if prepared.answer_context.trim().is_empty() {
        return true;
    }
    if focused_configuration_inventory_waits_for_preflight(
        &prepared.query_ir,
        &prepared.answer_context,
    ) {
        log_single_shot_coverage(
            &prepared.query_ir.act,
            "focused configuration inventory waits for evidence preflight",
        );
        return true;
    }
    if structural_literal_comparison_waits_for_preflight(
        &prepared.query_ir,
        &prepared.answer_context,
    ) {
        log_single_shot_coverage(
            &prepared.query_ir.act,
            "comparison with exact structural literals waits for evidence preflight",
        );
        return true;
    }
    if super::super::answer::build_update_procedure_sequence_answer(
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    )
    .is_some()
    {
        log_single_shot_coverage(
            &prepared.query_ir.act,
            "deterministic update procedure is available; single-shot must not preempt it",
        );
        return true;
    }
    false
}

fn log_single_shot_coverage(act: &QueryAct, message: &str) {
    tracing::info!(
        stage = "answer.single_shot_coverage",
        query_ir_act = ?act,
        "{message}"
    );
}

/// Single-shot is evidence-gated, not act-blacklisted. The prepared retrieval
/// context is authoritative when it structurally covers the operands required
/// by the IR. Questions that still depend on unresolved conversation anchors or
/// library operations remain off the initial fast path until those requirements
/// are represented in the same coverage model. Retrieval injects version-sorted
/// release chunks directly into `answer_context`; a second retrieval pass would
/// only repeat document reads without adding authoritative evidence.
fn single_shot_coverage_verdict(
    prepared: &PreparedAnswerQueryResult,
    has_conversation_history: bool,
) -> bool {
    match evaluate_single_shot_evidence_coverage(prepared, has_conversation_history) {
        EvidenceCoverage::Sufficient => true,
        EvidenceCoverage::Partial { missing_operands, .. } => {
            log_single_shot_partial_coverage(&prepared.query_ir.act, missing_operands.len());
            true
        }
        EvidenceCoverage::Insufficient(reason) => {
            log_single_shot_insufficient_coverage(&prepared.query_ir.act, reason);
            false
        }
    }
}

fn log_single_shot_partial_coverage(act: &QueryAct, missing_operand_count: usize) {
    tracing::info!(
        stage = "answer.single_shot_coverage",
        query_ir_act = ?act,
        missing_operand_count,
        "prepared answer context partially covers comparison operands; single-shot must answer with explicit insufficiency for uncovered operands"
    );
}

fn log_single_shot_insufficient_coverage(act: &QueryAct, reason: &'static str) {
    tracing::info!(
        stage = "answer.single_shot_coverage",
        query_ir_act = ?act,
        reason,
        "prepared answer context does not structurally cover the single-shot requirements"
    );
}

pub(super) fn structural_literal_comparison_waits_for_preflight(
    query_ir: &QueryIR,
    answer_context: &str,
) -> bool {
    if !matches!(query_ir.act, QueryAct::Compare) || query_ir.comparison.is_none() {
        return false;
    }
    let literal_context = answer_context_without_evidence_metadata(answer_context);
    let exact_literals = extract_ranked_structural_literal_candidates(
        &literal_context,
        &comparison_focus_keywords(query_ir),
        &mut HashSet::new(),
    );
    if exact_literals.is_empty() {
        return false;
    }
    !matches!(
        compare_operands_covered_by_context(query_ir, answer_context),
        EvidenceCoverage::Insufficient(_)
    )
}

pub(super) fn comparison_focus_keywords(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = HashSet::new();
    [
        query_ir.retrieval_query.as_deref(),
        query_ir.comparison.as_ref().map(|comparison| comparison.dimension.as_str()),
    ]
    .into_iter()
    .flatten()
    .flat_map(structural_question_tokens)
    .map(|token| token.to_lowercase())
    .filter(|token| structural_literal_focus_keyword_is_eligible(token))
    .filter(|token| seen.insert(token.clone()))
    .collect()
}

pub(super) fn focused_configuration_inventory_waits_for_preflight(
    query_ir: &QueryIR,
    answer_context: &str,
) -> bool {
    let literal_context = answer_context_without_evidence_metadata(answer_context);
    let package_count = extract_package_command_literals(&literal_context, 4).len();
    let configuration_path_count = count_configuration_file_paths(&literal_context, 16);
    let section_count = extract_config_section_literals(&literal_context, 8).len();
    let parameter_count = extract_parameter_literals(&literal_context, 32).len();

    let carries_configuration_inventory = (package_count > 0 && configuration_path_count > 0)
        || (configuration_path_count > 0 && section_count > 0 && parameter_count >= 2)
        || (configuration_path_count > 0 && parameter_count >= 4)
        || (section_count > 0 && parameter_count >= 6);
    if !carries_configuration_inventory {
        return false;
    }

    if low_confidence_unfocused_configuration_ir(query_ir)
        || low_confidence_structural_configuration_ir(query_ir)
    {
        return true;
    }

    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    let requests_configuration =
        query_ir.targets_any(&[QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey]);
    if !requests_configuration {
        return false;
    }
    query_ir.document_focus.is_some()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
}

pub(super) fn low_confidence_unfocused_configuration_ir(query_ir: &QueryIR) -> bool {
    query_ir_is_low_confidence_unfocused_answer(query_ir)
}

pub(super) fn low_confidence_structural_configuration_ir(query_ir: &QueryIR) -> bool {
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

pub(super) fn answer_context_without_evidence_metadata(answer_context: &str) -> String {
    let mut stripped = String::with_capacity(answer_context.len());
    for line in answer_context.lines() {
        stripped.push_str(strip_typed_evidence_provenance_prefix(line));
        stripped.push('\n');
    }
    stripped
}

pub(super) fn strip_typed_evidence_provenance_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    let candidate = trimmed.strip_prefix("- ").unwrap_or(trimmed);
    if !candidate.starts_with('[') {
        return line;
    }
    let Some(close_index) = candidate.find(']') else {
        return line;
    };
    let kind = candidate[1..close_index].split_ascii_whitespace().next();
    if !matches!(kind, Some("EVIDENCE_CHUNK" | "graph-evidence")) {
        return line;
    }
    candidate[(close_index + 1)..].trim_start()
}

pub(super) fn count_configuration_file_paths(text: &str, limit: usize) -> usize {
    let mut seen = HashSet::new();
    for path in extract_explicit_path_literals(text, limit) {
        if is_configuration_file_path(&path) {
            seen.insert(path);
        }
    }
    for token in text.split_whitespace() {
        if seen.len() >= limit {
            break;
        }
        let cleaned = clean_configuration_path_candidate(token);
        if cleaned.starts_with('/') && is_configuration_file_path(cleaned) {
            seen.insert(cleaned.to_string());
        }
    }
    seen.len()
}

pub(super) fn clean_configuration_path_candidate(token: &str) -> &str {
    let mut value = token.trim();
    loop {
        let trimmed = value
            .trim_start_matches(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, '`' | '"' | '\'' | '(' | '[' | '{' | ',' | ';' | ':')
            })
            .trim_end_matches(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, '`' | '"' | '\'' | ')' | ']' | '}' | ',' | ';' | ':' | '.')
            });
        if trimmed.len() == value.len() {
            return trimmed;
        }
        value = trimmed;
    }
}

pub(super) fn is_configuration_file_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
        .iter()
        .any(|extension| lowered.ends_with(extension))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EvidenceCoverage {
    Sufficient,
    Partial { covered_operands: Vec<String>, missing_operands: Vec<String> },
    Insufficient(&'static str),
}

pub(super) fn evaluate_single_shot_evidence_coverage(
    prepared: &PreparedAnswerQueryResult,
    has_conversation_history: bool,
) -> EvidenceCoverage {
    evaluate_single_shot_evidence_coverage_for_context(
        &prepared.query_ir,
        &prepared.answer_context,
        has_conversation_history,
    )
}

pub(super) fn evaluate_single_shot_evidence_coverage_for_context(
    ir: &QueryIR,
    answer_context: &str,
    has_conversation_history: bool,
) -> EvidenceCoverage {
    if ir.is_follow_up() && has_conversation_history {
        return EvidenceCoverage::Insufficient("follow_up_context_anchor_unresolved");
    }
    if matches!(ir.act, QueryAct::Meta) {
        return EvidenceCoverage::Insufficient("library_meta_requires_catalog_evidence");
    }
    if matches!(ir.act, QueryAct::Compare) {
        return compare_operands_covered_by_context(ir, answer_context);
    }
    if single_shot_context_lacks_query_focus_support(ir, answer_context) {
        return EvidenceCoverage::Insufficient("query_focus_uncovered");
    }
    EvidenceCoverage::Sufficient
}

pub(super) fn single_shot_coverage_allows_attempt(coverage: &EvidenceCoverage) -> bool {
    matches!(coverage, EvidenceCoverage::Sufficient | EvidenceCoverage::Partial { .. })
}

pub(super) fn compare_operands_covered_by_context(
    ir: &QueryIR,
    answer_context: &str,
) -> EvidenceCoverage {
    let operands = comparison_operands(ir);
    if operands.len() < 2 {
        return EvidenceCoverage::Insufficient("compare_operands_missing");
    }
    let evidence_lines =
        answer_context.lines().map(str::trim).filter(is_context_evidence_line).collect::<Vec<_>>();
    if evidence_lines.is_empty() {
        return EvidenceCoverage::Insufficient("compare_evidence_empty");
    }
    let mut covered_operands = Vec::<String>::new();
    let mut missing_operands = Vec::<String>::new();
    for operand in operands {
        if operand_covered_by_evidence(&operand, &evidence_lines) {
            covered_operands.push(operand);
        } else {
            missing_operands.push(operand);
        }
    }
    if missing_operands.is_empty() {
        EvidenceCoverage::Sufficient
    } else if covered_operands.is_empty() {
        EvidenceCoverage::Insufficient("compare_operands_uncovered")
    } else {
        EvidenceCoverage::Partial { covered_operands, missing_operands }
    }
}

pub(super) fn is_context_evidence_line(line: &&str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && !line.starts_with("COMPARISON_COVERAGE ")
        && !line.starts_with("- covered_operand:")
        && !line.starts_with("- uncovered_operand:")
}

pub(super) fn comparison_operands(ir: &QueryIR) -> Vec<String> {
    let mut operands = Vec::<String>::new();
    if let Some(comparison) = &ir.comparison {
        if let Some(value) = comparison.a.as_deref() {
            push_operand(&mut operands, value);
        }
        if let Some(value) = comparison.b.as_deref() {
            push_operand(&mut operands, value);
        }
    }
    if operands.len() >= 2 {
        return operands;
    }
    for entity in &ir.target_entities {
        push_operand(&mut operands, &entity.label);
    }
    operands
}

pub(super) fn push_operand(operands: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if operands.iter().any(|existing| existing.eq_ignore_ascii_case(trimmed)) {
        return;
    }
    operands.push(trimmed.to_string());
}

pub(super) fn operand_covered_by_evidence(operand: &str, evidence_lines: &[&str]) -> bool {
    let operand_tokens = crate::services::query::text_match::normalized_alnum_tokens(operand, 2);
    if operand_tokens.is_empty() {
        return false;
    }
    let required_overlap = operand_tokens.len().clamp(1, 2);
    evidence_lines.iter().any(|line| {
        let line_tokens = crate::services::query::text_match::normalized_alnum_tokens(line, 2);
        crate::services::query::text_match::near_token_overlap_count(&operand_tokens, &line_tokens)
            >= required_overlap
    })
}

/// Treat a single-shot answer as acceptable when it carries enough
/// text to be useful, the verifier did not rewrite it, AND the
/// model did not obviously capitulate in front of a non-empty
/// retrieval bundle.
///
/// Structural signals:
///   * Absolute length floor — below `SINGLE_SHOT_MIN_ANSWER_CHARS`
///     is always treated as a decline.
///   * Verifier rewrite — `verify_generated_answer` only rewrites
///     the answer under strict-mode suppression of a hallucinated
///     literal; a matching trimmed raw vs. verified string means
///     the verifier let the answer through.
///   * Retrieval-vs-length heuristic — when retrieval surfaced
///     `>= SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS` and the
///     answer is still `< SINGLE_SHOT_CONFIDENT_ANSWER_CHARS`, the
///     single-shot path almost certainly refused on partial
///     evidence (see the one-word vs. "who is X" observation above).
///     Escalate instead of returning the stub.
///
/// No decline-phrase matching, no language-specific strings: the
/// verifier owns grounding, length owns "did the model produce
/// something", and the retrieval footprint owns "did the model
/// refuse in the face of real evidence".
pub(super) fn single_shot_answer_is_acceptable(
    raw_answer: &str,
    verification: &AnswerVerificationStage,
    retrieved_document_count: usize,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    let trimmed = raw_answer.trim();
    let answer_chars = trimmed.chars().count();
    if answer_chars < SINGLE_SHOT_MIN_ANSWER_CHARS {
        return false;
    }
    if answer_needs_literal_revision(verification) {
        return false;
    }
    if answer_has_partial_coverage_warning(verification) {
        return false;
    }
    let verified = verification.generation.answer.trim();
    if verified.is_empty() {
        return false;
    }
    if trimmed != verified {
        return false;
    }
    if answer_chars < SINGLE_SHOT_CONFIDENT_ANSWER_CHARS
        && retrieved_document_count >= SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS
    {
        return false;
    }
    if let Some(min_chars) = source_slice_single_shot_min_chars(query_ir)
        && answer_chars < min_chars
    {
        return false;
    }
    if answer_omits_expected_technical_literals(trimmed, query_ir, grounding_context) {
        return false;
    }
    if single_shot_lacks_query_focus_support(trimmed, query_ir, grounding_context) {
        return false;
    }
    true
}

pub(super) fn single_shot_context_lacks_query_focus_support(
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !query_requires_single_shot_focus_support(query_ir) {
        return false;
    }
    let focus_segments = query_focus_support_segments(query_ir);
    if focus_segments.is_empty() {
        return false;
    }
    let context_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(grounding_context, 4);
    !focus_segments
        .iter()
        .any(|segment| focus_segment_supported_by_tokens(segment, &context_tokens))
}

pub(super) fn single_shot_lacks_query_focus_support(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !query_requires_single_shot_focus_support(query_ir) {
        return false;
    }
    let focus_segments = query_focus_support_segments(query_ir);
    if focus_segments.is_empty() {
        return false;
    }
    let context_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(grounding_context, 4);
    let answer_tokens = crate::services::query::text_match::normalized_alnum_tokens(answer, 4);
    let supported_segments = focus_segments
        .iter()
        .filter(|segment| focus_segment_supported_by_tokens(segment, &context_tokens))
        .collect::<Vec<_>>();
    if supported_segments.is_empty() {
        return true;
    }
    !supported_segments
        .iter()
        .any(|segment| focus_segment_supported_by_tokens(segment, &answer_tokens))
}

pub(super) fn query_requires_single_shot_focus_support(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::RetrieveValue) {
        return false;
    }
    let intent = detect_explicit_technical_literal_intent_from_query_ir("", query_ir);
    intent.any() || query_ir.document_focus.is_some() || query_ir.has_exact_technical_literal()
}

pub(super) fn query_focus_support_segments(query_ir: &QueryIR) -> Vec<BTreeSet<String>> {
    let mut segments = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject | EntityRole::Object))
        .filter_map(|entity| focus_support_tokens(&entity.label))
        .collect::<Vec<_>>();
    if segments.is_empty()
        && let Some(document_focus) = &query_ir.document_focus
        && let Some(tokens) = focus_support_tokens(&document_focus.hint)
    {
        segments.push(tokens);
    }
    for literal in &query_ir.literal_constraints {
        if let Some(tokens) = focus_support_tokens(&literal.text) {
            segments.push(tokens);
        }
    }
    segments
}

pub(super) fn focus_support_tokens(value: &str) -> Option<BTreeSet<String>> {
    let tokens = crate::services::query::text_match::normalized_alnum_tokens(value, 4);
    (!tokens.is_empty()).then_some(tokens)
}

pub(super) fn focus_segment_supported_by_tokens(
    segment_tokens: &BTreeSet<String>,
    available_tokens: &BTreeSet<String>,
) -> bool {
    if segment_tokens.is_empty() {
        return false;
    }
    crate::services::query::text_match::near_token_overlap_count(segment_tokens, available_tokens)
        >= focus_segment_required_overlap(segment_tokens)
}

pub(super) fn focus_segment_required_overlap(segment_tokens: &BTreeSet<String>) -> usize {
    if segment_tokens.len() <= 2 { 1 } else { 2 }
}
