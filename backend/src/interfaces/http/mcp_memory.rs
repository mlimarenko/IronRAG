use std::error::Error as _;

use axum::{
    Json, body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use http_body_util::LengthLimitError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::mcp_memory::{McpAuditActionKind, McpMutationReceipt, McpSearchDocumentsResponse},
    interfaces::http::{
        auth::AuthContext,
        router_support::{ApiError, attach_request_id_header, ensure_or_generate_request_id},
    },
    services::audit_service::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
    services::mcp_memory::{
        McpAuditScope, McpCreateLibraryRequest, McpCreateWorkspaceRequest,
        McpGetMutationStatusRequest, McpListLibrariesRequest, McpReadDocumentRequest,
        McpSearchDocumentsRequest, McpUpdateDocumentRequest, McpUploadDocumentsRequest,
    },
    shared::file_extract::UploadAdmissionError,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpJsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpJsonRpcResponse {
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
struct McpJsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpServerInfo {
    name: &'static str,
    version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpToolDescriptor {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpToolResult {
    content: Vec<McpContentBlock>,
    structured_content: Value,
    is_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpContentBlock {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
}

pub(crate) async fn get_capabilities(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);
    let result = state.mcp_memory_services.memory.capability_snapshot(&auth, &state).await;

    let response = match result {
        Ok(capabilities) => {
            state
                .mcp_memory_services
                .memory
                .record_success_audit(
                    &auth,
                    &state,
                    &request_id,
                    McpAuditActionKind::CapabilitySnapshot,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    json!({
                        "route": "/v1/mcp/capabilities",
                        "visibleWorkspaceCount": capabilities.visible_workspace_count,
                        "visibleLibraryCount": capabilities.visible_library_count,
                    }),
                )
                .await;
            Json(serde_json::to_value(capabilities).unwrap_or_else(|_| json!({}))).into_response()
        }
        Err(error) => {
            state
                .mcp_memory_services
                .memory
                .record_error_audit(
                    &auth,
                    &state,
                    &request_id,
                    McpAuditActionKind::CapabilitySnapshot,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "route": "/v1/mcp/capabilities" }),
                )
                .await;
            error.into_response()
        }
    };

    with_request_id(response, &request_id)
}

pub(crate) async fn handle_mcp(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
) -> Response {
    let request_id = ensure_or_generate_request_id(request.headers());
    let request = match parse_mcp_jsonrpc_request(&state, request).await {
        Ok(request) => request,
        Err(response) => return with_request_id(Json(response).into_response(), &request_id),
    };
    if request.jsonrpc != "2.0" {
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

async fn parse_mcp_jsonrpc_request(
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

fn handle_resources_list(id: Option<Value>) -> McpJsonRpcResponse {
    success_response(id, json!({ "resources": [] }))
}

fn handle_resource_templates_list(id: Option<Value>) -> McpJsonRpcResponse {
    success_response(id, json!({ "resourceTemplates": [] }))
}

async fn handle_initialize(
    auth: &AuthContext,
    state: &AppState,
    request_id: &str,
    id: Option<Value>,
) -> McpJsonRpcResponse {
    match state.mcp_memory_services.memory.capability_snapshot(auth, state).await {
        Ok(capabilities) => {
            state
                .mcp_memory_services
                .memory
                .record_success_audit(
                    auth,
                    state,
                    request_id,
                    McpAuditActionKind::CapabilitySnapshot,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    json!({
                        "method": "initialize",
                        "visibleWorkspaceCount": capabilities.visible_workspace_count,
                        "visibleLibraryCount": capabilities.visible_library_count,
                    }),
                )
                .await;
            success_response(
                id,
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {
                        "tools": { "listChanged": false },
                        "resources": { "listChanged": false, "subscribe": false }
                    },
                    "serverInfo": McpServerInfo { name: "rustrag-mcp-memory", version: "0.1.0" },
                    "memoryCapabilities": capabilities,
                }),
            )
        }
        Err(error) => {
            state
                .mcp_memory_services
                .memory
                .record_error_audit(
                    auth,
                    state,
                    request_id,
                    McpAuditActionKind::CapabilitySnapshot,
                    McpAuditScope {
                        workspace_id: auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "method": "initialize" }),
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
    let tool_names = state.mcp_memory_services.memory.visible_tool_names(auth);
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
                        "workspaceId": { "type": "string", "format": "uuid" }
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
                            "items": { "type": "string", "format": "uuid" }
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
                        "documentId": { "type": "string", "format": "uuid" },
                        "mode": { "type": "string", "enum": ["full", "excerpt"] },
                        "startOffset": { "type": "integer", "minimum": 0 },
                        "length": { "type": "integer", "minimum": 1 },
                        "continuationToken": { "type": "string" }
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
                        "libraryId": { "type": "string", "format": "uuid" },
                        "idempotencyKey": { "type": "string" },
                        "documents": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "required": ["fileName", "contentBase64"],
                                "properties": {
                                    "fileName": { "type": "string" },
                                    "contentBase64": { "type": "string" },
                                    "mimeType": { "type": "string" },
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
                    "properties": {
                        "libraryId": { "type": "string", "format": "uuid" },
                        "documentId": { "type": "string", "format": "uuid" },
                        "operationKind": { "type": "string", "enum": ["append", "replace"] },
                        "idempotencyKey": { "type": "string" },
                        "appendedText": { "type": "string" },
                        "replacementFileName": { "type": "string" },
                        "replacementContentBase64": { "type": "string" },
                        "replacementMimeType": { "type": "string" }
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
                        "receiptId": { "type": "string", "format": "uuid" }
                    }
                }),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    state
        .mcp_memory_services
        .memory
        .record_success_audit(
            auth,
            state,
            request_id,
            McpAuditActionKind::CapabilitySnapshot,
            McpAuditScope { workspace_id: auth.workspace_id, library_id: None, document_id: None },
            json!({
                "method": "tools/list",
                "visibleToolCount": tools.len(),
            }),
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
                Ok(args) => {
                    match state.mcp_memory_services.memory.create_workspace(auth, state, args).await
                    {
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
                            state
                                .mcp_memory_services
                                .memory
                                .record_success_audit(
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
                            state
                                .mcp_memory_services
                                .memory
                                .record_error_audit(
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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
            Ok(args) => match state
                .mcp_memory_services
                .memory
                .create_library(auth, state, args.clone())
                .await
            {
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_success_audit(
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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
                state
                    .mcp_memory_services
                    .memory
                    .record_error_audit(
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
        "list_workspaces" => {
            match state.mcp_memory_services.memory.visible_workspaces(auth, state).await {
                Ok(payload) => {
                    state
                        .mcp_memory_services
                        .memory
                        .record_success_audit(
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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
            }
        }
        "list_libraries" => match parse_tool_args::<McpListLibrariesRequest>(parsed.arguments) {
            Ok(args) => match state
                .mcp_memory_services
                .memory
                .visible_libraries(auth, state, args.workspace_id)
                .await
            {
                Ok(payload) => {
                    state
                        .mcp_memory_services
                        .memory
                        .record_success_audit(
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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
                state
                    .mcp_memory_services
                    .memory
                    .record_error_audit(
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
                Ok(args) => match state
                    .mcp_memory_services
                    .memory
                    .search_documents(auth, state, args.clone())
                    .await
                {
                    Ok(payload) => {
                        state
                            .mcp_memory_services
                            .memory
                            .record_success_audit(
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
                        state
                            .mcp_memory_services
                            .memory
                            .record_error_audit(
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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
            Ok(args) => match state
                .mcp_memory_services
                .memory
                .read_document(auth, state, args.clone())
                .await
            {
                Ok(payload) => {
                    state
                        .mcp_memory_services
                        .memory
                        .record_success_audit(
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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
                state
                    .mcp_memory_services
                    .memory
                    .record_error_audit(
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
                Ok(args) => match state
                    .mcp_memory_services
                    .memory
                    .upload_documents(auth, state, args.clone())
                    .await
                {
                    Ok(payload) => {
                        state
                            .mcp_memory_services
                            .memory
                            .record_success_audit(
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
                        ok_tool_result("Document uploads accepted.", json!({ "receipts": payload }))
                    }
                    Err(error) => {
                        state
                            .mcp_memory_services
                            .memory
                            .record_error_audit(
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
                },
                Err(error) => {
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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
            Ok(args) => match state
                .mcp_memory_services
                .memory
                .update_document(auth, state, args.clone())
                .await
            {
                Ok(payload) => {
                    state
                        .mcp_memory_services
                        .memory
                        .record_success_audit(
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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
                state
                    .mcp_memory_services
                    .memory
                    .record_error_audit(
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
                Ok(args) => match state
                    .mcp_memory_services
                    .memory
                    .get_mutation_status(auth, state, args)
                    .await
                {
                    Ok(payload) => {
                        state
                            .mcp_memory_services
                            .memory
                            .record_success_audit(
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
                        state
                            .mcp_memory_services
                            .memory
                            .record_error_audit(
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
                    state
                        .mcp_memory_services
                        .memory
                        .record_error_audit(
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

fn parse_tool_args<T>(arguments: Value) -> Result<T, ApiError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(arguments).map_err(|error| {
        ApiError::invalid_mcp_tool_call(format!("invalid MCP tool arguments: {error}"))
    })
}

fn ok_tool_result(message: &str, structured_content: Value) -> McpToolResult {
    McpToolResult {
        content: vec![McpContentBlock { content_type: "text", text: message.to_string() }],
        structured_content,
        is_error: false,
    }
}

fn tool_error_result(error: ApiError) -> McpToolResult {
    McpToolResult {
        content: vec![McpContentBlock { content_type: "text", text: error.to_string() }],
        structured_content: json!({
            "errorKind": error.kind(),
            "message": error.to_string(),
        }),
        is_error: true,
    }
}

fn success_response(id: Option<Value>, result: Value) -> McpJsonRpcResponse {
    McpJsonRpcResponse { jsonrpc: "2.0", id, result: Some(result), error: None }
}

fn error_response(
    id: Option<Value>,
    code: i32,
    message: &str,
    data: Option<Value>,
) -> McpJsonRpcResponse {
    McpJsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(McpJsonRpcError { code, message: message.to_string(), data }),
    }
}

fn mcp_api_error_response(id: Option<Value>, error: ApiError) -> McpJsonRpcResponse {
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

fn with_request_id(mut response: Response, request_id: &str) -> Response {
    attach_request_id_header(response.headers_mut(), request_id);
    response
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
