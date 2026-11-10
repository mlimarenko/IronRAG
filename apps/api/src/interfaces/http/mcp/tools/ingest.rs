//! MCP diagnostics tools over the ingest job queue.

use serde_json::{Value, json};

use crate::mcp_types::{
    McpCancelIngestJobRequest, McpGetIngestJobRequest, McpListIngestJobsRequest,
    McpRetryIngestJobRequest,
};

use super::super::{
    McpToolDescriptor, McpToolResult, ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "list_ingest_jobs" => Some(McpToolDescriptor {
            name: "list_ingest_jobs",
            description: "List ingest jobs for one authorized library, newest first. Use it to find a job id or to check whether a connector sync is queued, leased, or finished.",
            input_schema: json!({
                "type": "object",
                "required": ["library"],
                "properties": {
                    "library": {
                        "type": "string",
                        "description": "Catalog library reference as `<workspace>/<library>`."
                    },
                    "queueStates": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["queued", "leased", "completed", "failed", "canceled", "paused"]
                        },
                        "description": "Optional queue-state filter. Absent or empty keeps every state."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "default": 50,
                        "description": "Page size."
                    },
                    "cursor": {
                        "type": "string",
                        "description": "Opaque nextCursor from a previous list_ingest_jobs response."
                    }
                }
            }),
        }),
        "get_ingest_job" => Some(McpToolDescriptor {
            name: "get_ingest_job",
            description: "Load one ingest job with its recent attempts and the stage timeline of the returned attempt. Use it to diagnose why a connector sync stalled or failed.",
            input_schema: json!({
                "type": "object",
                "required": ["jobId"],
                "properties": {
                    "jobId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Ingest job identifier."
                    },
                    "attemptLimit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "default": 10,
                        "description": "How many attempts to return, newest first."
                    }
                }
            }),
        }),
        "retry_ingest_job" => Some(McpToolDescriptor {
            name: "retry_ingest_job",
            description: "Requeue one ingest job. Requeues only queued, paused, or stale-leased jobs; completed, canceled, and failed jobs are rejected.",
            input_schema: json!({
                "type": "object",
                "required": ["jobId"],
                "properties": {
                    "jobId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Ingest job identifier."
                    }
                }
            }),
        }),
        "cancel_ingest_job" => Some(McpToolDescriptor {
            name: "cancel_ingest_job",
            description: "Cancel one queued or running ingest job. Already terminal jobs are rejected.",
            input_schema: json!({
                "type": "object",
                "required": ["jobId"],
                "properties": {
                    "jobId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Ingest job identifier."
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
        "list_ingest_jobs" => list_ingest_jobs(context, arguments).await,
        "get_ingest_job" => get_ingest_job(context, arguments).await,
        "retry_ingest_job" => retry_ingest_job(context, arguments).await,
        "cancel_ingest_job" => cancel_ingest_job(context, arguments).await,
        _ => return None,
    };
    Some(result)
}

async fn list_ingest_jobs(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpListIngestJobsRequest>(arguments.clone()) {
        Ok(args) => {
            let library = args.library.clone();
            match crate::services::mcp::access::list_ingest_jobs(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    let page_note =
                        if payload.next_cursor.is_some() { "; more pages available" } else { "" };
                    let summary = format!(
                        "Listed {} ingest job(s) for {library}{page_note}.",
                        payload.items.len()
                    );
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn get_ingest_job(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpGetIngestJobRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::get_ingest_job(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    let summary = format!(
                        "Ingest job {} is {} with {} attempt(s); {} stage event(s) loaded.",
                        payload.job.job_id,
                        payload.job.queue_state,
                        payload.attempts.len(),
                        payload.stage_events.len()
                    );
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn retry_ingest_job(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpRetryIngestJobRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::retry_ingest_job(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    let summary = format!(
                        "Job {} requeued in state {}.",
                        payload.job_id, payload.queue_state
                    );
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn cancel_ingest_job(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpCancelIngestJobRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::cancel_ingest_job(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    let summary = format!(
                        "Job {} canceled in state {}.",
                        payload.job_id, payload.queue_state
                    );
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}
