use serde::Serialize;
use serde_json::{Value, json};

use crate::mcp_types::{
    McpGetRuntimeExecutionRequest, McpGetRuntimeExecutionTraceRequest, McpRuntimeExecutionSummary,
    McpRuntimeExecutionTrace,
};

use crate::interfaces::http::router_support::ApiError;

use super::super::{
    McpToolDescriptor, McpToolResult, ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "get_caller_context" => Some(McpToolDescriptor {
            name: "get_caller_context",
            description: "Return this caller's own non-secret financial-fence identity: the server-derived fingerprint of the validated MCP session and its verified owner principal. Use it when an operator must admit this client into a financial scope. It takes no arguments and never returns a token, header, permitted-caller list or another caller's identity.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }),
        "get_runtime_execution" => Some(McpToolDescriptor {
            name: "get_runtime_execution",
            description: "Load the authoritative runtime lifecycle summary for one runtime execution ID. Use this when an IronRAG payload already includes runtimeExecutionId and you need its lifecycle state, active stage, or failure code.",
            input_schema: json!({
                "type": "object",
                "required": ["runtimeExecutionId"],
                "properties": {
                    "runtimeExecutionId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Runtime execution UUID."
                    }
                }
            }),
        }),
        "get_runtime_execution_trace" => Some(McpToolDescriptor {
            name: "get_runtime_execution_trace",
            description: "Load the active runtime stage, action, and policy trace for one runtime execution ID. Use this for debugging or automation that must inspect what the runtime actually did.",
            input_schema: json!({
                "type": "object",
                "required": ["runtimeExecutionId"],
                "properties": {
                    "runtimeExecutionId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Runtime execution UUID."
                    }
                }
            }),
        }),
        _ => None,
    }
}

pub(crate) async fn call_tool(
    name: &str,
    context: ToolCallContext<'_>,
    arguments: &Value,
) -> Option<McpToolResult> {
    let result = match name {
        "get_runtime_execution" => get_runtime_execution(context, arguments).await,
        "get_runtime_execution_trace" => get_runtime_execution_trace(context, arguments).await,
        "get_caller_context" => get_caller_context(context),
        _ => return None,
    };
    Some(result)
}

/// The caller's own validated fence identity, or a typed refusal.
///
/// Split from the tool handler so the projection can be tested without an
/// `AppState`: it publishes only the two non-secret values the fence needs.
pub(crate) fn caller_context_projection(
    identity: Option<super::ToolCallerIdentity>,
) -> Result<Value, ApiError> {
    match identity {
        Some(caller) => Ok(json!({
            "sessionFingerprint": hex::encode(caller.session_fingerprint),
            "owner": { "principalId": caller.owner_principal_id },
        })),
        None => Err(ApiError::invalid_mcp_tool_call(
            "this request carries no validated MCP session; no caller identity is published"
                .to_string(),
        )),
    }
}

/// The caller's own validated fence identity.
fn get_caller_context(context: ToolCallContext<'_>) -> McpToolResult {
    match caller_context_projection(context.caller_context) {
        Ok(payload) => ok_tool_result("This session's financial-fence identity.", payload),
        Err(error) => tool_error_result(error),
    }
}

async fn get_runtime_execution(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpGetRuntimeExecutionRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::access::get_runtime_execution(
            context.auth,
            context.state,
            args.runtime_execution_id,
        )
        .await
        {
            Ok(payload) => {
                ok_tool_result(&describe_runtime_execution_summary(&payload), json!(payload))
            }
            Err(error) => tool_error_result(error),
        },
        Err(error) => tool_error_result(error),
    }
}

async fn get_runtime_execution_trace(
    context: ToolCallContext<'_>,
    arguments: &Value,
) -> McpToolResult {
    match parse_tool_args::<McpGetRuntimeExecutionTraceRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::access::get_runtime_execution_trace(
            context.auth,
            context.state,
            args.runtime_execution_id,
        )
        .await
        {
            Ok(payload) => {
                ok_tool_result(&describe_runtime_trace_summary(&payload), json!(payload))
            }
            Err(error) => tool_error_result(error),
        },
        Err(error) => tool_error_result(error),
    }
}

// --- Runtime-text formatting -------------------------------------------
//
// Split out of the former `services/mcp/support.rs` god-file (plan §6.4).
// `describe_runtime_trace_summary` stays module-private.

/// Canonical one-line execution state, also used by the source-evidence
/// projection.
pub(super) fn describe_runtime_execution_summary(execution: &McpRuntimeExecutionSummary) -> String {
    let policy_suffix = if execution.policy_summary.reject_count > 0
        || execution.policy_summary.terminate_count > 0
    {
        format!(
            " Policy interventions: {} rejected, {} terminated.",
            execution.policy_summary.reject_count, execution.policy_summary.terminate_count
        )
    } else {
        String::new()
    };
    match (execution.lifecycle_state, execution.active_stage) {
        (crate::domains::agent_runtime::RuntimeLifecycleState::Running, Some(active_stage)) => {
            format!(
                "Runtime execution {} is running in stage {}.{}",
                execution.runtime_execution_id,
                serialized_runtime_value(&active_stage),
                policy_suffix
            )
        }
        (
            crate::domains::agent_runtime::RuntimeLifecycleState::Completed
            | crate::domains::agent_runtime::RuntimeLifecycleState::Recovered,
            Some(active_stage),
        ) => format!(
            "Runtime execution {} finished in state {} after stage {}.{}",
            execution.runtime_execution_id,
            serialized_runtime_value(&execution.lifecycle_state),
            serialized_runtime_value(&active_stage),
            policy_suffix
        ),
        _ => format!(
            "Runtime execution {} is {}.{}",
            execution.runtime_execution_id,
            serialized_runtime_value(&execution.lifecycle_state),
            policy_suffix
        ),
    }
}

fn describe_runtime_trace_summary(trace: &McpRuntimeExecutionTrace) -> String {
    format!(
        "Runtime trace loaded for execution {} with {} stage(s), {} action(s), and {} policy decision(s).",
        trace.execution.runtime_execution_id,
        trace.stages.len(),
        trace.actions.len(),
        trace.policy_decisions.len()
    )
}

fn serialized_runtime_value<T>(value: &T) -> String
where
    T: Serialize,
{
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}
