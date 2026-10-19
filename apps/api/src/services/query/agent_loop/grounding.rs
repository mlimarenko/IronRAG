use super::*;

pub(super) fn agent_tool_limit_cap(tool_name: &str) -> Option<usize> {
    match tool_name {
        SEARCH_DOCUMENTS_TOOL_NAME | "search_entities" => Some(12),
        "list_relations" | "get_communities" => Some(16),
        "get_graph_topology" => Some(24),
        _ => None,
    }
}

pub(super) fn push_tool_grounding_fragment(
    grounding: &mut AssistantGroundingEvidence,
    tool_name: &str,
    message_content: &str,
) {
    let trimmed = message_content.trim();
    if trimmed.is_empty() {
        return;
    }
    let existing_chars = grounding
        .verification_corpus
        .iter()
        .map(|fragment| fragment.chars().count())
        .sum::<usize>();
    if existing_chars >= TOOL_GROUNDING_TOTAL_CHAR_LIMIT {
        return;
    }
    let remaining = TOOL_GROUNDING_TOTAL_CHAR_LIMIT - existing_chars;
    let fragment_limit = TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT.min(remaining);
    let fragment = trimmed.chars().take(fragment_limit).collect::<String>();
    grounding.verification_corpus.push(format!("[MCP tool result: {tool_name}]\n{fragment}"));
}

pub(super) fn compact_ledger_text(value: &str) -> String {
    single_line_text(value).chars().take(GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS).collect()
}

pub(super) fn push_guard_answer_line(
    lines: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    value: &str,
) {
    if lines.len() >= GROUNDED_EVIDENCE_LEDGER_ENTRY_LIMIT {
        return;
    }
    let text = value.trim();
    if text.is_empty() || !text.chars().any(char::is_alphanumeric) {
        return;
    }
    if seen.insert(text.to_string()) {
        lines.push(text.to_string());
    }
}

pub(super) fn push_grounded_evidence_ledger_anchors(
    anchors: &mut BTreeSet<String>,
    values: &[String],
) {
    for value in values {
        let anchor = single_line_text(value);
        let anchor = anchor.trim();
        if anchor.chars().count() >= 4 && anchor.chars().count() <= 240 {
            anchors.insert(anchor.to_string());
        }
        if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
            break;
        }
    }
}

pub(super) fn push_grounded_evidence_ledger_high_signal_anchors(
    anchors: &mut BTreeSet<String>,
    values: &[String],
    unsupported_literals: &BTreeSet<String>,
) {
    for value in values {
        let anchor = single_line_text(value);
        let anchor = anchor.trim();
        if unsupported_literals.contains(anchor) {
            continue;
        }
        if is_high_signal_grounded_answer_anchor(anchor) {
            anchors.insert(anchor.to_string());
        }
        if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
            break;
        }
    }
}

pub(super) fn is_high_signal_grounded_answer_anchor(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.chars().count() < 4 || trimmed.chars().count() > 240 {
        return false;
    }
    trimmed.contains('=')
        || trimmed.contains('/')
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || literal_text_is_identifier_shaped(trimmed)
}

pub(super) fn answer_contains_guard_anchor(answer: &str, anchor: &str) -> bool {
    answer.to_lowercase().contains(&anchor.to_lowercase())
}

pub(super) fn collect_unsupported_literal_spans(
    execution_detail: Option<&Value>,
    structured_content: Option<&Value>,
) -> BTreeSet<String> {
    let mut literals = BTreeSet::new();
    let warnings = execution_detail
        .and_then(|detail| detail.get("verificationWarnings"))
        .and_then(Value::as_array)
        .or_else(|| {
            structured_content
                .and_then(|structured| structured.get("warnings"))
                .and_then(Value::as_array)
        });
    let Some(warnings) = warnings else {
        return literals;
    };
    for warning in warnings {
        if warning.get("code").and_then(Value::as_str) != Some("unsupported_literal") {
            continue;
        }
        let Some(message) = warning.get("message").and_then(Value::as_str) else {
            continue;
        };
        for span in backtick_literal_spans(message) {
            let literal = single_line_text(&span);
            if !literal.trim().is_empty() {
                literals.insert(literal);
            }
        }
    }
    literals
}

pub(super) fn single_line_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn push_json_string_array(
    values: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    value: Option<&Value>,
    limit: usize,
) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for item in items {
        let Some(text) = item.as_str() else {
            continue;
        };
        push_ledger_string(values, seen, text, limit);
        if values.len() >= limit {
            break;
        }
    }
}

pub(super) fn push_reference_field_values(
    values: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    execution_detail: Option<&Value>,
    array_pointer: &str,
    field_name: &str,
    limit: usize,
) {
    let Some(Value::Array(items)) =
        execution_detail.and_then(|detail| detail.pointer(array_pointer))
    else {
        return;
    };
    for item in items {
        let Some(text) = item.get(field_name).and_then(Value::as_str) else {
            continue;
        };
        push_ledger_string(values, seen, text, limit);
        if values.len() >= limit {
            break;
        }
    }
}

pub(super) fn push_ledger_string(
    values: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    value: &str,
    limit: usize,
) {
    if values.len() >= limit {
        return;
    }
    let text = single_line_text(value);
    let text = text.trim();
    if text.is_empty() || !text.chars().any(|ch| ch.is_alphanumeric()) {
        return;
    }
    let text = text.chars().take(GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS).collect::<String>();
    if seen.insert(text.clone()) {
        values.push(text);
    }
}

pub(super) fn collect_verification_warning_codes(
    execution_detail: Option<&Value>,
    structured_content: Option<&Value>,
) -> Vec<String> {
    let mut codes = Vec::new();
    let mut seen = BTreeSet::new();
    let warnings = execution_detail
        .and_then(|detail| detail.get("verificationWarnings"))
        .or_else(|| structured_content.and_then(|structured| structured.get("warnings")));
    let Some(Value::Array(warnings)) = warnings else {
        return codes;
    };
    for warning in warnings {
        let Some(code) = warning.get("code").and_then(Value::as_str) else {
            continue;
        };
        push_ledger_string(&mut codes, &mut seen, code, 12);
    }
    codes
}

pub(super) fn tool_execution_error(message: impl Into<String>) -> ToolExecutionOutcome {
    tool_execution_error_with_kind("agent_tool_call", message, false)
}

pub(super) fn invalid_mcp_tool_call_outcome(message: impl Into<String>) -> ToolExecutionOutcome {
    tool_execution_error_with_kind("invalid_mcp_tool_call", message, false)
}

fn tool_execution_error_with_kind(
    error_kind: &'static str,
    message: impl Into<String>,
    retryable: bool,
) -> ToolExecutionOutcome {
    let message = message.into();
    let result_json = serde_json::json!({
        "content": [{
            "type": "text",
            "text": message.clone()
        }],
        "structuredContent": {
            "errorKind": error_kind,
            "message": message.clone(),
            "retryable": retryable
        },
        "isError": true
    });
    ToolExecutionOutcome {
        arguments_json: None,
        requested_arguments_json: None,
        message_content: serde_json::to_string(&result_json).unwrap_or_else(|_| {
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": "failed to serialize agent tool error"
                }],
                "structuredContent": {
                    "errorKind": "serialization"
                },
                "isError": true
            })
            .to_string()
        }),
        result_text: result_json
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        result_json: Some(result_json),
        grounding_text: None,
        grounded_answer_body: None,
        grounded_answer_outcome: None,
        grounded_answer_ready: false,
        grounded_answer_completed: false,
        grounded_answer_needs_follow_up: false,
        grounded_answer_repair_reason: None,
        grounded_answer_language: QueryLanguage::Auto,
        grounded_answer_clarification_required: false,
        is_error: true,
        is_replay: false,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    }
}

pub(super) fn deadline_remaining(started: Instant, deadline: Duration) -> Option<Duration> {
    deadline.checked_sub(started.elapsed()).filter(|remaining| !remaining.is_zero())
}

/// Bound a single tool-call's wait by the smaller of the remaining turn
/// deadline and a per-tool-call max. The max is the caller-supplied soft
/// tool-collection target (the configured per-tool-call SLO threaded into the
/// turn) when present, otherwise [`PER_TOOL_CALL_MAX_WAIT`]; either way it is
/// itself clamped to `PER_TOOL_CALL_MAX_WAIT` so a misconfigured soft target
/// can never let one call run for the whole turn.
pub(super) fn per_tool_call_wait(
    remaining_turn_deadline: Duration,
    soft_final_answer_deadline: Option<Duration>,
) -> Duration {
    let per_call_max =
        soft_final_answer_deadline.unwrap_or(PER_TOOL_CALL_MAX_WAIT).min(PER_TOOL_CALL_MAX_WAIT);
    remaining_turn_deadline.min(per_call_max)
}

pub(super) fn per_tool_call_wait_for_tool(
    tool_name: &str,
    remaining_turn_deadline: Duration,
    soft_final_answer_deadline: Option<Duration>,
    focused_grounded_follow_up: bool,
) -> Duration {
    if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        if focused_grounded_follow_up {
            return remaining_turn_deadline.min(GROUNDED_ANSWER_REPAIR_MAX_WAIT);
        }
        return remaining_turn_deadline;
    }
    per_tool_call_wait(remaining_turn_deadline, soft_final_answer_deadline)
}

/// Run one tool-execution future under a per-call wait. If the future does not
/// resolve within `wait`, produce a structured error outcome (`is_error =
/// true`) stating the call timed out, so the loop classifies it as a failed
/// call: the dedup fingerprint is cleared on the error path, no `is_replay`
/// flag is set, and the no-progress guard counts it as no progress.
pub(super) async fn run_tool_call_within_budget(
    fut: impl std::future::Future<Output = ToolExecutionOutcome>,
    wait: Duration,
    tool_name: &str,
    iteration: usize,
) -> ToolExecutionOutcome {
    let call_started = Instant::now();
    let provider_call = crate::integrations::retry::with_provider_retry_execution(wait, fut);
    match tokio::time::timeout(wait, provider_call).await {
        Ok(outcome) => outcome,
        Err(_) => {
            let waited_ms = call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            tracing::warn!(
                tool_name,
                iteration,
                waited_ms,
                "MCP-backed assistant agent tool call timed out before producing a result"
            );
            tool_execution_error_with_kind(
                "agent_tool_timeout",
                format!(
                    "tool '{tool_name}' timed out after {waited_ms} ms before producing a result"
                ),
                true,
            )
        }
    }
}
