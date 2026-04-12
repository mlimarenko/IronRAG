use serde_json::{Value, json};

use crate::{
    mcp_types::{
        McpAuditActionKind, McpAuditScope, McpGetRuntimeExecutionRequest,
        McpGetRuntimeExecutionTraceRequest,
    },
    services::mcp::support::{describe_runtime_execution_summary, describe_runtime_trace_summary},
};

use super::super::{
    McpToolDescriptor, McpToolResult,
    audit::{record_error_audit, record_success_audit},
    ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "get_runtime_execution" => Some(McpToolDescriptor {
            name: "get_runtime_execution",
            description: "Load the canonical runtime lifecycle summary for one runtime execution ID. Use this when a IronRAG payload already includes runtimeExecutionId and you need the authoritative lifecycle, active stage, or failure code.",
            input_schema: json!({
                "type": "object",
                "required": ["executionId"],
                "properties": {
                    "executionId": {
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
                "required": ["executionId"],
                "properties": {
                    "executionId": {
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
            args.execution_id,
        )
        .await
        {
            Ok(payload) => {
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::GetRuntimeExecution,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    json!({
                        "tool": "get_runtime_execution",
                        "runtimeExecutionId": payload.runtime_execution_id,
                        "lifecycleState": payload.lifecycle_state,
                        "activeStage": payload.active_stage,
                        "failureCode": payload.failure_code,
                        "policyRejectCount": payload.policy_summary.reject_count,
                        "policyTerminateCount": payload.policy_summary.terminate_count,
                    }),
                )
                .await;
                ok_tool_result(&describe_runtime_execution_summary(&payload), json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::GetRuntimeExecution,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({
                        "tool": "get_runtime_execution",
                        "executionId": args.execution_id,
                    }),
                )
                .await;
                tool_error_result(error)
            }
        },
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::GetRuntimeExecution,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "get_runtime_execution" }),
            )
            .await;
            tool_error_result(error)
        }
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
            args.execution_id,
        )
        .await
        {
            Ok(payload) => {
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::GetRuntimeExecutionTrace,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    json!({
                        "tool": "get_runtime_execution_trace",
                        "runtimeExecutionId": payload.execution.runtime_execution_id,
                        "lifecycleState": payload.execution.lifecycle_state,
                        "activeStage": payload.execution.active_stage,
                        "failureCode": payload.execution.failure_code,
                        "stageCount": payload.stages.len(),
                        "actionCount": payload.actions.len(),
                        "policyDecisionCount": payload.policy_decisions.len(),
                        "policyRejectCount": payload.execution.policy_summary.reject_count,
                        "policyTerminateCount": payload.execution.policy_summary.terminate_count,
                    }),
                )
                .await;
                ok_tool_result(&describe_runtime_trace_summary(&payload), json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::GetRuntimeExecutionTrace,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({
                        "tool": "get_runtime_execution_trace",
                        "executionId": args.execution_id,
                    }),
                )
                .await;
                tool_error_result(error)
            }
        },
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::GetRuntimeExecutionTrace,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "get_runtime_execution_trace" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}
