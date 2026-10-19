use super::*;

pub(super) fn verification_state_storage_label(state: QueryVerificationState) -> &'static str {
    match state {
        QueryVerificationState::NotRun => "not_run",
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
    }
}

pub(super) fn no_verifiable_tool_evidence_warnings() -> serde_json::Value {
    serde_json::to_value([QueryVerificationWarning {
        code: "no_verifiable_tool_evidence".to_string(),
        message: "The UI agent used MCP tools, but none returned evidence that can verify the final answer.".to_string(),
        related_segment_id: None,
        related_fact_id: None,
    }])
    .unwrap_or_else(|_| serde_json::json!([]))
}

pub(super) fn no_agent_tool_evidence_warnings() -> serde_json::Value {
    serde_json::to_value([QueryVerificationWarning {
        code: "no_agent_tool_evidence".to_string(),
        message:
            "The UI agent produced a final answer before collecting verifier-grade MCP evidence."
                .to_string(),
        related_segment_id: None,
        related_fact_id: None,
    }])
    .unwrap_or_else(|_| serde_json::json!([]))
}

pub(super) fn literal_inventory_revision_context(
    assistant_grounding: &AssistantGroundingEvidence,
) -> String {
    let mut sections = assistant_grounding.verification_corpus.clone();
    sections.extend(assistant_grounding.document_references.iter().map(|reference| {
        format!("Document reference: {}\n{}", reference.document_title, reference.excerpt)
    }));
    sections.join("\n\n")
}

pub(super) fn literal_inventory_coverage_revision_targets(
    answer: &str,
    history: &[ExternalConversationTurn],
    assistant_grounding: &AssistantGroundingEvidence,
) -> Vec<String> {
    const MIN_HISTORY_LITERALS: usize = 4;
    const MIN_PRESENT_LITERALS: usize = 2;
    const MAX_REVISION_TARGETS: usize = 16;

    let inventory = latest_dense_history_identifier_literals(history);
    if inventory.len() < MIN_HISTORY_LITERALS {
        return Vec::new();
    }
    let present = inventory
        .iter()
        .filter(|literal| text_contains_literal(answer, literal))
        .cloned()
        .collect::<Vec<_>>();
    if present.len() < MIN_PRESENT_LITERALS {
        return Vec::new();
    }
    inventory
        .into_iter()
        .filter(|literal| !text_contains_literal(answer, literal))
        .filter(|literal| {
            assistant_grounding_contains_literal_with_present_inventory(
                assistant_grounding,
                literal,
                &present,
                MIN_PRESENT_LITERALS,
            )
        })
        .take(MAX_REVISION_TARGETS)
        .collect()
}

pub(super) fn literal_revision_covers_required_literals(
    answer: &str,
    required_literals: &[String],
) -> bool {
    required_literals.iter().all(|literal| text_contains_literal(answer, literal))
}

pub(super) fn literal_revision_history_literal_coverage(
    draft_answer: &str,
    revised_answer: &str,
    history: &[ExternalConversationTurn],
) -> Option<(usize, usize, usize)> {
    const MIN_HISTORY_ANCHOR_LITERALS: usize = 4;
    const MIN_DRAFT_VISIBLE_LITERALS: usize = 2;

    let inventory = latest_dense_history_revision_anchor_literals(history);
    if inventory.len() < MIN_HISTORY_ANCHOR_LITERALS {
        return None;
    }
    let draft_visible = literal_visibility_count(draft_answer, &inventory);
    if draft_visible < MIN_DRAFT_VISIBLE_LITERALS {
        return None;
    }
    let revised_visible = literal_visibility_count(revised_answer, &inventory);
    Some((draft_visible, revised_visible, inventory.len()))
}

fn literal_visibility_count(answer: &str, literals: &[String]) -> usize {
    literals.iter().filter(|literal| text_contains_literal(answer, literal)).count()
}

fn latest_dense_history_revision_anchor_literals(
    history: &[ExternalConversationTurn],
) -> Vec<String> {
    let mut literals = history
        .iter()
        .rev()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .map(|turn| dense_history_literals(&turn.content_text))
        .unwrap_or_default()
        .into_iter()
        .filter(|literal| literal_text_is_revision_anchor_shaped(literal))
        .collect::<Vec<_>>();
    let mut seen = HashSet::<String>::new();
    literals.retain(|literal| seen.insert(literal.clone()));
    literals
}

fn literal_text_is_revision_anchor_shaped(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && !trimmed.chars().any(char::is_whitespace)
        && (literal_text_is_identifier_shaped(trimmed)
            || trimmed.chars().any(|ch| !ch.is_alphanumeric()))
}

pub(super) fn latest_dense_history_identifier_literals(
    history: &[ExternalConversationTurn],
) -> Vec<String> {
    let mut literals = history
        .iter()
        .rev()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .map(|turn| dense_history_literals(&turn.content_text))
        .unwrap_or_default()
        .into_iter()
        .filter(|literal| literal_text_is_identifier_shaped(literal))
        .collect::<Vec<_>>();
    let mut seen = HashSet::<String>::new();
    literals.retain(|literal| seen.insert(literal.clone()));
    literals
}

fn dense_history_literals(text: &str) -> Vec<String> {
    let literal_line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ir.memory.literals.v1:"))
        .unwrap_or("");
    extract_backtick_literals(literal_line)
}

fn extract_backtick_literals(text: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut search_from = 0usize;
    while let Some(start) = text[search_from..].find('`') {
        let abs_start = search_from + start + 1;
        if abs_start >= text.len() {
            break;
        }
        let Some(end) = text[abs_start..].find('`') else {
            break;
        };
        let literal = text[abs_start..abs_start + end].trim();
        if !literal.is_empty() && !literal.contains('\n') {
            literals.push(literal.to_string());
        }
        search_from = abs_start + end + 1;
    }
    literals
}

fn assistant_grounding_contains_literal_with_present_inventory(
    assistant_grounding: &AssistantGroundingEvidence,
    literal: &str,
    present_literals: &[String],
    required_present_count: usize,
) -> bool {
    assistant_grounding.verification_corpus.iter().any(|fragment| {
        grounding_fragment_supports_inventory_literal(
            fragment,
            literal,
            present_literals,
            required_present_count,
        )
    }) || assistant_grounding.document_references.iter().any(|reference| {
        let fragment = format!("{}\n{}", reference.document_title, reference.excerpt);
        grounding_fragment_supports_inventory_literal(
            &fragment,
            literal,
            present_literals,
            required_present_count,
        )
    })
}

fn grounding_fragment_supports_inventory_literal(
    fragment: &str,
    literal: &str,
    present_literals: &[String],
    required_present_count: usize,
) -> bool {
    text_contains_literal(fragment, literal)
        && present_literals
            .iter()
            .filter(|present| text_contains_literal(fragment, present))
            .take(required_present_count)
            .count()
            >= required_present_count
}

pub(super) fn text_contains_literal(text: &str, literal: &str) -> bool {
    if literal.is_empty() {
        return true;
    }
    let mut search_from = 0usize;
    while let Some(relative) = text[search_from..].find(literal) {
        let start = search_from + relative;
        let end = start + literal.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !literal_boundary_continues(before) && !literal_boundary_continues(after) {
            return true;
        }
        search_from = end;
    }
    false
}

fn literal_boundary_continues(ch: Option<char>) -> bool {
    ch.is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

pub(super) fn context_reference_set_has_grounding(
    reference_set: &KnowledgeContextBundleReferenceSetRow,
) -> bool {
    !reference_set.chunk_references.is_empty()
        || !reference_set.entity_references.is_empty()
        || !reference_set.relation_references.is_empty()
        || !reference_set.evidence_references.is_empty()
        || !reference_set.bundle.selected_fact_ids.is_empty()
}

pub(super) fn agent_has_verifiable_tool_evidence(
    child_query_execution_ids: &[Uuid],
    assistant_grounding: &AssistantGroundingEvidence,
) -> bool {
    !child_query_execution_ids.is_empty() || assistant_grounding.has_verifier_grade_evidence()
}

pub(super) fn agent_answer_requires_parent_tool_evidence_verification(
    has_verifiable_tool_evidence: bool,
) -> bool {
    has_verifiable_tool_evidence
}

pub(super) fn agent_answer_allows_parent_model_revision(provenance: AgentAnswerProvenance) -> bool {
    matches!(provenance, AgentAnswerProvenance::Composed)
}

pub(super) fn agent_verification_needs_literal_revision(
    verification: &RuntimeAnswerVerification,
) -> bool {
    verification.warnings.iter().any(|warning| {
        warning.code == "unsupported_literal" || warning.code == "unsupported_\u{63}anonical_claim"
    })
}

pub(super) fn ui_agent_iteration_cap() -> usize {
    usize::from(QueryAnswerTask::spec().max_turns)
        .saturating_add(2)
        .max(ASSISTANT_AGENT_LOOP_MIN_ITERATIONS)
}
