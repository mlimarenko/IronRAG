use serde_json::{Value, json};

use crate::{
    mcp_types::{
        McpCreateLibraryRequest, McpCreateWorkspaceRequest, McpListLibrariesRequest,
        McpUpdateLibraryRequest, McpUpdateWorkspaceRequest,
    },
    services::iam::audit::AppendAuditEventSubjectCommand,
};

use super::super::{
    McpToolDescriptor, McpToolResult, audit::record_canonical_mcp_audit, ok_tool_result,
    parse_tool_args, tool_error_result,
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
            description: "List visible libraries, optionally filtered to one visible workspace. Each library descriptor includes ingestionReadiness so agents can detect missing upload prerequisites before calling create_documents.",
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
        "update_workspace" => Some(McpToolDescriptor {
            name: "update_workspace",
            description: "Update a workspace's display name and/or lifecycle state when the current token has system-admin rights. Fields left unset keep their current value — you do not have to restate the lifecycle state just to rename a workspace.",
            input_schema: json!({
                "type": "object",
                "required": ["workspace"],
                "properties": {
                    "workspace": {
                        "type": "string",
                        "description": "Canonical workspace ref from list_workspaces."
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional new display name. Omit to keep the current display name."
                    },
                    "lifecycleState": {
                        "type": "string",
                        "enum": ["active", "archived"],
                        "description": "Optional new lifecycle state. Omit to keep the current lifecycle state."
                    }
                }
            }),
        }),
        "update_library" => Some(McpToolDescriptor {
            name: "update_library",
            description: "Update a library's display name, description, and/or lifecycle state. Fields left unset keep their current value. Knobs not exposed here (extraction prompt, includeDocumentHintInMcpAnswers) are always preserved unchanged.",
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
                        "description": "Optional new display name. Omit to keep the current display name."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional new operator-facing description. Omit to keep the current description; pass an empty string to clear it."
                    },
                    "lifecycleState": {
                        "type": "string",
                        "enum": ["active", "archived"],
                        "description": "Optional new lifecycle state. Omit to keep the current lifecycle state."
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
        "update_workspace" => update_workspace(context, arguments).await,
        "update_library" => update_library(context, arguments).await,
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
            tool_error_result(error)
        }
    }
}

async fn list_workspaces(context: ToolCallContext<'_>) -> McpToolResult {
    match crate::services::mcp::access::visible_workspaces(context.auth, context.state).await {
        Ok(payload) => {
            ok_tool_result("Visible workspaces loaded.", json!({ "workspaces": payload }))
        }
        Err(error) => tool_error_result(error),
    }
}

async fn update_workspace(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpUpdateWorkspaceRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::update_workspace(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    record_canonical_mcp_audit(
                        context.state,
                        context.auth,
                        context.request_id,
                        "catalog.workspace.update",
                        "succeeded",
                        Some(format!("workspace {} updated", payload.name)),
                        Some(format!(
                            "principal {} updated workspace {} via MCP",
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
                    ok_tool_result("Workspace updated.", json!({ "workspace": payload }))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn update_library(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpUpdateLibraryRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::update_library(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    record_canonical_mcp_audit(
                        context.state,
                        context.auth,
                        context.request_id,
                        "catalog.library.update",
                        "succeeded",
                        Some(format!("library {} updated", payload.name)),
                        Some(format!(
                            "principal {} updated library {} via MCP",
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
                    ok_tool_result("Library updated.", json!({ "library": payload }))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn list_libraries(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpListLibrariesRequest>(arguments.clone()) {
        Ok(args) => {
            if let Some(workspace_ref) = args.workspace.as_deref()
                && let Err(error) =
                    crate::services::mcp::access::load_workspace_by_catalog_ref_for_discovery(
                        context.auth,
                        context.state,
                        workspace_ref,
                    )
                    .await
            {
                return tool_error_result(error);
            }
            match crate::services::mcp::access::visible_libraries(
                context.auth,
                context.state,
                args.workspace.as_deref(),
            )
            .await
            {
                Ok(payload) => {
                    ok_tool_result("Visible libraries loaded.", json!({ "libraries": payload }))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}
