use super::coverage::strip_typed_evidence_provenance_prefix;
use super::literal_revision::contains_label_mention;
use super::preflight::{
    STRUCTURAL_COVERAGE_FOCUS_MIN_TOKEN_CHARS, STRUCTURAL_COVERAGE_MAX_ANCHOR_CHARS,
    STRUCTURAL_COVERAGE_MAX_ANCHORS, STRUCTURAL_COVERAGE_MIN_ANSWER_ANCHORS,
    STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHOR_LINES, STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHORS,
};
use super::{
    AnswerGenerationStage, AnswerKind, AnswerVerificationRequest, AnswerVerificationStage,
    BTreeSet, HashMap, HashSet, QueryAct, QueryIR, QueryTargetKind, QueryVerificationState,
    QueryVerificationWarning, Uuid, extract_config_section_literals,
    extract_explicit_path_literals, extract_parameter_literals,
    query_ir_requires_remediation_synthesis, verify_answer_against_authoritative_evidence,
};

pub(crate) async fn verify_generated_answer(
    execution_id: Uuid,
    question: &str,
    generation: AnswerGenerationStage,
) -> anyhow::Result<AnswerVerificationStage> {
    let mut verification =
        verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
            question,
            answer: &generation.answer,
            intent_profile: &generation.intent_profile,
            evidence: &generation.authoritative_evidence,
            chunks: &generation.primary_answer_chunks,
            prompt_context: &generation.prompt_context,
            assistant_grounding: &generation.assistant_grounding,
        });
    apply_structural_coverage_warning(
        question,
        &generation.answer,
        &generation.query_ir,
        &generation.prompt_context,
        &generation.usage_json,
        &mut verification,
    );
    apply_enumeration_coverage_warning(
        &generation.answer,
        &generation.query_ir,
        &generation.authoritative_evidence,
        &mut verification,
    );
    let tripped = verifier_warning_tripped(&verification);
    log_verification_outcome(execution_id, &generation, &verification, tripped);

    Ok(AnswerVerificationStage { generation, verification })
}

/// Whether the verifier flagged a hallucinated literal or an unsupported
/// canonical evidence claim.
fn verifier_warning_tripped(verification: &super::super::RuntimeAnswerVerification) -> bool {
    has_verification_warning(verification, "unsupported_literal")
        || has_verification_warning(verification, "unsupported_\u{63}anonical_claim")
}

fn has_verification_warning(
    verification: &super::super::RuntimeAnswerVerification,
    code: &str,
) -> bool {
    verification.warnings.iter().any(|warning| warning.code == code)
}

/// Surfaces verification warnings as metadata; the grounded answer body is not
/// replaced by a static fallback.
fn log_verification_outcome(
    execution_id: Uuid,
    generation: &AnswerGenerationStage,
    verification: &super::super::RuntimeAnswerVerification,
    verifier_tripped: bool,
) {
    if verifier_tripped {
        log_verification_warnings_kept(execution_id, generation, verification);
    } else if matches!(verification.state, QueryVerificationState::Conflicting) {
        log_conflicting_evidence_kept(execution_id);
    }
}

fn log_verification_warnings_kept(
    execution_id: Uuid,
    generation: &AnswerGenerationStage,
    verification: &super::super::RuntimeAnswerVerification,
) {
    tracing::info!(
        %execution_id,
        verification_level = ?generation.query_ir.verification_level(),
        warnings = verification.warnings.len(),
        confidence = generation.query_ir.confidence,
        "answer kept despite verification warnings; surfacing via state + warnings only"
    );
}

fn log_conflicting_evidence_kept(execution_id: Uuid) {
    tracing::info!(
        %execution_id,
        "answer kept despite conflicting evidence (verification flag only)"
    );
}

pub(super) fn apply_structural_coverage_warning(
    question: &str,
    answer: &str,
    query_ir: &QueryIR,
    prompt_context: &str,
    usage_json: &serde_json::Value,
    verification: &mut super::super::RuntimeAnswerVerification,
) {
    if deterministic_generation_skips_structural_coverage_warning(usage_json) {
        return;
    }
    if !super::super::answer::answer_omits_structural_identifier_mapping_target(
        question,
        answer,
        query_ir,
        prompt_context,
    ) && !answer_omits_structural_context_coverage(answer, query_ir, prompt_context)
    {
        return;
    }
    if verification.warnings.iter().any(|warning| warning.code == "partial_coverage") {
        return;
    }
    verification.warnings.push(QueryVerificationWarning {
        code: "partial_coverage".to_string(),
        message: "Answer omitted structural anchors that were present in retrieved evidence."
            .to_string(),
        related_segment_id: None,
        related_fact_id: None,
    });
    if matches!(verification.state, QueryVerificationState::Verified) {
        verification.state = QueryVerificationState::PartiallySupported;
    }
}

/// Verification confirms grounding, not completeness: a one-item answer to
/// an enumeration question was marked `verified` because every claim in it
/// was trivially supported. Typed evidence closes that gap without natural
/// language heuristics — when the answer covers fewer than two distinct
/// values of a technical-fact kind for which the evidence holds several,
/// the enumeration is demonstrably partial and `verified` is demoted. Runs
/// for deterministic answers too: the observed stubs were deterministic.
pub(crate) fn apply_enumeration_coverage_warning(
    answer: &str,
    query_ir: &QueryIR,
    evidence: &super::super::types::AuthoritativeAnswerEvidence,
    verification: &mut super::super::RuntimeAnswerVerification,
) {
    if !matches!(query_ir.act, crate::domains::query_ir::QueryAct::Enumerate) {
        return;
    }
    if verification.warnings.iter().any(|warning| warning.code == "enumeration_coverage_partial") {
        return;
    }
    let Some((fact_kind, distinct, mentioned)) =
        least_covered_mentioned_fact_kind(answer, evidence)
    else {
        return;
    };
    if distinct < 2 || mentioned >= 2 {
        return;
    }
    tracing::info!(
        stage = "answer.enumeration_coverage",
        fact_kind,
        distinct_evidence_values = distinct,
        mentioned_values = mentioned,
        "enumeration answer covers a fraction of the typed evidence for its fact kind"
    );
    verification.warnings.push(QueryVerificationWarning {
        code: "enumeration_coverage_partial".to_string(),
        message: format!(
            "Answer mentions {mentioned} of {distinct} distinct documented values for the \
             enumerated fact kind."
        ),
        related_segment_id: None,
        related_fact_id: None,
    });
    if matches!(verification.state, QueryVerificationState::Verified) {
        verification.state = QueryVerificationState::PartiallySupported;
    }
}

/// For every technical-fact kind the answer touches at least once, count the
/// distinct documented values and how many the answer mentions; return the
/// touched kind with the weakest coverage. Kinds the answer never mentions
/// are ignored — they belong to a different aspect of the evidence, not to
/// the enumeration being answered.
pub(super) fn least_covered_mentioned_fact_kind(
    answer: &str,
    evidence: &super::super::types::AuthoritativeAnswerEvidence,
) -> Option<(String, usize, usize)> {
    let mut distinct_by_kind = HashMap::<&str, HashSet<&str>>::new();
    let mut mentioned_by_kind = HashMap::<&str, HashSet<&str>>::new();
    for fact in &evidence.technical_facts {
        let value = fact.normalized_value_exact.trim();
        if value.is_empty() {
            continue;
        }
        distinct_by_kind.entry(fact.fact_kind.as_str()).or_default().insert(value);
        if contains_label_mention(answer, value)
            || contains_label_mention(answer, fact.display_value.trim())
        {
            mentioned_by_kind.entry(fact.fact_kind.as_str()).or_default().insert(value);
        }
    }
    mentioned_by_kind
        .into_iter()
        .map(|(kind, mentioned)| {
            let distinct = distinct_by_kind.get(kind).map_or(0, HashSet::len);
            (kind.to_string(), distinct, mentioned.len())
        })
        .min_by(|left, right| {
            left.2.cmp(&right.2).then(right.1.cmp(&left.1)).then(left.0.cmp(&right.0))
        })
}

pub(super) fn deterministic_generation_skips_structural_coverage_warning(
    usage_json: &serde_json::Value,
) -> bool {
    if usage_json.get("deterministic").and_then(serde_json::Value::as_bool) != Some(true) {
        return false;
    }
    matches!(
        AnswerKind::from_usage_json(usage_json),
        Some(
            AnswerKind::SetupConfigurationAnchor
                | AnswerKind::UpdateProcedureSequence
                | AnswerKind::DeterministicGroundedAnswer
        )
    )
}

pub(super) fn answer_omits_structural_context_coverage(
    answer: &str,
    query_ir: &QueryIR,
    prompt_context: &str,
) -> bool {
    if !query_ir_needs_structural_coverage_guard(query_ir) {
        return false;
    }
    let anchors = collect_structural_coverage_anchors(query_ir, prompt_context);
    if anchors.items.len() < STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHORS
        || anchors.line_count < STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHOR_LINES
    {
        return false;
    }
    let answer_lower = answer.to_lowercase();
    let answer_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(&answer_lower, 1);
    let covered = anchors
        .items
        .iter()
        .filter(|anchor| structural_answer_contains_anchor(&answer_lower, &answer_tokens, anchor))
        .take(STRUCTURAL_COVERAGE_MIN_ANSWER_ANCHORS)
        .count();
    covered < STRUCTURAL_COVERAGE_MIN_ANSWER_ANCHORS
}

pub(super) fn query_ir_needs_structural_coverage_guard(query_ir: &QueryIR) -> bool {
    if query_ir.requests_broad_procedure_variant_coverage()
        || query_ir_requires_remediation_synthesis(query_ir)
        || matches!(query_ir.act, QueryAct::Compare | QueryAct::Enumerate | QueryAct::Meta)
    {
        return false;
    }
    query_ir.targets_any(&[
        QueryTargetKind::Procedure,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Parameter,
    ])
}

pub(super) fn query_ir_is_low_confidence_unfocused_answer(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.35
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
        && query_ir.temporal_constraints.is_empty()
        && query_ir.conversation_refs.is_empty()
}

#[derive(Debug, Default)]
pub(super) struct StructuralCoverageAnchors {
    pub(super) items: Vec<String>,
    pub(super) line_count: usize,
}

pub(super) fn collect_structural_coverage_anchors(
    query_ir: &QueryIR,
    prompt_context: &str,
) -> StructuralCoverageAnchors {
    let mut seen = HashSet::<String>::new();
    let mut items = Vec::<String>::new();
    let mut line_count = 0usize;
    let focus_tokens = structural_coverage_focus_tokens(query_ir);
    let mut focused_graph_block = false;
    for line in prompt_context.lines() {
        if line.trim_start().starts_with("[graph-evidence") {
            focused_graph_block = structural_coverage_line_matches_focus(&focus_tokens, line);
        }
        let line_matches_focus =
            focused_graph_block || structural_coverage_line_matches_focus(&focus_tokens, line);
        if !line_matches_focus {
            continue;
        }
        let before = items.len();
        push_structural_anchor_literals(
            strip_typed_evidence_provenance_prefix(line),
            &mut seen,
            &mut items,
        );
        if items.len() > before {
            line_count += 1;
        }
        if items.len() >= STRUCTURAL_COVERAGE_MAX_ANCHORS {
            break;
        }
    }
    StructuralCoverageAnchors { items, line_count }
}

pub(super) fn structural_coverage_focus_tokens(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut seen = HashSet::<String>::new();
    let mut tokens = BTreeSet::<String>::new();
    for text in query_ir
        .target_entities
        .iter()
        .map(|entity| entity.label.as_str())
        .chain(query_ir.literal_constraints.iter().map(|literal| literal.text.as_str()))
        .chain(query_ir.document_focus.iter().map(|focus| focus.hint.as_str()))
    {
        for token in crate::services::query::text_match::normalized_alnum_tokens(
            text,
            STRUCTURAL_COVERAGE_FOCUS_MIN_TOKEN_CHARS,
        ) {
            if seen.insert(token.clone()) {
                tokens.insert(token);
            }
        }
    }
    tokens
}

pub(super) fn structural_coverage_line_matches_focus(
    focus_tokens: &BTreeSet<String>,
    line: &str,
) -> bool {
    if focus_tokens.is_empty() {
        return true;
    }
    let line_tokens = crate::services::query::text_match::normalized_alnum_tokens(
        line,
        STRUCTURAL_COVERAGE_FOCUS_MIN_TOKEN_CHARS,
    );
    if line_tokens.is_empty() {
        return false;
    }
    crate::services::query::text_match::near_token_overlap_count(focus_tokens, &line_tokens) >= 1
}

pub(super) fn push_structural_anchor_literals(
    text: &str,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    for literal in structural_anchor_literal_candidates(text) {
        if out.len() >= STRUCTURAL_COVERAGE_MAX_ANCHORS {
            return;
        }
        let Some(anchor) = normalize_structural_coverage_anchor(&literal) else {
            continue;
        };
        if seen.insert(anchor.clone()) {
            out.push(anchor);
        }
    }
}

pub(super) fn structural_anchor_literal_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::<String>::new();
    candidates.extend(extract_delimited_spans(text, '`', '`'));
    candidates.extend(extract_delimited_spans(text, '"', '"'));
    candidates.extend(extract_delimited_spans(text, '\'', '\''));
    candidates.extend(extract_delimited_spans(text, '«', '»'));
    candidates.extend(extract_delimited_spans(text, '“', '”'));
    candidates.extend(extract_explicit_path_literals(text, 16));
    candidates.extend(extract_parameter_literals(text, 16));
    candidates.extend(extract_config_section_literals(text, 16));
    candidates
}

pub(super) fn extract_delimited_spans(text: &str, open: char, close: char) -> Vec<String> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if start.is_none() {
            if ch == open {
                start = Some(index + ch.len_utf8());
            }
            continue;
        }
        if ch == close
            && let Some(open_index) = start.take()
            && open_index <= index
        {
            let value = text[open_index..index].trim();
            if !value.is_empty() {
                spans.push(value.to_string());
            }
        }
    }
    spans
}

pub(super) fn normalize_structural_coverage_anchor(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.')
    });
    if trimmed.chars().count() > STRUCTURAL_COVERAGE_MAX_ANCHOR_CHARS
        || !trimmed.chars().any(char::is_alphanumeric)
        || structural_coverage_anchor_has_uri_syntax(trimmed)
    {
        return None;
    }
    let token_count = crate::services::query::text_match::normalized_alnum_tokens(trimmed, 1).len();
    if !(1..=8).contains(&token_count) {
        return None;
    }
    Some(trimmed.to_lowercase())
}

pub(super) fn structural_coverage_anchor_has_uri_syntax(value: &str) -> bool {
    if value.starts_with("//") {
        return true;
    }
    let Some((scheme, remainder)) = value.split_once(':') else {
        return false;
    };
    if remainder.is_empty()
        || remainder.chars().next().is_some_and(char::is_whitespace)
        || (scheme.chars().count() == 1
            && (remainder.starts_with('/') || remainder.starts_with('\\')))
    {
        return false;
    }
    let mut chars = scheme.chars();
    chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

pub(super) fn structural_answer_contains_anchor(
    answer_lower: &str,
    answer_tokens: &BTreeSet<String>,
    anchor: &str,
) -> bool {
    let anchor_tokens = crate::services::query::text_match::normalized_alnum_tokens(anchor, 1);
    if anchor_tokens.len() == 1
        && let Some(token) = anchor_tokens.iter().next()
    {
        return answer_tokens.contains(token);
    }
    answer_lower.contains(anchor)
}
