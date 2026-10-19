use super::iteration::tool_failure_diagnostics::model_tool_validation_failure;
use super::*;

#[test]
fn retryable_tool_failure_recovered_from_agent_tool_timeout() {
    let result_json = tool_result_json("agent_tool_timeout", true);

    let failure = retryable_tool_failure_from_result_json(Some(&result_json))
        .expect("agent tool timeout must classify as retryable");

    assert_eq!(failure.kind, "agent_tool_timeout");
}

#[test]
fn retryable_tool_failure_recovered_from_known_transient_error_kind() {
    let result_json = tool_result_json("document_projection_converging", true);

    let failure = retryable_tool_failure_from_result_json(Some(&result_json))
        .expect("known transient errorKind must classify as retryable");

    assert_eq!(failure.kind, "document_projection_converging");
    assert_eq!(failure.message, "document projection is still updating; retry shortly");
}

#[test]
fn retryable_tool_failure_ignores_non_retryable_error_kind() {
    let result_json = tool_result_json("query_execution_failed", false);

    assert!(retryable_tool_failure_from_result_json(Some(&result_json)).is_none());
}

#[test]
fn retryable_tool_failure_ignores_unknown_error_kind_even_if_marked_retryable() {
    // A future tool surface could mark some new kind `retryable: true`
    // before this loop knows about it; refuse to invent a classification
    // for a kind it doesn't recognize rather than leaking an arbitrary
    // string as if it were a stable, known kind.
    let result_json = tool_result_json("some_future_kind", true);

    assert!(retryable_tool_failure_from_result_json(Some(&result_json)).is_none());
}

#[test]
fn retryable_tool_failure_ignores_success_outcomes() {
    let result_json = debug_tool_result_json(&crate::interfaces::http::mcp::McpToolResult {
        content: vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: "ok".to_string(),
        }],
        structured_content: serde_json::json!({ "answerBody": "ok" }),
        is_error: false,
    });

    assert!(retryable_tool_failure_from_result_json(Some(&result_json)).is_none());
}

#[test]
fn terminal_agent_loop_error_preserves_retryable_tool_failure_for_tool_error_stop() {
    let failure = RetryableToolFailure {
        kind: "document_projection_converging",
        message: "document projection is still updating; retry shortly".to_string(),
    };

    let error = terminal_agent_loop_error(
        AgentStopReason::ToolError,
        agent_loop_stop_message(AgentStopReason::ToolError).to_string(),
        Some(&failure),
        None,
    );

    let QueryServiceError::Internal(inner) = error else {
        panic!("expected an Internal error carrying the preserved ApiError classification");
    };
    let api_error =
        inner.downcast_ref::<ApiError>().expect("preserved error must downcast to ApiError");
    assert!(
        matches!(
            api_error,
            ApiError::ServiceUnavailable { kind: "document_projection_converging", .. }
        ),
        "expected a preserved document projection failure, got {api_error:?}"
    );
}

#[test]
fn terminal_agent_loop_error_stays_generic_internal_without_a_classified_tool_failure() {
    let error = terminal_agent_loop_error(
        AgentStopReason::IterationCap,
        agent_loop_stop_message(AgentStopReason::IterationCap).to_string(),
        None,
        None,
    );

    let QueryServiceError::Internal(inner) = error else {
        panic!("expected an Internal error");
    };
    assert!(inner.downcast_ref::<ApiError>().is_none());
}

#[test]
fn terminal_agent_loop_error_still_prefers_deadline_and_provider_error_variants() {
    let failure = RetryableToolFailure {
        kind: "document_projection_converging",
        message: "irrelevant".to_string(),
    };

    assert!(matches!(
        terminal_agent_loop_error(
            AgentStopReason::Deadline,
            "msg".to_string(),
            Some(&failure),
            None,
        ),
        QueryServiceError::DeadlineExceeded
    ));
    assert!(matches!(
        terminal_agent_loop_error(
            AgentStopReason::ProviderError,
            "msg".to_string(),
            Some(&failure),
            None,
        ),
        QueryServiceError::Provider(_)
    ));
}

#[test]
fn exhausted_model_tool_retries_include_redacted_validation_detail() {
    let private_value = "private argument value";
    let call = ChatToolCall {
        id: "call-malformed".to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({"unexpectedField": private_value}).to_string(),
    };
    let schema = serde_json::json!({
        "type": "object",
        "required": ["query"],
        "properties": {"query": {"type": "string"}}
    });
    let outcome =
        invalid_mcp_tool_call_outcome("invalid grounded_answer arguments: missing field `query`");
    let validation_failure = model_tool_validation_failure(&call, &schema, &outcome)
        .expect("invalid MCP arguments must classify");

    let error = terminal_agent_loop_error(
        AgentStopReason::IterationCap,
        agent_loop_stop_message(AgentStopReason::IterationCap).to_string(),
        None,
        Some(&validation_failure),
    );

    let QueryServiceError::Internal(inner) = error else {
        panic!("expected an Internal error");
    };
    let rendered = inner.to_string();
    assert!(rendered.contains("invalid_mcp_tool_call"));
    assert!(rendered.contains("missing:query"));
    assert!(rendered.contains("unexpected:unexpectedField"));
    assert!(!rendered.contains("stopped after a tool error"));
    assert!(!rendered.contains(private_value));
}
