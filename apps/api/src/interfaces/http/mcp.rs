use std::error::Error as _;

use axum::{
    Json, Router, body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use http_body_util::LengthLimitError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        router_support::{ApiError, attach_request_id_header, ensure_or_generate_request_id},
    },
    mcp_types::McpCapabilitySnapshot,
    shared::extraction::file_extract::UploadAdmissionError,
};

mod audit;
pub(crate) mod tools;

pub mod agent_bridge;

pub const MCP_JSONRPC_ROUTE: &str = "/mcp";
pub const MCP_CAPABILITIES_ROUTE: &str = "/mcp/capabilities";
pub const MCP_PUBLIC_JSONRPC_ROUTE: &str = "/v1/mcp";
pub const MCP_PUBLIC_CAPABILITIES_ROUTE: &str = "/v1/mcp/capabilities";
pub(super) const MCP_JSONRPC_VERSION: &str = "2.0";
pub(super) const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub(super) const MCP_SERVER_NAME: &str = "ironrag-mcp-memory";
pub(super) const MCP_SERVER_VERSION: &str = "0.1.0";

pub const MCP_CANONICAL_TOOL_NAMES: &[&str] = &[
    "list_workspaces",
    "list_libraries",
    "create_workspace",
    "create_library",
    "search_documents",
    "read_document",
    "list_documents",
    "upload_documents",
    "update_document",
    "delete_document",
    "get_mutation_status",
    "get_runtime_execution",
    "get_runtime_execution_trace",
    "submit_web_ingest_run",
    "get_web_ingest_run",
    "list_web_ingest_run_pages",
    "cancel_web_ingest_run",
    "search_entities",
    "get_graph_topology",
    "list_relations",
    "get_communities",
];

pub const MCP_CANONICAL_METHOD_NAMES: &[&str] = &["initialize", "tools/list", "tools/call"];

pub const MCP_CANONICAL_NOTIFICATION_METHOD_NAMES: &[&str] = &["notifications/initialized"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpJsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpJsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<McpJsonRpcError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpJsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpCapabilitiesHttpResponse {
    route: &'static str,
    json_rpc_route: &'static str,
    canonical_method_names: &'static [&'static str],
    canonical_notification_method_names: &'static [&'static str],
    canonical_tool_names: &'static [&'static str],
    #[serde(flatten)]
    capabilities: McpCapabilitySnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpToolResult {
    pub content: Vec<McpContentBlock>,
    pub structured_content: Value,
    pub is_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpContentBlock {
    #[serde(rename = "type")]
    pub content_type: &'static str,
    pub text: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(MCP_JSONRPC_ROUTE, post(handle_jsonrpc))
        .route(MCP_CAPABILITIES_ROUTE, get(get_capabilities))
}

async fn capability_snapshot(
    auth: &AuthContext,
    state: &AppState,
) -> Result<McpCapabilitySnapshot, ApiError> {
    let workspaces = crate::services::mcp::access::visible_workspaces(auth, state).await?;
    let libraries = crate::services::mcp::access::visible_libraries(auth, state, None).await?;
    Ok(McpCapabilitySnapshot {
        token_id: auth.token_id,
        token_kind: auth.token_kind().to_string(),
        workspace_scope: auth.workspace_id,
        visible_workspace_count: workspaces.len(),
        visible_library_count: libraries.len(),
        tools: tools::visible_tool_names(auth),
        generated_at: Utc::now(),
    })
}

async fn get_capabilities(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);
    let result = capability_snapshot(&auth, &state).await;

    let mut response = match result {
        Ok(capabilities) => {
            audit::record_canonical_mcp_audit(
                &state,
                &auth,
                &request_id,
                "mcp.capabilities.read",
                "succeeded",
                Some("MCP capabilities snapshot returned.".to_string()),
                Some(format!("principal {} fetched MCP capabilities snapshot", auth.principal_id)),
                Vec::new(),
            )
            .await;
            canonical_capabilities_response(capabilities).into_response()
        }
        Err(error) => {
            audit::record_canonical_mcp_audit(
                &state,
                &auth,
                &request_id,
                "mcp.capabilities.read",
                "failed",
                Some("MCP capabilities snapshot failed.".to_string()),
                Some(format!(
                    "principal {} failed to fetch MCP capabilities snapshot: {}",
                    auth.principal_id, error
                )),
                Vec::new(),
            )
            .await;
            error.into_response()
        }
    };

    attach_request_id_header(response.headers_mut(), &request_id);
    response
}

async fn handle_jsonrpc(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
) -> Response {
    let request_id = ensure_or_generate_request_id(request.headers());
    let request = match parse_mcp_jsonrpc_request(&state, request).await {
        Ok(request) => request,
        Err(response) => return with_request_id(Json(response).into_response(), &request_id),
    };
    if request.jsonrpc != MCP_JSONRPC_VERSION {
        let response = error_response(
            request.id,
            -32600,
            "invalid request",
            Some(json!({ "errorKind": "invalid_jsonrpc_version" })),
        );
        return with_request_id(Json(response).into_response(), &request_id);
    }

    if request.id.is_none() && request.method.starts_with("notifications/") {
        return with_request_id(StatusCode::ACCEPTED.into_response(), &request_id);
    }

    let response = match request.method.as_str() {
        "initialize" => handle_initialize(&auth, &state, &request_id, request.id).await,
        "tools/list" => tools::handle_tools_list(&auth, &state, &request_id, request.id).await,
        "tools/call" => {
            tools::handle_tools_call(&auth, &state, &request_id, request.id, request.params).await
        }
        _ => error_response(
            request.id,
            -32601,
            "method not found",
            Some(json!({ "errorKind": "unsupported_method" })),
        ),
    };

    with_request_id(Json(response).into_response(), &request_id)
}

fn canonical_capabilities_response(
    capabilities: McpCapabilitySnapshot,
) -> Json<McpCapabilitiesHttpResponse> {
    Json(McpCapabilitiesHttpResponse {
        route: MCP_PUBLIC_CAPABILITIES_ROUTE,
        json_rpc_route: MCP_PUBLIC_JSONRPC_ROUTE,
        canonical_method_names: MCP_CANONICAL_METHOD_NAMES,
        canonical_notification_method_names: MCP_CANONICAL_NOTIFICATION_METHOD_NAMES,
        canonical_tool_names: MCP_CANONICAL_TOOL_NAMES,
        capabilities,
    })
}

pub(super) async fn parse_mcp_jsonrpc_request(
    state: &AppState,
    request: Request,
) -> Result<McpJsonRpcRequest, McpJsonRpcResponse> {
    let body = body::to_bytes(request.into_body(), state.mcp_memory.max_request_body_bytes())
        .await
        .map_err(|error| {
            if error.source().and_then(|source| source.downcast_ref::<LengthLimitError>()).is_some()
            {
                let rejection = UploadAdmissionError::request_body_too_large(
                    state.mcp_memory.upload_max_size_mb,
                );
                return error_response(
                    None,
                    -32600,
                    "invalid request",
                    Some(json!({
                        "errorKind": rejection.error_kind(),
                        "message": rejection.message(),
                        "details": rejection.details(),
                    })),
                );
            }

            error_response(
                None,
                -32603,
                "internal error",
                Some(json!({
                    "errorKind": "request_body_read_failed",
                    "message": format!("failed to read MCP request body: {error}"),
                })),
            )
        })?;

    serde_json::from_slice(&body).map_err(|error| {
        error_response(
            None,
            -32700,
            "parse error",
            Some(json!({
                "errorKind": "invalid_json",
                "message": format!("invalid JSON-RPC request body: {error}"),
            })),
        )
    })
}

pub(super) fn parse_tool_args<T>(arguments: Value) -> Result<T, ApiError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(arguments).map_err(|error| {
        ApiError::invalid_mcp_tool_call(format!("invalid MCP tool arguments: {error}"))
    })
}

pub(super) fn ok_tool_result(message: &str, structured_content: Value) -> McpToolResult {
    McpToolResult {
        content: vec![McpContentBlock { content_type: "text", text: message.to_string() }],
        structured_content,
        is_error: false,
    }
}

pub(super) fn tool_error_result(error: ApiError) -> McpToolResult {
    McpToolResult {
        content: vec![McpContentBlock { content_type: "text", text: error.to_string() }],
        structured_content: json!({
            "errorKind": error.kind(),
            "message": error.to_string(),
        }),
        is_error: true,
    }
}

pub(super) fn success_response(id: Option<Value>, result: Value) -> McpJsonRpcResponse {
    McpJsonRpcResponse { jsonrpc: MCP_JSONRPC_VERSION, id, result: Some(result), error: None }
}

pub(super) fn error_response(
    id: Option<Value>,
    code: i32,
    message: &str,
    data: Option<Value>,
) -> McpJsonRpcResponse {
    McpJsonRpcResponse {
        jsonrpc: MCP_JSONRPC_VERSION,
        id,
        result: None,
        error: Some(McpJsonRpcError { code, message: message.to_string(), data }),
    }
}

pub(super) fn mcp_api_error_response(id: Option<Value>, error: ApiError) -> McpJsonRpcResponse {
    let code = match error {
        ApiError::BadRequest(_)
        | ApiError::InvalidMcpToolCall(_)
        | ApiError::InvalidContinuationToken(_) => -32602,
        ApiError::Unauthorized | ApiError::InaccessibleMemoryScope(_) => -32001,
        ApiError::NotFound(_) => -32004,
        _ => -32603,
    };
    error_response(
        id,
        code,
        &error.to_string(),
        Some(json!({
            "errorKind": error.kind(),
            "message": error.to_string(),
        })),
    )
}

pub(super) fn with_request_id(mut response: Response, request_id: &str) -> Response {
    attach_request_id_header(response.headers_mut(), request_id);
    response
}

async fn handle_initialize(
    auth: &AuthContext,
    state: &AppState,
    request_id: &str,
    id: Option<Value>,
) -> McpJsonRpcResponse {
    match capability_snapshot(auth, state).await {
        Ok(capabilities) => {
            audit::record_canonical_mcp_audit(
                state,
                auth,
                request_id,
                "mcp.initialize",
                "succeeded",
                Some("MCP initialize completed.".to_string()),
                Some(format!("principal {} initialized MCP session", auth.principal_id)),
                Vec::new(),
            )
            .await;
            success_response(
                id,
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": McpServerInfo { name: MCP_SERVER_NAME, version: MCP_SERVER_VERSION },
                    "memoryCapabilities": capabilities,
                }),
            )
        }
        Err(error) => {
            audit::record_canonical_mcp_audit(
                state,
                auth,
                request_id,
                "mcp.initialize",
                "failed",
                Some("MCP initialize failed.".to_string()),
                Some(format!(
                    "principal {} failed to initialize MCP session: {}",
                    auth.principal_id, error
                )),
                Vec::new(),
            )
            .await;
            mcp_api_error_response(id, error)
        }
    }
}
