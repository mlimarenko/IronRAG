use serde_json::{Value, json};

use crate::{
    mcp_types::{
        McpAuditActionKind, McpAuditScope, McpCreateLibraryRequest, McpCreateWorkspaceRequest,
        McpListLibrariesRequest,
    },
    services::iam::audit::AppendAuditEventSubjectCommand,
};

use super::super::{
    McpToolDescriptor, McpToolResult,
    audit::{record_canonical_mcp_audit, record_error_audit, record_success_audit},
    ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "create_workspace" => Some(McpToolDescriptor {
            name: "create_workspace",
            description: "Create a workspace when the current token has system-admin rights. Use this for workspace provisioning, not routine document ingestion.",
            input_schema: json!({
                "type": "object",
                "required": ["workspace"],
                "properties": {
                    "workspace": {
                        "type": "string",
                        "description": "Canonical workspace ref. This becomes the stable workspace slug agents use in later MCP calls."
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional human-readable display name shown in the UI. Defaults to the workspace ref."
                    }
                }
            }),
        }),
        "create_library" => Some(McpToolDescriptor {
            name: "create_library",
            description: "Create an empty library inside one authorized workspace. The returned library descriptor includes ingestionReadiness so agents can see immediately whether uploads are blocked by missing AI bindings.",
            input_schema: json!({
                "type": "object",
                "required": ["library"],
                "properties": {
                    "library": {
                        "type": "string",
                        "description": "Canonical fully-qualified library ref in the form '<workspace>/<library>'."
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional human-readable display name shown in the UI. Defaults to the library segment from the ref."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional operator-facing description for the library."
                    }
                }
            }),
        }),
        "list_workspaces" => Some(McpToolDescriptor {
            name: "list_workspaces",
            description: "List workspaces visible to the current bearer token. Call this first when the agent does not yet know which IronRAG workspace should be searched or modified.",
            input_schema: json!({ "type": "object", "properties": {} }),
        }),
        "list_libraries" => Some(McpToolDescriptor {
            name: "list_libraries",
            description: "List visible libraries, optionally filtered to one visible workspace. Each library descriptor includes ingestionReadiness so agents can detect missing upload prerequisites before calling upload_documents.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace": {
                        "type": "string",
                        "description": "Optional canonical workspace ref from list_workspaces."
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
        "create_workspace" => create_workspace(context, arguments).await,
        "create_library" => create_library(context, arguments).await,
        "list_workspaces" => list_workspaces(context).await,
        "list_libraries" => list_libraries(context, arguments).await,
        _ => return None,
    };
    Some(result)
}

async fn create_workspace(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpCreateWorkspaceRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::create_workspace(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    record_canonical_mcp_audit(
                        context.state,
                        context.auth,
                        context.request_id,
                        "catalog.workspace.create",
                        "succeeded",
                        Some(format!("workspace {} created", payload.name)),
                        Some(format!(
                            "principal {} created workspace {} via MCP",
                            context.auth.principal_id, payload.workspace_id
                        )),
                        vec![AppendAuditEventSubjectCommand {
                            subject_kind: "workspace".to_string(),
                            subject_id: payload.workspace_id,
                            workspace_id: Some(payload.workspace_id),
                            library_id: None,
                            document_id: None,
                        }],
                    )
                    .await;
                    record_success_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::CreateWorkspace,
                        McpAuditScope {
                            workspace_id: Some(payload.workspace_id),
                            library_id: None,
                            document_id: None,
                        },
                        json!({ "tool": "create_workspace" }),
                    )
                    .await;
                    ok_tool_result("Workspace created.", json!({ "workspace": payload }))
                }
                Err(error) => {
                    record_canonical_mcp_audit(
                        context.state,
                        context.auth,
                        context.request_id,
                        "catalog.workspace.create",
                        "rejected",
                        Some("workspace create denied".to_string()),
                        Some(format!(
                            "principal {} was denied workspace create via MCP",
                            context.auth.principal_id
                        )),
                        Vec::new(),
                    )
                    .await;
                    record_error_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::CreateWorkspace,
                        McpAuditScope::default(),
                        &error,
                        json!({ "tool": "create_workspace" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        Err(error) => {
            record_canonical_mcp_audit(
                context.state,
                context.auth,
                context.request_id,
                "catalog.workspace.create",
                "rejected",
                Some("workspace create payload rejected".to_string()),
                Some(format!(
                    "principal {} submitted invalid MCP workspace create payload",
                    context.auth.principal_id
                )),
                Vec::new(),
            )
            .await;
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::CreateWorkspace,
                McpAuditScope::default(),
                &error,
                json!({ "tool": "create_workspace" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn create_library(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpCreateLibraryRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::access::create_library(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                record_canonical_mcp_audit(
                    context.state,
                    context.auth,
                    context.request_id,
                    "catalog.library.create",
                    "succeeded",
                    Some(format!("library {} created", payload.name)),
                    Some(format!(
                        "principal {} created library {} via MCP",
                        context.auth.principal_id, payload.library_id
                    )),
                    vec![AppendAuditEventSubjectCommand {
                        subject_kind: "library".to_string(),
                        subject_id: payload.library_id,
                        workspace_id: Some(payload.workspace_id),
                        library_id: Some(payload.library_id),
                        document_id: None,
                    }],
                )
                .await;
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::CreateLibrary,
                    McpAuditScope {
                        workspace_id: Some(payload.workspace_id),
                        library_id: Some(payload.library_id),
                        document_id: None,
                    },
                    json!({ "tool": "create_library" }),
                )
                .await;
                ok_tool_result("Library created.", json!({ "library": payload }))
            }
            Err(error) => {
                let workspace_ref = args.library.split('/').next().unwrap_or_default();
                let workspace_scope =
                    crate::services::mcp::access::load_workspace_by_catalog_ref_for_discovery(
                        context.auth,
                        context.state,
                        workspace_ref,
                    )
                    .await
                    .ok();
                record_canonical_mcp_audit(
                    context.state,
                    context.auth,
                    context.request_id,
                    "catalog.library.create",
                    "rejected",
                    Some("library create denied".to_string()),
                    Some(format!(
                        "principal {} was denied library create for ref {} via MCP",
                        context.auth.principal_id, args.library
                    )),
                    workspace_scope
                        .iter()
                        .map(|workspace| AppendAuditEventSubjectCommand {
                            subject_kind: "workspace".to_string(),
                            subject_id: workspace.id,
                            workspace_id: Some(workspace.id),
                            library_id: None,
                            document_id: None,
                        })
                        .collect(),
                )
                .await;
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::CreateLibrary,
                    McpAuditScope {
                        workspace_id: workspace_scope.as_ref().map(|workspace| workspace.id),
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "create_library" }),
                )
                .await;
                tool_error_result(error)
            }
        },
        Err(error) => {
            record_canonical_mcp_audit(
                context.state,
                context.auth,
                context.request_id,
                "catalog.library.create",
                "rejected",
                Some("library create payload rejected".to_string()),
                Some(format!(
                    "principal {} submitted invalid MCP library create payload",
                    context.auth.principal_id
                )),
                Vec::new(),
            )
            .await;
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::CreateLibrary,
                McpAuditScope::default(),
                &error,
                json!({ "tool": "create_library" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn list_workspaces(context: ToolCallContext<'_>) -> McpToolResult {
    match crate::services::mcp::access::visible_workspaces(context.auth, context.state).await {
        Ok(payload) => {
            record_success_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::ListWorkspaces,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                json!({
                    "tool": "list_workspaces",
                    "workspaceCount": payload.len(),
                }),
            )
            .await;
            ok_tool_result("Visible workspaces loaded.", json!({ "workspaces": payload }))
        }
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::ListWorkspaces,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "list_workspaces" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn list_libraries(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpListLibrariesRequest>(arguments.clone()) {
        Ok(args) => {
            let workspace_scope = match args.workspace.as_deref() {
                Some(workspace_ref) => {
                    match crate::services::mcp::access::load_workspace_by_catalog_ref_for_discovery(
                        context.auth,
                        context.state,
                        workspace_ref,
                    )
                    .await
                    {
                        Ok(workspace) => Some(workspace.id),
                        Err(error) => {
                            record_error_audit(
                                context.auth,
                                context.state,
                                context.request_id,
                                McpAuditActionKind::ListLibraries,
                                McpAuditScope {
                                    workspace_id: context.auth.workspace_id,
                                    library_id: None,
                                    document_id: None,
                                },
                                &error,
                                json!({ "tool": "list_libraries" }),
                            )
                            .await;
                            return tool_error_result(error);
                        }
                    }
                }
                None => context.auth.workspace_id,
            };
            match crate::services::mcp::access::visible_libraries(
                context.auth,
                context.state,
                args.workspace.as_deref(),
            )
            .await
            {
                Ok(payload) => {
                    record_success_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::ListLibraries,
                        McpAuditScope {
                            workspace_id: workspace_scope,
                            library_id: None,
                            document_id: None,
                        },
                        json!({
                            "tool": "list_libraries",
                            "libraryCount": payload.len(),
                        }),
                    )
                    .await;
                    ok_tool_result("Visible libraries loaded.", json!({ "libraries": payload }))
                }
                Err(error) => {
                    record_error_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::ListLibraries,
                        McpAuditScope {
                            workspace_id: workspace_scope,
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "list_libraries" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::ListLibraries,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "list_libraries" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}
