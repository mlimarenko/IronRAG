use super::literal_revision::{ANSWER_CANDIDATE_KIND_DOCUMENT, CLARIFICATION_LABEL_MAX_CHARS};
use super::{
    AppState, AssistantGroundingEvidence, CLARIFY_MAX_VARIANTS, HashSet, QueryAnswerCandidate,
    QueryAnswerCandidateProvenance, QueryAnswerDisposition, QueryClarification, QueryIR,
    QueryLanguage, QueryVerificationPersistence, QueryVerificationState, QueryVerificationWarning,
    RuntimeAnswerVerification, Uuid, deterministic_query_messages,
};

/// Collapse candidate labels to one bounded display line before placing them
/// in either response prose or typed metadata. Hidden directionality/control
/// characters and embedded newlines must not create additional instructions.
pub(super) fn sanitized_clarification_label(value: &str) -> Option<String> {
    let mut label = String::new();
    let mut char_count = 0usize;
    let mut pending_space = false;
    let mut truncated = false;

    for ch in value.chars() {
        if ch.is_whitespace() || ch.is_control() || clarification_format_char_is_unsafe(ch) {
            pending_space |= !label.is_empty();
            continue;
        }
        if pending_space {
            if char_count >= CLARIFICATION_LABEL_MAX_CHARS.saturating_sub(1) {
                truncated = true;
                break;
            }
            label.push(' ');
            char_count += 1;
            pending_space = false;
        }
        if char_count >= CLARIFICATION_LABEL_MAX_CHARS.saturating_sub(1) {
            truncated = true;
            break;
        }
        label.push(ch);
        char_count += 1;
    }
    if label.is_empty() {
        return None;
    }
    if truncated {
        label.push('…');
    }
    Some(label)
}

pub(super) fn clarification_format_char_is_unsafe(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
    )
}

/// Build a [`QueryClarification`] for the disposition-router clarify
/// branches. Those branches only have human-readable label strings (document
/// titles / graph node labels / grouped-reference labels), so each candidate
/// is `kind = "document"` with no provenance id and no confidence. This is a
/// serialization of the `variants` the branch already computed — no new
/// retrieval.
pub(super) fn disposition_clarification(question: &str, variants: &[String]) -> QueryClarification {
    let mut seen = HashSet::new();
    QueryClarification {
        required: true,
        question: Some(question.to_string()),
        answer_candidates: variants
            .iter()
            .filter_map(|label| sanitized_clarification_label(label))
            .filter(|label| seen.insert(label.to_lowercase()))
            .take(CLARIFY_MAX_VARIANTS)
            .map(|label| QueryAnswerCandidate {
                label,
                kind: ANSWER_CANDIDATE_KIND_DOCUMENT.to_string(),
                confidence: None,
                provenance: QueryAnswerCandidateProvenance::default(),
            })
            .collect(),
    }
}

pub(super) fn render_typed_clarification_answer(
    language: QueryLanguage,
    clarification: &QueryClarification,
) -> String {
    let mut answer = clarification.question.as_deref().unwrap_or_default().trim().to_string();
    if clarification.answer_candidates.is_empty() {
        return answer;
    }
    if !answer.is_empty() {
        answer.push_str("\n\n");
    }
    answer.push_str(deterministic_query_messages(language).options_heading);
    for (index, candidate) in clarification.answer_candidates.iter().enumerate() {
        let quoted_label = serde_json::to_string(&candidate.label)
            .unwrap_or_else(|_| "\"invalid option label\"".to_string());
        answer.push('\n');
        answer.push_str(&(index + 1).to_string());
        answer.push_str(". ");
        answer.push_str(&quoted_label);
    }
    answer
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ClarificationPromptKind {
    Source,
}

pub(super) fn deterministic_clarification_question(
    language: QueryLanguage,
    kind: ClarificationPromptKind,
) -> &'static str {
    let messages = deterministic_query_messages(language);
    match kind {
        ClarificationPromptKind::Source => messages.clarify_source,
    }
}

pub(super) fn clarification_not_answer_verification() -> RuntimeAnswerVerification {
    RuntimeAnswerVerification {
        state: QueryVerificationState::NotRun,
        warnings: vec![QueryVerificationWarning {
            code: "clarification_not_answer".to_string(),
            message: "Verification was not run because this response is a typed clarification, not an answer."
                .to_string(),
            related_segment_id: None,
            related_fact_id: None,
        }],
        unsupported_literals: Vec::new(),
    }
}

pub(super) async fn finalize_typed_clarification_verification(
    state: &AppState,
    execution_id: Uuid,
    answer: &str,
    query_ir: &QueryIR,
    clarification: &QueryClarification,
) -> anyhow::Result<()> {
    let verification = clarification_not_answer_verification();
    let finalized = super::super::verification_policy::finalize_answer_visibility(
        query_ir.verification_level(),
        verification.state,
        &verification.warnings,
        query_ir.language,
        answer,
        super::super::verification_policy::AnswerVisibilityKind::Clarification,
    );
    anyhow::ensure!(
        matches!(finalized.disposition, QueryAnswerDisposition::Clarification),
        "typed clarification finalizer rejected its public answer body"
    );
    super::super::persist_query_verification(QueryVerificationPersistence {
        state,
        execution_id,
        verification: &verification,
        answer_disposition: finalized.disposition,
        clarification,
        authoritative_evidence: &super::super::AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        assistant_grounding: &AssistantGroundingEvidence::default(),
        answer_kind: None,
    })
    .await
}
