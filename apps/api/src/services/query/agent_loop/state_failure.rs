use super::*;

use crate::shared::provider_failure::ProviderFailure;

pub(super) fn terminal_agent_loop_failure(
    context: &McpAgentLoopContext<'_>,
    state: McpAgentLoopState,
) -> AgentTurnFailure {
    let mut message = agent_loop_stop_message(state.stopped_reason).to_string();
    if state.successful_tool_call_count == 0 && state.total_tool_call_count > 0 {
        message.push_str("; no successful MCP tool result was received");
    }
    let error = terminal_agent_loop_error(
        state.stopped_reason,
        message,
        state.last_retryable_tool_failure.as_ref(),
        state.last_model_tool_validation_failure.as_ref(),
    );
    AgentTurnFailure::with_loop(
        error,
        state.provider_calls,
        state.debug_iterations,
        agent_loop_metadata(
            context.iteration_cap,
            context.input.deadline,
            state.stopped_reason,
            state.total_tool_call_count,
        ),
    )
}

/// Classify the terminal `QueryServiceError` for a loop that ended without a
/// final answer. `Deadline`/`ProviderError` map to their own dedicated
/// variants; every other stop reason (`ToolError`, `IterationCap`,
/// `NoProgress`) reports the latest redacted model-validation detail when
/// available. Otherwise, if the tool call that actually caused the stop
/// already classified its own failure as retryable, preserve that
/// classification (wrapped as `ApiError::ServiceUnavailable`, which the
/// failure-code mapper in `service::turn` already downcasts to a 503) instead
/// of collapsing to a generic, non-retryable internal error.
pub(super) fn terminal_agent_loop_error(
    stopped_reason: AgentStopReason,
    message: String,
    last_retryable_tool_failure: Option<&RetryableToolFailure>,
    last_model_tool_validation_failure: Option<&ModelToolValidationFailure>,
) -> QueryServiceError {
    match stopped_reason {
        AgentStopReason::Deadline => QueryServiceError::DeadlineExceeded,
        AgentStopReason::ProviderError => {
            QueryServiceError::Provider(ProviderFailure::unavailable(message))
        }
        _ => {
            if let Some(failure) = last_model_tool_validation_failure {
                return QueryServiceError::Internal(anyhow::anyhow!(
                    "assistant agent exhausted its bounded tool-call recovery budget; {failure}"
                ));
            }
            match last_retryable_tool_failure {
                Some(failure) => QueryServiceError::Internal(anyhow::Error::new(
                    ApiError::service_unavailable(failure.message.clone(), failure.kind),
                )),
                None => QueryServiceError::Internal(anyhow::anyhow!(message)),
            }
        }
    }
}

pub(super) fn agent_loop_stop_message(reason: AgentStopReason) -> &'static str {
    match reason {
        AgentStopReason::Deadline => {
            "assistant agent exceeded its turn deadline before producing a final answer"
        }
        AgentStopReason::IterationCap => {
            "assistant agent reached its iteration cap before producing a final answer"
        }
        AgentStopReason::FinalAnswer => "assistant agent stopped before producing a final answer",
        AgentStopReason::NoProgress => {
            "assistant agent stopped after consecutive iterations made no progress"
        }
        AgentStopReason::ToolError => "assistant agent stopped after a tool error",
        AgentStopReason::ProviderError => "assistant agent stopped after a provider error",
        AgentStopReason::Unknown => "assistant agent stopped for an unknown historical reason",
    }
}

/// Recover a tool call's own retryable-failure classification from its debug
/// result JSON (`{"structuredContent": {"errorKind", "retryable", "message"}}`,
/// the same shape `tool_error_result` in `interfaces::http::mcp` produces).
/// Returns `None` for non-error outcomes, outcomes with no such
/// classification, and outcomes classified `retryable: false`.
pub(super) fn retryable_tool_failure_from_result_json(
    result_json: Option<&Value>,
) -> Option<RetryableToolFailure> {
    let structured_content = result_json?.get("structuredContent")?;
    if !structured_content.get("retryable")?.as_bool()? {
        return None;
    }
    let kind = structured_content.get("errorKind")?.as_str()?;
    let kind = static_retryable_tool_error_kind(kind)?;
    let message =
        structured_content.get("message").and_then(Value::as_str).unwrap_or(kind).to_string();
    Some(RetryableToolFailure { kind, message })
}

/// Map a tool result's `errorKind` string to one of the stable, known
/// `&'static str` kinds worth preserving as `ApiError::ServiceUnavailable`
/// downstream. Deliberately narrow: `api_failure_code` in `service::turn`
/// flattens every `ApiError::ServiceUnavailable` to the generic
/// `query_dependency_unavailable` (503) regardless of `kind`, so wrapping a
/// kind whose *dedicated* classification is not a plain 503 (for example
/// `query_deadline_exceeded`, which has its own 504-class
/// `QueryServiceError::DeadlineExceeded` path) would silently reclassify its
/// status code. Only include a kind here once its correct terminal status
/// really is "retry the same request, 503" and the grounded-answer tool
/// already treats it that way at its own call site.
/// Unknown or unverified kinds return `None` rather than leaking an arbitrary
/// string as if it were one of the recognized retryable classes.
pub(super) fn static_retryable_tool_error_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "agent_tool_timeout" => Some("agent_tool_timeout"),
        "document_projection_converging" => Some("document_projection_converging"),
        _ => None,
    }
}
