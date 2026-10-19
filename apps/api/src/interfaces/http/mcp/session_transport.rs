use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use axum::{
    Json, body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        router_support::{attach_request_id_header, ensure_or_generate_request_id},
    },
};

use super::{
    MCP_IN_FLIGHT_TOOL_CALLS, MCP_PROTOCOL_VERSION, build_mcp_response_or_internal_error, session,
    with_request_id,
};

/// Interval between SSE keep-alive comments emitted on the idle
/// `GET /v1/mcp` stream. 25 s sits comfortably below every proxy
/// idle-read timeout we care about (nginx default 60 s, the gateway's
/// default 75 s) so the connection stays warm without generating
/// meaningful traffic. mcp-remote only stops its reconnect loop when
/// the stream stays alive past the handshake.
const MCP_GET_STREAM_KEEPALIVE: Duration = Duration::from_secs(25);

static MCP_SESSIONS: LazyLock<Arc<session::McpLocalSessionRegistry>> = LazyLock::new(|| {
    Arc::new(session::McpLocalSessionRegistry::new(
        session::MCP_SESSION_PROCESS_LIMIT,
        session::MCP_SESSION_PER_PRINCIPAL_PROCESS_LIMIT,
        session::MCP_GET_STREAM_PROCESS_LIMIT,
        session::MCP_GET_STREAM_PER_PRINCIPAL_PROCESS_LIMIT,
    ))
});

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpSessionAccessError {
    MissingOrForeign,
    CoordinationUnavailable,
    ProcessCapacity,
    PrincipalProcessCapacity,
}

pub(super) fn mcp_session_access_error_response(
    error: McpSessionAccessError,
    request_id: &str,
) -> Response {
    let status = match error {
        McpSessionAccessError::MissingOrForeign => StatusCode::NOT_FOUND,
        McpSessionAccessError::CoordinationUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        McpSessionAccessError::ProcessCapacity
        | McpSessionAccessError::PrincipalProcessCapacity => StatusCode::TOO_MANY_REQUESTS,
    };
    with_request_id(status.into_response(), request_id)
}

pub(super) fn mcp_protocol_header_error_response(request_id: &str) -> Response {
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

#[derive(Debug, Eq, PartialEq)]
enum McpSessionHeaderError {
    Session,
    Protocol,
}

impl McpSessionHeaderError {
    fn into_response(self, request_id: &str) -> Response {
        match self {
            Self::Session => mcp_session_access_error_response(
                McpSessionAccessError::MissingOrForeign,
                request_id,
            ),
            Self::Protocol => mcp_protocol_header_error_response(request_id),
        }
    }
}

fn required_session_headers(
    headers: &HeaderMap,
) -> Result<session::McpSessionId, McpSessionHeaderError> {
    let session_id =
        session::required_session_id(headers).map_err(|_| McpSessionHeaderError::Session)?;
    session::required_protocol_version(headers).map_err(|_| McpSessionHeaderError::Protocol)?;
    Ok(session_id)
}

/// One owning outcome for every bounded Redis coordination call this module
/// makes. `Completed` carries the operation's own typed domain outcome; the
/// remaining two keep a backend transport error distinct from budget expiry so
/// callers can map each to the exact status, log level and cleanup behaviour
/// instead of re-deriving the `timeout(Result<…>)` shape four times.
#[derive(Debug)]
enum McpSessionCoordinationOutcome<T> {
    Completed(T),
    BackendError(redis::RedisError),
    TimedOut,
}

/// Run one MCP session coordination operation under the shared coordination
/// budget and decode it into [`McpSessionCoordinationOutcome`]. This is the
/// single decoder for the repeated timeout/backend-error pattern; every
/// coordination call site folds the result through it.
async fn await_session_coordination<T>(
    operation: impl std::future::Future<Output = Result<T, redis::RedisError>>,
) -> McpSessionCoordinationOutcome<T> {
    match tokio::time::timeout(session::MCP_SESSION_COORDINATION_TIMEOUT, operation).await {
        Ok(Ok(outcome)) => McpSessionCoordinationOutcome::Completed(outcome),
        Ok(Err(error)) => McpSessionCoordinationOutcome::BackendError(error),
        Err(_) => McpSessionCoordinationOutcome::TimedOut,
    }
}

/// Transport decision for a Redis session-validation outcome. `Active`
/// admits; a missing/foreign/terminated session is a client-visible 404; an
/// unparseable script result is a coordinator fault and is logged here.
/// Decoding requires no Redis connection.
fn decode_session_validation_outcome(
    outcome: session::McpSessionValidationOutcome,
) -> Result<(), McpSessionAccessError> {
    match outcome {
        session::McpSessionValidationOutcome::Active => Ok(()),
        session::McpSessionValidationOutcome::MissingOrForeign
        | session::McpSessionValidationOutcome::Terminated => {
            Err(McpSessionAccessError::MissingOrForeign)
        }
        session::McpSessionValidationOutcome::InvalidResponse => {
            tracing::error!("invalid Redis response for MCP session validation");
            Err(McpSessionAccessError::CoordinationUnavailable)
        }
    }
}

/// Pure transport decision for a local session-admission outcome. `None`
/// admits the registered wire id; `Some(error)` is the exact access error the
/// caller must surface after cleaning up the just-registered distributed
/// session. Explicit process/principal capacity is preserved rather than
/// collapsed into the generic coordinator-unavailable case.
fn decode_session_admission_outcome(
    outcome: session::McpLocalSessionAdmissionOutcome,
) -> Option<McpSessionAccessError> {
    match outcome {
        session::McpLocalSessionAdmissionOutcome::Accepted => None,
        session::McpLocalSessionAdmissionOutcome::MissingOrForeign => {
            Some(McpSessionAccessError::CoordinationUnavailable)
        }
        session::McpLocalSessionAdmissionOutcome::ProcessCapacity => {
            Some(McpSessionAccessError::ProcessCapacity)
        }
        session::McpLocalSessionAdmissionOutcome::PrincipalProcessCapacity => {
            Some(McpSessionAccessError::PrincipalProcessCapacity)
        }
    }
}

fn log_session_coordination_error(error: &redis::RedisError, message: &'static str) {
    tracing::warn!(%error, "{message}");
}

fn log_session_coordination_warning(message: &'static str) {
    tracing::warn!("{message}");
}

fn log_session_coordination_fault(message: &'static str) {
    tracing::error!("{message}");
}

async fn validate_distributed_mcp_session(
    redis: &redis::Client,
    owner: session::McpSessionOwner,
    session_id: session::McpSessionId,
) -> Result<(), McpSessionAccessError> {
    match await_session_coordination(session::validate_session(
        redis,
        owner,
        session_id,
        session::MCP_SESSION_TTL,
    ))
    .await
    {
        McpSessionCoordinationOutcome::Completed(outcome) => {
            decode_session_validation_outcome(outcome)
        }
        McpSessionCoordinationOutcome::BackendError(error) => {
            log_session_coordination_error(&error, "MCP session validation failed");
            Err(McpSessionAccessError::CoordinationUnavailable)
        }
        McpSessionCoordinationOutcome::TimedOut => {
            log_session_coordination_warning("MCP session validation timed out");
            Err(McpSessionAccessError::CoordinationUnavailable)
        }
    }
}

pub(super) async fn validate_and_accept_mcp_session(
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
    match await_session_coordination(session::terminate_session(
        redis,
        owner,
        session_id,
        session::MCP_SESSION_TERMINATION_TTL,
    ))
    .await
    {
        McpSessionCoordinationOutcome::Completed(
            session::McpSessionTerminationOutcome::Terminated,
        ) => {}
        McpSessionCoordinationOutcome::Completed(_) => {
            log_session_coordination_warning(
                "new MCP session cleanup returned an invalid ownership outcome",
            );
        }
        McpSessionCoordinationOutcome::BackendError(error) => {
            log_session_coordination_error(
                &error,
                "new MCP session cleanup failed; TTL recovery remains active",
            );
        }
        McpSessionCoordinationOutcome::TimedOut => {
            log_session_coordination_warning(
                "new MCP session cleanup timed out; TTL recovery remains active",
            );
        }
    }
}

/// Admit a just-registered distributed session to the local registry and
/// return the wire id. On refusal the distributed registration is tombstoned
/// before the exact access error is surfaced, so a capacity or missing-owner
/// rejection cannot leave an owned coordination key behind. The admission
/// outcome is decoded purely; this helper only sequences cleanup.
async fn admit_registered_mcp_session(
    redis: &redis::Client,
    owner: session::McpSessionOwner,
    session_id: session::McpSessionId,
    wire: String,
) -> Result<String, McpSessionAccessError> {
    match decode_session_admission_outcome(MCP_SESSIONS.accept_validated(owner, session_id)) {
        None => Ok(wire),
        Some(error) => {
            cleanup_registered_mcp_session(redis, owner, session_id).await;
            Err(error)
        }
    }
}

pub(super) async fn issue_registered_mcp_session(
    redis: &redis::Client,
    owner: session::McpSessionOwner,
) -> Result<String, McpSessionAccessError> {
    const ISSUE_ATTEMPTS: usize = 3;
    for _ in 0..ISSUE_ATTEMPTS {
        let (wire, session_id) = session::issue_session_id();
        let registration = await_session_coordination(session::register_session(
            redis,
            owner,
            session_id,
            session::MCP_SESSION_TTL,
        ))
        .await;
        match registration {
            McpSessionCoordinationOutcome::Completed(
                session::McpSessionRegistrationOutcome::Registered,
            ) => return admit_registered_mcp_session(redis, owner, session_id, wire).await,
            McpSessionCoordinationOutcome::Completed(
                session::McpSessionRegistrationOutcome::Collision,
            ) => continue,
            McpSessionCoordinationOutcome::Completed(
                session::McpSessionRegistrationOutcome::InvalidResponse,
            ) => {
                log_session_coordination_fault(
                    "invalid Redis response for MCP session registration",
                );
                return Err(McpSessionAccessError::CoordinationUnavailable);
            }
            McpSessionCoordinationOutcome::BackendError(error) => {
                log_session_coordination_error(&error, "MCP session registration failed");
                return Err(McpSessionAccessError::CoordinationUnavailable);
            }
            McpSessionCoordinationOutcome::TimedOut => {
                log_session_coordination_warning("MCP session registration timed out");
                return Err(McpSessionAccessError::CoordinationUnavailable);
            }
        }
    }
    log_session_coordination_warning("MCP session identifier collision budget exhausted");
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
pub(super) async fn handle_get_stream(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);
    let session_id = match required_session_headers(&headers) {
        Ok(session_id) => session_id,
        Err(error) => return error.into_response(&request_id),
    };
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
pub(super) async fn handle_delete_session(
    auth: AuthContext,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = ensure_or_generate_request_id(&headers);
    let session_id = match required_session_headers(&headers) {
        Ok(session_id) => session_id,
        Err(error) => return error.into_response(&request_id),
    };
    let owner = session::McpSessionOwner::from_auth(&auth);
    match await_session_coordination(session::terminate_session(
        &state.persistence.redis,
        owner,
        session_id,
        session::MCP_SESSION_TERMINATION_TTL,
    ))
    .await
    {
        McpSessionCoordinationOutcome::Completed(
            session::McpSessionTerminationOutcome::Terminated,
        ) => {}
        McpSessionCoordinationOutcome::Completed(
            session::McpSessionTerminationOutcome::MissingOrForeign,
        ) => {
            return mcp_session_access_error_response(
                McpSessionAccessError::MissingOrForeign,
                &request_id,
            );
        }
        McpSessionCoordinationOutcome::Completed(
            session::McpSessionTerminationOutcome::InvalidResponse,
        ) => {
            tracing::error!("invalid Redis response for MCP session termination");
            cancel_local_mcp_session(owner, session_id);
            return mcp_session_access_error_response(
                McpSessionAccessError::CoordinationUnavailable,
                &request_id,
            );
        }
        McpSessionCoordinationOutcome::BackendError(error) => {
            tracing::warn!(%error, "MCP session termination failed");
            cancel_local_mcp_session(owner, session_id);
            return mcp_session_access_error_response(
                McpSessionAccessError::CoordinationUnavailable,
                &request_id,
            );
        }
        McpSessionCoordinationOutcome::TimedOut => {
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

#[cfg(test)]
mod tests {
    use super::{
        HeaderMap, HeaderValue, McpSessionAccessError, McpSessionCoordinationOutcome,
        McpSessionHeaderError, StatusCode, await_session_coordination,
        decode_session_admission_outcome, decode_session_validation_outcome,
        required_session_headers,
    };
    use crate::interfaces::http::mcp::session::{
        McpLocalSessionAdmissionOutcome, McpSessionValidationOutcome, issue_session_id,
    };
    use crate::interfaces::http::mcp::{
        MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER,
    };

    #[test]
    fn session_headers_keep_session_error_precedence_and_request_id() {
        let mut headers = HeaderMap::new();
        assert_eq!(required_session_headers(&headers), Err(McpSessionHeaderError::Session));
        let (wire, session_id) = issue_session_id();
        headers.insert(MCP_SESSION_HEADER, wire.parse().expect("issued session header"));
        assert_eq!(required_session_headers(&headers), Err(McpSessionHeaderError::Protocol));
        headers.insert(MCP_PROTOCOL_HEADER, HeaderValue::from_static(MCP_PROTOCOL_VERSION));
        assert_eq!(required_session_headers(&headers), Ok(session_id));
        headers.append(MCP_SESSION_HEADER, wire.parse().expect("duplicate session header"));
        assert_eq!(required_session_headers(&headers), Err(McpSessionHeaderError::Session));

        for (error, status) in [
            (McpSessionHeaderError::Session, StatusCode::NOT_FOUND),
            (McpSessionHeaderError::Protocol, StatusCode::BAD_REQUEST),
        ] {
            let response = error.into_response("session-headers-test");
            assert_eq!(response.status(), status);
            assert_eq!(response.headers()["x-request-id"], "session-headers-test");
        }
    }

    #[test]
    fn validation_outcome_decision_preserves_admit_foreign_and_coordinator_fault() {
        assert_eq!(decode_session_validation_outcome(McpSessionValidationOutcome::Active), Ok(()));
        assert_eq!(
            decode_session_validation_outcome(McpSessionValidationOutcome::MissingOrForeign),
            Err(McpSessionAccessError::MissingOrForeign)
        );
        assert_eq!(
            decode_session_validation_outcome(McpSessionValidationOutcome::Terminated),
            Err(McpSessionAccessError::MissingOrForeign)
        );
        assert_eq!(
            decode_session_validation_outcome(McpSessionValidationOutcome::InvalidResponse),
            Err(McpSessionAccessError::CoordinationUnavailable)
        );
    }

    #[test]
    fn admission_outcome_decision_keeps_capacity_distinct_from_missing_owner() {
        assert_eq!(
            decode_session_admission_outcome(McpLocalSessionAdmissionOutcome::Accepted),
            None
        );
        assert_eq!(
            decode_session_admission_outcome(McpLocalSessionAdmissionOutcome::MissingOrForeign),
            Some(McpSessionAccessError::CoordinationUnavailable)
        );
        assert_eq!(
            decode_session_admission_outcome(McpLocalSessionAdmissionOutcome::ProcessCapacity),
            Some(McpSessionAccessError::ProcessCapacity)
        );
        assert_eq!(
            decode_session_admission_outcome(
                McpLocalSessionAdmissionOutcome::PrincipalProcessCapacity
            ),
            Some(McpSessionAccessError::PrincipalProcessCapacity)
        );
    }

    #[tokio::test]
    async fn coordination_decoder_keeps_completed_outcome_and_backend_error_distinct() {
        let completed =
            await_session_coordination(async { Ok::<_, redis::RedisError>(7_u8) }).await;
        assert!(matches!(completed, McpSessionCoordinationOutcome::Completed(7)));

        let backend = await_session_coordination(async {
            Err::<u8, _>(redis::RedisError::from((
                redis::ErrorKind::Extension,
                "synthetic backend failure",
            )))
        })
        .await;
        assert!(matches!(backend, McpSessionCoordinationOutcome::BackendError(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn coordination_decoder_reports_budget_expiry_as_timed_out() {
        // `start_paused` auto-advances deterministically to the coordination
        // budget (`MCP_SESSION_COORDINATION_TIMEOUT`) instead of waiting 2 s.
        let outcome =
            await_session_coordination(std::future::pending::<Result<u8, redis::RedisError>>())
                .await;
        assert!(matches!(outcome, McpSessionCoordinationOutcome::TimedOut));
    }
}
