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
use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_MCP_MEMORY_READ, POLICY_WORKSPACE_ADMIN,
        },
        router_support::{ApiError, attach_request_id_header, ensure_or_generate_request_id},
    },
    mcp_types::{
        McpAuditActionKind, McpAuditScope, McpCapabilitySnapshot, McpCreateLibraryRequest,
        McpCreateWorkspaceRequest, McpGetMutationStatusRequest, McpListLibrariesRequest,
        McpMutationReceipt, McpReadDocumentRequest, McpSearchDocumentsRequest,
        McpSearchDocumentsResponse, McpUpdateDocumentRequest, McpUploadDocumentsRequest,
    },
    services::{
        audit_service::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
        mcp_access, mcp_mutations,
    },
    shared::file_extract::UploadAdmissionError,
};

pub const MCP_JSONRPC_ROUTE: &str = "/mcp";
pub const MCP_CAPABILITIES_ROUTE: &str = "/mcp/capabilities";
pub const MCP_PUBLIC_JSONRPC_ROUTE: &str = "/v1/mcp";
pub const MCP_PUBLIC_CAPABILITIES_ROUTE: &str = "/v1/mcp/capabilities";
pub(super) const MCP_JSONRPC_VERSION: &str = "2.0";
pub(super) const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub(super) const MCP_SERVER_NAME: &str = "rustrag-mcp-memory";
pub(super) const MCP_SERVER_VERSION: &str = "0.1.0";

pub const MCP_CANONICAL_TOOL_NAMES: &[&str] = &[
    "list_workspaces",
    "list_libraries",
    "create_workspace",
    "create_library",
    "search_documents",
    "read_document",
    "upload_documents",
    "update_document",
    "get_mutation_status",
];

pub const MCP_CANONICAL_METHOD_NAMES: &[&str] =
    &["initialize", "resources/list", "resources/templates/list", "tools/list", "tools/call"];

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
pub(super) struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpToolResult {
    pub content: Vec<McpContentBlock>,
    pub structured_content: Value,
    pub is_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpContentBlock {
    #[serde(rename = "type")]
    pub content_type: &'static str,
    pub text: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(MCP_JSONRPC_ROUTE, post(handle_jsonrpc))
        .route(MCP_CAPABILITIES_ROUTE, get(get_capabilities))
}

fn visible_tool_names(auth: &AuthContext) -> Vec<String> {
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
    if auth.can_write_any_document_memory(POLICY_DOCUMENTS_WRITE) {
        tools.push("upload_documents".to_string());
    }
    if auth.can_write_any_document_memory(POLICY_DOCUMENTS_WRITE) {
        tools.push("update_document".to_string());
        tools.push("get_mutation_status".to_string());
    }
    tools
}

async fn capability_snapshot(
    auth: &AuthContext,
    state: &AppState,
) -> Result<McpCapabilitySnapshot, ApiError> {
    let workspaces = mcp_access::visible_workspaces(auth, state).await?;
    let libraries = mcp_access::visible_libraries(auth, state, None).await?;
    Ok(McpCapabilitySnapshot {
        token_id: auth.token_id,
        token_kind: auth.token_kind.clone(),
        workspace_scope: auth.workspace_id,
        visible_workspace_count: workspaces.len(),
        visible_library_count: libraries.len(),
        tools: visible_tool_names(auth),
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
            record_canonical_mcp_audit(
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
            record_canonical_mcp_audit(
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
        "tools/list" => handle_tools_list(&auth, &state, &request_id, request.id).await,
        "resources/list" => handle_resources_list(request.id),
        "resources/templates/list" => handle_resource_templates_list(request.id),
        "tools/call" => {
            handle_tools_call(&auth, &state, &request_id, request.id, request.params).await
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

pub(super) fn handle_resources_list(id: Option<Value>) -> McpJsonRpcResponse {
    success_response(id, json!({ "resources": [] }))
}

pub(super) fn handle_resource_templates_list(id: Option<Value>) -> McpJsonRpcResponse {
    success_response(id, json!({ "resourceTemplates": [] }))
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
            record_canonical_mcp_audit(
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
                        "tools": { "listChanged": false },
                        "resources": { "listChanged": false, "subscribe": false }
                    },
                    "serverInfo": McpServerInfo { name: MCP_SERVER_NAME, version: MCP_SERVER_VERSION },
                    "memoryCapabilities": capabilities,
                }),
            )
        }
        Err(error) => {
            record_canonical_mcp_audit(
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

async fn handle_tools_list(
    auth: &AuthContext,
    state: &AppState,
    request_id: &str,
    id: Option<Value>,
) -> McpJsonRpcResponse {
    let tool_names = visible_tool_names(auth);
    let tools = tool_names
        .into_iter()
        .filter_map(|name| match name.as_str() {
            "create_workspace" => Some(McpToolDescriptor {
                name: "create_workspace",
                description: "Create a workspace when the current token has workspace-admin rights.",
                input_schema: json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "slug": {
                            "type": "string",
                            "description": "Optional custom slug. If omitted, RustRAG derives a stable slug from the workspace name."
                        },
                        "name": { "type": "string" }
                    }
                }),
            }),
            "create_library" => Some(McpToolDescriptor {
                name: "create_library",
                description: "Create a library inside one authorized workspace when the token can write projects.",
                input_schema: json!({
                    "type": "object",
                    "required": ["workspaceId", "name"],
                    "properties": {
                        "workspaceId": { "type": "string", "format": "uuid" },
                        "slug": {
                            "type": "string",
                            "description": "Optional custom slug. If omitted, RustRAG derives a stable slug from the library name."
                        },
                        "name": { "type": "string" },
                        "description": { "type": "string" }
                    }
                }),
            }),
            "list_workspaces" => Some(McpToolDescriptor {
                name: "list_workspaces",
                description: "List workspaces visible to the current bearer token.",
                input_schema: json!({ "type": "object", "properties": {} }),
            }),
            "list_libraries" => Some(McpToolDescriptor {
                name: "list_libraries",
                description: "List visible libraries, optionally filtered to one visible workspace.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "workspaceId": {
                            "type": "string",
                            "format": "uuid",
                            "description": "Workspace UUID. Also accepts snake_case alias workspace_id."
                        }
                    }
                }),
            }),
            "search_documents" => Some(McpToolDescriptor {
                name: "search_documents",
                description: "Search authorized library memory and return document-level hits.",
                input_schema: json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string" },
                        "libraryIds": {
                            "type": "array",
                            "items": { "type": "string", "format": "uuid" },
                            "description": "Optional library UUID filter. Also accepts snake_case alias library_ids, or singular library_id for one library."
                        },
                        "limit": { "type": "integer", "minimum": 1 }
                    }
                }),
            }),
            "read_document" => Some(McpToolDescriptor {
                name: "read_document",
                description: "Read a full document or a wide excerpt using continuation tokens for large text.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "documentId": {
                            "type": "string",
                            "format": "uuid",
                            "description": "Document UUID. Also accepts snake_case alias document_id."
                        },
                        "mode": { "type": "string", "enum": ["full", "excerpt"] },
                        "startOffset": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Start character offset. Also accepts snake_case alias start_offset."
                        },
                        "length": { "type": "integer", "minimum": 1 },
                        "continuationToken": {
                            "type": "string",
                            "description": "Opaque token returned by a previous read. Also accepts snake_case alias continuation_token."
                        }
                    }
                }),
            }),
            "upload_documents" => Some(McpToolDescriptor {
                name: "upload_documents",
                description: "Upload one or more new documents into an authorized library and return mutation receipts.",
                input_schema: json!({
                    "type": "object",
                    "required": ["libraryId", "documents"],
                    "properties": {
                        "libraryId": {
                            "type": "string",
                            "format": "uuid",
                            "description": "Target library UUID. Also accepts snake_case alias library_id."
                        },
                        "idempotencyKey": {
                            "type": "string",
                            "description": "Caller-chosen dedupe key. Also accepts snake_case alias idempotency_key."
                        },
                        "documents": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "anyOf": [
                                    { "required": ["contentBase64"] },
                                    { "required": ["body"] }
                                ],
                                "properties": {
                                    "fileName": {
                                        "type": "string",
                                        "description": "Original file name. Optional for inline body uploads; autogenerated if omitted. Also accepts snake_case alias file_name."
                                    },
                                    "contentBase64": {
                                        "type": "string",
                                        "description": "Base64-encoded file payload for binary/file uploads. Also accepts snake_case alias content_base64."
                                    },
                                    "body": {
                                        "type": "string",
                                        "description": "Inline text body for agent-authored notes and snippets."
                                    },
                                    "sourceType": {
                                        "type": "string",
                                        "description": "Optional hint: use inline for text body uploads or file for base64 payload uploads. Also accepts snake_case alias source_type."
                                    },
                                    "sourceUri": {
                                        "type": "string",
                                        "description": "Optional logical source URI used to derive a default file name for inline uploads. Also accepts snake_case alias source_uri."
                                    },
                                    "mimeType": {
                                        "type": "string",
                                        "description": "Optional MIME type. Also accepts snake_case alias mime_type."
                                    },
                                    "title": { "type": "string" }
                                }
                            }
                        }
                    }
                }),
            }),
            "update_document" => Some(McpToolDescriptor {
                name: "update_document",
                description: "Append to or replace one logical document while preserving mutation receipts and idempotency.",
                input_schema: json!({
                    "type": "object",
                    "required": ["libraryId", "documentId", "operationKind"],
                    "allOf": [
                        {
                            "if": { "properties": { "operationKind": { "const": "append" } } },
                            "then": { "required": ["appendedText"] }
                        },
                        {
                            "if": { "properties": { "operationKind": { "const": "replace" } } },
                            "then": { "required": ["replacementFileName", "replacementContentBase64"] }
                        }
                    ],
                    "properties": {
                        "libraryId": {
                            "type": "string",
                            "format": "uuid",
                            "description": "Target library UUID. Also accepts snake_case alias library_id."
                        },
                        "documentId": {
                            "type": "string",
                            "format": "uuid",
                            "description": "Target document UUID. Also accepts snake_case alias document_id."
                        },
                        "operationKind": {
                            "type": "string",
                            "enum": ["append", "replace"],
                            "description": "Mutation kind. Also accepts snake_case alias operation_kind."
                        },
                        "idempotencyKey": {
                            "type": "string",
                            "description": "Caller-chosen dedupe key. Also accepts snake_case alias idempotency_key."
                        },
                        "appendedText": {
                            "type": "string",
                            "description": "Required for append. Also accepts snake_case alias appended_text."
                        },
                        "replacementFileName": {
                            "type": "string",
                            "description": "Required for replace. Also accepts snake_case alias replacement_file_name."
                        },
                        "replacementContentBase64": {
                            "type": "string",
                            "description": "Required for replace. Also accepts snake_case alias replacement_content_base64."
                        },
                        "replacementMimeType": {
                            "type": "string",
                            "description": "Optional for replace. Also accepts snake_case alias replacement_mime_type."
                        }
                    }
                }),
            }),
            "get_mutation_status" => Some(McpToolDescriptor {
                name: "get_mutation_status",
                description: "Read the current backend-visible lifecycle state for a previously accepted MCP mutation receipt.",
                input_schema: json!({
                    "type": "object",
                    "required": ["receiptId"],
                    "properties": {
                        "receiptId": {
                            "type": "string",
                            "format": "uuid",
                            "description": "Mutation receipt UUID. Also accepts snake_case alias receipt_id."
                        }
                    }
                }),
            }),
            _ => None,
        })
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

async fn handle_tools_call(
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

    let result = match parsed.name.as_str() {
        "create_workspace" => {
            match parse_tool_args::<McpCreateWorkspaceRequest>(parsed.arguments) {
                Ok(args) => match mcp_access::create_workspace(auth, state, args).await {
                    Ok(payload) => {
                        record_canonical_mcp_audit(
                            state,
                            auth,
                            request_id,
                            "catalog.workspace.create",
                            "succeeded",
                            Some(format!("workspace {} created", payload.name)),
                            Some(format!(
                                "principal {} created workspace {} via MCP",
                                auth.principal_id, payload.workspace_id
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
                            auth,
                            state,
                            request_id,
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
                            state,
                            auth,
                            request_id,
                            "catalog.workspace.create",
                            "rejected",
                            Some("workspace create denied".to_string()),
                            Some(format!(
                                "principal {} was denied workspace create via MCP",
                                auth.principal_id
                            )),
                            Vec::new(),
                        )
                        .await;
                        record_error_audit(
                            auth,
                            state,
                            request_id,
                            McpAuditActionKind::CreateWorkspace,
                            McpAuditScope::default(),
                            &error,
                            json!({ "tool": "create_workspace" }),
                        )
                        .await;
                        tool_error_result(error)
                    }
                },
                Err(error) => {
                    record_canonical_mcp_audit(
                        state,
                        auth,
                        request_id,
                        "catalog.workspace.create",
                        "rejected",
                        Some("workspace create payload rejected".to_string()),
                        Some(format!(
                            "principal {} submitted invalid MCP workspace create payload",
                            auth.principal_id
                        )),
                        Vec::new(),
                    )
                    .await;
                    record_error_audit(
                        auth,
                        state,
                        request_id,
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
        "create_library" => match parse_tool_args::<McpCreateLibraryRequest>(parsed.arguments) {
            Ok(args) => match mcp_access::create_library(auth, state, args.clone()).await {
                Ok(payload) => {
                    record_canonical_mcp_audit(
                        state,
                        auth,
                        request_id,
                        "catalog.library.create",
                        "succeeded",
                        Some(format!("library {} created", payload.name)),
                        Some(format!(
                            "principal {} created library {} via MCP",
                            auth.principal_id, payload.library_id
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
                        auth,
                        state,
                        request_id,
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
                    record_canonical_mcp_audit(
                        state,
                        auth,
                        request_id,
                        "catalog.library.create",
                        "rejected",
                        Some("library create denied".to_string()),
                        Some(format!(
                            "principal {} was denied library create for workspace {} via MCP",
                            auth.principal_id, args.workspace_id
                        )),
                        vec![AppendAuditEventSubjectCommand {
                            subject_kind: "workspace".to_string(),
                            subject_id: args.workspace_id,
                            workspace_id: Some(args.workspace_id),
                            library_id: None,
                            document_id: None,
                        }],
                    )
                    .await;
                    record_error_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::CreateLibrary,
                        McpAuditScope {
                            workspace_id: Some(args.workspace_id),
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
                    state,
                    auth,
                    request_id,
                    "catalog.library.create",
                    "rejected",
                    Some("library create payload rejected".to_string()),
                    Some(format!(
                        "principal {} submitted invalid MCP library create payload",
                        auth.principal_id
                    )),
                    Vec::new(),
                )
                .await;
                record_error_audit(
                    auth,
                    state,
                    request_id,
                    McpAuditActionKind::CreateLibrary,
                    McpAuditScope::default(),
                    &error,
                    json!({ "tool": "create_library" }),
                )
                .await;
                tool_error_result(error)
            }
        },
        "list_workspaces" => match mcp_access::visible_workspaces(auth, state).await {
            Ok(payload) => {
                record_success_audit(
                    auth,
                    state,
                    request_id,
                    McpAuditActionKind::ListWorkspaces,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
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
                    auth,
                    state,
                    request_id,
                    McpAuditActionKind::ListWorkspaces,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "list_workspaces" }),
                )
                .await;
                tool_error_result(error)
            }
        },
        "list_libraries" => match parse_tool_args::<McpListLibrariesRequest>(parsed.arguments) {
            Ok(args) => match mcp_access::visible_libraries(auth, state, args.workspace_id).await {
                Ok(payload) => {
                    record_success_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::ListLibraries,
                        McpAuditScope {
                            workspace_id: args.workspace_id.or(auth.workspace_id),
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
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::ListLibraries,
                        McpAuditScope {
                            workspace_id: args.workspace_id.or(auth.workspace_id),
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "list_libraries" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            },
            Err(error) => {
                record_error_audit(
                    auth,
                    state,
                    request_id,
                    McpAuditActionKind::ListLibraries,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "list_libraries" }),
                )
                .await;
                tool_error_result(error)
            }
        },
        "search_documents" => {
            match parse_tool_args::<McpSearchDocumentsRequest>(parsed.arguments) {
                Ok(args) => match mcp_access::search_documents(auth, state, args.clone()).await {
                    Ok(payload) => {
                        record_canonical_mcp_audit(
                        state,
                        auth,
                        request_id,
                        "agent.memory.search",
                        "succeeded",
                        Some(format!("completed MCP document search with {} hit(s)", payload.hits.len())),
                        Some(format!(
                            "principal {} completed MCP document search across {} library scope(s)",
                            auth.principal_id,
                            payload.library_ids.len()
                        )),
                        build_mcp_search_subjects(state, &payload),
                    )
                    .await;
                        record_success_audit(
                            auth,
                            state,
                            request_id,
                            McpAuditActionKind::SearchDocuments,
                            search_scope_from_response(auth, &payload),
                            json!({
                                "tool": "search_documents",
                                "query": payload.query,
                                "hitCount": payload.hits.len(),
                            }),
                        )
                        .await;
                        ok_tool_result("Document memory search completed.", json!(payload))
                    }
                    Err(error) => {
                        record_error_audit(
                            auth,
                            state,
                            request_id,
                            McpAuditActionKind::SearchDocuments,
                            search_scope_from_request(auth, args.library_ids.as_deref()),
                            &error,
                            json!({
                                "tool": "search_documents",
                                "query": args.query,
                            }),
                        )
                        .await;
                        tool_error_result(error)
                    }
                },
                Err(error) => {
                    record_error_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::SearchDocuments,
                        McpAuditScope {
                            workspace_id: auth.workspace_id,
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "search_documents" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        "read_document" => match parse_tool_args::<McpReadDocumentRequest>(parsed.arguments) {
            Ok(args) => match mcp_access::read_document(auth, state, args.clone()).await {
                Ok(payload) => {
                    record_canonical_mcp_audit(
                        state,
                        auth,
                        request_id,
                        "agent.memory.read",
                        "succeeded",
                        Some("MCP document read completed".to_string()),
                        Some(format!(
                            "principal {} read knowledge document {} via MCP",
                            auth.principal_id, payload.document_id
                        )),
                        vec![state.canonical_services.audit.knowledge_document_subject(
                            payload.document_id,
                            payload.workspace_id,
                            payload.library_id,
                        )],
                    )
                    .await;
                    record_success_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::ReadDocument,
                        McpAuditScope {
                            workspace_id: Some(payload.workspace_id),
                            library_id: Some(payload.library_id),
                            document_id: Some(payload.document_id),
                        },
                        json!({
                            "tool": "read_document",
                            "readMode": payload.read_mode,
                            "readabilityState": payload.readability_state,
                            "hasMore": payload.has_more,
                        }),
                    )
                    .await;
                    ok_tool_result("Document read completed.", json!(payload))
                }
                Err(error) => {
                    record_error_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::ReadDocument,
                        McpAuditScope {
                            workspace_id: auth.workspace_id,
                            library_id: None,
                            document_id: args.document_id,
                        },
                        &error,
                        json!({ "tool": "read_document" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            },
            Err(error) => {
                record_error_audit(
                    auth,
                    state,
                    request_id,
                    McpAuditActionKind::ReadDocument,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "read_document" }),
                )
                .await;
                tool_error_result(error)
            }
        },
        "upload_documents" => {
            match parse_tool_args::<McpUploadDocumentsRequest>(parsed.arguments) {
                Ok(args) => {
                    match mcp_mutations::upload_documents(auth, state, args.clone()).await {
                        Ok(payload) => {
                            let canonical_subjects =
                                build_mcp_mutation_subjects(state, &payload).await;
                            record_canonical_mcp_audit(
                                state,
                                auth,
                                request_id,
                                "agent.memory.upload",
                                "succeeded",
                                Some(format!("accepted {} MCP upload mutation(s)", payload.len())),
                                Some(format!(
                                    "principal {} accepted {} MCP upload mutation(s) in library {}",
                                    auth.principal_id,
                                    payload.len(),
                                    args.library_id
                                )),
                                canonical_subjects,
                            )
                            .await;
                            record_success_audit(
                                auth,
                                state,
                                request_id,
                                McpAuditActionKind::UploadDocuments,
                                mutation_scope_from_receipts(&payload).unwrap_or(McpAuditScope {
                                    workspace_id: auth.workspace_id,
                                    library_id: Some(args.library_id),
                                    document_id: None,
                                }),
                                json!({
                                    "tool": "upload_documents",
                                    "receiptCount": payload.len(),
                                }),
                            )
                            .await;
                            ok_tool_result(
                                "Document uploads accepted.",
                                json!({ "receipts": payload }),
                            )
                        }
                        Err(error) => {
                            record_error_audit(
                                auth,
                                state,
                                request_id,
                                McpAuditActionKind::UploadDocuments,
                                McpAuditScope {
                                    workspace_id: auth.workspace_id,
                                    library_id: Some(args.library_id),
                                    document_id: None,
                                },
                                &error,
                                json!({ "tool": "upload_documents" }),
                            )
                            .await;
                            tool_error_result(error)
                        }
                    }
                }
                Err(error) => {
                    record_error_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::UploadDocuments,
                        McpAuditScope {
                            workspace_id: auth.workspace_id,
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "upload_documents" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        "update_document" => match parse_tool_args::<McpUpdateDocumentRequest>(parsed.arguments) {
            Ok(args) => match mcp_mutations::update_document(auth, state, args.clone()).await {
                Ok(payload) => {
                    let canonical_subjects =
                        build_mcp_mutation_subjects(state, std::slice::from_ref(&payload)).await;
                    record_canonical_mcp_audit(
                        state,
                        auth,
                        request_id,
                        "agent.memory.update",
                        "succeeded",
                        Some(format!(
                            "accepted MCP document {:?} mutation",
                            payload.operation_kind
                        )),
                        Some(format!(
                            "principal {} accepted MCP mutation {} for document {:?}",
                            auth.principal_id, payload.receipt_id, payload.document_id
                        )),
                        canonical_subjects,
                    )
                    .await;
                    record_success_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::UpdateDocument,
                        McpAuditScope {
                            workspace_id: Some(payload.workspace_id),
                            library_id: Some(payload.library_id),
                            document_id: payload.document_id,
                        },
                        json!({
                            "tool": "update_document",
                            "operationKind": payload.operation_kind,
                        }),
                    )
                    .await;
                    ok_tool_result("Document mutation accepted.", json!(payload))
                }
                Err(error) => {
                    record_error_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::UpdateDocument,
                        McpAuditScope {
                            workspace_id: auth.workspace_id,
                            library_id: Some(args.library_id),
                            document_id: Some(args.document_id),
                        },
                        &error,
                        json!({ "tool": "update_document" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            },
            Err(error) => {
                record_error_audit(
                    auth,
                    state,
                    request_id,
                    McpAuditActionKind::UpdateDocument,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "update_document" }),
                )
                .await;
                tool_error_result(error)
            }
        },
        "get_mutation_status" => {
            match parse_tool_args::<McpGetMutationStatusRequest>(parsed.arguments) {
                Ok(args) => match mcp_mutations::get_mutation_status(auth, state, args).await {
                    Ok(payload) => {
                        record_success_audit(
                            auth,
                            state,
                            request_id,
                            McpAuditActionKind::GetMutationStatus,
                            McpAuditScope {
                                workspace_id: Some(payload.workspace_id),
                                library_id: Some(payload.library_id),
                                document_id: payload.document_id,
                            },
                            json!({
                                "tool": "get_mutation_status",
                                "status": payload.status,
                            }),
                        )
                        .await;
                        ok_tool_result("Mutation status loaded.", json!(payload))
                    }
                    Err(error) => {
                        record_error_audit(
                            auth,
                            state,
                            request_id,
                            McpAuditActionKind::GetMutationStatus,
                            McpAuditScope {
                                workspace_id: auth.workspace_id,
                                library_id: None,
                                document_id: None,
                            },
                            &error,
                            json!({ "tool": "get_mutation_status" }),
                        )
                        .await;
                        tool_error_result(error)
                    }
                },
                Err(error) => {
                    record_error_audit(
                        auth,
                        state,
                        request_id,
                        McpAuditActionKind::GetMutationStatus,
                        McpAuditScope {
                            workspace_id: auth.workspace_id,
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "get_mutation_status" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        _ => tool_error_result(ApiError::invalid_mcp_tool_call(format!(
            "unsupported MCP tool '{}'",
            parsed.name
        ))),
    };

    success_response(id, json!(result))
}

async fn record_canonical_mcp_audit(
    state: &AppState,
    auth: &AuthContext,
    request_id: &str,
    action_kind: &str,
    result_kind: &str,
    redacted_message: Option<String>,
    internal_message: Option<String>,
    subjects: Vec<AppendAuditEventSubjectCommand>,
) {
    let _ = state
        .canonical_services
        .audit
        .append_event(
            state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "mcp".to_string(),
                action_kind: action_kind.to_string(),
                request_id: Some(request_id.to_string()),
                trace_id: None,
                result_kind: result_kind.to_string(),
                redacted_message,
                internal_message,
                subjects,
            },
        )
        .await;
}

async fn record_success_audit(
    _auth: &AuthContext,
    _state: &AppState,
    _request_id: &str,
    _action_kind: McpAuditActionKind,
    _scope: McpAuditScope,
    _metadata_json: serde_json::Value,
) {
    // Canonical MCP audit now persists through `audit_event` only.
}

async fn record_error_audit(
    _auth: &AuthContext,
    _state: &AppState,
    _request_id: &str,
    _action_kind: McpAuditActionKind,
    _scope: McpAuditScope,
    _error: &ApiError,
    _metadata_json: serde_json::Value,
) {
    // Canonical MCP audit now persists through `audit_event` only.
}

async fn build_mcp_mutation_subjects(
    state: &AppState,
    receipts: &[McpMutationReceipt],
) -> Vec<AppendAuditEventSubjectCommand> {
    let mut subjects = Vec::new();
    for receipt in receipts {
        if let Some(document_id) = receipt.document_id {
            subjects.push(state.canonical_services.audit.knowledge_document_subject(
                document_id,
                receipt.workspace_id,
                receipt.library_id,
            ));
        }
        if let Ok(admission) =
            state.canonical_services.content.get_mutation_admission(state, receipt.receipt_id).await
            && let Some(async_operation_id) = admission.async_operation_id
        {
            subjects.push(state.canonical_services.audit.async_operation_subject(
                async_operation_id,
                receipt.workspace_id,
                receipt.library_id,
            ));
        }
    }
    subjects.sort_by(|left, right| {
        left.subject_kind
            .cmp(&right.subject_kind)
            .then_with(|| left.subject_id.cmp(&right.subject_id))
    });
    subjects.dedup_by(|left, right| {
        left.subject_kind == right.subject_kind && left.subject_id == right.subject_id
    });
    subjects
}

fn build_mcp_search_subjects(
    state: &AppState,
    payload: &McpSearchDocumentsResponse,
) -> Vec<AppendAuditEventSubjectCommand> {
    let mut subjects = Vec::new();
    for hit in &payload.hits {
        subjects.push(state.canonical_services.audit.knowledge_document_subject(
            hit.document_id,
            hit.workspace_id,
            hit.library_id,
        ));
    }
    subjects.sort_by(|left, right| {
        left.subject_kind
            .cmp(&right.subject_kind)
            .then_with(|| left.subject_id.cmp(&right.subject_id))
    });
    subjects.dedup_by(|left, right| {
        left.subject_kind == right.subject_kind && left.subject_id == right.subject_id
    });
    subjects
}

fn single_scope_id(values: &[Uuid]) -> Option<Uuid> {
    (values.len() == 1).then_some(values[0])
}

fn search_scope_from_request(auth: &AuthContext, library_ids: Option<&[Uuid]>) -> McpAuditScope {
    McpAuditScope {
        workspace_id: auth.workspace_id,
        library_id: library_ids.and_then(single_scope_id),
        document_id: None,
    }
}

fn search_scope_from_response(
    auth: &AuthContext,
    payload: &McpSearchDocumentsResponse,
) -> McpAuditScope {
    McpAuditScope {
        workspace_id: auth
            .workspace_id
            .or_else(|| payload.hits.first().map(|hit| hit.workspace_id)),
        library_id: single_scope_id(&payload.library_ids),
        document_id: None,
    }
}

fn mutation_scope_from_receipts(receipts: &[McpMutationReceipt]) -> Option<McpAuditScope> {
    receipts.first().map(|receipt| McpAuditScope {
        workspace_id: Some(receipt.workspace_id),
        library_id: Some(receipt.library_id),
        document_id: (receipts.len() == 1).then_some(receipt.document_id).flatten(),
    })
}
