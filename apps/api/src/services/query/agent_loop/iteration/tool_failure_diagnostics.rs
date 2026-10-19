use super::*;

#[derive(Debug, Default, Eq, PartialEq)]
pub(in super::super) struct RedactedToolFailureDetail<'a> {
    pub(in super::super) error_kind: Option<&'a str>,
    pub(in super::super) failure_code: Option<&'a str>,
    pub(in super::super) retryable: Option<bool>,
    pub(in super::super) repair_hint: Option<&'a str>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(in super::super) struct ModelToolValidationFailure {
    pub(in super::super) tool_name: String,
    pub(in super::super) error_kind: String,
    pub(in super::super) argument_shape: String,
    pub(in super::super) expected_schema: String,
    pub(in super::super) schema_mismatch: String,
}

impl fmt::Display for ModelToolValidationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "tool_error detail: errorKind={}; tool={}; argumentShape={}; expectedSchema={}; schemaMismatch={}",
            self.error_kind,
            self.tool_name,
            self.argument_shape,
            self.expected_schema,
            self.schema_mismatch,
        )
    }
}

pub(in super::super) fn tool_iteration_stop_reason(
    origin: AgentToolCallOrigin,
    initial_grounded_answer_iteration: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    focused_follow_up_scheduled: bool,
) -> Option<AgentStopReason> {
    (origin == AgentToolCallOrigin::RuntimeEnforced
        && runtime_initial_grounded_answer_failed(
            initial_grounded_answer_iteration,
            tool_calls,
            outcomes,
            focused_follow_up_scheduled,
        ))
    .then_some(AgentStopReason::ToolError)
}

pub(in super::super) fn observe_model_tool_validation_outcomes(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    origin: AgentToolCallOrigin,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
) {
    let mut last_model_failure = None;
    for (call, outcome) in tool_calls.iter().zip(outcomes) {
        let schema = context
            .tool_defs
            .iter()
            .find(|tool| tool.name == call.name)
            .map(|tool| &tool.parameters)
            .unwrap_or(&Value::Null);
        let Some(failure) = model_tool_validation_failure(call, schema, outcome) else {
            continue;
        };
        tracing::warn!(
            request_id = context.input.request_id,
            library_id = %context.input.library_id,
            iteration,
            tool_call_origin = origin.as_str(),
            tool_name = %failure.tool_name,
            tool_error_kind = %failure.error_kind,
            argument_shape = %failure.argument_shape,
            expected_schema = %failure.expected_schema,
            schema_mismatch = %failure.schema_mismatch,
            "query.agent_loop.invalid_tool_arguments"
        );
        if origin == AgentToolCallOrigin::Model {
            last_model_failure = Some(failure);
        }
    }
    if origin != AgentToolCallOrigin::Model {
        return;
    }
    if last_model_failure.is_some() {
        state.last_model_tool_validation_failure = last_model_failure;
    } else if !outcomes.is_empty() {
        state.last_model_tool_validation_failure = None;
    }
}

pub(in super::super) fn model_tool_validation_failure(
    call: &ChatToolCall,
    schema: &Value,
    outcome: &ToolExecutionOutcome,
) -> Option<ModelToolValidationFailure> {
    let detail = redacted_tool_failure_detail(Some(outcome));
    let error_kind = detail.error_kind.filter(|kind| *kind == "invalid_mcp_tool_call")?;
    let parsed_arguments = serde_json::from_str::<Value>(&call.arguments_json).ok();
    let argument_shape = parsed_arguments
        .as_ref()
        .map(json_argument_shape)
        .unwrap_or_else(|| "invalid_json".to_string());
    let expected_schema = json_schema_shape(schema);
    let schema_mismatch = parsed_arguments.as_ref().map_or_else(
        || "invalid_json".to_string(),
        |arguments| {
            let mut mismatches = Vec::new();
            collect_schema_mismatches(arguments, schema, "", &mut mismatches);
            if mismatches.is_empty() {
                "validation_constraint".to_string()
            } else {
                mismatches.join(",")
            }
        },
    );
    Some(ModelToolValidationFailure {
        tool_name: safe_diagnostic_field_name(&call.name),
        error_kind: error_kind.to_string(),
        argument_shape,
        expected_schema,
        schema_mismatch,
    })
}

fn json_argument_shape(value: &Value) -> String {
    bounded_diagnostic_text(&json_value_shape(value, 0))
}

fn json_value_shape(value: &Value, depth: usize) -> String {
    if depth >= 3 {
        return json_value_type(value).to_string();
    }
    match value {
        Value::Object(object) => {
            let mut fields = object.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(name, _)| *name);
            let fields = fields
                .into_iter()
                .take(24)
                .map(|(name, value)| {
                    format!(
                        "{}:{}",
                        safe_diagnostic_field_name(name),
                        json_value_shape(value, depth + 1)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("object{{{fields}}}")
        }
        Value::Array(items) => {
            let item_shape = items
                .first()
                .map_or("unknown".to_string(), |item| json_value_shape(item, depth + 1));
            format!("array<{item_shape}>")
        }
        _ => json_value_type(value).to_string(),
    }
}

fn json_schema_shape(schema: &Value) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return schema.get("type").and_then(Value::as_str).unwrap_or("unspecified").to_string();
    };
    let mut fields = properties.iter().collect::<Vec<_>>();
    fields.sort_by_key(|(name, _)| *name);
    let fields = fields
        .into_iter()
        .take(24)
        .map(|(name, field_schema)| {
            format!(
                "{}:{}",
                safe_diagnostic_field_name(name),
                field_schema.get("type").and_then(Value::as_str).unwrap_or("unspecified")
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    bounded_diagnostic_text(&format!("object{{{fields}}}"))
}

fn collect_schema_mismatches(
    value: &Value,
    schema: &Value,
    path: &str,
    mismatches: &mut Vec<String>,
) {
    if mismatches.len() >= 24 {
        return;
    }
    if let Some(expected) = schema.get("type").and_then(Value::as_str)
        && !json_value_matches_schema_type(value, expected)
    {
        mismatches.push(format!(
            "type:{}:{}->{expected}",
            diagnostic_path(path),
            json_value_type(value)
        ));
        return;
    }
    match value {
        Value::Object(object) => collect_object_mismatches(object, schema, path, mismatches),
        Value::Array(items) => collect_array_mismatches(items, schema, path, mismatches),
        Value::String(text) => collect_string_mismatches(text, schema, path, mismatches),
        Value::Number(number) => collect_number_mismatches(number, schema, path, mismatches),
        Value::Null | Value::Bool(_) => {}
    }
}

/// Missing required fields, unexpected fields, and a recursive walk into the
/// declared properties.
fn collect_object_mismatches(
    object: &serde_json::Map<String, Value>,
    schema: &Value,
    path: &str,
    mismatches: &mut Vec<String>,
) {
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(field) {
                mismatches.push(format!("missing:{}", join_diagnostic_path(path, field)));
            }
        }
    }
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let mut actual_fields = object.keys().collect::<Vec<_>>();
    actual_fields.sort();
    for field in actual_fields {
        match (properties.get(field), object.get(field)) {
            (Some(field_schema), Some(field_value)) => collect_schema_mismatches(
                field_value,
                field_schema,
                &join_diagnostic_path(path, field),
                mismatches,
            ),
            (Some(_), None) => {}
            (None, _) => {
                mismatches.push(format!("unexpected:{}", join_diagnostic_path(path, field)));
            }
        }
    }
}

/// Length ceiling plus a bounded recursive walk into the item schema.
fn collect_array_mismatches(
    items: &[Value],
    schema: &Value,
    path: &str,
    mismatches: &mut Vec<String>,
) {
    if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64)
        && items.len() as u64 > max_items
    {
        mismatches.push(format!("maxItems:{}", diagnostic_path(path)));
    }
    let Some(item_schema) = schema.get("items") else {
        return;
    };
    for (index, item) in items.iter().take(24).enumerate() {
        collect_schema_mismatches(
            item,
            item_schema,
            &format!("{}[{index}]", diagnostic_path(path)),
            mismatches,
        );
    }
}

/// Length ceiling and enum membership.
fn collect_string_mismatches(text: &str, schema: &Value, path: &str, mismatches: &mut Vec<String>) {
    if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64)
        && text.chars().count() as u64 > max_length
    {
        mismatches.push(format!("maxLength:{}", diagnostic_path(path)));
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|variants| !variants.iter().any(|variant| variant.as_str() == Some(text)))
    {
        mismatches.push(format!("enum:{}", diagnostic_path(path)));
    }
}

/// Numeric bounds. Reported as one `range:` mismatch either way: the caller
/// only needs to know the value is outside what the schema allows.
fn collect_number_mismatches(
    number: &serde_json::Number,
    schema: &Value,
    path: &str,
    mismatches: &mut Vec<String>,
) {
    let below_minimum = schema
        .get("minimum")
        .and_then(Value::as_f64)
        .zip(number.as_f64())
        .is_some_and(|(minimum, actual)| actual < minimum);
    let above_maximum = schema
        .get("maximum")
        .and_then(Value::as_f64)
        .zip(number.as_f64())
        .is_some_and(|(maximum, actual)| actual > maximum);
    if below_minimum || above_maximum {
        mismatches.push(format!("range:{}", diagnostic_path(path)));
    }
}

fn json_value_matches_schema_type(value: &Value, expected: &str) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn json_value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn join_diagnostic_path(parent: &str, field: &str) -> String {
    let field = safe_diagnostic_field_name(field);
    if parent.is_empty() { field } else { format!("{parent}.{field}") }
}

fn diagnostic_path(path: &str) -> &str {
    if path.is_empty() { "$" } else { path }
}

fn safe_diagnostic_field_name(value: &str) -> String {
    value
        .chars()
        .take(80)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
                character
            } else {
                '?'
            }
        })
        .collect()
}

fn bounded_diagnostic_text(value: &str) -> String {
    value.chars().take(800).collect()
}

pub(in super::super) fn redacted_tool_failure_detail(
    outcome: Option<&ToolExecutionOutcome>,
) -> RedactedToolFailureDetail<'_> {
    let structured_content = outcome
        .and_then(|outcome| outcome.result_json.as_ref())
        .and_then(|result| result.get("structuredContent"));
    RedactedToolFailureDetail {
        error_kind: structured_content
            .and_then(|value| value.get("errorKind"))
            .and_then(Value::as_str)
            .filter(|value| diagnostic_code_is_safe(value)),
        failure_code: structured_content
            .and_then(|value| value.get("failureCode"))
            .and_then(Value::as_str)
            .filter(|value| diagnostic_code_is_safe(value)),
        retryable: structured_content
            .and_then(|value| value.get("retryable"))
            .and_then(Value::as_bool),
        repair_hint: structured_content
            .and_then(|value| value.get("repairHint"))
            .and_then(Value::as_str)
            .filter(|value| diagnostic_code_is_safe(value)),
    }
}

fn diagnostic_code_is_safe(value: &str) -> bool {
    let len = value.len();
    (1..=80).contains(&len)
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
}

pub(in super::super) fn log_runtime_initial_grounded_answer_failure(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
) {
    let failed_outcome_index = outcomes
        .iter()
        .position(|outcome| outcome.is_error)
        .or_else(|| (!outcomes.is_empty()).then_some(0));
    let failed_outcome = failed_outcome_index.and_then(|index| outcomes.get(index));
    let failed_tool_name = failed_outcome_index
        .and_then(|index| tool_calls.get(index))
        .or_else(|| tool_calls.first())
        .map(|call| call.name.as_str());
    let detail = redacted_tool_failure_detail(failed_outcome);
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        tool_call_count = tool_calls.len(),
        outcome_count = outcomes.len(),
        failed_outcome_index,
        tool_name = failed_tool_name,
        outcome_is_error = failed_outcome.is_some_and(|outcome| outcome.is_error),
        grounded_answer_completed =
            failed_outcome.is_some_and(|outcome| outcome.grounded_answer_completed),
        tool_error_kind = detail.error_kind,
        tool_failure_code = detail.failure_code,
        tool_error_retryable = detail.retryable,
        tool_repair_hint = detail.repair_hint,
        "query.agent_loop.runtime_initial_grounded_answer_failed"
    );
}
