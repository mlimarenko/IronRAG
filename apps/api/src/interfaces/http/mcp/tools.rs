use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE,
            POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN, POLICY_RUNTIME_READ, POLICY_WORKSPACE_ADMIN,
        },
        router_support::ApiError,
    },
};

use super::{
    McpJsonRpcResponse, McpToolCallParams, McpToolDescriptor, McpToolSurface,
    audit::record_canonical_mcp_audit, success_response, tool_error_result,
};
use documents::{READ_DOCUMENT_TOOL_NAME, SEARCH_DOCUMENTS_TOOL_NAME};

pub(crate) mod catalog;
pub(crate) mod documents;
pub(crate) mod graph;
pub(crate) mod grounded;
pub(crate) mod runtime;
pub(crate) mod web_ingest;

#[derive(Clone, Copy)]
pub(crate) struct ToolCallContext<'a> {
    pub auth: &'a AuthContext,
    pub state: &'a AppState,
    pub request_id: &'a str,
}

pub(crate) fn visible_tool_names(auth: &AuthContext, surface: McpToolSurface) -> Vec<String> {
    match surface {
        McpToolSurface::Answer => visible_answer_tool_names(auth),
        McpToolSurface::Diagnostics => visible_diagnostics_tool_names(auth),
    }
}

fn visible_answer_tool_names(auth: &AuthContext) -> Vec<String> {
    let mut tools = vec!["list_workspaces".to_string(), "list_libraries".to_string()];
    if auth.can_read_any_library_memory(POLICY_QUERY_RUN) {
        tools.push("grounded_answer".to_string());
    }
    tools
}

fn visible_diagnostics_tool_names(auth: &AuthContext) -> Vec<String> {
    let mut tools = vec!["list_workspaces".to_string(), "list_libraries".to_string()];
    if auth.can_read_any_library_memory(POLICY_QUERY_RUN) {
        tools.push("grounded_answer".to_string());
    }
    if auth.is_system_admin {
        tools.push("create_workspace".to_string());
    }
    if auth.can_admin_any_workspace(POLICY_WORKSPACE_ADMIN) {
        tools.push("create_library".to_string());
    }
    if auth.can_read_any_library_memory(POLICY_MCP_MEMORY_READ) {
        tools.push(SEARCH_DOCUMENTS_TOOL_NAME.to_string());
    }
    if auth.can_read_any_document_memory(POLICY_MCP_MEMORY_READ) {
        tools.push(READ_DOCUMENT_TOOL_NAME.to_string());
    }
    if auth.can_read_any_library_memory(POLICY_MCP_MEMORY_READ) {
        tools.push("list_documents".to_string());
    }
    if auth.can_write_any_document_memory(POLICY_DOCUMENTS_WRITE) {
        tools.push("upload_documents".to_string());
        tools.push("update_document".to_string());
        tools.push("delete_document".to_string());
        tools.push("get_mutation_status".to_string());
    }
    if auth.can_read_any_document_memory(POLICY_RUNTIME_READ) {
        tools.push("get_runtime_execution".to_string());
        tools.push("get_runtime_execution_trace".to_string());
    }
    if auth.can_write_any_library_memory(POLICY_LIBRARY_WRITE) {
        tools.push("submit_web_ingest_run".to_string());
        tools.push("cancel_web_ingest_run".to_string());
    }
    if auth.can_read_any_library_memory(POLICY_LIBRARY_READ) {
        tools.push("get_web_ingest_run".to_string());
        tools.push("list_web_ingest_run_pages".to_string());
    }
    if auth.can_read_any_library_memory(POLICY_MCP_MEMORY_READ) {
        tools.push("search_entities".to_string());
        tools.push("get_graph_topology".to_string());
        tools.push("list_relations".to_string());
        tools.push("get_communities".to_string());
    }
    tools
}

pub(super) async fn handle_tools_list(
    auth: &AuthContext,
    state: &AppState,
    request_id: &str,
    id: Option<Value>,
    surface: McpToolSurface,
) -> McpJsonRpcResponse {
    let tools = visible_tool_names(auth, surface)
        .into_iter()
        .filter_map(|name| descriptor_for(&name))
        .collect::<Vec<_>>();

    record_canonical_mcp_audit(
        state,
        auth,
        request_id,
        "mcp.tools.list",
        "succeeded",
        Some("MCP tools list returned.".to_string()),
        Some(format!("principal {} listed {} MCP tools", auth.principal_id, tools.len())),
        Vec::new(),
    )
    .await;

    success_response(id, json!({ "tools": tools }))
}

pub(super) async fn handle_tools_call(
    auth: &AuthContext,
    state: &AppState,
    request_id: &str,
    id: Option<Value>,
    params: Option<Value>,
    surface: McpToolSurface,
) -> McpJsonRpcResponse {
    let params_value = params.unwrap_or_else(|| json!({}));
    let parsed: McpToolCallParams = match serde_json::from_value(params_value) {
        Ok(parsed) => parsed,
        Err(error) => {
            return success_response(
                id,
                json!(tool_error_result(ApiError::invalid_mcp_tool_call(format!(
                    "invalid tools/call params: {error}"
                )))),
            );
        }
    };
    if !visible_tool_names(auth, surface).iter().any(|tool_name| tool_name == &parsed.name) {
        return success_response(
            id,
            json!(tool_error_result(ApiError::invalid_mcp_tool_call(format!(
                "tool '{}' is not available on the {} MCP surface",
                parsed.name,
                surface.label()
            )))),
        );
    }

    let context = ToolCallContext { auth, state, request_id };
    let result = if let Some(result) =
        catalog::call_tool(parsed.name.as_str(), context, &parsed.arguments).await
    {
        result
    } else if let Some(result) =
        documents::call_tool(parsed.name.as_str(), context, &parsed.arguments).await
    {
        result
    } else if let Some(result) =
        grounded::call_tool(parsed.name.as_str(), context, &parsed.arguments).await
    {
        result
    } else if let Some(result) =
        runtime::call_tool(parsed.name.as_str(), context, &parsed.arguments).await
    {
        result
    } else if let Some(result) =
        web_ingest::call_tool(parsed.name.as_str(), context, &parsed.arguments).await
    {
        result
    } else if let Some(result) =
        graph::call_tool(parsed.name.as_str(), context, &parsed.arguments).await
    {
        result
    } else {
        tool_error_result(ApiError::invalid_mcp_tool_call(format!(
            "unsupported MCP tool '{}'",
            parsed.name
        )))
    };

    success_response(id, json!(result))
}

fn descriptor_for(name: &str) -> Option<McpToolDescriptor> {
    catalog::descriptor(name)
        .or_else(|| documents::descriptor(name))
        .or_else(|| grounded::descriptor(name))
        .or_else(|| runtime::descriptor(name))
        .or_else(|| web_ingest::descriptor(name))
        .or_else(|| graph::descriptor(name))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use uuid::Uuid;

    use crate::{
        domains::iam::PrincipalKind,
        interfaces::http::{
            auth::{AuthContext, AuthGrant, AuthTokenKind},
            authorization::{POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN},
            mcp::{
                McpToolSurface,
                tools::{READ_DOCUMENT_TOOL_NAME, documents, visible_tool_names},
            },
        },
    };

    fn auth_with_query_and_memory_access() -> AuthContext {
        AuthContext {
            token_id: Uuid::nil(),
            principal_id: Uuid::nil(),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: Vec::new(),
            grants: vec![
                AuthGrant {
                    id: Uuid::from_u128(1),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_QUERY_RUN[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(2),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_MCP_MEMORY_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
            ],
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
        }
    }

    #[test]
    fn visible_tools_prioritize_grounded_answer_before_raw_search_tools() {
        let tools =
            visible_tool_names(&auth_with_query_and_memory_access(), McpToolSurface::Diagnostics);
        let grounded_index =
            tools.iter().position(|name| name == "grounded_answer").expect("grounded_answer");
        let search_index =
            tools.iter().position(|name| name == "search_documents").expect("search_documents");
        let read_index =
            tools.iter().position(|name| name == READ_DOCUMENT_TOOL_NAME).expect("read_document");

        assert!(grounded_index < search_index);
        assert!(grounded_index < read_index);
    }

    #[test]
    fn answer_surface_exposes_only_catalog_and_grounded_answer_tools() {
        let tools =
            visible_tool_names(&auth_with_query_and_memory_access(), McpToolSurface::Answer);

        assert!(tools.iter().any(|name| name == "grounded_answer"));
        assert!(tools.iter().any(|name| name == "list_workspaces"));
        assert!(tools.iter().any(|name| name == "list_libraries"));
        assert!(!tools.iter().any(|name| name == "list_documents"));
        assert!(!tools.iter().any(|name| name == "search_documents"));
        assert!(!tools.iter().any(|name| name == READ_DOCUMENT_TOOL_NAME));
    }

    #[test]
    fn list_documents_descriptor_keeps_change_summaries_on_grounded_answer() {
        let descriptor = documents::descriptor("list_documents").expect("list_documents");

        assert!(descriptor.description.contains("versioned change-summary questions"));
        assert!(descriptor.description.contains("call `grounded_answer` first"));
    }
}
