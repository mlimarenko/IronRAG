use std::{error::Error as _, sync::LazyLock};

use axum::{
    Json, Router, body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, response::Builder as ResponseBuilder},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{NaiveDate, Utc};
use http_body_util::LengthLimitError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::error;

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
mod jsonrpc;
mod lease;
mod session;
mod session_transport;
#[cfg(test)]
mod tests;
pub(crate) mod tools;

use session_transport::{
    McpSessionAccessError, handle_delete_session, handle_get_stream, issue_registered_mcp_session,
    mcp_protocol_header_error_response, mcp_session_access_error_response,
    validate_and_accept_mcp_session,
};

// `grounded_answer_contract_payload{,_with_profile,_for_query_ir}` live in
// `tools::grounded` (plan §6.4 god-file split) but must stay reachable at
// this crate-external path: `tests/mcp_grounded_answer_contract.rs` imports
// them as `ironrag_backend::interfaces::http::mcp::grounded_answer_contract_payload`.
pub use tools::grounded::{
    grounded_answer_contract_payload, grounded_answer_contract_payload_for_query_ir,
    grounded_answer_contract_payload_with_profile,
};
#[cfg(test)]
use tools::grounded::{
    grounded_answer_must_preserve_spans_for_evidence,
    grounded_answer_must_preserve_spans_for_source_titles,
};

#[cfg(test)]
use lease::{
    MCP_DISTRIBUTED_CANCEL_SCRIPT, MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT,
    MCP_DISTRIBUTED_RELEASE_SCRIPT, MCP_TOOL_CALL_CANCEL_PENDING_MARKER,
    McpDistributedAdmissionOutcome, McpDistributedAdmissionResolution,
    mcp_cancel_marker_matches_generation, mcp_distributed_admission_outcome,
    mcp_distributed_admission_resolution, mcp_distributed_cancel_markers_cancel_call,
    mcp_request_id_key, mcp_scope_digest, mcp_tool_call_redis_ttl_millis,
    start_bounded_mcp_tool_call, start_bounded_mcp_tool_call_with_mutation,
};
use lease::{
    MCP_IN_FLIGHT_TOOL_CALLS, MCP_TOOL_CALL_COORDINATION_TIMEOUT, MCP_TOOL_CALL_DEADLINE,
    McpDistributedToolCallAdmission, McpInFlightRequestKey, McpInFlightToolCallRegistry,
    McpSessionScope, McpToolCallDispatch, handle_owned_mcp_tool_call,
    mark_distributed_mcp_tool_call_cancelled, mcp_distributed_coordination_keys,
    mcp_in_flight_request_key, mcp_session_scope, start_distributed_bounded_mcp_tool_call,
};

#[cfg(test)]
use jsonrpc::{
    McpAcceptPreference, accept_preference, cancel_mcp_in_flight_tool_call, finalize_mcp_response,
    finalize_mcp_tool_call_sse_response_with_interval,
};

pub const MCP_JSONRPC_ROUTE: &str = "/mcp";
pub const MCP_CAPABILITIES_ROUTE: &str = "/mcp/capabilities";
pub const MCP_DIAGNOSTICS_JSONRPC_ROUTE: &str = "/mcp/diagnostics";
pub const MCP_DIAGNOSTICS_CAPABILITIES_ROUTE: &str = "/mcp/diagnostics/capabilities";
pub const MCP_PUBLIC_JSONRPC_ROUTE: &str = "/v1/mcp";
pub const MCP_PUBLIC_CAPABILITIES_ROUTE: &str = "/v1/mcp/capabilities";
pub const MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE: &str = "/v1/mcp/diagnostics";
pub const MCP_PUBLIC_DIAGNOSTICS_CAPABILITIES_ROUTE: &str = "/v1/mcp/diagnostics/capabilities";
pub(super) const MCP_JSONRPC_VERSION: &str = "2.0";
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub(super) const MCP_SERVER_NAME: &str = "ironrag-mcp-memory";
pub(super) const MCP_SERVER_VERSION: &str = "0.1.0";

/// Exhaustive answer-surface tool names, independent of any caller's
/// grants. Derived from `tools::TOOL_REGISTRY` — the single shared
/// tool registry (plan §6.4) — rather than hand-maintained in parallel
/// with it, so this list cannot silently drift from the actual predicate
/// table. `LazyLock` (not `const`) because deriving it calls into the
/// registry at first use.
pub static MCP_ANSWER_TOOL_NAMES: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| tools::supported_tool_names(McpToolSurface::Answer));

/// Exhaustive diagnostics-surface tool names — see [`MCP_ANSWER_TOOL_NAMES`].
pub static MCP_DIAGNOSTICS_TOOL_NAMES: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| tools::supported_tool_names(McpToolSurface::Diagnostics));

fn build_mcp_response_or_internal_error(
    builder: ResponseBuilder,
    body: body::Body,
    response_kind: &'static str,
) -> Response {
    match builder.body(body) {
        Ok(response) => response,
        Err(error) => {
            error!(response_kind, ?error, "failed to build MCP response");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub const MCP_SUPPORTED_METHOD_NAMES: &[&str] = &["initialize", "tools/list", "tools/call"];

pub const MCP_SUPPORTED_NOTIFICATION_METHOD_NAMES: &[&str] =
    &["notifications/initialized", "notifications/cancelled"];

/// Session identifier header defined by the MCP Streamable HTTP transport
/// (spec 2025-11-25). The server sets it on the HTTP response to
/// `initialize`; the client MUST echo it on every subsequent request
/// belonging to that session. `IronRAG` stores only a digest and an
/// authenticated principal/token binding with a bounded TTL; raw session
/// values are never used in coordination keys or logs.
pub const MCP_SESSION_HEADER: &str = "mcp-session-id";

/// Protocol-version header defined by the MCP Streamable HTTP transport.
///
/// Clients MUST include this header on every non-`initialize` transport
/// request after a successful `initialize`.
pub const MCP_PROTOCOL_HEADER: &str = "mcp-protocol-version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpToolSurface {
    Answer,
    Diagnostics,
}

impl McpToolSurface {
    const fn jsonrpc_route(self) -> &'static str {
        match self {
            Self::Answer => MCP_PUBLIC_JSONRPC_ROUTE,
            Self::Diagnostics => MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE,
        }
    }

    const fn capabilities_route(self) -> &'static str {
        match self {
            Self::Answer => MCP_PUBLIC_CAPABILITIES_ROUTE,
            Self::Diagnostics => MCP_PUBLIC_DIAGNOSTICS_CAPABILITIES_ROUTE,
        }
    }

    fn supported_tool_names(self) -> &'static [&'static str] {
        match self {
            Self::Answer => MCP_ANSWER_TOOL_NAMES.as_slice(),
            Self::Diagnostics => MCP_DIAGNOSTICS_TOOL_NAMES.as_slice(),
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Diagnostics => "diagnostics",
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpJsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpInitializeParams {
    protocol_version: String,
    capabilities: Value,
    client_info: McpClientInfo,
}

#[derive(Debug, Deserialize)]
struct McpClientInfo {
    name: String,
    version: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpJsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

fn valid_initialize_params(params: Option<&Value>) -> bool {
    let Some(params) = params else {
        return false;
    };
    serde_json::from_value::<McpInitializeParams>(params.clone()).is_ok_and(|params| {
        params.protocol_version.len() == 10
            && NaiveDate::parse_from_str(&params.protocol_version, "%Y-%m-%d").is_ok()
            && params.capabilities.is_object()
            && !params.client_info.name.trim().is_empty()
            && !params.client_info.version.trim().is_empty()
    })
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct McpCapabilitiesHttpResponse {
    route: &'static str,
    json_rpc_route: &'static str,
    supported_request_method_names: &'static [&'static str],
    supported_notification_method_names: &'static [&'static str],
    supported_tool_names: &'static [&'static str],
    #[serde(flatten)]
    capabilities: McpCapabilitySnapshot,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpToolResult {
    pub content: Vec<McpContentBlock>,
    pub structured_content: Value,
    pub is_error: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpContentBlock {
    #[serde(rename = "type")]
    pub content_type: &'static str,
    pub text: String,
}

pub fn router() -> Router<AppState> {
    // IronRAG exposes two MCP surfaces:
    //   * `/mcp` — answer-first surface for ordinary user questions.
    //   * `/mcp/diagnostics` — explicit raw inspection / ops surface.
    //
    // Both use the same Streamable HTTP transport and handlers; the
    // only difference is the tool contract returned by `initialize`
    // + `tools/list`, which is parameterized by `McpToolSurface`.
    Router::new()
        .route(
            MCP_JSONRPC_ROUTE,
            post(handle_answer_jsonrpc).get(handle_get_stream).delete(handle_delete_session),
        )
        .route(MCP_CAPABILITIES_ROUTE, get(get_answer_capabilities))
        .route(
            MCP_DIAGNOSTICS_JSONRPC_ROUTE,
            post(handle_diagnostics_jsonrpc).get(handle_get_stream).delete(handle_delete_session),
        )
        .route(MCP_DIAGNOSTICS_CAPABILITIES_ROUTE, get(get_diagnostics_capabilities))
}

async fn capability_snapshot(
    auth: &AuthContext,
    state: &AppState,
    surface: McpToolSurface,
) -> Result<McpCapabilitySnapshot, ApiError> {
    let catalog = crate::services::mcp::access::visible_catalog_summary(auth, state).await?;
    let agent_vision_available = tools::document_image::any_library_agent_binding_supports_vision(
        state,
        &catalog.library_ids,
    )
    .await?;
    let tool_contract = tools::visible_tool_contract_with_capabilities(
        auth,
        surface,
        tools::ToolVisibilityCapabilities { agent_vision_available },
    )
    .map_err(|error| {
        ApiError::internal_with_log(error, "MCP capability tool contract preflight failed")
    })?;
    let visible_tools =
        tool_contract.descriptors.iter().map(|descriptor| descriptor.name.to_string()).collect();
    Ok(McpCapabilitySnapshot {
        // Full detail for the HTTP capabilities endpoint; the
        // initialize handler strips token_id / tools / generated_at
        // before embedding the snapshot in the JSON-RPC response so
        // the LLM context stays minimal.
        token_id: Some(auth.token_id),
        token_kind: auth.token_kind().to_string(),
        workspace_scope: auth.workspace_id,
        visible_workspace_count: catalog.workspace_count,
        visible_library_count: catalog.library_ids.len(),
        tools: visible_tools,
        tool_contract_version: tools::MCP_TOOL_CONTRACT_VERSION,
        tool_contract_hash: tool_contract.hash,
        generated_at: Some(Utc::now()),
    })
}

#[utoipa::path(
    get,
    path = "/v1/mcp/capabilities",
    tag = "automation",
    operation_id = "getMcpCapabilities",
    summary = "List answer MCP capabilities for the caller.",
    description = "Returns the answer-surface MCP tools visible to the authenticated principal. External MCP clients and the UI assistant setup flow can use this endpoint to discover whether the token can see workspaces, libraries, documents, graph tools, runtime tools, and `grounded_answer`. The response is authorization-scoped: workspace and library counts reflect only what the token can access, and the tool list omits tools disallowed for the token kind or policy.",
    responses(
        (status = 200, description = "MCP capability snapshot scoped to the caller's principal", body = crate::mcp_types::McpCapabilitySnapshot),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.mcp.get_capabilities", skip_all)]
pub async fn get_answer_capabilities(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    get_capabilities_for_surface(auth, State(state), headers, McpToolSurface::Answer).await
}

#[utoipa::path(
    get,
    path = "/v1/mcp/diagnostics/capabilities",
    tag = "automation",
    operation_id = "getMcpDiagnosticsCapabilities",
    summary = "List diagnostics MCP capabilities for the caller.",
    description = "Returns the diagnostics MCP tool surface visible to the authenticated principal. Use this before wiring operational agents that inspect runtime state, traces, or backend diagnostics rather than answering library-content questions. The diagnostics surface is separate from `/v1/mcp` so tokens can expose operational tools without expanding the normal answer surface.",
    responses(
        (status = 200, description = "Diagnostics MCP capability snapshot scoped to the caller's principal", body = crate::mcp_types::McpCapabilitySnapshot),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.mcp.get_diagnostics_capabilities", skip_all)]
pub async fn get_diagnostics_capabilities(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    get_capabilities_for_surface(auth, State(state), headers, McpToolSurface::Diagnostics).await
}

async fn get_capabilities_for_surface(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
    surface: McpToolSurface,
) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);
    let result = capability_snapshot(&auth, &state, surface).await;

    let mut response = match result {
        Ok(capabilities) => {
            audit::record_mcp_audit(
                &state,
                &auth,
                &request_id,
                audit::McpAuditEvent {
                    action_kind: "mcp.capabilities.read",
                    result_kind: "succeeded",
                    redacted_message: Some("MCP capabilities snapshot returned.".to_string()),
                    internal_message: Some(format!(
                        "principal {} fetched MCP capabilities snapshot",
                        auth.principal_id
                    )),
                    subjects: Vec::new(),
                },
            )
            .await;
            full_capabilities_response(surface, capabilities).into_response()
        }
        Err(error) => {
            audit::record_mcp_audit(
                &state,
                &auth,
                &request_id,
                audit::McpAuditEvent {
                    action_kind: "mcp.capabilities.read",
                    result_kind: "failed",
                    redacted_message: Some("MCP capabilities snapshot failed.".to_string()),
                    internal_message: Some(format!(
                        "principal {} failed to fetch MCP capabilities snapshot: {}",
                        auth.principal_id, error
                    )),
                    subjects: Vec::new(),
                },
            )
            .await;
            error.into_response()
        }
    };

    attach_request_id_header(response.headers_mut(), &request_id);
    response
}

#[utoipa::path(
    post,
    path = "/v1/mcp",
    tag = "automation",
    operation_id = "postMcpRequest",
    summary = "Execute answer-surface MCP JSON-RPC.",
    description = "Main JSON-RPC endpoint for external MCP clients and other agent runtimes. It implements MCP Streamable HTTP 2025-11-25: every well-formed initialize request negotiates to the required protocol version, and every subsequent request must send the exact negotiated `Mcp-Protocol-Version` header. It supports MCP initialization, tool listing, and tool invocation over the answer surface. Tools on this surface are read-oriented and designed for agentic question answering: catalog discovery, document search/read, graph inspection, runtime trace lookup, and `grounded_answer`. For user-facing answers, clients should let the agent choose tools from the listed schemas. Do not mechanically wrap every user message as a `grounded_answer` call; composite questions often need several document, graph, runtime, and grounded-answer probes before final synthesis.",
    request_body(content = serde_json::Value, content_type = "application/json", description = "JSON-RPC 2.0 request envelope. Typical methods are initialize, tools/list, and tools/call with method-specific params."),
    responses(
        (status = 200, description = "JSON-RPC response for the MCP tool surface", body = serde_json::Value),
        (status = 400, description = "Missing or unsupported MCP protocol-version header after initialization"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "MCP session is missing, expired, terminated, or foreign"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.mcp.handle_jsonrpc", skip_all)]
pub async fn handle_answer_jsonrpc(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
) -> Response {
    Box::pin(jsonrpc::handle_jsonrpc_for_surface(
        auth,
        State(state),
        request,
        McpToolSurface::Answer,
    ))
    .await
}

#[utoipa::path(
    post,
    path = "/v1/mcp/diagnostics",
    tag = "automation",
    operation_id = "postMcpDiagnosticsRequest",
    summary = "Execute diagnostics MCP JSON-RPC.",
    description = "JSON-RPC endpoint for operational MCP clients using the same MCP Streamable HTTP 2025-11-25 lifecycle as the answer surface. Every well-formed initialize request negotiates to the required protocol version, and every subsequent request must send the exact negotiated `Mcp-Protocol-Version` header. This surface is intended for debugging and automation that needs runtime state or traces, not for ordinary library question answering. Use `/v1/mcp/diagnostics/capabilities` first to check which diagnostic tools the token may call. Keep this surface separate from answer agents when a token should not expose operational inspection tools to normal chat flows.",
    request_body(content = serde_json::Value, content_type = "application/json", description = "JSON-RPC 2.0 request envelope for the diagnostics MCP tool surface."),
    responses(
        (status = 200, description = "JSON-RPC response for the diagnostics MCP tool surface", body = serde_json::Value),
        (status = 400, description = "Missing or unsupported MCP protocol-version header after initialization"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "MCP session is missing, expired, terminated, or foreign"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.mcp.handle_diagnostics_jsonrpc", skip_all)]
pub async fn handle_diagnostics_jsonrpc(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
) -> Response {
    Box::pin(jsonrpc::handle_jsonrpc_for_surface(
        auth,
        State(state),
        request,
        McpToolSurface::Diagnostics,
    ))
    .await
}

fn full_capabilities_response(
    surface: McpToolSurface,
    capabilities: McpCapabilitySnapshot,
) -> Json<McpCapabilitiesHttpResponse> {
    Json(McpCapabilitiesHttpResponse {
        route: surface.capabilities_route(),
        json_rpc_route: surface.jsonrpc_route(),
        supported_request_method_names: MCP_SUPPORTED_METHOD_NAMES,
        supported_notification_method_names: MCP_SUPPORTED_NOTIFICATION_METHOD_NAMES,
        supported_tool_names: surface.supported_tool_names(),
        capabilities,
    })
}

// The JSON-RPC error envelope is a serialized response body, not a live
// protocol value: boxing the `Err` arm keeps
// `clippy::result_large_err` quiet without touching the wire shape, which
// `finalize_mcp_response` consumes by value after dereferencing the box.
pub(super) async fn parse_mcp_jsonrpc_request(
    state: &AppState,
    request: Request,
) -> Result<McpJsonRpcRequest, Box<McpJsonRpcResponse>> {
    let body = body::to_bytes(request.into_body(), state.mcp_memory.max_request_body_bytes())
        .await
        .map_err(|error| {
            if error.source().and_then(|source| source.downcast_ref::<LengthLimitError>()).is_some()
            {
                let rejection = UploadAdmissionError::request_body_too_large(
                    state.mcp_memory.upload_max_size_mb,
                );
                return Box::new(error_response(
                    None,
                    -32600,
                    "invalid request",
                    Some(json!({
                        "errorKind": rejection.error_kind(),
                        "message": rejection.message(),
                        "details": rejection.details(),
                    })),
                ));
            }

            Box::new(error_response(
                None,
                -32603,
                "internal error",
                Some(json!({
                    "errorKind": "request_body_read_failed",
                    "message": format!("failed to read MCP request body: {error}"),
                })),
            ))
        })?;

    serde_json::from_slice(&body).map_err(|error| {
        Box::new(error_response(
            None,
            -32700,
            "parse error",
            Some(json!({
                "errorKind": "invalid_json",
                "message": format!("invalid JSON-RPC request body: {error}"),
            })),
        ))
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
    // The text block is the only part of a tool result that text-only MCP
    // clients (opencode, Claude Code, Codex) surface to the model: they read
    // `content` as-is and fall back to `structuredContent` only when `content`
    // is empty. A summary-only text therefore looks like an empty result to
    // those clients, so the payload is appended to the text while
    // `structuredContent` stays for clients that consume it directly.
    let text = if structured_content.is_null() {
        message.to_string()
    } else {
        match serde_json::to_string_pretty(&structured_content) {
            Ok(payload) => format!("{message}\n{payload}"),
            Err(_) => message.to_string(),
        }
    };
    McpToolResult {
        content: vec![McpContentBlock { content_type: "text", text }],
        structured_content,
        is_error: false,
    }
}

pub(super) fn tool_error_result(error: ApiError) -> McpToolResult {
    let error_kind = error.kind();
    let message = error.to_string();
    if error_kind == "internal" {
        // An internal fault handed to an MCP client carries no detail by
        // design, so without this line the only record of it is on the client
        // side. This logs the exit, not the construction site: it says a tool
        // call failed internally, not which expression produced the error.
        tracing::error!(error_kind, "MCP tool call failed with an internal error");
    }
    let mut structured_content = json!({
        "errorKind": error_kind,
        "message": message,
        "retryable": false,
    });
    if let Some((retry_after_ms, repair_hint)) = mcp_tool_error_retry_guidance(error_kind) {
        structured_content["retryable"] = json!(true);
        structured_content["repairHint"] = json!(repair_hint);
        if let Some(retry_after_ms) = retry_after_ms {
            structured_content["retryAfterMs"] = json!(retry_after_ms);
        }
    }
    McpToolResult {
        content: vec![McpContentBlock { content_type: "text", text: message }],
        structured_content,
        is_error: true,
    }
}

fn mcp_tool_error_retry_guidance(error_kind: &str) -> Option<(Option<u64>, &'static str)> {
    match error_kind {
        "document_projection_converging" => Some((Some(500), "retry_same_request")),
        "query_deadline_exceeded" | "query_retrieval_unavailable" => {
            Some((Some(1_000), "retry_same_request"))
        }
        "query_provider_unavailable" => Some((Some(2_000), "retry_same_request")),
        "query_binding_unavailable" => Some((None, "restore_query_binding")),
        _ => None,
    }
}

pub(super) const fn success_response(id: Option<Value>, result: Value) -> McpJsonRpcResponse {
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

fn mcp_initialize_result(capabilities: McpCapabilitySnapshot) -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": McpServerInfo { name: MCP_SERVER_NAME, version: MCP_SERVER_VERSION },
        "instructions": crate::services::mcp::agent_policy::instructions(),
        "memoryCapabilities": capabilities,
    })
}

async fn handle_initialize(
    auth: &AuthContext,
    state: &AppState,
    request_id: &str,
    id: Option<Value>,
    surface: McpToolSurface,
) -> McpJsonRpcResponse {
    match capability_snapshot(auth, state, surface).await {
        Ok(mut capabilities) => {
            audit::record_mcp_audit(
                state,
                auth,
                request_id,
                audit::McpAuditEvent {
                    action_kind: "mcp.initialize",
                    result_kind: "succeeded",
                    redacted_message: Some("MCP initialize completed.".to_string()),
                    internal_message: Some(format!(
                        "principal {} initialized MCP session",
                        auth.principal_id
                    )),
                    subjects: Vec::new(),
                },
            )
            .await;
            // Strip fields the LLM doesn't need. The full tool name
            // list is already in `tools/list`; token_id and
            // generated_at are pure noise in the agent's context.
            capabilities.token_id = None;
            capabilities.tools.clear();
            capabilities.generated_at = None;
            success_response(id, mcp_initialize_result(capabilities))
        }
        Err(error) => {
            audit::record_mcp_audit(
                state,
                auth,
                request_id,
                audit::McpAuditEvent {
                    action_kind: "mcp.initialize",
                    result_kind: "failed",
                    redacted_message: Some("MCP initialize failed.".to_string()),
                    internal_message: Some(format!(
                        "principal {} failed to initialize MCP session: {}",
                        auth.principal_id, error
                    )),
                    subjects: Vec::new(),
                },
            )
            .await;
            mcp_api_error_response(id, error)
        }
    }
}
