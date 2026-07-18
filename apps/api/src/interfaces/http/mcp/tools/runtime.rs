use serde::Serialize;
use serde_json::{Value, json};

use crate::mcp_types::{
    McpGetRuntimeExecutionRequest, McpGetRuntimeExecutionTraceRequest, McpRuntimeExecutionSummary,
    McpRuntimeExecutionTrace,
};

use super::super::{
    McpToolDescriptor, McpToolResult, ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "get_runtime_execution" => Some(McpToolDescriptor {
            name: "get_runtime_execution",
            description: "Load the canonical runtime lifecycle summary for one runtime execution ID. Use this when a IronRAG payload already includes runtimeExecutionId and you need the authoritative lifecycle, active stage, or failure code.",
            input_schema: json!({
                "type": "object",
                "required": ["runtimeExecutionId"],
                "properties": {
                    "runtimeExecutionId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Canonical runtime execution UUID."
                    }
                }
            }),
        }),
        "get_runtime_execution_trace" => Some(McpToolDescriptor {
            name: "get_runtime_execution_trace",
            description: "Load the canonical runtime stage, action, and policy trace for one runtime execution ID. Use this for debugging or automation that must inspect what the runtime actually did.",
            input_schema: json!({
                "type": "object",
                "required": ["runtimeExecutionId"],
                "properties": {
                    "runtimeExecutionId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Canonical runtime execution UUID."
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
        _ => return None,
    };
    Some(result)
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
// Split out of the former `services/mcp/support.rs` god-file (plan
// §6.4): these two functions had exactly one caller each, both in this
// module, so moving them here (module-private) closes the last of the
// five unrelated concerns that file used to bundle.

fn describe_runtime_execution_summary(execution: &McpRuntimeExecutionSummary) -> String {
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
                canonical_runtime_value(&active_stage),
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
            canonical_runtime_value(&execution.lifecycle_state),
            canonical_runtime_value(&active_stage),
            policy_suffix
        ),
        _ => format!(
            "Runtime execution {} is {}.{}",
            execution.runtime_execution_id,
            canonical_runtime_value(&execution.lifecycle_state),
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

fn canonical_runtime_value<T>(value: &T) -> String
where
    T: Serialize,
{
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}
