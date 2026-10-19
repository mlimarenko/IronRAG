use super::*;

#[cfg(test)]
pub(super) fn effective_tool_call_fingerprint(
    tool_name: &str,
    arguments_json: &str,
    user_question: &str,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> Option<String> {
    effective_tool_call_fingerprint_with_context(
        tool_name,
        arguments_json,
        AgentToolArgumentContext {
            user_question,
            contextual_follow_up: false,
            grounded_top_k,
            library_ref,
            grounded_answer_tool_history,
        },
    )
}

pub(super) fn effective_tool_call_fingerprint_with_context(
    tool_name: &str,
    arguments_json: &str,
    context: AgentToolArgumentContext<'_>,
) -> Option<String> {
    let mut arguments = serde_json::from_str::<Value>(arguments_json).ok()?;
    normalize_agent_tool_argument_types(tool_name, &mut arguments);
    validate_agent_tool_library_scope(tool_name, &arguments, context.library_ref).ok()?;
    apply_agent_tool_argument_defaults_with_context(tool_name, &mut arguments, context);
    let normalized_arguments = normalized_json_value(&arguments);
    let serialized = serde_json::to_string(&normalized_arguments).ok()?;
    Some(format!("{tool_name}:{serialized}"))
}

pub(super) fn normalized_json_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(normalized_json_value).collect()),
        Value::Object(object) => {
            let mut sorted = serde_json::Map::new();
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if let Some(item) = object.get(key) {
                    sorted.insert(key.clone(), normalized_json_value(item));
                }
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

pub(super) fn split_literal_anchor_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut token_start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if is_literal_anchor_boundary(ch) {
            if let Some(start) = token_start.take() {
                push_split_literal_anchor_candidate(&mut candidates, &text[start..index]);
            }
            continue;
        }
        token_start.get_or_insert(index);
    }
    if let Some(start) = token_start {
        push_split_literal_anchor_candidate(&mut candidates, &text[start..]);
    }
    candidates
}

pub(super) fn is_literal_anchor_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            ',' | ';'
                | '('
                | ')'
                | '{'
                | '}'
                | '«'
                | '»'
                | '“'
                | '”'
                | '„'
                | '‹'
                | '›'
                | '"'
                | '\''
                | '`'
        )
}

pub(super) fn push_split_literal_anchor_candidate(candidates: &mut Vec<String>, raw: &str) {
    let candidate = trim_literal_anchor_candidate(raw);
    if !candidate.is_empty() {
        candidates.push(candidate.to_string());
    }
}

pub(super) fn trim_literal_anchor_candidate(raw: &str) -> &str {
    raw.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                ',' | ';'
                    | '.'
                    | ')'
                    | '('
                    | '{'
                    | '}'
                    | '"'
                    | '\''
                    | '`'
                    | '«'
                    | '»'
                    | '“'
                    | '”'
                    | '„'
                    | '‹'
                    | '›'
            )
    })
}

pub(super) fn is_structural_literal_anchor(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return false;
    }
    let char_count = candidate.chars().count();
    if !(2..=STRUCTURAL_LITERAL_ANCHOR_MAX_CHARS).contains(&char_count) {
        return false;
    }
    let bracket_inner =
        candidate.strip_prefix('[').and_then(|value| value.strip_suffix(']')).map(str::trim);
    if let Some(inner) = bracket_inner
        && !inner.is_empty()
        && inner.chars().any(|ch| ch.is_alphanumeric())
    {
        return true;
    }
    let lower = candidate.to_ascii_lowercase();
    if (lower.starts_with("http://") || lower.starts_with("https://"))
        && candidate.chars().any(|ch| ch.is_alphanumeric())
    {
        return true;
    }
    if (candidate.starts_with('/') || candidate.starts_with('\\'))
        && candidate.chars().any(|ch| ch.is_alphanumeric())
    {
        return true;
    }
    if candidate.contains('/') || candidate.contains('\\') {
        return candidate.chars().any(|ch| ch.is_alphanumeric());
    }
    if candidate.contains('=') {
        return candidate.split_once('=').is_some_and(|(left, right)| {
            !left.trim().is_empty()
                && !right.trim().is_empty()
                && left.trim().chars().any(|ch| ch.is_alphanumeric())
        });
    }
    let unwrapped =
        candidate.trim_matches('[').trim_matches(']').trim_matches('`').trim_matches('"');
    literal_text_is_identifier_shaped(unwrapped) || is_identifier_shaped_fragment(unwrapped)
}

pub(super) fn is_plain_code_literal(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.chars().any(char::is_whitespace) {
        return false;
    }
    let char_count = candidate.chars().count();
    if !(2..=STRUCTURAL_LITERAL_ANCHOR_MAX_CHARS).contains(&char_count) {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    alnum_count >= 2
        && candidate.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '=')
        })
}

pub(super) fn activity_result_preview(text: &str) -> String {
    text.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).take(240).collect()
}
