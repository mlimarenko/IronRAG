use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tracing::Instrument as _;

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        router_support::{attach_request_id_header, ensure_or_generate_request_id},
    },
};

use super::{
    MCP_IN_FLIGHT_TOOL_CALLS, MCP_JSONRPC_VERSION, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER,
    MCP_TOOL_CALL_COORDINATION_TIMEOUT, MCP_TOOL_CALL_DEADLINE, McpDistributedToolCallAdmission,
    McpInFlightRequestKey, McpInFlightToolCallRegistry, McpJsonRpcRequest, McpJsonRpcResponse,
    McpSessionAccessError, McpSessionScope, McpToolCallDispatch, McpToolSurface,
    build_mcp_response_or_internal_error, error_response, handle_initialize,
    handle_owned_mcp_tool_call, issue_registered_mcp_session,
    mark_distributed_mcp_tool_call_cancelled, mcp_distributed_coordination_keys,
    mcp_in_flight_request_key, mcp_protocol_header_error_response,
    mcp_session_access_error_response, mcp_session_scope, parse_mcp_jsonrpc_request, session,
    start_distributed_bounded_mcp_tool_call, tools, valid_initialize_params,
    validate_and_accept_mcp_session, with_request_id,
};

/// Maximum idle period between frames on a long-running SSE `tools/call`.
/// The stream carries only one bounded comment per interval, so it keeps
/// proxies and clients alive without buffering progress or detaching work.
const MCP_TOOL_CALL_SSE_KEEPALIVE: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpCancelledNotificationParams {
    request_id: Value,
    reason: Option<String>,
}

struct ParsedMcpTransportRequest {
    request_id: String,
    accept: McpAcceptPreference,
    supplied_session: Result<Option<session::McpSessionId>, session::McpSessionHeaderError>,
    has_supported_protocol_version: bool,
    request: McpJsonRpcRequest,
}

/// Cohesive transport-side identity shared by every response finalizer of one
/// MCP HTTP request: which request id to echo, which representation to emit,
/// and which tool surface the call belongs to. Bundling these three removes the
/// repeated `(request_id, accept, surface)` positional triple from every
/// dispatch/finalize signature.
struct McpTransportContext {
    request_id: String,
    accept: McpAcceptPreference,
    surface: McpToolSurface,
}

impl McpTransportContext {
    fn from_parsed(parsed: &ParsedMcpTransportRequest, surface: McpToolSurface) -> Self {
        Self { request_id: parsed.request_id.clone(), accept: parsed.accept, surface }
    }
}

// The error side is an already-built HTTP response. Boxing it keeps the
// `Err` arm small enough for `clippy::result_large_err`; the response is
// returned immediately by the single caller, so the extra indirection is a
// pointer move, not a copy.
async fn parse_mcp_transport_request(
    state: &AppState,
    request: Request,
) -> Result<ParsedMcpTransportRequest, Box<Response>> {
    let request_id = ensure_or_generate_request_id(request.headers());
    let accept = accept_preference(request.headers());
    let supplied_session = session::optional_session_id(request.headers());
    let has_supported_protocol_version =
        session::required_protocol_version(request.headers()).is_ok();
    let request = parse_mcp_jsonrpc_request(state, request)
        .await
        .map_err(|response| finalize_mcp_response(*response, accept, None, &request_id))?;
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
    context: &McpTransportContext,
    supplied_session: Result<Option<session::McpSessionId>, session::McpSessionHeaderError>,
    request: McpJsonRpcRequest,
) -> Response {
    let request_id = context.request_id.as_str();
    let accept = context.accept;
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

    let mut response =
        handle_initialize(auth, state, request_id, request.id, context.surface).await;
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
    session_scope: McpSessionScope,
    context: McpTransportContext,
) -> Response {
    let request_id = context.request_id.as_str();
    let accept = context.accept;
    let surface = context.surface;
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
    // IRG-203 SITE-5: read the mutation class from the single registry
    // (`TOOL_REGISTRY` via `tools::is_mutating_tool`) before the params are
    // moved into the dispatch. `start_distributed_bounded_mcp_tool_call`
    // forwards it to the chokepoint that detaches a mutating dispatch and
    // holds its admission until the durable write finishes. The same name is
    // parsed again by `handle_tools_call`; a malformed params value is
    // non-mutating here and is rejected there with an invalid-arguments
    // result.
    let mutating = params
        .as_ref()
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .is_some_and(tools::is_mutating_tool);
    let redis = state.persistence.redis.clone();
    let tool_future =
        handle_owned_mcp_tool_call(auth, state, request_id.to_string(), id, params, surface)
            .instrument(tracing::Span::current());
    let receiver =
        Box::pin(start_distributed_bounded_mcp_tool_call(McpDistributedToolCallAdmission {
            registry: Arc::clone(&MCP_IN_FLIGHT_TOOL_CALLS),
            redis,
            key,
            dispatch: McpToolCallDispatch {
                response_id,
                response_future: tool_future,
                deadline: MCP_TOOL_CALL_DEADLINE,
                mutating,
            },
        }))
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

pub(super) async fn handle_jsonrpc_for_surface(
    auth: AuthContext,
    State(state): State<AppState>,
    request: Request,
    surface: McpToolSurface,
) -> Response {
    let parsed = match parse_mcp_transport_request(&state, request).await {
        Ok(parsed) => parsed,
        Err(response) => return *response,
    };
    let context = McpTransportContext::from_parsed(&parsed, surface);
    if let Some(response) = validate_mcp_transport_envelope(&auth, &state, &context, &parsed).await
    {
        return response;
    }
    Box::pin(handle_session_mcp_transport_request(auth, state, context, parsed)).await
}

async fn validate_mcp_transport_envelope(
    auth: &AuthContext,
    state: &AppState,
    context: &McpTransportContext,
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
            context,
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
    context: McpTransportContext,
    parsed: ParsedMcpTransportRequest,
) -> Response {
    let ParsedMcpTransportRequest {
        supplied_session, has_supported_protocol_version, request, ..
    } = parsed;
    let session_scope = match resolve_mcp_transport_session(
        &auth,
        &state,
        supplied_session,
        has_supported_protocol_version,
        &context.request_id,
    )
    .await
    {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    if let Some(response) =
        handle_mcp_notification(&auth, &state, session_scope, &context.request_id, &request).await
    {
        return response;
    }
    dispatch_session_mcp_method(auth, state, request, session_scope, context).await
}

async fn resolve_mcp_transport_session(
    auth: &AuthContext,
    state: &AppState,
    supplied_session: Result<Option<session::McpSessionId>, session::McpSessionHeaderError>,
    has_supported_protocol_version: bool,
    request_id: &str,
) -> Result<McpSessionScope, Box<Response>> {
    let Ok(Some(session_id)) = supplied_session else {
        return Err(Box::new(mcp_session_access_error_response(
            McpSessionAccessError::MissingOrForeign,
            request_id,
        )));
    };
    if !has_supported_protocol_version {
        return Err(Box::new(mcp_protocol_header_error_response(request_id)));
    }
    let owner = session::McpSessionOwner::from_auth(auth);
    validate_and_accept_mcp_session(&state.persistence.redis, owner, session_id)
        .await
        .map_err(|error| Box::new(mcp_session_access_error_response(error, request_id)))?;
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
    session_scope: McpSessionScope,
    context: McpTransportContext,
) -> Response {
    if request.method == "tools/call" {
        return handle_session_tool_call(auth, state, request, session_scope, context).await;
    }
    let response = match request.method.as_str() {
        "tools/list" => {
            tools::handle_tools_list(
                &auth,
                &state,
                &context.request_id,
                request.id,
                context.surface,
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
    finalize_mcp_response(response, context.accept, None, &context.request_id)
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
    let distributed_cancelled =
        mark_owned_mcp_tool_call_cancelled_within_coordination_timeout(redis, &key).await;
    let cancelled = registry.cancel(&key);
    tracing::debug!(
        local_cancelled = cancelled,
        distributed_cancelled,
        reason_present,
        "processed distributed MCP cancelled notification"
    );
    distributed_cancelled || cancelled
}

/// Writes the distributed cancellation marker for one in-flight key under the
/// shared coordination timeout. Any connection error or timeout is logged and
/// reported as not-marked so the caller still attempts the local cancellation.
async fn mark_owned_mcp_tool_call_cancelled_within_coordination_timeout(
    redis: &redis::Client,
    key: &McpInFlightRequestKey,
) -> bool {
    let coordination_keys = mcp_distributed_coordination_keys(key);
    match tokio::time::timeout(
        MCP_TOOL_CALL_COORDINATION_TIMEOUT,
        mark_distributed_mcp_tool_call_cancelled(redis, &coordination_keys),
    )
    .await
    {
        Ok(Ok(marked)) => marked,
        Ok(Err(error)) => {
            log_distributed_mcp_cancellation_write_failed(&error);
            false
        }
        Err(_) => {
            log_distributed_mcp_cancellation_write_timed_out();
            false
        }
    }
}

fn log_distributed_mcp_cancellation_write_failed(error: &redis::RedisError) {
    tracing::warn!(%error, "MCP distributed cancellation write failed");
}

fn log_distributed_mcp_cancellation_write_timed_out() {
    tracing::warn!("MCP distributed cancellation write timed out");
}

#[cfg(test)]
pub(super) fn cancel_mcp_in_flight_tool_call(
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
pub(super) enum McpAcceptPreference {
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

pub(super) fn finalize_mcp_tool_call_sse_response_with_interval(
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

pub(super) fn accept_preference(headers: &HeaderMap) -> McpAcceptPreference {
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

pub(super) fn finalize_mcp_response(
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
