use super::*;

pub(super) fn compact_agent_prompt_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    messages.iter().filter(|message| !history_message_is_failed_payload(message)).cloned().collect()
}

pub(super) fn compact_grounded_answer_tool_history(
    turns: &[ExternalConversationTurn],
) -> Vec<ExternalConversationTurn> {
    let max_chars = crate::services::query::service::MAX_GROUNDED_ANSWER_TOOL_HISTORY_CHARS;
    turns
        .iter()
        .filter_map(|turn| {
            if matches!(turn.turn_kind, QueryTurnKind::Assistant)
                && history_text_is_failed_payload(&turn.content_text)
            {
                return None;
            }
            let content_text = bounded_history_text(&turn.content_text, max_chars);
            (!content_text.is_empty()).then(|| ExternalConversationTurn {
                turn_kind: turn.turn_kind.clone(),
                content_text,
            })
        })
        .collect()
}

fn history_message_is_failed_payload(message: &ChatMessage) -> bool {
    matches!(message.role.as_str(), "assistant" | "tool")
        && message.content.as_deref().is_some_and(history_text_is_failed_payload)
}

fn history_text_is_failed_payload(text: &str) -> bool {
    let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(text.trim()) else {
        return false;
    };
    payload.get("isError").and_then(Value::as_bool) == Some(true)
        && (payload.get("structuredContent").is_some() || payload.get("content").is_some())
}

fn bounded_history_text(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut compacted = text.chars().take(max_chars.saturating_sub(1)).collect::<String>();
    compacted.push('…');
    compacted
}

pub(super) fn compact_grounded_answer_structured_content_for_model(
    value: &Value,
    fallback_limit: usize,
) -> Value {
    let reference_limit = if try_parse_grounded_answer_completion_envelope(value)
        .is_some_and(|envelope| envelope.finalizable)
    {
        TOOL_MODEL_FINALIZABLE_GROUNDED_REFERENCE_LIMIT
    } else {
        TOOL_MODEL_GROUNDED_REFERENCE_LIMIT
    };
    compact_grounded_answer_structured_content(value, fallback_limit, reference_limit)
}

pub(super) fn compact_grounded_answer_structured_content_for_verification(
    value: &Value,
    fallback_limit: usize,
) -> Value {
    compact_grounded_answer_structured_content(
        value,
        fallback_limit,
        TOOL_VERIFICATION_GROUNDED_REFERENCE_LIMIT,
    )
}

pub(super) fn compact_grounded_answer_structured_content(
    value: &Value,
    fallback_limit: usize,
    reference_limit: usize,
) -> Value {
    if value.get("responseProfile").and_then(Value::as_str) == Some("compact") {
        return compact_existing_grounded_answer_profile(value, reference_limit, fallback_limit);
    }
    let Some(execution_detail) = value.get("executionDetail") else {
        return compact_structured_content(value, fallback_limit);
    };
    let completion_envelope = try_parse_grounded_answer_completion_envelope(value);
    let compact = serde_json::json!({
        "executionId": value.get("executionId").cloned().unwrap_or(Value::Null),
        "runtimeExecutionId": value.get("runtimeExecutionId").cloned().unwrap_or(Value::Null),
        "conversationId": value.get("conversationId").cloned().unwrap_or(Value::Null),
        "libraryId": value.get("libraryId").cloned().unwrap_or(Value::Null),
        "workspaceId": value.get("workspaceId").cloned().unwrap_or(Value::Null),
        "lifecycleState": value.get("lifecycleState").cloned().unwrap_or(Value::Null),
        "finalAnswerReady": completion_envelope.as_ref().is_some_and(|envelope| envelope.final_answer_ready),
        "finalizable": completion_envelope.as_ref().is_some_and(|envelope| envelope.finalizable),
        "completion": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.completion)).unwrap_or(Value::Null),
        "repairPolicy": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.repair_policy)).unwrap_or(Value::Null),
        "readiness": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.readiness)).unwrap_or(Value::Null),
        "mustPreserveSpans": value.get("mustPreserveSpans").cloned().unwrap_or_else(|| serde_json::json!([])),
        "clarification": value.get("clarification").cloned().unwrap_or_else(|| serde_json::json!({"required": false})),
        "answerCandidates": compact_reference_array(value.get("answerCandidates"), TOOL_MODEL_GROUNDED_REFERENCE_LIMIT),
        "verificationState": execution_detail.get("verificationState").cloned().unwrap_or(Value::Null),
        "verificationWarnings": execution_detail.get("verificationWarnings").cloned().unwrap_or_else(|| serde_json::json!([])),
        "referenceCounts": {
            "chunkReferences": reference_array_len(execution_detail.get("chunkReferences")),
            "preparedSegmentReferences": reference_array_len(execution_detail.get("preparedSegmentReferences")),
            "technicalFactReferences": reference_array_len(execution_detail.get("technicalFactReferences")),
            "entityReferences": reference_array_len(execution_detail.get("entityReferences")),
            "relationReferences": reference_array_len(execution_detail.get("relationReferences"))
        },
        "references": compact_grounded_reference_groups(execution_detail, reference_limit)
    });
    enforce_grounded_structured_content_limit(compact, fallback_limit)
}

pub(super) fn compact_existing_grounded_answer_profile(
    value: &Value,
    reference_limit: usize,
    fallback_limit: usize,
) -> Value {
    let reference_summary = value.get("referenceSummary");
    let references = bounded_json_array(
        reference_summary.and_then(|summary| summary.get("references")),
        reference_limit,
    );
    let total_reference_count = reference_summary
        .and_then(|summary| summary.get("totalCount"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| u64::try_from(references.len()).unwrap_or(u64::MAX));
    let returned_reference_count = u64::try_from(references.len()).unwrap_or(u64::MAX);

    let candidate_summary = value.get("answerCandidateSummary");
    let answer_candidates = bounded_json_array(
        candidate_summary.and_then(|summary| summary.get("candidates")),
        TOOL_MODEL_GROUNDED_REFERENCE_LIMIT,
    );
    let total_candidate_count = candidate_summary
        .and_then(|summary| summary.get("totalCount"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| u64::try_from(answer_candidates.len()).unwrap_or(u64::MAX));
    let returned_candidate_count = u64::try_from(answer_candidates.len()).unwrap_or(u64::MAX);
    let verifier_state = value.pointer("/verifier/state").cloned().unwrap_or(Value::Null);
    let warnings = bounded_json_array(value.get("warnings"), TOOL_MODEL_GROUNDED_REFERENCE_LIMIT);
    let completion_envelope = parse_grounded_answer_completion_envelope(value);

    let compact = serde_json::json!({
        "responseProfile": "compact",
        "executionId": value.get("executionId").cloned().unwrap_or(Value::Null),
        "runtimeExecutionId": value.get("runtimeExecutionId").cloned().unwrap_or(Value::Null),
        "conversationId": value.get("conversationId").cloned().unwrap_or(Value::Null),
        "libraryId": value.get("libraryId").cloned().unwrap_or(Value::Null),
        "workspaceId": value.get("workspaceId").cloned().unwrap_or(Value::Null),
        "lifecycleState": value.get("lifecycleState").cloned().unwrap_or(Value::Null),
        "finalAnswerReady": completion_envelope.as_ref().is_some_and(|envelope| envelope.final_answer_ready),
        "finalizable": completion_envelope.as_ref().is_some_and(|envelope| envelope.finalizable),
        "completion": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.completion)).unwrap_or(Value::Null),
        "repairPolicy": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.repair_policy)).unwrap_or(Value::Null),
        "readiness": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.readiness)).unwrap_or(Value::Null),
        "verifier": value.get("verifier").cloned().unwrap_or_else(|| serde_json::json!({"state": verifier_state.clone()})),
        "verificationState": verifier_state,
        "verificationWarnings": warnings,
        "mustPreserveSpans": value.get("mustPreserveSpans").cloned().unwrap_or_else(|| serde_json::json!([])),
        "clarification": value.get("clarification").cloned().unwrap_or_else(|| serde_json::json!({"required": false})),
        "referenceSummary": {
            "totalCount": total_reference_count,
            "returnedCount": returned_reference_count,
            "truncated": total_reference_count > returned_reference_count,
            "references": references,
        },
        "answerCandidateSummary": {
            "totalCount": total_candidate_count,
            "returnedCount": returned_candidate_count,
            "truncated": total_candidate_count > returned_candidate_count,
            "candidates": answer_candidates,
        }
    });
    enforce_grounded_structured_content_limit(compact, fallback_limit)
}

pub(super) fn compact_grounded_reference_groups(
    execution_detail: &Value,
    total_limit: usize,
) -> Value {
    let mut remaining = total_limit;
    let mut groups = serde_json::Map::new();
    for field in [
        "chunkReferences",
        "preparedSegmentReferences",
        "technicalFactReferences",
        "entityReferences",
        "relationReferences",
    ] {
        let items = execution_detail.get(field).and_then(Value::as_array);
        let selected = items
            .map(|items| {
                let take = items.len().min(remaining);
                remaining = remaining.saturating_sub(take);
                let mut selected = items.iter().take(take).cloned().collect::<Vec<_>>();
                if items.len() > take {
                    selected.push(serde_json::json!({
                        "truncated": true,
                        "omittedCount": items.len() - take,
                    }));
                }
                selected
            })
            .unwrap_or_default();
        groups.insert(field.to_string(), Value::Array(selected));
    }
    Value::Object(groups)
}

pub(super) fn enforce_grounded_structured_content_limit(value: Value, char_limit: usize) -> Value {
    let original_char_count = serde_json::to_string(&value)
        .map(|serialized| serialized.chars().count())
        .unwrap_or(usize::MAX);
    if original_char_count <= char_limit {
        return value;
    }
    let warning_codes = value
        .get("verificationWarnings")
        .or_else(|| value.get("warnings"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|warning| warning.get("code").and_then(Value::as_str))
        .take(8)
        .map(|code| code.chars().take(80).collect::<String>())
        .collect::<Vec<_>>();
    let preserve_spans = value
        .get("mustPreserveSpans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(8)
        .map(|span| span.chars().take(160).collect::<String>())
        .collect::<Vec<_>>();
    let completion_envelope = parse_grounded_answer_completion_envelope(&value);
    serde_json::json!({
        "truncated": true,
        "originalCharCount": original_char_count,
        "executionId": value.get("executionId").cloned().unwrap_or(Value::Null),
        "runtimeExecutionId": value.get("runtimeExecutionId").cloned().unwrap_or(Value::Null),
        "conversationId": value.get("conversationId").cloned().unwrap_or(Value::Null),
        "libraryId": value.get("libraryId").cloned().unwrap_or(Value::Null),
        "workspaceId": value.get("workspaceId").cloned().unwrap_or(Value::Null),
        "lifecycleState": value.get("lifecycleState").cloned().unwrap_or(Value::Null),
        "finalAnswerReady": completion_envelope.as_ref().is_some_and(|envelope| envelope.final_answer_ready),
        "finalizable": completion_envelope.as_ref().is_some_and(|envelope| envelope.finalizable),
        "completion": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.completion)).unwrap_or(Value::Null),
        "repairPolicy": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.repair_policy)).unwrap_or(Value::Null),
        "readiness": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.readiness)).unwrap_or(Value::Null),
        "clarification": value.get("clarification").cloned().unwrap_or_else(|| serde_json::json!({"required": false})),
        "verificationState": value.get("verificationState").cloned().unwrap_or(Value::Null),
        "verificationWarningCodes": warning_codes,
        "mustPreserveSpans": preserve_spans,
        "referenceCounts": value.get("referenceCounts").cloned().unwrap_or(Value::Null),
        "referenceSummary": value.get("referenceSummary").map(|summary| serde_json::json!({
            "totalCount": summary.get("totalCount").cloned().unwrap_or(Value::Null),
            "returnedCount": 0,
            "truncated": true,
            "references": [],
        })).unwrap_or(Value::Null),
    })
}

pub(super) fn bounded_json_array(value: Option<&Value>, limit: usize) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.iter().take(limit).cloned().collect(),
        _ => Vec::new(),
    }
}

pub(super) fn reference_array_len(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(items)) => items.len(),
        _ => 0,
    }
}

pub(super) fn compact_reference_array(value: Option<&Value>, limit: usize) -> Value {
    let Some(Value::Array(items)) = value else {
        return serde_json::json!([]);
    };
    let mut truncated = items.iter().take(limit).cloned().collect::<Vec<_>>();
    if items.len() > limit {
        truncated.push(serde_json::json!({
            "truncated": true,
            "omittedCount": items.len() - limit
        }));
    }
    Value::Array(truncated)
}

pub(super) fn compact_structured_content_for_model(value: &Value) -> Value {
    compact_structured_content(value, TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT)
}

pub(super) fn compact_structured_content_for_verification(value: &Value) -> Value {
    compact_structured_content(value, TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT)
}

pub(super) fn compact_structured_content(value: &Value, char_limit: usize) -> Value {
    let serialized = match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(error) => {
            return serde_json::json!({
                "truncated": true,
                "errorKind": "serialization",
                "message": error.to_string()
            });
        }
    };
    if serialized.chars().count() <= char_limit {
        return value.clone();
    }
    serde_json::json!({
        "truncated": true,
        "jsonPrefix": serialized.chars().take(char_limit).collect::<String>(),
        "originalCharCount": serialized.chars().count()
    })
}

pub(super) fn tool_result_preview(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
) -> Option<String> {
    tool_result_preview_with_limit(content, 2_000)
}

pub(super) fn tool_result_preview_with_limit(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
    limit: usize,
) -> Option<String> {
    let joined = content
        .iter()
        .map(|block| block.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then(|| joined.chars().take(limit).collect())
}

pub(super) fn extract_child_query_execution_ids(tool_name: &str, value: &Value) -> Vec<Uuid> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME {
        return Vec::new();
    }
    value
        .get("executionId")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .into_iter()
        .collect()
}

pub(super) fn extract_child_runtime_execution_ids(tool_name: &str, value: &Value) -> Vec<Uuid> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME {
        return Vec::new();
    }
    value
        .get("runtimeExecutionId")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .into_iter()
        .collect()
}
