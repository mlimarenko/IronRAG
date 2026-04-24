// `Response::builder()` is only called with hard-coded status codes,
// header names, and header values in this file — all of which are
// `const` / static and infallible by construction. The `expect()`s
// on `.body()` document that invariant; they cannot panic in practice.
// Swapping them to `?` would force every handler to pick an ApiError
// for a code path that is unreachable by construction.
#![allow(clippy::expect_used)]

use std::error::Error as _;
use std::time::Duration;

use axum::{
    Json, Router, body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use futures::stream::StreamExt;
use http_body_util::LengthLimitError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// Interval between SSE keep-alive comments emitted on the idle
/// `GET /v1/mcp` stream. 25 s sits comfortably below every proxy
/// idle-read timeout we care about (nginx default 60 s, the gateway's
/// default 75 s) so the connection stays warm without generating
/// meaningful traffic. mcp-remote treats a cleanly kept-alive stream
/// as healthy and stops its reconnect storm — previously it was
/// reopening the GET every ~300 ms because our handshake closed
/// immediately after emitting a single ready comment.
const MCP_GET_STREAM_KEEPALIVE: Duration = Duration::from_secs(25);

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

pub const MCP_ANSWER_TOOL_NAMES: &[&str] =
    &["list_workspaces", "list_libraries", "list_documents", "grounded_answer"];

pub const MCP_DIAGNOSTICS_TOOL_NAMES: &[&str] = &[
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
    // Canonical grounded-answer entry point — parity with the UI
    // assistant: same pipeline, same citations, same verifier.
    "grounded_answer",
];

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

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
        .header(header::CONNECTION, "keep-alive")
        // X-Accel-Buffering: no tells nginx/traefik style proxies to
        // flush bytes as they arrive instead of buffering the stream —
        // without this the `: ready` comment can sit in a proxy buffer
        // for 30+ seconds, re-triggering client-side reconnect loops.
        .header(HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no"))
        .body(body::Body::from_stream(stream))
        .expect("streaming SSE response must build");
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

#[tracing::instrument(level = "info", name = "http.mcp.get_capabilities", skip_all)]
async fn get_answer_capabilities(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    get_capabilities_for_surface(auth, State(state), headers, McpToolSurface::Answer).await
}

#[tracing::instrument(level = "info", name = "http.mcp.get_diagnostics_capabilities", skip_all)]
async fn get_diagnostics_capabilities(
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

#[tracing::instrument(level = "info", name = "http.mcp.handle_jsonrpc", skip_all)]
async fn handle_answer_jsonrpc(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
) -> Response {
    handle_jsonrpc_for_surface(auth, State(state), request, McpToolSurface::Answer).await
}

#[tracing::instrument(level = "info", name = "http.mcp.handle_diagnostics_jsonrpc", skip_all)]
async fn handle_diagnostics_jsonrpc(
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
        McpAcceptPreference::Json => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body::Body::from(body_json))
            .expect("static JSON response must build"),
        McpAcceptPreference::EventStream => {
            // Single-event SSE response. MCP Streamable HTTP treats
            // POST replies as short-lived streams: one `message`
            // event carrying the JSON-RPC frame, then the server
            // may close immediately. We do not keep the stream open
            // because IronRAG emits no progress notifications — the
            // client receives the final frame and the connection
            // ends.
            let sse_body = format!("event: message\ndata: {body_json}\n\n");
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
                .header(header::CONNECTION, "keep-alive")
                .body(body::Body::from(sse_body))
                .expect("static SSE response must build")
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
