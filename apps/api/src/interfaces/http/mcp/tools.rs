use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE,
            POLICY_MCP_MEMORY_READ, POLICY_RUNTIME_READ, POLICY_WORKSPACE_ADMIN,
        },
        router_support::ApiError,
    },
};

use super::{
    McpJsonRpcResponse, McpToolCallParams, McpToolDescriptor, audit::record_canonical_mcp_audit,
    success_response, tool_error_result,
};

pub(crate) mod catalog;
pub(crate) mod documents;
pub(crate) mod graph;
pub(crate) mod runtime;
pub(crate) mod web_ingest;

#[derive(Clone, Copy)]
pub(crate) struct ToolCallContext<'a> {
    pub auth: &'a AuthContext,
    pub state: &'a AppState,
    pub request_id: &'a str,
}

pub(crate) fn visible_tool_names(auth: &AuthContext) -> Vec<String> {
    let mut tools = vec!["list_workspaces".to_string(), "list_libraries".to_string()];
    if auth.is_system_admin {
        tools.push("create_workspace".to_string());
    }
    if auth.can_admin_any_workspace(POLICY_WORKSPACE_ADMIN) {
        tools.push("create_library".to_string());
    }
    if auth.can_read_any_library_memory(POLICY_MCP_MEMORY_READ) {
        tools.push("search_documents".to_string());
    }
    if auth.can_read_any_document_memory(POLICY_MCP_MEMORY_READ) {
        tools.push("read_document".to_string());
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
) -> McpJsonRpcResponse {
    let tools = visible_tool_names(auth)
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
        .or_else(|| runtime::descriptor(name))
        .or_else(|| web_ingest::descriptor(name))
        .or_else(|| graph::descriptor(name))
}
