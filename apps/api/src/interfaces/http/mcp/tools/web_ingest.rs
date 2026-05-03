use serde_json::{Value, json};

use crate::mcp_types::{
    McpAuditActionKind, McpAuditScope, McpCancelWebIngestRunRequest, McpGetWebIngestRunRequest,
    McpListWebIngestRunPagesRequest, McpSubmitWebIngestRunRequest,
};

use super::super::{
    McpToolDescriptor, McpToolResult,
    audit::{
        build_mcp_web_ingest_subjects, record_canonical_mcp_audit, record_error_audit,
        record_success_audit,
    },
    ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "submit_web_ingest_run" => Some(McpToolDescriptor {
            name: "submit_web_ingest_run",
            description: "Submit a web ingest run for one seed URL. Default to mode single_page so only the submitted page is processed unless recursive_crawl is explicitly requested.",
            input_schema: json!({
                "type": "object",
                "required": ["library", "seedUrl", "mode", "urlFilter"],
                "properties": {
                    "library": {
                        "type": "string",
                        "description": "Target fully-qualified library ref from list_libraries."
                    },
                    "seedUrl": {
                        "type": "string",
                        "format": "uri",
                        "description": "Seed HTTP or HTTPS URL to ingest."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["single_page", "recursive_crawl"],
                        "description": "Use single_page to process only the submitted URL, or recursive_crawl to discover additional in-scope pages."
                    },
                    "boundaryPolicy": {
                        "type": "string",
                        "enum": ["same_host", "allow_external"],
                        "description": "Optional crawl boundary policy."
                    },
                    "maxDepth": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Optional crawl depth. single_page forces depth 0; recursive_crawl defaults to 3."
                    },
                    "maxPages": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional crawl budget."
                    },
                    "urlFilter": {
                        "type": "object",
                        "required": ["mode", "patterns"],
                        "description": "Complete URL filtering snapshot for this run. blocklist excludes matching URLs; allowlist processes only matching URLs.",
                        "properties": {
                            "mode": {
                                "type": "string",
                                "enum": ["blocklist", "allowlist"]
                            },
                            "patterns": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "required": ["kind", "value"],
                                    "properties": {
                                        "kind": {
                                            "type": "string",
                                            "enum": ["url_prefix", "path_prefix", "glob"]
                                        },
                                        "value": {
                                            "type": "string"
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "idempotencyKey": {
                        "type": "string",
                        "description": "Caller-chosen dedupe key."
                    }
                }
            }),
        }),
        "get_web_ingest_run" => Some(McpToolDescriptor {
            name: "get_web_ingest_run",
            description: "Load one web ingest run and return the same run truth, counts, failure code, and cancellation state used by REST and the documents workspace.",
            input_schema: json!({
                "type": "object",
                "required": ["runId"],
                "properties": {
                    "runId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Run UUID returned by submit_web_ingest_run."
                    }
                }
            }),
        }),
        "list_web_ingest_run_pages" => Some(McpToolDescriptor {
            name: "list_web_ingest_run_pages",
            description: "List candidate pages and outcomes for one web ingest run using the same candidate-state and reason-code vocabulary exposed by REST.",
            input_schema: json!({
                "type": "object",
                "required": ["runId"],
                "properties": {
                    "runId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Run UUID returned by submit_web_ingest_run."
                    }
                }
            }),
        }),
        "cancel_web_ingest_run" => Some(McpToolDescriptor {
            name: "cancel_web_ingest_run",
            description: "Request cancellation for an active web ingest run and return the updated receipt state, counts, failure code, and cancel acceptance timestamp.",
            input_schema: json!({
                "type": "object",
                "required": ["runId"],
                "properties": {
                    "runId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Run UUID returned by submit_web_ingest_run."
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
        "submit_web_ingest_run" => submit_web_ingest_run(context, arguments).await,
        "get_web_ingest_run" => get_web_ingest_run(context, arguments).await,
        "list_web_ingest_run_pages" => list_web_ingest_run_pages(context, arguments).await,
        "cancel_web_ingest_run" => cancel_web_ingest_run(context, arguments).await,
        _ => return None,
    };
    Some(result)
}

async fn submit_web_ingest_run(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpSubmitWebIngestRunRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::mutations::submit_web_ingest_run(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                let canonical_subjects =
                    build_mcp_web_ingest_subjects(context.state, std::slice::from_ref(&payload))
                        .await;
                record_canonical_mcp_audit(
                    context.state,
                    context.auth,
                    context.request_id,
                    "agent.memory.web_ingest.submit",
                    "succeeded",
                    Some(format!("accepted web ingest run {}", payload.run_id)),
                    Some(format!(
                        "principal {} accepted web ingest run {} in library {}",
                        context.auth.principal_id, payload.run_id, payload.library_id
                    )),
                    canonical_subjects,
                )
                .await;
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::SubmitWebIngestRun,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    json!({
                        "tool": "submit_web_ingest_run",
                        "runId": payload.run_id,
                        "mode": payload.mode,
                        "runState": payload.run_state,
                    }),
                )
                .await;
                ok_tool_result("Web ingest run accepted.", json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::SubmitWebIngestRun,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "submit_web_ingest_run" }),
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
                McpAuditActionKind::SubmitWebIngestRun,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "submit_web_ingest_run" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn get_web_ingest_run(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpGetWebIngestRunRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::mutations::get_web_ingest_run(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::GetWebIngestRun,
                    McpAuditScope {
                        workspace_id: Some(payload.workspace_id),
                        library_id: Some(payload.library_id),
                        document_id: None,
                    },
                    json!({
                        "tool": "get_web_ingest_run",
                        "runId": payload.run_id,
                        "runState": payload.run_state,
                    }),
                )
                .await;
                ok_tool_result("Web ingest run loaded.", json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::GetWebIngestRun,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "get_web_ingest_run", "runId": args.run_id }),
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
                McpAuditActionKind::GetWebIngestRun,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "get_web_ingest_run" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn list_web_ingest_run_pages(
    context: ToolCallContext<'_>,
    arguments: &Value,
) -> McpToolResult {
    match parse_tool_args::<McpListWebIngestRunPagesRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::mutations::list_web_ingest_run_pages(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                let scope = payload.first().map_or(
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    |page| McpAuditScope {
                        workspace_id: None,
                        library_id: None,
                        document_id: page.document_id,
                    },
                );
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::ListWebIngestRunPages,
                    scope,
                    json!({
                        "tool": "list_web_ingest_run_pages",
                        "runId": args.run_id,
                        "pageCount": payload.len(),
                    }),
                )
                .await;
                ok_tool_result("Web ingest run pages loaded.", json!({ "pages": payload }))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::ListWebIngestRunPages,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "list_web_ingest_run_pages", "runId": args.run_id }),
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
                McpAuditActionKind::ListWebIngestRunPages,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "list_web_ingest_run_pages" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn cancel_web_ingest_run(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpCancelWebIngestRunRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::mutations::cancel_web_ingest_run(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                let canonical_subjects =
                    build_mcp_web_ingest_subjects(context.state, std::slice::from_ref(&payload))
                        .await;
                record_canonical_mcp_audit(
                    context.state,
                    context.auth,
                    context.request_id,
                    "agent.memory.web_ingest.cancel",
                    "succeeded",
                    Some(format!("accepted cancel request for web ingest run {}", payload.run_id)),
                    Some(format!(
                        "principal {} accepted cancel request for web ingest run {}",
                        context.auth.principal_id, payload.run_id
                    )),
                    canonical_subjects,
                )
                .await;
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::CancelWebIngestRun,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: Some(payload.library_id),
                        document_id: None,
                    },
                    json!({
                        "tool": "cancel_web_ingest_run",
                        "runId": payload.run_id,
                        "runState": payload.run_state,
                    }),
                )
                .await;
                ok_tool_result("Web ingest run cancellation accepted.", json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::CancelWebIngestRun,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "cancel_web_ingest_run", "runId": args.run_id }),
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
                McpAuditActionKind::CancelWebIngestRun,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "cancel_web_ingest_run" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}
