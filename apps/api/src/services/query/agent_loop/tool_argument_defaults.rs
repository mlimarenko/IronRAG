use super::*;

pub(super) fn normalize_agent_tool_argument_types(tool_name: &str, arguments: &mut Value) {
    let Value::Object(object) = arguments else {
        return;
    };
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME {
        normalize_stringified_json_array_field(object, "libraries");
    }
    for field in ["limit", "topK", "startOffset", "length", "maxBytes", "maxReferences"] {
        normalize_unsigned_integer_string_field(object, field);
    }
}

pub(super) fn normalize_stringified_json_array_field(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
) {
    let Some(raw) = object.get(field).and_then(Value::as_str).map(str::trim) else {
        return;
    };
    if !raw.starts_with('[') {
        return;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    if parsed.is_array() {
        object.insert(field.to_string(), parsed);
    }
}

pub(super) fn normalize_unsigned_integer_string_field(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
) {
    let Some(raw) = object.get(field).and_then(Value::as_str).map(str::trim) else {
        return;
    };
    let Ok(value) = raw.parse::<u64>() else {
        return;
    };
    object.insert(field.to_string(), serde_json::json!(value));
}

/// Shared inputs for the grounded-answer / agent tool argument normalization
/// and effective-payload fingerprinting pipeline. Bundled so the defaults and
/// the fingerprint see one parameter list across their call sites.
#[derive(Debug, Clone, Copy)]
pub(super) struct AgentToolArgumentContext<'a> {
    pub(super) user_question: &'a str,
    pub(super) contextual_follow_up: bool,
    pub(super) grounded_top_k: usize,
    pub(super) library_ref: &'a str,
    pub(super) grounded_answer_tool_history: &'a [ExternalConversationTurn],
}

#[cfg(test)]
pub(super) fn apply_agent_tool_argument_defaults(
    tool_name: &str,
    arguments: &mut Value,
    user_question: &str,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) {
    apply_agent_tool_argument_defaults_with_context(
        tool_name,
        arguments,
        AgentToolArgumentContext {
            user_question,
            contextual_follow_up: false,
            grounded_top_k,
            library_ref,
            grounded_answer_tool_history,
        },
    );
}

pub(super) fn apply_agent_tool_argument_defaults_with_context(
    tool_name: &str,
    arguments: &mut Value,
    context: AgentToolArgumentContext<'_>,
) {
    let Value::Object(object) = arguments else {
        return;
    };
    let runtime_repair = take_runtime_grounded_repair_metadata(object);
    apply_agent_tool_library_defaults(tool_name, object, context.library_ref);
    let bounded_top_k = context.grounded_top_k.max(1);
    if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        apply_grounded_answer_argument_defaults(
            object,
            context.user_question,
            context.contextual_follow_up,
            bounded_top_k,
            context.grounded_answer_tool_history,
            runtime_repair.is_some(),
        );
        return;
    }
    apply_agent_tool_limit_default(tool_name, object, bounded_top_k);
}

pub(super) fn take_runtime_grounded_repair_metadata(
    object: &mut serde_json::Map<String, Value>,
) -> Option<RuntimeGroundedRepairMetadata> {
    object
        .remove(RUNTIME_REPAIR_ARGUMENT_FIELD)
        .and_then(|value| serde_json::from_value::<RuntimeGroundedRepairMetadata>(value).ok())
        .filter(|metadata| metadata.is_consistent())
}

pub(super) fn apply_agent_tool_library_defaults(
    tool_name: &str,
    object: &mut serde_json::Map<String, Value>,
    library_ref: &str,
) {
    if tool_uses_single_library_scope(tool_name) {
        object.insert("library".to_string(), serde_json::json!(library_ref));
    }
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME {
        object.insert("libraries".to_string(), serde_json::json!([library_ref]));
    }
}

pub(super) fn apply_grounded_answer_argument_defaults(
    object: &mut serde_json::Map<String, Value>,
    user_question: &str,
    contextual_follow_up: bool,
    bounded_top_k: usize,
    grounded_answer_tool_history: &[ExternalConversationTurn],
    is_runtime_repair: bool,
) {
    apply_grounded_answer_response_profile_defaults(object);
    let contextual_history = if contextual_follow_up {
        compact_grounded_answer_tool_history(grounded_answer_tool_history)
    } else {
        Vec::new()
    };
    // UI and MCP must cross the same semantic boundary. The current
    // question and server-owned typed history are authoritative; a model or
    // runtime repair may not reconstruct intent by rewriting either one.
    object.insert("query".to_string(), serde_json::json!(user_question));
    let requested_top_k =
        object.get("topK").and_then(Value::as_u64).and_then(|value| usize::try_from(value).ok());
    object.insert(
        "conversationTurns".to_string(),
        Value::Array(grounded_answer_conversation_turn_defaults(&contextual_history)),
    );
    let effective_top_k = resolve_agent_grounded_answer_top_k(
        requested_top_k,
        !contextual_history.is_empty(),
        if is_runtime_repair { MAX_TOP_K } else { bounded_top_k },
    );
    if requested_top_k != Some(effective_top_k) {
        object.insert("topK".to_string(), serde_json::json!(effective_top_k));
    }
}

pub(super) fn apply_grounded_answer_response_profile_defaults(
    object: &mut serde_json::Map<String, Value>,
) {
    if object.get("includeDebug").and_then(Value::as_bool) == Some(true) {
        object.insert("responseProfile".to_string(), serde_json::json!("full"));
        object.remove("maxReferences");
        return;
    }
    object.insert("responseProfile".to_string(), serde_json::json!("compact"));
    let reference_limit = crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT;
    let requested_references = object
        .get("maxReferences")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    if requested_references.is_none_or(|value| value == 0 || value > reference_limit) {
        object.insert("maxReferences".to_string(), serde_json::json!(reference_limit));
    }
}

pub(super) fn apply_agent_tool_limit_default(
    tool_name: &str,
    object: &mut serde_json::Map<String, Value>,
    bounded_top_k: usize,
) {
    let Some(limit_cap) = agent_tool_limit_cap(tool_name) else {
        return;
    };
    // Static tool caps are ceilings; the parent turn's top-k budget
    // tightens them further so UI-agent subqueries cannot fan out wider
    // than the turn that spawned them.
    let bounded_limit = limit_cap.min(bounded_top_k.max(8)).max(1);
    let requested_limit =
        object.get("limit").and_then(Value::as_u64).and_then(|value| usize::try_from(value).ok());
    if requested_limit.is_none_or(|value| value > bounded_limit) {
        object.insert("limit".to_string(), serde_json::json!(bounded_limit));
    }
}

pub(super) fn resolve_agent_grounded_answer_top_k(
    requested_top_k: Option<usize>,
    has_contextual_turns: bool,
    max_top_k: usize,
) -> usize {
    resolve_contextual_grounded_answer_top_k(requested_top_k, has_contextual_turns, max_top_k)
        .max(crate::domains::query::DEFAULT_TOP_K.min(max_top_k.max(1)))
}

pub(super) fn grounded_answer_conversation_turn_defaults(
    conversation_turns: &[ExternalConversationTurn],
) -> Vec<Value> {
    conversation_turns
        .iter()
        .filter_map(|turn| {
            let role = match turn.turn_kind {
                QueryTurnKind::User => "user",
                QueryTurnKind::Assistant => "assistant",
                QueryTurnKind::System | QueryTurnKind::Tool => return None,
            };
            let content = turn.content_text.trim();
            if content.is_empty() {
                return None;
            }
            Some(serde_json::json!({
                "role": role,
                "content": content,
            }))
        })
        .collect()
}
