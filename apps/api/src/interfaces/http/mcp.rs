use std::{collections::BTreeSet, error::Error as _, time::Duration};

use axum::{
    Json, Router, body,
    extract::{Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, StatusCode, header,
        response::Builder as ResponseBuilder,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use futures::stream::StreamExt;
use http_body_util::LengthLimitError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::error;
use uuid::Uuid;

/// Interval between SSE keep-alive comments emitted on the idle
/// `GET /v1/mcp` stream. 25 s sits comfortably below every proxy
/// idle-read timeout we care about (nginx default 60 s, the gateway's
/// default 75 s) so the connection stays warm without generating
/// meaningful traffic. mcp-remote only stops its reconnect loop when
/// the stream stays alive past the handshake.
const MCP_GET_STREAM_KEEPALIVE: Duration = Duration::from_secs(25);
const GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT: usize = 128;
const GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS: usize = 240;
const GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT: usize = 24;
const GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK: i32 = 32;

use crate::{
    app::state::AppState,
    domains::query_ir::literal_text_is_identifier_shaped,
    interfaces::http::{
        auth::AuthContext,
        router_support::{ApiError, attach_request_id_header, ensure_or_generate_request_id},
    },
    mcp_types::McpCapabilitySnapshot,
    shared::extraction::file_extract::UploadAdmissionError,
};

mod audit;
pub(crate) mod tools;

pub const MCP_JSONRPC_ROUTE: &str = "/mcp";
pub const MCP_CAPABILITIES_ROUTE: &str = "/mcp/capabilities";
pub const MCP_DIAGNOSTICS_JSONRPC_ROUTE: &str = "/mcp/diagnostics";
pub const MCP_DIAGNOSTICS_CAPABILITIES_ROUTE: &str = "/mcp/diagnostics/capabilities";
pub const MCP_PUBLIC_JSONRPC_ROUTE: &str = "/v1/mcp";
pub const MCP_PUBLIC_CAPABILITIES_ROUTE: &str = "/v1/mcp/capabilities";
pub const MCP_PUBLIC_DIAGNOSTICS_JSONRPC_ROUTE: &str = "/v1/mcp/diagnostics";
pub const MCP_PUBLIC_DIAGNOSTICS_CAPABILITIES_ROUTE: &str = "/v1/mcp/diagnostics/capabilities";
pub(super) const MCP_JSONRPC_VERSION: &str = "2.0";
pub(super) const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub(super) const MCP_SERVER_NAME: &str = "ironrag-mcp-memory";
pub(super) const MCP_SERVER_VERSION: &str = "0.1.0";

pub const MCP_ANSWER_TOOL_NAMES: &[&str] = &[
    "list_workspaces",
    "list_libraries",
    "grounded_answer",
    "search_documents",
    "read_document",
    "view_document_image",
    "list_documents",
    "search_entities",
    "get_graph_topology",
    "list_relations",
    "get_communities",
    "get_runtime_execution",
    "get_runtime_execution_trace",
    "get_web_ingest_run",
    "list_web_ingest_run_pages",
];

pub const MCP_DIAGNOSTICS_TOOL_NAMES: &[&str] = &[
    "list_workspaces",
    "list_libraries",
    "create_workspace",
    "create_library",
    "search_documents",
    "read_document",
    "view_document_image",
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
    "grounded_answer",
];

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

pub const MCP_CANONICAL_METHOD_NAMES: &[&str] = &["initialize", "tools/list", "tools/call"];

pub const MCP_CANONICAL_NOTIFICATION_METHOD_NAMES: &[&str] = &["notifications/initialized"];

/// Session identifier header defined by the MCP Streamable HTTP transport
/// (spec 2025-06-18). The server sets it on the HTTP response to
/// `initialize`; the client MUST echo it on every subsequent request
/// belonging to that session. IronRAG is stateless between requests —
/// the header is generated for protocol compliance but the server does
/// not validate or correlate sessions across calls.
pub const MCP_SESSION_HEADER: &str = "mcp-session-id";

/// Protocol-version header defined by the MCP Streamable HTTP transport.
/// Clients MUST include this header on non-`initialize` requests after a
/// successful `initialize`. IronRAG tolerates its absence for
/// compatibility with simpler clients.
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

    const fn canonical_tool_names(self) -> &'static [&'static str] {
        match self {
            Self::Answer => MCP_ANSWER_TOOL_NAMES,
            Self::Diagnostics => MCP_DIAGNOSTICS_TOOL_NAMES,
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

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
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
    // IronRAG exposes two canonical MCP surfaces:
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

/// `GET /v1/mcp` — server-initiated SSE stream per MCP Streamable HTTP.
///
/// Spec 2025-06-18 lets the server either refuse the GET with 405 or
/// open an SSE stream. IronRAG emits no server-initiated
/// notifications today, so the stream is effectively idle — but it
/// must stay *open* and be kept alive, otherwise mcp-remote style
/// clients interpret an immediate close as "stream broken" and
/// reopen the GET every ~300 ms in a tight loop. That reconnect
/// storm was burning gateway CPU, polluting access logs, and
/// starving the same Tokio runtime that serves the actual
/// `POST /v1/mcp` tool calls.
///
/// The stream now:
///   1. Emits one `: ready` comment so the parser has something to
///      consume on the first read.
///   2. Emits `: keep-alive` SSE comments every
///      `MCP_GET_STREAM_KEEPALIVE` seconds so every intervening
///      proxy, hyper's write buffer, and the client's read loop all
///      see traffic before any idle timeout fires.
///   3. Keeps going forever, ending only when the client disconnects
///      (axum/hyper drops the stream on TCP close) or the runtime
///      shuts down.
///
/// SSE comments (`:`-prefix lines) are ignored by every compliant
/// SSE parser, so the client never sees a synthetic "message" — the
/// channel stays semantically silent while the transport stays alive.
///
/// Auth is intentionally *not* required on this handler: some
/// bundled clients open the stream before propagating the session's
/// Bearer, and a 401 here was a prior fatal mode. The handler
/// discloses nothing beyond the presence of an idle SSE endpoint.
// openapi-skip: MCP SSE keep-alive transport is shared by both MCP surfaces and has no JSON response contract.
#[tracing::instrument(level = "debug", name = "http.mcp.get_stream", skip_all)]
async fn handle_get_stream(headers: HeaderMap) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);

    // Initial ready frame followed by an infinite heartbeat stream.
    // We chain two streams rather than write a stateful generator so
    // the ordering ("ready first, then keep-alive forever") is
    // structurally obvious and impossible to reorder by accident.
    let ready = futures::stream::once(async {
        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b": ready\n\n"))
    });
    let heartbeat = futures::stream::unfold((), |()| async {
        tokio::time::sleep(MCP_GET_STREAM_KEEPALIVE).await;
        Some((Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b": keep-alive\n\n")), ()))
    });
    let stream = ready.chain(heartbeat);

    let mut response = build_mcp_response_or_internal_error(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
            .header(header::CONNECTION, "keep-alive")
            // X-Accel-Buffering: no tells nginx/traefik style proxies to
            // flush bytes as they arrive instead of buffering the stream —
            // without this the `: ready` comment can sit in a proxy buffer
            // for 30+ seconds, re-triggering client-side reconnect loops.
            .header(HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no")),
        body::Body::from_stream(stream),
        "mcp_get_stream",
    );
    attach_request_id_header(response.headers_mut(), &request_id);
    response
}

/// `DELETE /v1/mcp` — client-requested session termination per MCP
/// Streamable HTTP. IronRAG is stateless between requests (no session
/// store, no pending streams), so termination is a no-op; we always
/// respond 200 OK so cleanup flows succeed. Auth is optional for the
/// same reason as `handle_get_stream` — clients may issue DELETE during
/// shutdown with a stale or missing header and the cleanup flow must
/// still terminate cleanly on the client side.
// openapi-skip: MCP session cleanup is a shared no-op transport hook rather than a resource operation.
#[tracing::instrument(level = "debug", name = "http.mcp.delete_session", skip_all)]
async fn handle_delete_session(headers: HeaderMap) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);
    let mut response = StatusCode::OK.into_response();
    attach_request_id_header(response.headers_mut(), &request_id);
    response
}

async fn capability_snapshot(
    auth: &AuthContext,
    state: &AppState,
    surface: McpToolSurface,
) -> Result<McpCapabilitySnapshot, ApiError> {
    // Issue the workspace and library queries concurrently and derive
    // BOTH snapshots from one library load. The old path did:
    //   1. visible_workspaces (internally loops N times over libs)
    //   2. visible_libraries(None) — a second full load
    // For a stack with 2 workspaces and ~10 libraries that was 4-5
    // serialized Postgres round-trips per capability probe. This
    // collapses to exactly 2 concurrent queries.
    let (workspaces, libraries) =
        crate::services::mcp::access::visible_catalog(auth, state).await?;
    Ok(McpCapabilitySnapshot {
        // Full detail for the HTTP capabilities endpoint; the
        // initialize handler strips token_id / tools / generated_at
        // before embedding the snapshot in the JSON-RPC response so
        // the LLM context stays minimal.
        token_id: Some(auth.token_id),
        token_kind: auth.token_kind().to_string(),
        workspace_scope: auth.workspace_id,
        visible_workspace_count: workspaces.len(),
        visible_library_count: libraries.len(),
        tools: tools::visible_tool_names(auth, surface),
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
            canonical_capabilities_response(surface, capabilities).into_response()
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

#[utoipa::path(
    post,
    path = "/v1/mcp",
    tag = "automation",
    operation_id = "postMcpRequest",
    summary = "Execute answer-surface MCP JSON-RPC.",
    description = "Main JSON-RPC endpoint for external MCP clients and other agent runtimes. It supports MCP initialization, tool listing, and tool invocation over the answer surface. Tools on this surface are read-oriented and designed for agentic question answering: catalog discovery, document search/read, graph inspection, runtime trace lookup, and `grounded_answer`. For user-facing answers, clients should let the agent choose tools from the listed schemas. Do not mechanically wrap every user message as a `grounded_answer` call; composite questions often need several document, graph, runtime, and grounded-answer probes before final synthesis.",
    request_body(content = serde_json::Value, content_type = "application/json", description = "JSON-RPC 2.0 request envelope. Typical methods are initialize, tools/list, and tools/call with method-specific params."),
    responses(
        (status = 200, description = "JSON-RPC response for the MCP tool surface", body = serde_json::Value),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.mcp.handle_jsonrpc", skip_all)]
pub async fn handle_answer_jsonrpc(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
) -> Response {
    handle_jsonrpc_for_surface(auth, State(state), request, McpToolSurface::Answer).await
}

#[utoipa::path(
    post,
    path = "/v1/mcp/diagnostics",
    tag = "automation",
    operation_id = "postMcpDiagnosticsRequest",
    summary = "Execute diagnostics MCP JSON-RPC.",
    description = "JSON-RPC endpoint for operational MCP clients. This surface is intended for debugging and automation that needs runtime state or traces, not for ordinary library question answering. Use `/v1/mcp/diagnostics/capabilities` first to check which diagnostic tools the token may call. Keep this surface separate from answer agents when a token should not expose operational inspection tools to normal chat flows.",
    request_body(content = serde_json::Value, content_type = "application/json", description = "JSON-RPC 2.0 request envelope for the diagnostics MCP tool surface."),
    responses(
        (status = 200, description = "JSON-RPC response for the diagnostics MCP tool surface", body = serde_json::Value),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.mcp.handle_diagnostics_jsonrpc", skip_all)]
pub async fn handle_diagnostics_jsonrpc(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
) -> Response {
    handle_jsonrpc_for_surface(auth, State(state), request, McpToolSurface::Diagnostics).await
}

async fn handle_jsonrpc_for_surface(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
    surface: McpToolSurface,
) -> Response {
    let request_id = ensure_or_generate_request_id(request.headers());
    let accept = accept_preference(request.headers());
    let request = match parse_mcp_jsonrpc_request(&state, request).await {
        Ok(request) => request,
        Err(response) => {
            return finalize_mcp_response(response, accept, None, &request_id);
        }
    };
    if request.jsonrpc != MCP_JSONRPC_VERSION {
        let response = error_response(
            request.id,
            -32600,
            "invalid request",
            Some(json!({ "errorKind": "invalid_jsonrpc_version" })),
        );
        return finalize_mcp_response(response, accept, None, &request_id);
    }

    // Notifications carry no `id`; per MCP Streamable HTTP the server
    // acknowledges them with a bare 202 Accepted and no body.
    if request.id.is_none() && request.method.starts_with("notifications/") {
        return with_request_id(StatusCode::ACCEPTED.into_response(), &request_id);
    }

    let is_initialize = request.method == "initialize";
    let session_id = is_initialize.then(|| Uuid::now_v7().as_hyphenated().to_string());
    let response = match request.method.as_str() {
        "initialize" => handle_initialize(&auth, &state, &request_id, request.id, surface).await,
        "tools/list" => {
            tools::handle_tools_list(&auth, &state, &request_id, request.id, surface).await
        }
        "tools/call" => {
            tools::handle_tools_call(
                &auth,
                &state,
                &request_id,
                request.id,
                request.params,
                surface,
            )
            .await
        }
        _ => error_response(
            request.id,
            -32601,
            "method not found",
            Some(json!({ "errorKind": "unsupported_method" })),
        ),
    };

    finalize_mcp_response(response, accept, session_id.as_deref(), &request_id)
}

/// Content-negotiated view of the client's `Accept` header. Clients that
/// follow the MCP Streamable HTTP spec include both
/// `application/json` and `text/event-stream`; the server picks the
/// one it prefers to emit. Clients that omit `Accept` or send `*/*`
/// get the default JSON representation.
#[derive(Debug, Clone, Copy)]
enum McpAcceptPreference {
    Json,
    EventStream,
}

fn accept_preference(headers: &HeaderMap) -> McpAcceptPreference {
    // We render SSE only when the client asks for it explicitly. This
    // keeps curl/debugging friendly (default = JSON) while remaining
    // spec-compliant for SDK clients that advertise
    // `Accept: application/json, text/event-stream` on every request.
    let accept_header =
        headers.get(header::ACCEPT).and_then(|value| value.to_str().ok()).unwrap_or("");
    let wants_event_stream = accept_header
        .split(',')
        .map(|segment| segment.split(';').next().unwrap_or("").trim())
        .any(|segment| segment.eq_ignore_ascii_case("text/event-stream"));
    let wants_json = accept_header.is_empty()
        || accept_header
            .split(',')
            .map(|segment| segment.split(';').next().unwrap_or("").trim())
            .any(|segment| {
                segment.eq_ignore_ascii_case("application/json")
                    || segment.eq_ignore_ascii_case("application/*")
                    || segment == "*/*"
            });
    if wants_event_stream && !wants_json {
        McpAcceptPreference::EventStream
    } else if wants_event_stream {
        // When both are acceptable, honour the client's explicit
        // SSE request — agents that advertise it usually keep the
        // stream open for progress / notifications on long tool calls.
        McpAcceptPreference::EventStream
    } else {
        McpAcceptPreference::Json
    }
}

fn finalize_mcp_response(
    payload: McpJsonRpcResponse,
    accept: McpAcceptPreference,
    session_id: Option<&str>,
    request_id: &str,
) -> Response {
    let body_json = serde_json::to_string(&payload).unwrap_or_else(|error| {
        // Serialization of a known-small Serialize struct cannot
        // realistically fail; fall back to a hand-rolled JSON-RPC
        // error frame so we still emit valid JSON-RPC on the wire.
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32603,\"message\":\"internal serialization error: {}\"}}}}",
            error
        )
    });
    let mut response = match accept {
        McpAcceptPreference::Json => build_mcp_response_or_internal_error(
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json"),
            body::Body::from(body_json),
            "mcp_json",
        ),
        McpAcceptPreference::EventStream => {
            // Single-event SSE response. MCP Streamable HTTP treats
            // POST replies as short-lived streams: one `message`
            // event carrying the JSON-RPC frame, then the server
            // may close immediately. We do not keep the stream open
            // because IronRAG emits no progress notifications — the
            // client receives the final frame and the connection
            // ends.
            let sse_body = format!("event: message\ndata: {body_json}\n\n");
            build_mcp_response_or_internal_error(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
                    .header(header::CONNECTION, "keep-alive"),
                body::Body::from(sse_body),
                "mcp_sse",
            )
        }
    };
    if let Some(sid) = session_id {
        if let Ok(value) = HeaderValue::from_str(sid) {
            response.headers_mut().insert(HeaderName::from_static(MCP_SESSION_HEADER), value);
        }
    }
    attach_request_id_header(response.headers_mut(), request_id);
    response
}

fn canonical_capabilities_response(
    surface: McpToolSurface,
    capabilities: McpCapabilitySnapshot,
) -> Json<McpCapabilitiesHttpResponse> {
    Json(McpCapabilitiesHttpResponse {
        route: surface.capabilities_route(),
        json_rpc_route: surface.jsonrpc_route(),
        canonical_method_names: MCP_CANONICAL_METHOD_NAMES,
        canonical_notification_method_names: MCP_CANONICAL_NOTIFICATION_METHOD_NAMES,
        canonical_tool_names: surface.canonical_tool_names(),
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

/// Builds the JSON-serializable MCP `grounded_answer` tool result from
/// the same assistant execution detail returned by the UI query API.
///
/// The live MCP handler calls `grounded_answer_tool_result` directly.
/// This public JSON form gives integration tests a DB-free contract path
/// for snapshotting the MCP wrapper without duplicating the production
/// serializer. It is a test contract surface, not a stable application API.
#[doc(hidden)]
#[must_use]
pub fn grounded_answer_contract_payload(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> Value {
    json!(grounded_answer_tool_result(answer_text, execution_detail))
}

pub(crate) fn grounded_answer_tool_result(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> McpToolResult {
    ok_tool_result(
        &grounded_answer_human_text(answer_text),
        grounded_answer_structured_content(answer_text, execution_detail),
    )
}

fn grounded_answer_human_text(answer_text: &str) -> String {
    if answer_text.is_empty() {
        "The grounded-answer pipeline returned no answer text (execution may have failed or degraded). Inspect runtimeExecutionId via get_runtime_execution_trace for details.".to_string()
    } else {
        answer_text.to_string()
    }
}

fn grounded_answer_structured_content(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> Value {
    let mut sanitized_execution_detail = json!(execution_detail);
    if let Some(references) = sanitized_execution_detail
        .get_mut("preparedSegmentReferences")
        .and_then(Value::as_array_mut)
    {
        for reference in references {
            if let Some(object) = reference.as_object_mut() {
                object.remove("sourceUri");
                object.remove("sourceAccess");
            }
        }
    }
    let final_answer_ready = grounded_answer_final_answer_ready(execution_detail);
    let finalizable = final_answer_ready && !answer_text.trim().is_empty();
    json!({
        "answerBody": answer_text,
        "executionDetail": sanitized_execution_detail,
        "finalAnswerReady": final_answer_ready,
        "finalizable": finalizable,
        "mustPreserveSpans": grounded_answer_must_preserve_spans(
            answer_text,
            execution_detail,
            finalizable,
        ),
        "runtimeExecutionId": execution_detail.execution.runtime_execution_id,
        "executionId": execution_detail.execution.id,
        "conversationId": execution_detail.execution.conversation_id,
        "libraryId": execution_detail.execution.library_id,
        "workspaceId": execution_detail.execution.workspace_id,
        "lifecycleState": execution_detail.execution.lifecycle_state,
    })
}

fn grounded_answer_must_preserve_spans(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    include_source_titles: bool,
) -> Vec<String> {
    let graph_spans = include_source_titles
        .then(|| grounded_answer_graph_preserve_span_candidates(execution_detail))
        .unwrap_or_default();
    let source_titles = include_source_titles.then_some(()).into_iter().flat_map(|_| {
        execution_detail
            .prepared_segment_references
            .iter()
            .filter_map(|reference| reference.document_title.as_deref())
    });
    grounded_answer_must_preserve_spans_for_evidence(answer_text, graph_spans, source_titles)
}

#[cfg(test)]
fn grounded_answer_must_preserve_spans_for_source_titles<'a>(
    answer_text: &str,
    source_titles: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    grounded_answer_must_preserve_spans_for_evidence(answer_text, std::iter::empty(), source_titles)
}

fn grounded_answer_must_preserve_spans_for_evidence<'a>(
    answer_text: &str,
    graph_spans: impl IntoIterator<Item = &'a str>,
    source_titles: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut spans = Vec::new();
    let mut seen = BTreeSet::new();
    for span in backtick_literal_spans(answer_text) {
        push_grounded_answer_preserve_span(&mut spans, &mut seen, &span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for span in adjacent_code_span_assignments(answer_text) {
        push_grounded_answer_preserve_candidate(&mut spans, &mut seen, &span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for graph_span in graph_spans {
        push_grounded_answer_preserve_evidence_span(&mut spans, &mut seen, graph_span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for title in source_titles {
        push_grounded_answer_preserve_source_title(&mut spans, &mut seen, title);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    spans
}

fn grounded_answer_graph_preserve_span_candidates(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> Vec<&str> {
    let mut candidates = Vec::new();
    for relation in execution_detail.relation_references.iter().filter(|reference| {
        reference.rank > 0 && reference.rank <= GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK
    }) {
        if let Some(assertion) = relation.normalized_assertion.as_deref() {
            candidates.push(assertion);
        }
        if candidates.len() >= GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT {
            return candidates;
        }
    }
    for entity in execution_detail.entity_references.iter().filter(|reference| {
        reference.rank > 0 && reference.rank <= GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK
    }) {
        candidates.push(entity.label.as_str());
        if let Some(summary) = entity.summary.as_deref() {
            candidates.push(summary);
        }
        if candidates.len() >= GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT {
            candidates.truncate(GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT);
            return candidates;
        }
    }
    candidates
}

fn push_grounded_answer_preserve_evidence_span(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    evidence: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let evidence = evidence.trim();
    if evidence.is_empty()
        || evidence.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !evidence.chars().any(|ch| ch.is_alphanumeric())
    {
        return;
    }
    if seen.insert(evidence.to_string()) {
        spans.push(evidence.to_string());
    }
}

fn push_grounded_answer_preserve_source_title(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    title: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !title.chars().any(|ch| ch.is_alphanumeric())
    {
        return;
    }
    if seen.insert(title.to_string()) {
        spans.push(title.to_string());
    }
}

fn push_grounded_answer_preserve_span(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    span: &str,
) {
    let span = span.trim();
    if span.is_empty() {
        return;
    }
    if span.contains('\n') {
        let mut lines = span.lines();
        if let Some(first_line) = lines.next()
            && !is_probable_code_fence_info(first_line)
        {
            push_grounded_answer_preserve_line(spans, seen, first_line);
        }
        for line in lines {
            push_grounded_answer_preserve_line(spans, seen, line);
            if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
                break;
            }
        }
        return;
    }
    push_grounded_answer_preserve_candidate(spans, seen, span);
}

fn push_grounded_answer_preserve_line(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    line: &str,
) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    push_grounded_answer_preserve_candidate(spans, seen, line);
    if let Some((left, right)) = line.split_once('=') {
        push_grounded_answer_preserve_candidate(spans, seen, left.trim());
        push_grounded_answer_preserve_candidate(spans, seen, right.trim());
    }
}

fn push_grounded_answer_preserve_candidate(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    candidate: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let candidate = candidate.trim();
    if !is_grounded_answer_preserve_candidate(candidate) {
        return;
    }
    if seen.insert(candidate.to_string()) {
        spans.push(candidate.to_string());
    }
}

fn is_grounded_answer_preserve_candidate(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty()
        || candidate.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !candidate.chars().any(|ch| ch.is_alphanumeric())
    {
        return false;
    }
    if candidate.starts_with('/') || candidate.starts_with('\\') {
        return true;
    }
    if candidate.contains('/') || candidate.contains('\\') || candidate.contains("://") {
        return true;
    }
    if candidate.contains('=') {
        return candidate.split_once('=').is_some_and(|(left, right)| {
            !left.trim().is_empty()
                && !right.trim().is_empty()
                && left.trim().chars().any(|ch| ch.is_alphanumeric())
        });
    }
    let unwrapped =
        candidate.trim_matches('[').trim_matches(']').trim_matches('`').trim_matches('"');
    literal_text_is_identifier_shaped(unwrapped) || is_plain_code_span(unwrapped)
}

fn is_plain_code_span(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.chars().any(char::is_whitespace) {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    alnum_count >= 2
        && candidate.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '=')
        })
}

fn adjacent_code_span_assignments(text: &str) -> Vec<String> {
    let mut assignments = Vec::new();
    let mut seen = BTreeSet::new();
    let mut pending_key: Option<(String, usize)> = None;
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            pending_key = None;
            continue;
        }
        let spans = backtick_literal_spans(line);
        if spans.is_empty() {
            continue;
        }
        let keys = spans
            .iter()
            .map(String::as_str)
            .filter(|span| is_assignment_key_span(span))
            .collect::<Vec<_>>();
        let values = spans
            .iter()
            .map(String::as_str)
            .filter(|span| is_assignment_value_span(span))
            .collect::<Vec<_>>();
        if keys.is_empty()
            && let Some((key, key_line)) = pending_key.as_ref()
            && line_index.saturating_sub(*key_line) <= 6
            && values.len() == 1
        {
            let assignment = format!("{} = {}", key.trim(), values[0].trim());
            if seen.insert(assignment.clone()) {
                assignments.push(assignment);
            }
        }
        if keys.len() == 1 {
            if values.len() == 1 {
                let assignment = format!("{} = {}", keys[0].trim(), values[0].trim());
                if seen.insert(assignment.clone()) {
                    assignments.push(assignment);
                }
            }
            pending_key = Some((keys[0].trim().to_string(), line_index));
        }
    }
    assignments
}

fn is_assignment_key_span(span: &str) -> bool {
    let span = span.trim();
    if span.is_empty()
        || is_assignment_value_span(span)
        || span.starts_with('[')
        || span.starts_with('/')
        || span.starts_with('\\')
        || span.contains('/')
        || span.contains('\\')
        || span.contains("://")
        || span.contains('=')
    {
        return false;
    }
    let Some(first) = span.chars().next() else {
        return false;
    };
    first.is_alphabetic()
        && span.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && literal_text_is_identifier_shaped(span)
}

fn is_assignment_value_span(span: &str) -> bool {
    let span = span.trim();
    if span.is_empty() || span.chars().any(char::is_whitespace) {
        return false;
    }
    let lowered = span.to_ascii_lowercase();
    matches!(lowered.as_str(), "true" | "false")
        || span.contains("://")
        || span.starts_with('/')
        || span.starts_with('\\')
        || span.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+'))
}

fn is_probable_code_fence_info(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && line.chars().count() <= 32
        && line
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+' | '.' | '#'))
}

fn backtick_literal_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut span_start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if ch != '`' {
            continue;
        }
        if let Some(start) = span_start.take() {
            if start < index {
                spans.push(text[start..index].to_string());
            }
        } else {
            span_start = Some(index + ch.len_utf8());
        }
    }
    spans
}

fn grounded_answer_final_answer_ready(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> bool {
    execution_detail.execution.lifecycle_state == "completed"
        && matches!(
            execution_detail.verification_state,
            ironrag_contracts::assistant::AssistantVerificationState::Verified
        )
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
    surface: McpToolSurface,
) -> McpJsonRpcResponse {
    match capability_snapshot(auth, state, surface).await {
        Ok(mut capabilities) => {
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
            // Strip fields the LLM doesn't need. The full tool name
            // list is already in `tools/list`; token_id and
            // generated_at are pure noise in the agent's context.
            capabilities.token_id = None;
            capabilities.tools.clear();
            capabilities.generated_at = None;
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

#[cfg(test)]
mod tests {
    use super::{
        grounded_answer_must_preserve_spans_for_evidence,
        grounded_answer_must_preserve_spans_for_source_titles,
    };

    #[test]
    fn grounded_answer_preserve_spans_include_adjacent_scalar_assignments() {
        let spans = grounded_answer_must_preserve_spans_for_source_titles(
            "`Synthetic source`

- slot: `alphaFlag`
- values: `true` / `false`
- selected: `false`

- file: `/opt/acme/ui.ini`
- section: `[UI.Panel]`
- slot: `betaVisible`
- selected: `true`",
            [],
        );

        assert!(spans.iter().any(|span| span == "alphaFlag = false"), "{spans:?}");
        assert!(spans.iter().any(|span| span == "betaVisible = true"), "{spans:?}");
        assert!(!spans.iter().any(|span| span == "/opt/acme/ui.ini = [UI.Panel]"), "{spans:?}");
        assert!(!spans.iter().any(|span| span == "alphaFlag = true"), "{spans:?}");
    }

    #[test]
    fn grounded_answer_preserve_spans_include_source_titles() {
        let spans = grounded_answer_must_preserve_spans_for_source_titles(
            "The answer found provider alpha and the setup appendix.",
            [
                "Provider Alpha - Setup Guide",
                "Provider Alpha - Setup Guide",
                "Setup Appendix / Parameters",
            ],
        );

        assert!(spans.iter().any(|span| span == "Provider Alpha - Setup Guide"), "{spans:?}");
        assert!(spans.iter().any(|span| span == "Setup Appendix / Parameters"), "{spans:?}");
        assert_eq!(
            spans.iter().filter(|span| span.as_str() == "Provider Alpha - Setup Guide").count(),
            1
        );
    }

    #[test]
    fn grounded_answer_preserve_spans_include_graph_evidence_before_source_titles() {
        let spans = grounded_answer_must_preserve_spans_for_evidence(
            "Use `/opt/acme.ini`.",
            [
                "Alpha flow includes completed action",
                "Beta flow includes rollback action",
                "Alpha flow includes completed action",
            ],
            ["Alpha setup guide"],
        );

        assert_eq!(spans[0], "/opt/acme.ini");
        assert!(spans.iter().any(|span| span == "Alpha flow includes completed action"));
        assert!(spans.iter().any(|span| span == "Beta flow includes rollback action"));
        assert!(spans.iter().any(|span| span == "Alpha setup guide"));

        let graph_index = spans
            .iter()
            .position(|span| span == "Alpha flow includes completed action")
            .expect("graph span should be present");
        let source_index = spans
            .iter()
            .position(|span| span == "Alpha setup guide")
            .expect("source title should be present");
        assert!(graph_index < source_index, "{spans:?}");
        assert_eq!(
            spans
                .iter()
                .filter(|span| span.as_str() == "Alpha flow includes completed action")
                .count(),
            1
        );
    }
}
