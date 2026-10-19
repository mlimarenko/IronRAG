use super::*;

pub(super) fn debug_tool_result_json(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Value {
    let full = serde_json::to_value(result).unwrap_or_else(|error| {
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("failed to serialize MCP tool result: {error}")
            }],
            "structuredContent": {
                "errorKind": "serialization",
                "message": error.to_string()
            },
            "isError": true
        })
    });
    let serialized = match serde_json::to_string(&full) {
        Ok(serialized) => serialized,
        Err(_) => return full,
    };
    if serialized.chars().count() <= TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT {
        return full;
    }
    serde_json::json!({
        "content": serde_json::to_value(&result.content).unwrap_or_else(|_| serde_json::json!([])),
        "structuredContent": {
            "truncated": true,
            "clarification": result
                .structured_content
                .get("clarification")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"required": false})),
            "jsonPrefix": compact_json_string(
                &result.structured_content,
                TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT
            ),
            "originalCharCount": serialized.chars().count()
        },
        "isError": result.is_error
    })
}

pub(super) fn compact_json_string(value: &Value, char_limit: usize) -> String {
    let serialized = match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(error) => {
            return serde_json::json!({
                "truncated": true,
                "errorKind": "serialization",
                "message": error.to_string()
            })
            .to_string();
        }
    };
    if serialized.chars().count() <= char_limit {
        serialized
    } else {
        serde_json::json!({
            "truncated": true,
            "jsonPrefix": serialized.chars().take(char_limit).collect::<String>(),
            "originalCharCount": serialized.chars().count()
        })
        .to_string()
    }
}

pub(super) fn tool_result_model_message(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> String {
    let content_text =
        tool_result_preview_with_limit(&result.content, tool_model_content_char_limit(tool_name))
            .unwrap_or_default();
    let structured_content = if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        compact_grounded_answer_structured_content_for_model(
            &result.structured_content,
            TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
        )
    } else {
        compact_structured_content_for_model(&result.structured_content)
    };
    serde_json::to_string(&serde_json::json!({
        "content": [{
            "type": "text",
            "text": content_text
        }],
        "structuredContent": structured_content,
        "isError": result.is_error
    }))
    .unwrap_or_else(|error| {
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("failed to serialize compact tool result: {error}")
            }],
            "structuredContent": {
                "errorKind": "serialization",
                "message": error.to_string()
            },
            "isError": true
        })
        .to_string()
    })
}

pub(super) fn tool_result_verification_text(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<String> {
    if result.is_error || !tool_result_can_ground_final_answer(tool_name) {
        return None;
    }
    let content_text =
        tool_result_preview_with_limit(&result.content, TOOL_VERIFICATION_CONTENT_CHAR_LIMIT)
            .unwrap_or_default();
    let structured_content = if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        compact_grounded_answer_structured_content_for_verification(
            &result.structured_content,
            TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT,
        )
    } else {
        compact_structured_content_for_verification(&result.structured_content)
    };
    let structured_text = serde_json::to_string_pretty(&structured_content)
        .unwrap_or_else(|error| format!("failed to serialize compact structured content: {error}"));
    let mut sections = Vec::new();
    if !content_text.trim().is_empty() {
        sections.push(format!("content:\n{}", content_text.trim()));
    }
    if structured_content != Value::Null && structured_content != serde_json::json!({}) {
        sections.push(format!("structuredContent:\n{structured_text}"));
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

pub(super) fn tool_result_can_ground_final_answer(tool_name: &str) -> bool {
    matches!(tool_name, GROUNDED_ANSWER_TOOL_NAME)
        || is_document_content_tool(tool_name)
        || is_graph_content_tool(tool_name)
        || is_runtime_content_tool(tool_name)
}

pub(super) fn tool_model_content_char_limit(tool_name: &str) -> usize {
    match tool_name {
        GROUNDED_ANSWER_TOOL_NAME => TOOL_MODEL_GROUNDED_ANSWER_CONTENT_CHAR_LIMIT,
        READ_DOCUMENT_TOOL_NAME => TOOL_MODEL_READ_DOCUMENT_CONTENT_CHAR_LIMIT,
        _ => TOOL_MODEL_DEFAULT_CONTENT_CHAR_LIMIT,
    }
}

pub(super) fn grounded_answer_ready(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    grounded_answer_completion_envelope(result).is_some_and(|envelope| {
        envelope.completion.complete && grounded_answer_has_final_candidate(tool_name, result)
    })
}

pub(super) fn grounded_answer_has_final_candidate(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    let Some(envelope) = grounded_answer_completion_envelope(result) else {
        return false;
    };
    if !envelope.final_answer_ready || !envelope.finalizable {
        return false;
    }
    if result
        .structured_content
        .get("answerBody")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        return false;
    }
    if envelope.readiness.lifecycle_state != GROUNDED_ANSWER_LIFECYCLE_COMPLETED {
        return false;
    }
    true
}

pub(super) fn grounded_answer_ready_for_question(
    tool_name: &str,
    user_question: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    grounded_answer_ready(tool_name, result)
        && grounded_answer_repair_reason(tool_name, user_question, result).is_none()
}

pub(super) fn grounded_answer_repair_reason(
    tool_name: &str,
    _user_question: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<GroundedAnswerRepairReason> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return None;
    }
    let Some(envelope) = grounded_answer_completion_envelope(result) else {
        return Some(GroundedAnswerRepairReason::VerificationIncomplete);
    };
    if matches!(
        grounded_answer_typed_outcome(tool_name, result).map(|outcome| outcome.disposition),
        Some(QueryAnswerDisposition::SafeFallback | QueryAnswerDisposition::Clarification)
    ) {
        return None;
    }
    if let Some(reason) = completion_assessment_repair_reason(&envelope.completion) {
        return Some(reason);
    }
    if grounded_answer_ready(tool_name, result)
        || envelope.readiness.lifecycle_state != GROUNDED_ANSWER_LIFECYCLE_COMPLETED
    {
        return None;
    }
    Some(GroundedAnswerRepairReason::VerificationIncomplete)
}

pub(super) fn grounded_answer_query_language(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> QueryLanguage {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return QueryLanguage::Auto;
    }
    result
        .structured_content
        .get("queryLanguage")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(QueryLanguage::Auto)
}

pub(super) fn grounded_answer_completion_envelope(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<GroundedAnswerCompletionEnvelope> {
    // An error result carries an error payload, not a completion envelope;
    // there is nothing to parse and nothing worth warning about.
    if result.is_error {
        return None;
    }
    parse_grounded_answer_completion_envelope(&result.structured_content)
}

/// Parse where a completion envelope is REQUIRED: a non-error grounded
/// result without a valid envelope is a producer defect, so the rejection
/// is logged before the consumer fails closed.
pub(super) fn parse_grounded_answer_completion_envelope(
    structured_content: &Value,
) -> Option<GroundedAnswerCompletionEnvelope> {
    match GroundedAnswerCompletionEnvelope::from_structured_content(structured_content) {
        Ok(envelope) => Some(envelope),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "grounded-answer completion envelope rejected; consumer is failing closed"
            );
            None
        }
    }
}

/// Parse where the payload may legitimately not be an envelope (failed
/// calls, compaction of arbitrary grounded JSON): absence is expected and
/// silent.
pub(super) fn try_parse_grounded_answer_completion_envelope(
    structured_content: &Value,
) -> Option<GroundedAnswerCompletionEnvelope> {
    GroundedAnswerCompletionEnvelope::from_structured_content(structured_content).ok()
}

pub(super) fn grounded_answer_typed_outcome(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<AgentGroundedAnswerOutcome> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return None;
    }
    let envelope = grounded_answer_completion_envelope(result)?;
    if envelope.readiness.lifecycle_state != GROUNDED_ANSWER_LIFECYCLE_COMPLETED
        || result
            .structured_content
            .get("answerBody")
            .and_then(Value::as_str)
            .is_none_or(|answer| answer.trim().is_empty())
    {
        return None;
    }
    let disposition = match envelope.readiness.answer_disposition {
        ironrag_contracts::assistant::AssistantAnswerDisposition::NonTerminal => return None,
        ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady => {
            QueryAnswerDisposition::FactualReady
        }
        ironrag_contracts::assistant::AssistantAnswerDisposition::SafeFallback => {
            QueryAnswerDisposition::SafeFallback
        }
        ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification => {
            QueryAnswerDisposition::Clarification
        }
    };
    let clarification = grounded_answer_typed_clarification(
        &result.structured_content,
        matches!(disposition, QueryAnswerDisposition::Clarification),
    )?;
    Some(AgentGroundedAnswerOutcome { disposition, clarification })
}

pub(super) fn grounded_answer_typed_clarification(
    structured_content: &Value,
    clarification_expected: bool,
) -> Option<QueryClarification> {
    let clarification = structured_content.get("clarification")?.as_object()?;
    let required = clarification.get("required")?.as_bool()?;
    if required != clarification_expected {
        return None;
    }
    let question = match clarification.get("question") {
        None | Some(Value::Null) => None,
        Some(Value::String(question)) if !question.trim().is_empty() => Some(question.clone()),
        Some(_) => return None,
    };
    if !required {
        return question.is_none().then(QueryClarification::default);
    }

    let candidates = match structured_content.get("responseProfile").and_then(Value::as_str) {
        Some("compact") => {
            let summary = structured_content.get("answerCandidateSummary")?.as_object()?;
            if summary.get("truncated")?.as_bool()? {
                return None;
            }
            let total_count = summary.get("totalCount")?.as_u64()?;
            let returned_count = summary.get("returnedCount")?.as_u64()?;
            let candidates = summary.get("candidates")?.clone();
            if total_count != returned_count
                || returned_count != candidates.as_array()?.len() as u64
            {
                return None;
            }
            candidates
        }
        Some("full") => structured_content.get("answerCandidates")?.clone(),
        _ => return None,
    };
    let answer_candidates = serde_json::from_value::<Vec<QueryAnswerCandidate>>(candidates).ok()?;
    Some(QueryClarification { required, question, answer_candidates })
}

pub(super) fn completion_assessment_repair_reason(
    assessment: &AnswerCompletionAssessment,
) -> Option<GroundedAnswerRepairReason> {
    match assessment.reason? {
        AnswerCompletionGapReason::OrderedInventory => {
            Some(GroundedAnswerRepairReason::OrderedInventory {
                expected: assessment.expected.unwrap_or(1),
                observed: assessment.observed.unwrap_or(0),
            })
        }
        AnswerCompletionGapReason::Procedure => {
            Some(GroundedAnswerRepairReason::ProcedureIncomplete)
        }
        AnswerCompletionGapReason::Troubleshooting => {
            Some(GroundedAnswerRepairReason::TroubleshootingIncomplete)
        }
        AnswerCompletionGapReason::AnswerStructure => {
            Some(GroundedAnswerRepairReason::AnswerStructureIncomplete)
        }
    }
}

pub(super) fn grounded_answer_verbatim_body(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return None;
    }
    result
        .structured_content
        .get("answerBody")
        .and_then(Value::as_str)
        .filter(|answer| !answer.trim().is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn answer_ordered_item_count(answer: &str) -> usize {
    answer.lines().filter(|line| ordered_item_body(line).is_some()).count()
}

pub(super) fn ordered_item_body(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(body) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("• "))
    {
        return body.chars().any(char::is_alphanumeric).then_some(body);
    }
    let digit_chars = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digit_chars == 0 {
        return None;
    }
    let after_digits = &trimmed[digit_chars..];
    let body = after_digits
        .strip_prefix('.')
        .or_else(|| after_digits.strip_prefix(')'))
        .or_else(|| after_digits.strip_prefix(':'))?
        .trim_start();
    body.chars().any(char::is_alphanumeric).then_some(body)
}

pub(super) fn grounded_answer_needs_follow_up(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    let Some(envelope) = grounded_answer_completion_envelope(result) else {
        return true;
    };
    if matches!(
        envelope.readiness.answer_disposition,
        ironrag_contracts::assistant::AssistantAnswerDisposition::SafeFallback
            | ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification
    ) {
        return grounded_answer_typed_outcome(tool_name, result).is_none();
    }
    if grounded_answer_ready(tool_name, result) {
        return false;
    }
    if envelope.readiness.lifecycle_state != GROUNDED_ANSWER_LIFECYCLE_COMPLETED {
        return false;
    }
    envelope.repair_policy.required
}

pub(super) fn grounded_answer_completed(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    tool_name == GROUNDED_ANSWER_TOOL_NAME
        && !result.is_error
        && grounded_answer_lifecycle_completed(result)
}

pub(super) fn grounded_answer_lifecycle_completed(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    grounded_answer_completion_envelope(result).is_some_and(|envelope| {
        envelope.readiness.lifecycle_state == GROUNDED_ANSWER_LIFECYCLE_COMPLETED
    })
}

pub(super) fn tool_result_full_text(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
) -> Option<String> {
    let joined = content
        .iter()
        .map(|block| block.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!joined.is_empty()).then_some(joined)
}

pub(super) fn tool_result_answer_text(
    tool_name: &str,
    content: &[crate::interfaces::http::mcp::McpContentBlock],
) -> Option<String> {
    if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        tool_result_full_text(content)
    } else {
        tool_result_preview(content)
    }
}

/// The structured `answerBody` is the only valid answer text. The raw tool
/// `result_text` also carries the rendered evidence blocks, so falling back
/// to it serves source dumps as an "answer" exactly when the completion
/// envelope is missing or malformed — fail closed instead.
pub(super) fn grounded_answer_body_text(outcome: &ToolExecutionOutcome) -> Option<&str> {
    outcome
        .grounded_answer_body
        .as_deref()
        .or_else(|| {
            outcome
                .result_json
                .as_ref()
                .and_then(|json| json.pointer("/structuredContent/answerBody"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

pub(super) fn remember_verified_grounded_answer(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || !outcome.grounded_answer_ready {
        return current;
    }
    grounded_answer_body_text(outcome).map(ToOwned::to_owned).or(current)
}

pub(super) fn remember_verified_grounded_answer_guard_text(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || !outcome.grounded_answer_ready {
        return current;
    }
    let Some(answer) = grounded_answer_body_text(outcome) else {
        return current;
    };
    match current {
        Some(existing) if existing.contains(answer) => Some(existing),
        Some(mut existing) => {
            existing.push_str("\n\n");
            existing.push_str(answer);
            Some(existing)
        }
        None => Some(answer.to_string()),
    }
}

pub(super) fn remember_completed_grounded_answer(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || !outcome.grounded_answer_completed {
        return current;
    }
    let factual_disposition = outcome
        .result_json
        .as_ref()
        .and_then(|json| json.get("structuredContent"))
        .and_then(parse_grounded_answer_completion_envelope)
        .is_some_and(|envelope| {
            matches!(
                envelope.readiness.answer_disposition,
                ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
            )
        });
    if !factual_disposition {
        return current;
    }
    grounded_answer_body_text(outcome).map(ToOwned::to_owned).or(current)
}

pub(super) fn remember_completed_nonterminal_grounded_answer(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || !outcome.grounded_answer_completed
        || outcome.grounded_answer_ready
        || !outcome.grounded_answer_needs_follow_up
    {
        return current;
    }
    grounded_answer_body_text(outcome).map(ToOwned::to_owned).or(current)
}

pub(super) fn remember_verified_partial_grounded_answer(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || !outcome.grounded_answer_completed
        || outcome.grounded_answer_ready
        || !matches!(
            outcome.grounded_answer_repair_reason,
            Some(
                GroundedAnswerRepairReason::ProcedureIncomplete
                    | GroundedAnswerRepairReason::TroubleshootingIncomplete
                    | GroundedAnswerRepairReason::AnswerStructureIncomplete
                    | GroundedAnswerRepairReason::OrderedInventory { .. }
            )
        )
    {
        return current;
    }
    let Some(completion_envelope) = outcome
        .result_json
        .as_ref()
        .and_then(|json| json.get("structuredContent"))
        .and_then(parse_grounded_answer_completion_envelope)
    else {
        return current;
    };
    if completion_envelope.completion.complete {
        return current;
    }
    if !matches!(
        completion_envelope.readiness.answer_disposition,
        ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
    ) {
        return current;
    }
    grounded_answer_body_text(outcome).map(ToOwned::to_owned).or(current)
}

pub(super) fn explicitly_mark_partial_grounded_answer(
    answer: &str,
    language: QueryLanguage,
) -> String {
    let notice = grounded_repair_messages(language).partial_answer_notice;
    format!("{notice}\n\n{}", answer.trim())
}

pub(super) fn explicitly_mark_unverified_completed_grounded_answer(
    answer: &str,
    language: QueryLanguage,
) -> String {
    let notice = grounded_repair_messages(language).unverified_answer_notice;
    crate::integrations::retry::record_degraded_completion();
    let answer = answer.trim();
    if answer.is_empty() {
        return notice.to_string();
    }
    format!("{answer}\n\n{notice}")
}

pub(super) fn completed_grounded_answer_after_failed_focused_follow_up(
    incomplete_grounded_answer_needs_follow_up: bool,
    focused_grounded_follow_up_attempted: bool,
    focused_grounded_follow_up_unresolved: bool,
    last_completed_grounded_answer: Option<&str>,
) -> Option<&str> {
    (incomplete_grounded_answer_needs_follow_up
        && focused_grounded_follow_up_attempted
        && focused_grounded_follow_up_unresolved)
        .then_some(last_completed_grounded_answer)
        .flatten()
        .filter(|answer| !answer.trim().is_empty())
}

pub(super) fn should_return_completed_grounded_answer_on_iteration_cap(
    stopped_reason: AgentStopReason,
    successful_tool_call_count: usize,
    last_completed_grounded_answer: Option<&str>,
) -> bool {
    matches!(stopped_reason, AgentStopReason::IterationCap)
        && successful_tool_call_count == 1
        && last_completed_grounded_answer.is_some_and(|answer| !answer.trim().is_empty())
}

/// Select an iteration-cap answer exclusively from grounded results produced
/// during the current turn. Conversation history can shape follow-up query
/// compilation, but a prior assistant/system message is never itself a valid
/// answer for a new turn.
/// Inputs for [`current_turn_grounded_answer_on_iteration_cap`]: the stop
/// reason reached, whether a grounded follow-up is still required, and the
/// candidate answers accumulated during this turn.
pub(super) struct IterationCapAnswerInput<'a> {
    pub(super) stopped_reason: AgentStopReason,
    pub(super) incomplete_grounded_answer_needs_follow_up: bool,
    pub(super) focused_grounded_follow_up_attempted: bool,
    pub(super) successful_tool_call_count: usize,
    pub(super) verified_grounded_answer_count: usize,
    pub(super) last_verified_grounded_answer: Option<&'a str>,
    pub(super) last_completed_grounded_answer: Option<&'a str>,
}

pub(super) fn current_turn_grounded_answer_on_iteration_cap<'a>(
    input: IterationCapAnswerInput<'a>,
) -> Option<&'a str> {
    let IterationCapAnswerInput {
        stopped_reason,
        incomplete_grounded_answer_needs_follow_up,
        focused_grounded_follow_up_attempted,
        successful_tool_call_count,
        verified_grounded_answer_count,
        last_verified_grounded_answer,
        last_completed_grounded_answer,
    } = input;
    if incomplete_grounded_answer_needs_follow_up
        || !matches!(stopped_reason, AgentStopReason::IterationCap)
    {
        return None;
    }

    if let Some(answer) = verified_grounded_answer_fallback_candidate(
        last_verified_grounded_answer,
        verified_grounded_answer_count,
        successful_tool_call_count,
    )
    .filter(|answer| !answer.trim().is_empty())
    {
        return Some(answer);
    }

    if focused_grounded_follow_up_attempted
        && let Some(answer) =
            last_verified_grounded_answer.filter(|answer| !answer.trim().is_empty())
    {
        return Some(answer);
    }

    should_return_completed_grounded_answer_on_iteration_cap(
        stopped_reason,
        successful_tool_call_count,
        last_completed_grounded_answer,
    )
    .then_some(last_completed_grounded_answer)
    .flatten()
}
