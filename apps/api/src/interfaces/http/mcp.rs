use std::{
    convert::Infallible,
    error::Error as _,
    sync::{Arc, LazyLock},
    time::Duration,
};

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
use chrono::{NaiveDate, Utc};
use http_body_util::LengthLimitError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, error};

/// Interval between SSE keep-alive comments emitted on the idle
/// `GET /v1/mcp` stream. 25 s sits comfortably below every proxy
/// idle-read timeout we care about (nginx default 60 s, the gateway's
/// default 75 s) so the connection stays warm without generating
/// meaningful traffic. mcp-remote only stops its reconnect loop when
/// the stream stays alive past the handshake.
const MCP_GET_STREAM_KEEPALIVE: Duration = Duration::from_secs(25);
/// Maximum idle period between frames on a long-running SSE `tools/call`.
/// The stream carries only one bounded comment per interval, so it keeps
/// proxies and clients alive without buffering progress or detaching work.
const MCP_TOOL_CALL_SSE_KEEPALIVE: Duration = Duration::from_secs(10);

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
mod lease;
mod session;
pub(crate) mod tools;

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
    McpDistributedAdmissionOutcome, mcp_cancel_marker_matches_generation,
    mcp_distributed_admission_outcome, mcp_distributed_cancel_markers_cancel_call,
    mcp_request_id_key, mcp_scope_digest, mcp_tool_call_redis_ttl_millis,
    start_bounded_mcp_tool_call,
};
use lease::{
    MCP_IN_FLIGHT_TOOL_CALLS, MCP_TOOL_CALL_COORDINATION_TIMEOUT, MCP_TOOL_CALL_DEADLINE,
    McpInFlightRequestKey, McpInFlightToolCallRegistry, McpSessionScope,
    handle_owned_mcp_tool_call, mark_distributed_mcp_tool_call_cancelled,
    mcp_distributed_coordination_keys, mcp_in_flight_request_key, mcp_session_scope,
    start_distributed_bounded_mcp_tool_call,
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
/// grants. Derived from `tools::TOOL_REGISTRY` — the single canonical
/// tool registry (plan §6.4) — rather than hand-maintained in parallel
/// with it, so this list cannot silently drift from the actual predicate
/// table. `LazyLock` (not `const`) because deriving it calls into the
/// registry at first use.
pub static MCP_ANSWER_TOOL_NAMES: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| tools::canonical_tool_names(McpToolSurface::Answer));

/// Exhaustive diagnostics-surface tool names — see [`MCP_ANSWER_TOOL_NAMES`].
pub static MCP_DIAGNOSTICS_TOOL_NAMES: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| tools::canonical_tool_names(McpToolSurface::Diagnostics));

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

pub const MCP_CANONICAL_NOTIFICATION_METHOD_NAMES: &[&str] =
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

    fn canonical_tool_names(self) -> &'static [&'static str] {
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpCancelledNotificationParams {
    request_id: Value,
    reason: Option<String>,
}

static MCP_SESSIONS: LazyLock<Arc<session::McpLocalSessionRegistry>> = LazyLock::new(|| {
    Arc::new(session::McpLocalSessionRegistry::new(
        session::MCP_SESSION_PROCESS_LIMIT,
        session::MCP_SESSION_PER_PRINCIPAL_PROCESS_LIMIT,
        session::MCP_GET_STREAM_PROCESS_LIMIT,
        session::MCP_GET_STREAM_PER_PRINCIPAL_PROCESS_LIMIT,
    ))
});

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

#[derive(Debug, Clone, Copy)]
enum McpSessionAccessError {
    MissingOrForeign,
    CoordinationUnavailable,
    ProcessCapacity,
    PrincipalProcessCapacity,
}

fn mcp_session_access_error_response(error: McpSessionAccessError, request_id: &str) -> Response {
    let status = match error {
        McpSessionAccessError::MissingOrForeign => StatusCode::NOT_FOUND,
        McpSessionAccessError::CoordinationUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        McpSessionAccessError::ProcessCapacity
        | McpSessionAccessError::PrincipalProcessCapacity => StatusCode::TOO_MANY_REQUESTS,
    };
    with_request_id(status.into_response(), request_id)
}

fn mcp_protocol_header_error_response(request_id: &str) -> Response {
    let mut response = (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "errorKind": "invalid_mcp_protocol_version",
            "message": "the exact supported MCP protocol version header is required",
            "supportedProtocolVersion": MCP_PROTOCOL_VERSION,
        })),
    )
        .into_response();
    attach_request_id_header(response.headers_mut(), request_id);
    response
}

async fn validate_distributed_mcp_session(
    redis: &redis::Client,
    owner: session::McpSessionOwner,
    session_id: session::McpSessionId,
) -> Result<(), McpSessionAccessError> {
    let validation = tokio::time::timeout(
        session::MCP_SESSION_COORDINATION_TIMEOUT,
        session::validate_session(redis, owner, session_id, session::MCP_SESSION_TTL),
    )
    .await;
    match validation {
        Ok(Ok(session::McpSessionValidationOutcome::Active)) => Ok(()),
        Ok(Ok(
            session::McpSessionValidationOutcome::MissingOrForeign
            | session::McpSessionValidationOutcome::Terminated,
        )) => Err(McpSessionAccessError::MissingOrForeign),
        Ok(Ok(session::McpSessionValidationOutcome::InvalidResponse)) => {
            tracing::error!("invalid Redis response for MCP session validation");
            Err(McpSessionAccessError::CoordinationUnavailable)
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "MCP session validation failed");
            Err(McpSessionAccessError::CoordinationUnavailable)
        }
        Err(_) => {
            tracing::warn!("MCP session validation timed out");
            Err(McpSessionAccessError::CoordinationUnavailable)
        }
    }
}

async fn validate_and_accept_mcp_session(
    redis: &redis::Client,
    owner: session::McpSessionOwner,
    session_id: session::McpSessionId,
) -> Result<(), McpSessionAccessError> {
    if let Err(error) = validate_distributed_mcp_session(redis, owner, session_id).await {
        MCP_SESSIONS.terminate_owned(owner, session_id);
        return Err(error);
    }
    match MCP_SESSIONS.accept_validated(owner, session_id) {
        session::McpLocalSessionAdmissionOutcome::Accepted => Ok(()),
        session::McpLocalSessionAdmissionOutcome::MissingOrForeign => {
            Err(McpSessionAccessError::MissingOrForeign)
        }
        session::McpLocalSessionAdmissionOutcome::ProcessCapacity => {
            Err(McpSessionAccessError::ProcessCapacity)
        }
        session::McpLocalSessionAdmissionOutcome::PrincipalProcessCapacity => {
            Err(McpSessionAccessError::PrincipalProcessCapacity)
        }
    }
}

async fn cleanup_registered_mcp_session(
    redis: &redis::Client,
    owner: session::McpSessionOwner,
    session_id: session::McpSessionId,
) {
    match tokio::time::timeout(
        session::MCP_SESSION_COORDINATION_TIMEOUT,
        session::terminate_session(redis, owner, session_id, session::MCP_SESSION_TERMINATION_TTL),
    )
    .await
    {
        Ok(Ok(session::McpSessionTerminationOutcome::Terminated)) => {}
        Ok(Ok(_)) => {
            tracing::warn!("new MCP session cleanup returned an invalid ownership outcome");
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "new MCP session cleanup failed; TTL recovery remains active");
        }
        Err(_) => {
            tracing::warn!("new MCP session cleanup timed out; TTL recovery remains active");
        }
    }
}

async fn issue_registered_mcp_session(
    redis: &redis::Client,
    owner: session::McpSessionOwner,
) -> Result<String, McpSessionAccessError> {
    const ISSUE_ATTEMPTS: usize = 3;
    for _ in 0..ISSUE_ATTEMPTS {
        let (wire, session_id) = session::issue_session_id();
        let registration = tokio::time::timeout(
            session::MCP_SESSION_COORDINATION_TIMEOUT,
            session::register_session(redis, owner, session_id, session::MCP_SESSION_TTL),
        )
        .await;
        match registration {
            Ok(Ok(session::McpSessionRegistrationOutcome::Registered)) => {
                match MCP_SESSIONS.accept_validated(owner, session_id) {
                    session::McpLocalSessionAdmissionOutcome::Accepted => return Ok(wire),
                    session::McpLocalSessionAdmissionOutcome::MissingOrForeign => {
                        cleanup_registered_mcp_session(redis, owner, session_id).await;
                        return Err(McpSessionAccessError::CoordinationUnavailable);
                    }
                    session::McpLocalSessionAdmissionOutcome::ProcessCapacity => {
                        cleanup_registered_mcp_session(redis, owner, session_id).await;
                        return Err(McpSessionAccessError::ProcessCapacity);
                    }
                    session::McpLocalSessionAdmissionOutcome::PrincipalProcessCapacity => {
                        cleanup_registered_mcp_session(redis, owner, session_id).await;
                        return Err(McpSessionAccessError::PrincipalProcessCapacity);
                    }
                }
            }
            Ok(Ok(session::McpSessionRegistrationOutcome::Collision)) => continue,
            Ok(Ok(session::McpSessionRegistrationOutcome::InvalidResponse)) => {
                tracing::error!("invalid Redis response for MCP session registration");
                return Err(McpSessionAccessError::CoordinationUnavailable);
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "MCP session registration failed");
                return Err(McpSessionAccessError::CoordinationUnavailable);
            }
            Err(_) => {
                tracing::warn!("MCP session registration timed out");
                return Err(McpSessionAccessError::CoordinationUnavailable);
            }
        }
    }
    tracing::warn!("MCP session identifier collision budget exhausted");
    Err(McpSessionAccessError::CoordinationUnavailable)
}

struct McpGetStreamState {
    _guard: session::McpSessionStreamGuard,
    local_cancellation: CancellationToken,
    redis: redis::Client,
    owner: session::McpSessionOwner,
    session_id: session::McpSessionId,
    ready_pending: bool,
    deadline: tokio::time::Instant,
}

fn mcp_get_stream(
    state: McpGetStreamState,
) -> impl futures::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send + 'static {
    futures::stream::unfold(state, |mut state| async move {
        if state.ready_pending {
            state.ready_pending = false;
            return Some((Ok(axum::body::Bytes::from_static(b": ready\n\n")), state));
        }

        tokio::select! {
            biased;
            () = state.local_cancellation.cancelled() => None,
            () = tokio::time::sleep_until(state.deadline) => None,
            () = tokio::time::sleep(MCP_GET_STREAM_KEEPALIVE) => {
                if matches!(validate_distributed_mcp_session(
                    &state.redis,
                    state.owner,
                    state.session_id,
                ).await, Ok(())) { Some((
                    Ok(axum::body::Bytes::from_static(b": keep-alive\n\n")),
                    state,
                )) } else {
                    MCP_SESSIONS.terminate_owned(state.owner, state.session_id);
                    None
                }
            }
        }
    })
}

/// `GET /v1/mcp` opens one authenticated, session-bound SSE stream. The
/// process and per-principal caps are released by RAII when the body is
/// dropped. A local DELETE cancels immediately; remote teardown is observed on
/// the bounded heartbeat poll, and every stream has an absolute lifetime.
// openapi-skip: MCP SSE keep-alive transport is shared by both MCP surfaces and has no JSON response contract.
#[tracing::instrument(level = "debug", name = "http.mcp.get_stream", skip_all)]
async fn handle_get_stream(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);
    let Ok(session_id) = session::required_session_id(&headers) else {
        return mcp_session_access_error_response(
            McpSessionAccessError::MissingOrForeign,
            &request_id,
        );
    };
    if session::required_protocol_version(&headers).is_err() {
        return mcp_protocol_header_error_response(&request_id);
    }
    let owner = session::McpSessionOwner::from_auth(&auth);
    if let Err(error) =
        validate_and_accept_mcp_session(&state.persistence.redis, owner, session_id).await
    {
        return mcp_session_access_error_response(error, &request_id);
    }
    let (guard, local_cancellation) =
        match MCP_SESSIONS.try_open_validated_stream(owner, session_id) {
            Ok(stream) => stream,
            Err(
                session::McpStreamAdmissionError::ProcessCapacity
                | session::McpStreamAdmissionError::PrincipalProcessCapacity
                | session::McpStreamAdmissionError::SessionProcessCapacity
                | session::McpStreamAdmissionError::SessionPrincipalProcessCapacity,
            ) => {
                return with_request_id(StatusCode::TOO_MANY_REQUESTS.into_response(), &request_id);
            }
            Err(session::McpStreamAdmissionError::SessionMissingOrForeign) => {
                return mcp_session_access_error_response(
                    McpSessionAccessError::MissingOrForeign,
                    &request_id,
                );
            }
        };
    let stream = mcp_get_stream(McpGetStreamState {
        _guard: guard,
        local_cancellation,
        redis: state.persistence.redis.clone(),
        owner,
        session_id,
        ready_pending: true,
        deadline: tokio::time::Instant::now() + session::MCP_GET_STREAM_MAX_LIFETIME,
    });

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

/// `DELETE /v1/mcp` terminates only a session owned by the authenticated
/// principal/token pair. The Redis tombstone is idempotent, rejects new tool
/// admission, and is polled by work owned on other replicas. Local streams and
/// every locally-owned tool call for the session are cancelled immediately.
// openapi-skip: MCP session cleanup is shared by both MCP surfaces.
#[tracing::instrument(level = "debug", name = "http.mcp.delete_session", skip_all)]
async fn handle_delete_session(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);
    let Ok(session_id) = session::required_session_id(&headers) else {
        return mcp_session_access_error_response(
            McpSessionAccessError::MissingOrForeign,
            &request_id,
        );
    };
    if session::required_protocol_version(&headers).is_err() {
        return mcp_protocol_header_error_response(&request_id);
    }
    let owner = session::McpSessionOwner::from_auth(&auth);
    let termination = tokio::time::timeout(
        session::MCP_SESSION_COORDINATION_TIMEOUT,
        session::terminate_session(
            &state.persistence.redis,
            owner,
            session_id,
            session::MCP_SESSION_TERMINATION_TTL,
        ),
    )
    .await;
    match termination {
        Ok(Ok(session::McpSessionTerminationOutcome::Terminated)) => {}
        Ok(Ok(session::McpSessionTerminationOutcome::MissingOrForeign)) => {
            return mcp_session_access_error_response(
                McpSessionAccessError::MissingOrForeign,
                &request_id,
            );
        }
        Ok(Ok(session::McpSessionTerminationOutcome::InvalidResponse)) => {
            tracing::error!("invalid Redis response for MCP session termination");
            cancel_local_mcp_session(owner, session_id);
            return mcp_session_access_error_response(
                McpSessionAccessError::CoordinationUnavailable,
                &request_id,
            );
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "MCP session termination failed");
            cancel_local_mcp_session(owner, session_id);
            return mcp_session_access_error_response(
                McpSessionAccessError::CoordinationUnavailable,
                &request_id,
            );
        }
        Err(_) => {
            tracing::warn!("MCP session termination timed out");
            cancel_local_mcp_session(owner, session_id);
            return mcp_session_access_error_response(
                McpSessionAccessError::CoordinationUnavailable,
                &request_id,
            );
        }
    }

    cancel_local_mcp_session(owner, session_id);
    let mut response = StatusCode::OK.into_response();
    attach_request_id_header(response.headers_mut(), &request_id);
    response
}

fn cancel_local_mcp_session(owner: session::McpSessionOwner, session_id: session::McpSessionId) {
    let stream_cancelled = MCP_SESSIONS.terminate_owned(owner, session_id);
    let tool_calls_cancelled = MCP_IN_FLIGHT_TOOL_CALLS.cancel_session(owner, session_id);
    tracing::debug!(stream_cancelled, tool_calls_cancelled, "terminated owned MCP session");
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
    let agent_vision_available =
        tools::document_image::any_library_agent_binding_supports_vision(state, &libraries).await;
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
        visible_workspace_count: workspaces.len(),
        visible_library_count: libraries.len(),
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
    description = "Main JSON-RPC endpoint for external MCP clients and other agent runtimes. It implements MCP Streamable HTTP 2025-11-25: every well-formed initialize request negotiates to that canonical version, and every subsequent request must send the exact negotiated `Mcp-Protocol-Version` header. It supports MCP initialization, tool listing, and tool invocation over the answer surface. Tools on this surface are read-oriented and designed for agentic question answering: catalog discovery, document search/read, graph inspection, runtime trace lookup, and `grounded_answer`. For user-facing answers, clients should let the agent choose tools from the listed schemas. Do not mechanically wrap every user message as a `grounded_answer` call; composite questions often need several document, graph, runtime, and grounded-answer probes before final synthesis.",
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
    Box::pin(handle_jsonrpc_for_surface(auth, State(state), request, McpToolSurface::Answer)).await
}

#[utoipa::path(
    post,
    path = "/v1/mcp/diagnostics",
    tag = "automation",
    operation_id = "postMcpDiagnosticsRequest",
    summary = "Execute diagnostics MCP JSON-RPC.",
    description = "JSON-RPC endpoint for operational MCP clients using the same MCP Streamable HTTP 2025-11-25 lifecycle as the answer surface. Every well-formed initialize request negotiates to that canonical version, and every subsequent request must send the exact negotiated `Mcp-Protocol-Version` header. This surface is intended for debugging and automation that needs runtime state or traces, not for ordinary library question answering. Use `/v1/mcp/diagnostics/capabilities` first to check which diagnostic tools the token may call. Keep this surface separate from answer agents when a token should not expose operational inspection tools to normal chat flows.",
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
    Box::pin(handle_jsonrpc_for_surface(auth, State(state), request, McpToolSurface::Diagnostics))
        .await
}

struct ParsedMcpTransportRequest {
    request_id: String,
    accept: McpAcceptPreference,
    supplied_session: Result<Option<session::McpSessionId>, session::McpSessionHeaderError>,
    has_supported_protocol_version: bool,
    request: McpJsonRpcRequest,
}

async fn parse_mcp_transport_request(
    state: &AppState,
    request: Request,
) -> Result<ParsedMcpTransportRequest, Response> {
    let request_id = ensure_or_generate_request_id(request.headers());
    let accept = accept_preference(request.headers());
    let supplied_session = session::optional_session_id(request.headers());
    let has_supported_protocol_version =
        session::required_protocol_version(request.headers()).is_ok();
    let request = parse_mcp_jsonrpc_request(state, request)
        .await
        .map_err(|response| finalize_mcp_response(response, accept, None, &request_id))?;
    Ok(ParsedMcpTransportRequest {
        request_id,
        accept,
        supplied_session,
        has_supported_protocol_version,
        request,
    })
}

async fn handle_initialize_transport_request(
    auth: &AuthContext,
    state: &AppState,
    surface: McpToolSurface,
    accept: McpAcceptPreference,
    request_id: &str,
    supplied_session: Result<Option<session::McpSessionId>, session::McpSessionHeaderError>,
    request: McpJsonRpcRequest,
) -> Response {
    if supplied_session != Ok(None) {
        return mcp_session_access_error_response(
            McpSessionAccessError::MissingOrForeign,
            request_id,
        );
    }
    if request.id.is_none() {
        let response = error_response(
            None,
            -32600,
            "invalid request",
            Some(json!({ "errorKind": "invalid_request_id" })),
        );
        return finalize_mcp_response(response, accept, None, request_id);
    }
    if !valid_initialize_params(request.params.as_ref()) {
        let response = error_response(
            request.id,
            -32602,
            "invalid initialize parameters",
            Some(json!({
                "errorKind": "invalid_initialize_params",
                "supportedProtocolVersion": MCP_PROTOCOL_VERSION,
            })),
        );
        return finalize_mcp_response(response, accept, None, request_id);
    }

    let mut response = handle_initialize(auth, state, request_id, request.id, surface).await;
    let session_wire = if response.error.is_none() {
        issue_initialize_session(auth, state, &mut response).await
    } else {
        None
    };
    finalize_mcp_response(response, accept, session_wire.as_deref(), request_id)
}

async fn issue_initialize_session(
    auth: &AuthContext,
    state: &AppState,
    response: &mut McpJsonRpcResponse,
) -> Option<String> {
    let owner = session::McpSessionOwner::from_auth(auth);
    match issue_registered_mcp_session(&state.persistence.redis, owner).await {
        Ok(wire) => Some(wire),
        Err(
            McpSessionAccessError::ProcessCapacity
            | McpSessionAccessError::PrincipalProcessCapacity,
        ) => {
            *response = error_response(
                response.id.clone(),
                -32003,
                "MCP session capacity exceeded",
                Some(json!({
                    "errorKind": "mcp_session_capacity_exceeded",
                    "retryable": true,
                })),
            );
            None
        }
        Err(
            McpSessionAccessError::MissingOrForeign
            | McpSessionAccessError::CoordinationUnavailable,
        ) => {
            *response = error_response(
                response.id.clone(),
                -32002,
                "MCP session coordination unavailable",
                Some(json!({
                    "errorKind": "mcp_session_coordination_unavailable",
                    "retryable": true,
                })),
            );
            None
        }
    }
}

async fn handle_session_tool_call(
    auth: AuthContext,
    state: AppState,
    request: McpJsonRpcRequest,
    request_id: &str,
    session_scope: McpSessionScope,
    surface: McpToolSurface,
    accept: McpAcceptPreference,
) -> Response {
    let Some(response_id) = request.id.clone() else {
        let response = error_response(
            None,
            -32600,
            "invalid request",
            Some(json!({ "errorKind": "invalid_request_id" })),
        );
        return finalize_mcp_response(response, accept, None, request_id);
    };
    let Some(key) = mcp_in_flight_request_key(&auth, session_scope, &response_id) else {
        let response = error_response(
            Some(response_id),
            -32600,
            "invalid request",
            Some(json!({ "errorKind": "invalid_request_id" })),
        );
        return finalize_mcp_response(response, accept, None, request_id);
    };
    let McpJsonRpcRequest { id, params, .. } = request;
    let redis = state.persistence.redis.clone();
    let tool_future =
        handle_owned_mcp_tool_call(auth, state, request_id.to_string(), id, params, surface)
            .instrument(tracing::Span::current());
    let receiver = Box::pin(start_distributed_bounded_mcp_tool_call(
        Arc::clone(&MCP_IN_FLIGHT_TOOL_CALLS),
        redis,
        key,
        response_id,
        tool_future,
        MCP_TOOL_CALL_DEADLINE,
    ))
    .await;
    match accept {
        McpAcceptPreference::EventStream => {
            finalize_mcp_tool_call_sse_response(receiver, request_id)
        }
        McpAcceptPreference::Json => match receiver.await {
            Ok(response) => finalize_mcp_response(response, accept, None, request_id),
            Err(_) => with_request_id(StatusCode::ACCEPTED.into_response(), request_id),
        },
    }
}

async fn handle_jsonrpc_for_surface(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
    surface: McpToolSurface,
) -> Response {
    let parsed = match parse_mcp_transport_request(&state, request).await {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    if let Some(response) = validate_mcp_transport_envelope(&auth, &state, surface, &parsed).await {
        return response;
    }
    Box::pin(handle_session_mcp_transport_request(auth, state, surface, parsed)).await
}

async fn validate_mcp_transport_envelope(
    auth: &AuthContext,
    state: &AppState,
    surface: McpToolSurface,
    parsed: &ParsedMcpTransportRequest,
) -> Option<Response> {
    if parsed.request.jsonrpc != MCP_JSONRPC_VERSION {
        return Some(invalid_mcp_jsonrpc_version_response(parsed));
    }
    if parsed.request.method != "initialize" {
        return None;
    }
    Some(
        handle_initialize_transport_request(
            auth,
            state,
            surface,
            parsed.accept,
            &parsed.request_id,
            parsed.supplied_session,
            McpJsonRpcRequest {
                jsonrpc: parsed.request.jsonrpc.clone(),
                id: parsed.request.id.clone(),
                method: parsed.request.method.clone(),
                params: parsed.request.params.clone(),
            },
        )
        .await,
    )
}

fn invalid_mcp_jsonrpc_version_response(parsed: &ParsedMcpTransportRequest) -> Response {
    let response = error_response(
        parsed.request.id.clone(),
        -32600,
        "invalid request",
        Some(json!({ "errorKind": "invalid_jsonrpc_version" })),
    );
    finalize_mcp_response(response, parsed.accept, None, &parsed.request_id)
}

async fn handle_session_mcp_transport_request(
    auth: AuthContext,
    state: AppState,
    surface: McpToolSurface,
    parsed: ParsedMcpTransportRequest,
) -> Response {
    let ParsedMcpTransportRequest {
        request_id,
        accept,
        supplied_session,
        has_supported_protocol_version,
        request,
    } = parsed;
    let session_scope = match resolve_mcp_transport_session(
        &auth,
        &state,
        supplied_session,
        has_supported_protocol_version,
        &request_id,
    )
    .await
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Some(response) =
        handle_mcp_notification(&auth, &state, session_scope, &request_id, &request).await
    {
        return response;
    }
    dispatch_session_mcp_method(auth, state, request, &request_id, session_scope, surface, accept)
        .await
}

async fn resolve_mcp_transport_session(
    auth: &AuthContext,
    state: &AppState,
    supplied_session: Result<Option<session::McpSessionId>, session::McpSessionHeaderError>,
    has_supported_protocol_version: bool,
    request_id: &str,
) -> Result<McpSessionScope, Response> {
    let Ok(Some(session_id)) = supplied_session else {
        return Err(mcp_session_access_error_response(
            McpSessionAccessError::MissingOrForeign,
            request_id,
        ));
    };
    if !has_supported_protocol_version {
        return Err(mcp_protocol_header_error_response(request_id));
    }
    let owner = session::McpSessionOwner::from_auth(auth);
    validate_and_accept_mcp_session(&state.persistence.redis, owner, session_id)
        .await
        .map_err(|error| mcp_session_access_error_response(error, request_id))?;
    Ok(mcp_session_scope(session_id))
}

async fn handle_mcp_notification(
    auth: &AuthContext,
    state: &AppState,
    session_scope: McpSessionScope,
    request_id: &str,
    request: &McpJsonRpcRequest,
) -> Option<Response> {
    if request.id.is_some() {
        return None;
    }
    if request.method == "notifications/cancelled" {
        cancel_distributed_mcp_in_flight_tool_call(
            auth,
            session_scope,
            request.params.as_ref(),
            &MCP_IN_FLIGHT_TOOL_CALLS,
            &state.persistence.redis,
        )
        .await;
    }
    request
        .method
        .starts_with("notifications/")
        .then(|| with_request_id(StatusCode::ACCEPTED.into_response(), request_id))
}

async fn dispatch_session_mcp_method(
    auth: AuthContext,
    state: AppState,
    request: McpJsonRpcRequest,
    request_id: &str,
    session_scope: McpSessionScope,
    surface: McpToolSurface,
    accept: McpAcceptPreference,
) -> Response {
    if request.method == "tools/call" {
        return handle_session_tool_call(
            auth,
            state,
            request,
            request_id,
            session_scope,
            surface,
            accept,
        )
        .await;
    }
    let response = match request.method.as_str() {
        "tools/list" => {
            tools::handle_tools_list(&auth, &state, request_id, request.id, surface).await
        }
        _ => error_response(
            request.id,
            -32601,
            "method not found",
            Some(json!({ "errorKind": "unsupported_method" })),
        ),
    };
    finalize_mcp_response(response, accept, None, request_id)
}

fn mcp_cancelled_notification_key(
    auth: &AuthContext,
    session_scope: McpSessionScope,
    params: Option<&Value>,
) -> Option<(McpInFlightRequestKey, bool)> {
    let params = params?;
    let Ok(params) = serde_json::from_value::<McpCancelledNotificationParams>(params.clone())
    else {
        return None;
    };
    let key = mcp_in_flight_request_key(auth, session_scope, &params.request_id)?;
    let reason_present = params.reason.as_ref().is_some_and(|reason| !reason.trim().is_empty());
    Some((key, reason_present))
}

async fn cancel_distributed_mcp_in_flight_tool_call(
    auth: &AuthContext,
    session_scope: McpSessionScope,
    params: Option<&Value>,
    registry: &McpInFlightToolCallRegistry,
    redis: &redis::Client,
) -> bool {
    let Some((key, reason_present)) = mcp_cancelled_notification_key(auth, session_scope, params)
    else {
        return false;
    };
    let coordination_keys = mcp_distributed_coordination_keys(&key);
    let distributed_cancelled = match tokio::time::timeout(
        MCP_TOOL_CALL_COORDINATION_TIMEOUT,
        mark_distributed_mcp_tool_call_cancelled(redis, &coordination_keys),
    )
    .await
    {
        Ok(Ok(marked)) => marked,
        Ok(Err(error)) => {
            tracing::warn!(%error, "MCP distributed cancellation write failed");
            false
        }
        Err(_) => {
            tracing::warn!("MCP distributed cancellation write timed out");
            false
        }
    };
    let cancelled = registry.cancel(&key);
    tracing::debug!(
        local_cancelled = cancelled,
        distributed_cancelled,
        reason_present,
        "processed distributed MCP cancelled notification"
    );
    distributed_cancelled || cancelled
}

#[cfg(test)]
fn cancel_mcp_in_flight_tool_call(
    auth: &AuthContext,
    session_scope: McpSessionScope,
    params: Option<&Value>,
    registry: &McpInFlightToolCallRegistry,
) -> bool {
    let Some((key, _)) = mcp_cancelled_notification_key(auth, session_scope, params) else {
        return false;
    };
    registry.cancel(&key)
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

struct McpToolCallSseState {
    response_receiver: Option<oneshot::Receiver<McpJsonRpcResponse>>,
    keepalive_interval: Duration,
    ready_sent: bool,
}

fn mcp_tool_call_sse_stream(
    response_receiver: oneshot::Receiver<McpJsonRpcResponse>,
    keepalive_interval: Duration,
) -> impl futures::Stream<Item = Result<axum::body::Bytes, Infallible>> + Send + 'static {
    let state = McpToolCallSseState {
        response_receiver: Some(response_receiver),
        keepalive_interval,
        ready_sent: false,
    };
    futures::stream::unfold(state, |mut state| async move {
        if !state.ready_sent {
            state.ready_sent = true;
            return Some((Ok(axum::body::Bytes::from_static(b": ready\n\n")), state));
        }

        let keepalive_interval = state.keepalive_interval;
        let mut response_receiver = state.response_receiver.take()?;
        tokio::select! {
            biased;
            payload = &mut response_receiver => {
                let Ok(payload) = payload else {
                    return None;
                };
                let body_json = serialize_mcp_jsonrpc_response(&payload);
                let message = format!("event: message\ndata: {body_json}\n\n");
                Some((Ok(axum::body::Bytes::from(message)), state))
            }
            () = tokio::time::sleep(keepalive_interval) => {
                state.response_receiver = Some(response_receiver);
                Some((
                    Ok(axum::body::Bytes::from_static(b": keep-alive\n\n")),
                    state,
                ))
            }
        }
    })
}

fn finalize_mcp_tool_call_sse_response(
    response_receiver: oneshot::Receiver<McpJsonRpcResponse>,
    request_id: &str,
) -> Response {
    finalize_mcp_tool_call_sse_response_with_interval(
        response_receiver,
        request_id,
        MCP_TOOL_CALL_SSE_KEEPALIVE,
    )
}

fn finalize_mcp_tool_call_sse_response_with_interval(
    response_receiver: oneshot::Receiver<McpJsonRpcResponse>,
    request_id: &str,
    keepalive_interval: Duration,
) -> Response {
    let stream = mcp_tool_call_sse_stream(response_receiver, keepalive_interval);
    let mut response = build_mcp_response_or_internal_error(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
            .header(header::CONNECTION, "keep-alive")
            .header(HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no")),
        body::Body::from_stream(stream),
        "mcp_tool_call_sse",
    );
    attach_request_id_header(response.headers_mut(), request_id);
    response
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
        .filter(|segment| accept_media_range_is_enabled(segment))
        .map(accept_media_type)
        .any(|segment| segment.eq_ignore_ascii_case("text/event-stream"));
    let wants_json = accept_header.is_empty()
        || accept_header
            .split(',')
            .filter(|segment| accept_media_range_is_enabled(segment))
            .map(accept_media_type)
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

fn accept_media_type(segment: &str) -> &str {
    segment.split(';').next().unwrap_or("").trim()
}

fn accept_media_range_is_enabled(segment: &str) -> bool {
    let quality = segment.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.trim().eq_ignore_ascii_case("q").then(|| value.trim().parse::<f32>().ok()).flatten()
    });
    quality.is_none_or(|quality| quality > 0.0 && quality <= 1.0)
}

fn finalize_mcp_response(
    payload: McpJsonRpcResponse,
    accept: McpAcceptPreference,
    session_id: Option<&str>,
    request_id: &str,
) -> Response {
    let body_json = serialize_mcp_jsonrpc_response(&payload);
    let mut response = match accept {
        McpAcceptPreference::Json => build_mcp_response_or_internal_error(
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json"),
            body::Body::from(body_json),
            "mcp_json",
        ),
        McpAcceptPreference::EventStream => {
            // Non-tool SSE responses are short-lived: one `message`
            // event carrying the JSON-RPC frame, then close. Long-running
            // `tools/call` requests take the streaming branch above so they
            // can flush ready/keepalive comments before their final message.
            let sse_body = format!("event: message\ndata: {body_json}\n\n");
            build_mcp_response_or_internal_error(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
                    .header(header::CONNECTION, "keep-alive")
                    .header(
                        HeaderName::from_static("x-accel-buffering"),
                        HeaderValue::from_static("no"),
                    ),
                body::Body::from(sse_body),
                "mcp_sse",
            )
        }
    };
    if let Some(sid) = session_id
        && let Ok(value) = HeaderValue::from_str(sid)
    {
        response.headers_mut().insert(HeaderName::from_static(MCP_SESSION_HEADER), value);
    }
    attach_request_id_header(response.headers_mut(), request_id);
    response
}

fn serialize_mcp_jsonrpc_response(payload: &McpJsonRpcResponse) -> String {
    serde_json::to_string(payload).unwrap_or_else(|error| {
        // Serialization of a known-small Serialize struct cannot
        // realistically fail; fall back to a hand-rolled JSON-RPC
        // error frame so we still emit valid JSON-RPC on the wire.
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32603,\"message\":\"internal serialization error: {error}\"}}}}"
        )
    })
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
    let error_kind = error.kind();
    let message = error.to_string();
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
        "query_content_projection_converging" => Some((Some(500), "retry_same_request")),
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
            success_response(id, mcp_initialize_result(capabilities))
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
    use std::{
        collections::BTreeSet,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use axum::body;
    use axum::http::{HeaderMap, HeaderValue, header};
    use http_body_util::BodyExt as _;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use crate::{
        domains::iam::PrincipalKind,
        interfaces::http::auth::{AuthContext, AuthTokenKind},
    };

    use super::{
        MCP_DISTRIBUTED_CANCEL_SCRIPT, MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT,
        MCP_DISTRIBUTED_RELEASE_SCRIPT, MCP_TOOL_CALL_CANCEL_PENDING_MARKER, McpAcceptPreference,
        McpDistributedAdmissionOutcome, McpInFlightRequestKey, McpInFlightToolCallRegistry,
        McpJsonRpcResponse, McpSessionScope, accept_preference, cancel_mcp_in_flight_tool_call,
        finalize_mcp_response, finalize_mcp_tool_call_sse_response_with_interval,
        grounded_answer_must_preserve_spans_for_evidence,
        grounded_answer_must_preserve_spans_for_source_titles,
        mcp_cancel_marker_matches_generation, mcp_distributed_admission_outcome,
        mcp_distributed_cancel_markers_cancel_call, mcp_distributed_coordination_keys,
        mcp_initialize_result, mcp_request_id_key, mcp_scope_digest,
        mcp_tool_call_redis_ttl_millis, start_bounded_mcp_tool_call,
        start_distributed_bounded_mcp_tool_call, success_response, valid_initialize_params,
    };

    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn initialize_result_exposes_the_exact_shared_agent_policy() {
        let payload = mcp_initialize_result(crate::mcp_types::McpCapabilitySnapshot {
            token_id: None,
            token_kind: "api_token".to_string(),
            workspace_scope: None,
            visible_workspace_count: 0,
            visible_library_count: 0,
            tools: Vec::new(),
            tool_contract_version: 1,
            tool_contract_hash: "synthetic-contract-hash".to_string(),
            generated_at: None,
        });

        assert_eq!(
            payload["instructions"],
            json!(crate::services::mcp::agent_policy::instructions())
        );
        assert_eq!(payload["protocolVersion"], json!("2025-11-25"));
        assert_eq!(payload["capabilities"], json!({ "tools": { "listChanged": false } }));
        assert_eq!(
            payload["instructions"]
                .as_str()
                .expect("initialize instructions")
                .matches(crate::services::mcp::agent_policy::AGENT_POLICY_VERSION)
                .count(),
            1
        );
    }

    #[test]
    fn initialize_params_accept_version_negotiation_and_require_typed_client_shape() {
        let canonical = json!({
            "protocolVersion": super::MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1" },
        });
        assert!(valid_initialize_params(Some(&canonical)));
        assert!(valid_initialize_params(Some(&json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1" },
        }))));
        assert!(valid_initialize_params(Some(&json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1" },
        }))));

        for invalid in [
            json!({}),
            json!({
                "protocolVersion": " ",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1" },
            }),
            json!({
                "protocolVersion": "next",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1" },
            }),
            json!({
                "protocolVersion": "2025-11-25 ",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1" },
            }),
            json!({
                "protocolVersion": 20251125,
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1" },
            }),
            json!({
                "protocolVersion": super::MCP_PROTOCOL_VERSION,
                "capabilities": [],
                "clientInfo": { "name": "test-client", "version": "1" },
            }),
            json!({
                "protocolVersion": super::MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": " ", "version": "1" },
            }),
        ] {
            assert!(!valid_initialize_params(Some(&invalid)));
        }
        assert!(!valid_initialize_params(None));
    }

    async fn next_body_data(body: &mut body::Body) -> Option<axum::body::Bytes> {
        loop {
            let frame = body.frame().await?.expect("MCP response body frame");
            if let Ok(data) = frame.into_data() {
                return Some(data);
            }
        }
    }

    fn in_flight_key(
        principal: u128,
        token: u128,
        session: &str,
        request_id: &Value,
    ) -> McpInFlightRequestKey {
        McpInFlightRequestKey {
            principal_id: Uuid::from_u128(principal),
            token_id: Uuid::from_u128(token),
            session_scope: McpSessionScope(mcp_scope_digest(session.as_bytes())),
            request_id: mcp_request_id_key(request_id).expect("valid MCP request id"),
        }
    }

    fn auth_context(principal: u128, token: u128) -> AuthContext {
        AuthContext {
            token_id: Uuid::from_u128(token),
            principal_id: Uuid::from_u128(principal),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: Vec::new(),
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
            system_role: None,
        }
    }

    #[test]
    fn distributed_tool_call_keys_are_digest_only_and_domain_separated() {
        let principal = Uuid::from_u128(111);
        let token = Uuid::from_u128(222);
        let private_session = "private-customer-session";
        let private_request_id = "private-request-payload";
        let key = in_flight_key(
            principal.as_u128(),
            token.as_u128(),
            private_session,
            &json!(private_request_id),
        );

        let keys = mcp_distributed_coordination_keys(&key);
        let combined = format!("{} {} {}", keys.owner, keys.cancel, keys.session_terminated);

        assert_ne!(keys.owner, keys.cancel);
        assert!(keys.owner.starts_with("ironrag:mcp:tool-call:v1:"));
        assert!(keys.owner.ends_with(":owner"));
        assert!(keys.cancel.ends_with(":cancel"));
        assert!(keys.session_terminated.ends_with(":terminated"));
        for private_value in [
            principal.to_string(),
            token.to_string(),
            private_session.to_string(),
            private_request_id.to_string(),
        ] {
            assert!(!combined.contains(&private_value));
        }
    }

    #[test]
    fn distributed_tool_call_ttls_never_outlive_the_absolute_deadline() {
        for deadline in
            [Duration::from_millis(1), Duration::from_millis(1_500), Duration::from_secs(180)]
        {
            let ttl_millis = mcp_tool_call_redis_ttl_millis(deadline);
            assert!(ttl_millis >= 1);
            assert!(ttl_millis <= deadline.as_millis());
        }
    }

    #[test]
    fn distributed_coordination_scripts_are_generation_safe_and_race_closed() {
        assert!(MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT.contains("NX"));
        assert!(MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT.contains("PX"));
        assert!(MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT.contains("KEYS[3]"));
        assert!(MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT.contains("EXISTS"));
        assert!(MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT.contains("cancel == ARGV[2]"));
        assert!(MCP_DISTRIBUTED_CANCEL_SCRIPT.contains("PTTL"));
        assert!(MCP_DISTRIBUTED_CANCEL_SCRIPT.contains("NX"));
        assert!(MCP_DISTRIBUTED_RELEASE_SCRIPT.contains("ARGV[1]"));
        assert!(MCP_DISTRIBUTED_RELEASE_SCRIPT.matches("GET").count() >= 2);
    }

    #[test]
    fn distributed_admission_and_cancel_markers_preserve_generation_ownership() {
        let generation = Uuid::from_u128(333);

        assert_eq!(mcp_distributed_admission_outcome(1), McpDistributedAdmissionOutcome::Acquired);
        assert_eq!(
            mcp_distributed_admission_outcome(0),
            McpDistributedAdmissionOutcome::DuplicateRequestId
        );
        assert_eq!(
            mcp_distributed_admission_outcome(-1),
            McpDistributedAdmissionOutcome::CancelledBeforeAdmission
        );
        assert_eq!(
            mcp_distributed_admission_outcome(-2),
            McpDistributedAdmissionOutcome::CancelledBeforeAdmission
        );
        assert!(mcp_cancel_marker_matches_generation(&generation.to_string(), generation));
        assert!(!mcp_cancel_marker_matches_generation(
            MCP_TOOL_CALL_CANCEL_PENDING_MARKER,
            generation,
        ));
        assert!(!mcp_cancel_marker_matches_generation(
            &Uuid::from_u128(334).to_string(),
            generation
        ));
        assert!(mcp_distributed_cancel_markers_cancel_call(None, true, generation));
        assert!(mcp_distributed_cancel_markers_cancel_call(
            Some(&generation.to_string()),
            false,
            generation,
        ));
        assert!(!mcp_distributed_cancel_markers_cancel_call(None, false, generation));
    }

    #[tokio::test]
    #[ignore = "requires local redis service"]
    async fn distributed_session_tombstone_cancels_remote_tool_owner_without_key_scan() {
        let redis_url = std::env::var("IRONRAG_REDIS_URL").expect("IRONRAG_REDIS_URL");
        let redis = redis::Client::open(redis_url).expect("Redis client");
        let auth = auth_context(301, 302);
        let owner = super::session::McpSessionOwner::from_auth(&auth);
        let (session_wire, session_id) = super::session::issue_session_id();
        assert_eq!(
            super::session::register_session(&redis, owner, session_id, Duration::from_secs(10),)
                .await
                .expect("register distributed test session"),
            super::session::McpSessionRegistrationOutcome::Registered
        );

        let registry = Arc::new(McpInFlightToolCallRegistry::new(4, 2));
        let response_id = json!(401);
        let key = in_flight_key(301, 302, &session_wire, &response_id);
        let call_keys = mcp_distributed_coordination_keys(&key);
        let receiver = start_distributed_bounded_mcp_tool_call(
            Arc::clone(&registry),
            redis.clone(),
            key,
            response_id,
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(registry.len(), 1);

        assert_eq!(
            super::session::terminate_session(&redis, owner, session_id, Duration::from_secs(5),)
                .await
                .expect("terminate distributed test session"),
            super::session::McpSessionTerminationOutcome::Terminated
        );
        let remote_result = tokio::time::timeout(Duration::from_secs(2), receiver)
            .await
            .expect("remote owner must observe the session tombstone");
        assert!(remote_result.is_err(), "session teardown must not emit a final tool response");
        assert_eq!(registry.len(), 0);

        let session_keys = super::session::session_redis_keys(session_id);
        let mut connection =
            redis.get_multiplexed_async_connection().await.expect("Redis cleanup connection");
        let _: i64 = redis::cmd("DEL")
            .arg(session_keys.owner)
            .arg(session_keys.terminated)
            .arg(call_keys.owner)
            .arg(call_keys.cancel)
            .query_async(&mut connection)
            .await
            .expect("delete exact distributed test keys");
    }

    #[tokio::test]
    async fn tools_call_sse_sends_ready_and_keepalive_before_delayed_result() {
        let completed = Arc::new(AtomicBool::new(false));
        let completed_in_tool = Arc::clone(&completed);
        let registry = Arc::new(McpInFlightToolCallRegistry::new(4, 2));
        let response_id = json!(41);
        let receiver = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            in_flight_key(1, 2, "session-ready", &response_id),
            response_id,
            async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                completed_in_tool.store(true, Ordering::SeqCst);
                success_response(Some(json!(41)), json!({ "status": "done" }))
            },
            Duration::from_secs(1),
        );
        let response = finalize_mcp_tool_call_sse_response_with_interval(
            receiver,
            "request-sse-ready",
            Duration::from_millis(10),
        );

        assert_eq!(
            response.headers().get("content-type").and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            response.headers().get("x-accel-buffering").and_then(|value| value.to_str().ok()),
            Some("no")
        );

        let mut body = response.into_body();
        let first = tokio::time::timeout(Duration::from_millis(100), next_body_data(&mut body))
            .await
            .expect("ready frame must not wait for the tool")
            .expect("ready frame");
        assert_eq!(first.as_ref(), b": ready\n\n");
        assert!(!completed.load(Ordering::SeqCst));

        let mut remainder = String::new();
        while let Some(data) =
            tokio::time::timeout(Duration::from_millis(250), next_body_data(&mut body))
                .await
                .expect("SSE tool stream must finish after its result")
        {
            remainder.push_str(std::str::from_utf8(&data).expect("UTF-8 SSE frame"));
        }

        assert!(completed.load(Ordering::SeqCst));
        assert!(remainder.contains(": keep-alive\n\n"), "{remainder:?}");
        assert_eq!(remainder.matches("event: message\n").count(), 1, "{remainder:?}");
        assert_eq!(remainder.matches("data: ").count(), 1, "{remainder:?}");
        let data = remainder
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("one JSON-RPC data line");
        let payload: Value = serde_json::from_str(data).expect("JSON-RPC SSE payload");
        assert_eq!(
            payload,
            json!({
                "jsonrpc": "2.0",
                "id": 41,
                "result": { "status": "done" }
            })
        );
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn tools_call_sse_body_drop_does_not_cancel_owned_tool_future() {
        let dropped = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let probe = DropProbe(Arc::clone(&dropped));
        let completed_in_tool = Arc::clone(&completed);
        let registry = Arc::new(McpInFlightToolCallRegistry::new(4, 2));
        let response_id = json!(51);
        let receiver = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            in_flight_key(3, 4, "session-disconnect", &response_id),
            response_id,
            async move {
                let _probe = probe;
                tokio::time::sleep(Duration::from_millis(60)).await;
                completed_in_tool.store(true, Ordering::SeqCst);
                success_response(Some(json!(51)), json!({ "status": "done" }))
            },
            Duration::from_secs(1),
        );
        let response = finalize_mcp_tool_call_sse_response_with_interval(
            receiver,
            "request-sse-cancel",
            Duration::from_millis(5),
        );

        let mut body = response.into_body();
        let first = next_body_data(&mut body).await.expect("ready frame");
        assert_eq!(first.as_ref(), b": ready\n\n");
        let keepalive = next_body_data(&mut body).await.expect("keepalive frame");
        assert_eq!(keepalive.as_ref(), b": keep-alive\n\n");
        assert!(!dropped.load(Ordering::SeqCst));

        drop(body);
        tokio::time::timeout(Duration::from_millis(250), async {
            while !completed.load(Ordering::SeqCst)
                || !dropped.load(Ordering::SeqCst)
                || registry.len() != 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("tool must complete after the response stream disconnects");

        assert!(completed.load(Ordering::SeqCst));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn cancelled_notification_cancels_exact_owned_tool_call_without_final_response() {
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let probe = DropProbe(Arc::clone(&dropped));
        let started_in_tool = Arc::clone(&started);
        let registry = Arc::new(McpInFlightToolCallRegistry::new(4, 2));
        let response_id = json!(61);
        let key = in_flight_key(5, 6, "session-cancel", &response_id);
        let receiver = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            key.clone(),
            response_id,
            async move {
                let _probe = probe;
                started_in_tool.store(true, Ordering::SeqCst);
                std::future::pending::<McpJsonRpcResponse>().await
            },
            Duration::from_secs(1),
        );
        let response = finalize_mcp_tool_call_sse_response_with_interval(
            receiver,
            "request-sse-explicit-cancel",
            Duration::from_millis(5),
        );
        let mut body = response.into_body();
        assert_eq!(next_body_data(&mut body).await.expect("ready").as_ref(), b": ready\n\n");
        assert_eq!(
            next_body_data(&mut body).await.expect("keepalive").as_ref(),
            b": keep-alive\n\n"
        );
        assert!(started.load(Ordering::SeqCst));

        let wrong_auth = auth_context(500, 6);
        assert!(!cancel_mcp_in_flight_tool_call(
            &wrong_auth,
            McpSessionScope(mcp_scope_digest(b"session-cancel")),
            Some(&json!({"requestId": 61, "reason": "not owned"})),
            &registry,
        ));
        assert_eq!(registry.len(), 1);

        let owner_auth = auth_context(5, 6);
        assert!(cancel_mcp_in_flight_tool_call(
            &owner_auth,
            McpSessionScope(mcp_scope_digest(b"session-cancel")),
            Some(&json!({"requestId": 61, "reason": "user cancelled"})),
            &registry,
        ));
        let end = tokio::time::timeout(Duration::from_millis(100), next_body_data(&mut body))
            .await
            .expect("cancelled stream must close promptly");
        assert!(end.is_none(), "cancelled request must not emit a JSON-RPC response");
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn owned_session_teardown_cancels_all_local_calls_but_not_foreign_work() {
        let registry = Arc::new(McpInFlightToolCallRegistry::new(6, 6));
        let owner = auth_context(21, 22);
        let session = "0190ee45-42f0-7df1-8000-000000000001";
        let first_key = in_flight_key(21, 22, session, &json!(101));
        let second_key = in_flight_key(21, 22, session, &json!(102));
        let foreign_key = in_flight_key(21, 23, session, &json!(103));
        let first = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            first_key,
            json!(101),
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(1),
        );
        let second = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            second_key,
            json!(102),
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(1),
        );
        let foreign = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            foreign_key.clone(),
            json!(103),
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(1),
        );

        let session_id =
            super::session::McpSessionId::from_digest(mcp_scope_digest(session.as_bytes()));
        assert_eq!(
            registry.cancel_session(super::session::McpSessionOwner::from_auth(&owner), session_id),
            2
        );
        assert!(first.await.is_err());
        assert!(second.await.is_err());
        assert_eq!(registry.len(), 1);
        assert!(registry.cancel(&foreign_key));
        assert!(foreign.await.is_err());
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn tools_call_deadline_emits_one_error_and_cleans_registry() {
        let dropped = Arc::new(AtomicBool::new(false));
        let probe = DropProbe(Arc::clone(&dropped));
        let registry = Arc::new(McpInFlightToolCallRegistry::new(4, 2));
        let response_id = json!(71);
        let receiver = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            in_flight_key(7, 8, "session-timeout", &response_id),
            response_id,
            async move {
                let _probe = probe;
                std::future::pending::<McpJsonRpcResponse>().await
            },
            Duration::from_millis(25),
        );
        let response = finalize_mcp_tool_call_sse_response_with_interval(
            receiver,
            "request-sse-timeout",
            Duration::from_millis(5),
        );
        let mut body = response.into_body();
        let mut frames = String::new();
        while let Some(data) =
            tokio::time::timeout(Duration::from_millis(200), next_body_data(&mut body))
                .await
                .expect("deadline-bounded stream must terminate")
        {
            frames.push_str(std::str::from_utf8(&data).expect("UTF-8 SSE frame"));
        }

        assert_eq!(frames.matches("event: message\n").count(), 1, "{frames:?}");
        let data = frames
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("timeout JSON-RPC frame");
        let payload: Value = serde_json::from_str(data).expect("timeout JSON-RPC payload");
        assert_eq!(payload["id"], json!(71));
        assert_eq!(payload["error"]["data"]["errorKind"], json!("tool_call_deadline_exceeded"));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn duplicate_in_flight_request_id_fails_without_replacing_original() {
        let registry = Arc::new(McpInFlightToolCallRegistry::new(4, 2));
        let response_id = json!(81);
        let key = in_flight_key(9, 10, "session-duplicate", &response_id);
        let original = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            key.clone(),
            response_id.clone(),
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(1),
        );
        let duplicate = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            key.clone(),
            response_id,
            std::future::ready(success_response(Some(json!(81)), json!({"wrong": true}))),
            Duration::from_secs(1),
        );

        let duplicate_response = duplicate.await.expect("duplicate admission response");
        let duplicate_json = serde_json::to_value(duplicate_response).expect("serialize duplicate");
        assert_eq!(duplicate_json["error"]["data"]["errorKind"], json!("duplicate_request_id"));
        assert_eq!(registry.len(), 1, "original registration must remain owned");

        assert!(registry.cancel(&key));
        assert!(original.await.is_err(), "explicit cancellation sends no original response");
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn in_flight_registry_enforces_principal_and_process_caps() {
        let registry = Arc::new(McpInFlightToolCallRegistry::new(2, 1));
        let first_id = json!(91);
        let first_key = in_flight_key(11, 12, "session-cap-a", &first_id);
        let first = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            first_key.clone(),
            first_id,
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(1),
        );

        let principal_rejected = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            in_flight_key(11, 12, "session-cap-a", &json!(92)),
            json!(92),
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(1),
        )
        .await
        .expect("per-principal capacity response");
        assert_eq!(
            serde_json::to_value(principal_rejected).expect("serialize capacity")["error"]["data"]
                ["errorKind"],
            json!("tool_call_process_capacity_exceeded")
        );

        let second_id = json!(93);
        let second_key = in_flight_key(13, 14, "session-cap-b", &second_id);
        let second = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            second_key.clone(),
            second_id,
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(1),
        );
        let process_rejected = start_bounded_mcp_tool_call(
            Arc::clone(&registry),
            in_flight_key(15, 16, "session-cap-c", &json!(94)),
            json!(94),
            std::future::pending::<McpJsonRpcResponse>(),
            Duration::from_secs(1),
        )
        .await
        .expect("global capacity response");
        assert_eq!(
            serde_json::to_value(process_rejected).expect("serialize capacity")["error"]["data"]["errorKind"],
            json!("tool_call_process_capacity_exceeded")
        );

        assert!(registry.cancel(&first_key));
        assert!(registry.cancel(&second_key));
        assert!(first.await.is_err());
        assert!(second.await.is_err());
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn json_tools_response_contract_remains_unchanged() {
        let response = finalize_mcp_response(
            success_response(Some(json!(7)), json!({ "value": "stable" })),
            McpAcceptPreference::Json,
            None,
            "request-json-contract",
        );
        assert_eq!(
            response.headers().get("content-type").and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let bytes = body::to_bytes(response.into_body(), 4096).await.expect("JSON response body");
        let payload: Value = serde_json::from_slice(&bytes).expect("JSON-RPC JSON payload");

        assert_eq!(
            payload,
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "result": { "value": "stable" }
            })
        );
    }

    #[test]
    fn accept_preference_ignores_event_stream_with_zero_quality() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream;q=0"),
        );

        assert!(matches!(accept_preference(&headers), McpAcceptPreference::Json));
    }

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
