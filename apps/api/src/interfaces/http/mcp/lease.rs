//! Distributed and in-process admission control for MCP `tools/call`
//! dispatch: bounding per-process and per-principal concurrency, and — for
//! multi-replica deployments — coordinating a single owning replica per
//! request via Redis so a duplicate `tools/call` retry or an SSE
//! reconnect never runs the same tool invocation twice.
//!
//! Split out of the former `interfaces/http/mcp.rs` god-file (plan §6.4):
//! this was one of five unrelated concerns bundled into that single file
//! (transport envelope/lifecycle/dispatch, SSE streaming, session HTTP
//! handling, grounded-answer text processing, and this lease/admission
//! machinery). Everything here is either purely internal to admission
//! control, or `pub(super)` for the handful of call sites that remain in
//! `mcp.rs` (dispatch, cancellation) and its test module.

use std::{
    collections::HashMap,
    future::Future,
    panic::AssertUnwindSafe,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use futures::FutureExt as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{McpJsonRpcResponse, McpToolSurface, error_response, session, tools};
use crate::{app::state::AppState, interfaces::http::auth::AuthContext};

/// Absolute lifetime of one externally-issued tool call. The execution owner
/// is detached from the response body so a transient SSE disconnect does not
/// cancel work, but it is never allowed to outlive this bound.
pub(super) const MCP_TOOL_CALL_DEADLINE: Duration = Duration::from_mins(3);
/// These limits bound one API process. Deployment-wide duplicate ownership and
/// cancellation are coordinated separately through Redis; the chart's bounded
/// API replica/HPA maximum therefore gives a finite deployment-wide ceiling.
const MCP_TOOL_CALL_PROCESS_IN_FLIGHT_LIMIT: usize = 128;
const MCP_TOOL_CALL_PER_PRINCIPAL_PROCESS_IN_FLIGHT_LIMIT: usize = 8;
pub(super) const MCP_TOOL_CALL_COORDINATION_TIMEOUT: Duration = Duration::from_secs(2);
const MCP_TOOL_CALL_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MCP_TOOL_CALL_PENDING_CANCEL_TTL: Duration = Duration::from_secs(5);
const MCP_TOOL_CALL_COORDINATION_KEY_PREFIX: &str = "ironrag:mcp:tool-call:v1";
pub(super) const MCP_TOOL_CALL_CANCEL_PENDING_MARKER: &str = "pending-before-owner";
pub(super) const MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[3]) == 1 then
    return -2
end
local owner = redis.call('GET', KEYS[1])
if owner then
    return 0
end
local cancel = redis.call('GET', KEYS[2])
if cancel == ARGV[2] then
    redis.call('DEL', KEYS[2])
    return -1
end
local acquired = redis.call('SET', KEYS[1], ARGV[1], 'NX', 'PX', ARGV[3])
if acquired then
    return 1
end
return 0
";

pub(super) const MCP_DISTRIBUTED_CANCEL_SCRIPT: &str = r"
local owner = redis.call('GET', KEYS[1])
if owner then
    local ttl = redis.call('PTTL', KEYS[1])
    if ttl > 0 then
        redis.call('SET', KEYS[2], owner, 'PX', ttl)
        return 1
    end
end
local pending = redis.call('SET', KEYS[2], ARGV[1], 'NX', 'PX', ARGV[2])
if pending then
    return 2
end
return 0
";

pub(super) const MCP_DISTRIBUTED_RELEASE_SCRIPT: &str = r"
if redis.call('GET', KEYS[1]) == ARGV[1] then
    redis.call('DEL', KEYS[1])
    if redis.call('GET', KEYS[2]) == ARGV[1] then
        redis.call('DEL', KEYS[2])
    end
    return 1
end
return 0
";
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub(super) struct McpSessionScope(pub(super) [u8; 32]);

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub(super) enum McpRequestIdKey {
    String([u8; 32]),
    Integer(String),
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub(super) struct McpInFlightRequestKey {
    pub(super) principal_id: Uuid,
    pub(super) token_id: Uuid,
    pub(super) session_scope: McpSessionScope,
    pub(super) request_id: McpRequestIdKey,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct McpDistributedCoordinationKeys {
    pub(super) owner: String,
    pub(super) cancel: String,
    pub(super) session_terminated: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpDistributedAdmissionOutcome {
    Acquired,
    DuplicateRequestId,
    CancelledBeforeAdmission,
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum McpDistributedCancelWaitOutcome {
    Cancelled,
    CoordinationUnavailable,
}

/// The result of one distributed cancellation poll. Unlike the wait outcome it
/// keeps a distinct `Pending` state, so [`McpDistributedToolCallLease::poll_cancel_markers`]
/// reports "no marker yet" separately from "Redis unusable".
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum McpDistributedCancelPollOutcome {
    Cancelled,
    Pending,
    CoordinationUnavailable,
}

/// The execution half of one `tools/call`: the pending response the caller
/// waits on, the relative deadline that bounds it, and whether the tool writes
/// durable state (IRG-203 SITE-5). Carrying these together keeps the
/// registration, distributed-lease and spawn wiring from repeating the same
/// argument tuple, and gives the mutating/non-mutating split a single type.
pub(super) struct McpToolCallDispatch<F> {
    pub(super) response_id: Value,
    pub(super) response_future: F,
    pub(super) deadline: Duration,
    pub(super) mutating: bool,
}

/// A `tools/call` that must take the deployment-wide Redis lease before it may
/// execute: the process registry, the Redis client, the per-request
/// coordination key and the execution payload. This is the caller-facing input
/// of [`start_distributed_bounded_mcp_tool_call`].
pub(super) struct McpDistributedToolCallAdmission<F> {
    pub(super) registry: Arc<McpInFlightToolCallRegistry>,
    pub(super) redis: redis::Client,
    pub(super) key: McpInFlightRequestKey,
    pub(super) dispatch: McpToolCallDispatch<F>,
}

/// One registered `tools/call` ready to run: its in-process registration (held
/// for the call's whole life), the optional distributed lease that owns the
/// request id deployment-wide, and the payload the terminal response is built
/// from. [`Self::spawn`] detaches the execution from the caller's response body.
struct McpRegisteredToolCall<F> {
    registration: McpInFlightToolCallRegistration,
    distributed_lease: Option<McpDistributedToolCallLease>,
    response_id: Value,
    response_future: F,
    deadline_at: tokio::time::Instant,
    sender: oneshot::Sender<McpJsonRpcResponse>,
    mutating: bool,
}

impl<F> McpRegisteredToolCall<F>
where
    F: Future<Output = McpJsonRpcResponse> + Send + 'static,
{
    /// Builds the registration-free half from a dispatch plus the already
    /// fixed absolute `deadline_at` (computed by the admission path before the
    /// lease is taken, so Redis latency cannot extend the bound).
    fn from_dispatch(
        registration: McpInFlightToolCallRegistration,
        distributed_lease: Option<McpDistributedToolCallLease>,
        dispatch: McpToolCallDispatch<F>,
        deadline_at: tokio::time::Instant,
        sender: oneshot::Sender<McpJsonRpcResponse>,
    ) -> Self {
        Self {
            registration,
            distributed_lease,
            response_id: dispatch.response_id,
            response_future: dispatch.response_future,
            deadline_at,
            sender,
            mutating: dispatch.mutating,
        }
    }
}

struct McpDistributedToolCallLease {
    redis: redis::Client,
    keys: McpDistributedCoordinationKeys,
    generation: Uuid,
}

struct McpInFlightToolCallEntry {
    generation: Uuid,
    cancellation: CancellationToken,
}

pub(super) struct McpInFlightToolCallRegistry {
    entries: Mutex<HashMap<McpInFlightRequestKey, McpInFlightToolCallEntry>>,
    process_limit: usize,
    per_principal_process_limit: usize,
}

impl McpInFlightToolCallRegistry {
    pub(super) fn new(process_limit: usize, per_principal_process_limit: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            process_limit: process_limit.max(1),
            per_principal_process_limit: per_principal_process_limit.max(1),
        }
    }

    fn try_register(
        self: &Arc<Self>,
        key: McpInFlightRequestKey,
    ) -> Result<McpInFlightToolCallRegistration, McpToolCallAdmissionError> {
        let mut entries = self.lock_entries();
        if entries.contains_key(&key) {
            return Err(McpToolCallAdmissionError::DuplicateRequestId);
        }
        if entries.len() >= self.process_limit {
            return Err(McpToolCallAdmissionError::ProcessCapacity);
        }
        let principal_in_flight =
            entries.keys().filter(|entry| entry.principal_id == key.principal_id).count();
        if principal_in_flight >= self.per_principal_process_limit {
            return Err(McpToolCallAdmissionError::PrincipalProcessCapacity);
        }

        let generation = Uuid::now_v7();
        let cancellation = CancellationToken::new();
        entries.insert(
            key.clone(),
            McpInFlightToolCallEntry { generation, cancellation: cancellation.clone() },
        );
        drop(entries);
        Ok(McpInFlightToolCallRegistration {
            registry: Arc::clone(self),
            key,
            generation,
            cancellation,
        })
    }

    pub(super) fn cancel(&self, key: &McpInFlightRequestKey) -> bool {
        let cancellation = self.lock_entries().get(key).map(|entry| entry.cancellation.clone());
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
            true
        } else {
            false
        }
    }

    pub(super) fn cancel_session(
        &self,
        owner: session::McpSessionOwner,
        session_id: session::McpSessionId,
    ) -> usize {
        let cancellations = self
            .lock_entries()
            .iter()
            .filter(|(key, _)| {
                key.principal_id == owner.principal_id()
                    && key.token_id == owner.token_id()
                    && key.session_scope == McpSessionScope(session_id.digest())
            })
            .map(|(_, entry)| entry.cancellation.clone())
            .collect::<Vec<_>>();
        for cancellation in &cancellations {
            cancellation.cancel();
        }
        cancellations.len()
    }

    fn remove(&self, key: &McpInFlightRequestKey, generation: Uuid) {
        let mut entries = self.lock_entries();
        if entries.get(key).is_some_and(|entry| entry.generation == generation) {
            entries.remove(key);
        }
    }

    fn lock_entries(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<McpInFlightRequestKey, McpInFlightToolCallEntry>> {
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.lock_entries().len()
    }
}

struct McpInFlightToolCallRegistration {
    registry: Arc<McpInFlightToolCallRegistry>,
    key: McpInFlightRequestKey,
    generation: Uuid,
    cancellation: CancellationToken,
}

impl Drop for McpInFlightToolCallRegistration {
    fn drop(&mut self) {
        self.registry.remove(&self.key, self.generation);
    }
}

#[derive(Debug, Clone, Copy)]
enum McpToolCallAdmissionError {
    DuplicateRequestId,
    ProcessCapacity,
    PrincipalProcessCapacity,
}

pub(super) static MCP_IN_FLIGHT_TOOL_CALLS: LazyLock<Arc<McpInFlightToolCallRegistry>> =
    LazyLock::new(|| {
        Arc::new(McpInFlightToolCallRegistry::new(
            MCP_TOOL_CALL_PROCESS_IN_FLIGHT_LIMIT,
            MCP_TOOL_CALL_PER_PRINCIPAL_PROCESS_IN_FLIGHT_LIMIT,
        ))
    });

pub(super) fn mcp_scope_digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

pub(super) const fn mcp_session_scope(session_id: session::McpSessionId) -> McpSessionScope {
    McpSessionScope(session_id.digest())
}

pub(super) fn mcp_request_id_key(request_id: &Value) -> Option<McpRequestIdKey> {
    match request_id {
        Value::String(value) => Some(McpRequestIdKey::String(mcp_scope_digest(value.as_bytes()))),
        Value::Number(value) if value.is_i64() || value.is_u64() => {
            Some(McpRequestIdKey::Integer(value.to_string()))
        }
        _ => None,
    }
}

pub(super) fn mcp_in_flight_request_key(
    auth: &AuthContext,
    session_scope: McpSessionScope,
    request_id: &Value,
) -> Option<McpInFlightRequestKey> {
    Some(McpInFlightRequestKey {
        principal_id: auth.principal_id,
        token_id: auth.token_id,
        session_scope,
        request_id: mcp_request_id_key(request_id)?,
    })
}

pub(super) fn mcp_distributed_coordination_keys(
    key: &McpInFlightRequestKey,
) -> McpDistributedCoordinationKeys {
    let mut scope_hasher = Sha256::new();
    update_mcp_coordination_hash_frame(&mut scope_hasher, b"ironrag-mcp-tool-call-scope-v2");
    update_mcp_coordination_hash_frame(&mut scope_hasher, key.principal_id.as_bytes());
    update_mcp_coordination_hash_frame(&mut scope_hasher, key.token_id.as_bytes());
    update_mcp_coordination_hash_frame(&mut scope_hasher, b"session");
    update_mcp_coordination_hash_frame(&mut scope_hasher, &key.session_scope.0);
    let session_keys =
        session::session_redis_keys(session::McpSessionId::from_digest(key.session_scope.0));
    let hash_tag = hex::encode(key.session_scope.0);
    let session_terminated = session_keys.terminated;

    let mut request_hasher = Sha256::new();
    update_mcp_coordination_hash_frame(&mut request_hasher, b"ironrag-mcp-tool-call-owner-v2");
    update_mcp_coordination_hash_frame(&mut request_hasher, key.principal_id.as_bytes());
    update_mcp_coordination_hash_frame(&mut request_hasher, key.token_id.as_bytes());
    update_mcp_coordination_hash_frame(&mut request_hasher, &scope_hasher.finalize());
    match &key.request_id {
        McpRequestIdKey::String(digest) => {
            update_mcp_coordination_hash_frame(&mut request_hasher, b"string-id");
            update_mcp_coordination_hash_frame(&mut request_hasher, digest);
        }
        McpRequestIdKey::Integer(value) => {
            update_mcp_coordination_hash_frame(&mut request_hasher, b"integer-id");
            update_mcp_coordination_hash_frame(&mut request_hasher, value.as_bytes());
        }
    }
    let request_digest = hex::encode(request_hasher.finalize());
    // The session tombstone and all per-request keys share the session hash
    // slot, so admission remains atomic on standalone and clustered Redis.
    let base = format!("{MCP_TOOL_CALL_COORDINATION_KEY_PREFIX}:{{{hash_tag}}}:{request_digest}");
    McpDistributedCoordinationKeys {
        owner: format!("{base}:owner"),
        cancel: format!("{base}:cancel"),
        session_terminated,
    }
}

pub(super) fn update_mcp_coordination_hash_frame(hasher: &mut Sha256, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(length.to_be_bytes());
    hasher.update(value);
}

pub(super) fn mcp_tool_call_redis_ttl_millis(deadline: Duration) -> u128 {
    deadline.as_millis().max(1)
}

pub(super) const fn mcp_distributed_admission_outcome(code: i64) -> McpDistributedAdmissionOutcome {
    match code {
        1 => McpDistributedAdmissionOutcome::Acquired,
        0 => McpDistributedAdmissionOutcome::DuplicateRequestId,
        -1 | -2 => McpDistributedAdmissionOutcome::CancelledBeforeAdmission,
        _ => McpDistributedAdmissionOutcome::InvalidResponse,
    }
}

/// The whole distributed admission attempt: the coordination round-trip may
/// fail (Redis error), time out (`Elapsed`), or succeed with a coded outcome.
type McpDistributedAdmissionAttempt =
    Result<Result<McpDistributedAdmissionOutcome, redis::RedisError>, tokio::time::error::Elapsed>;

/// How the caller of [`start_distributed_bounded_mcp_tool_call`] must react to
/// an admission attempt. Pure, so the branch table is testable without Redis.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpDistributedAdmissionResolution {
    /// This process owns the call: spawn the registered execution.
    Owned,
    /// The request id is already owned elsewhere: answer duplicate, run nothing.
    Duplicate,
    /// A cancellation or session tombstone arrived before ownership: no answer.
    CancelledBeforeOwnership,
    /// Redis was unusable (error, invalid reply, or timeout): release whatever
    /// might have been acquired and answer with a coordination error.
    CoordinationUnavailable,
}

/// Classifies an admission attempt into the caller's reaction. Every unusable
/// coordination result collapses to [`McpDistributedAdmissionResolution::CoordinationUnavailable`];
/// the distinct log line is chosen separately by [`log_mcp_distributed_admission_unavailable`].
pub(super) fn mcp_distributed_admission_resolution(
    admission: &McpDistributedAdmissionAttempt,
) -> McpDistributedAdmissionResolution {
    match admission {
        Ok(Ok(McpDistributedAdmissionOutcome::Acquired)) => {
            McpDistributedAdmissionResolution::Owned
        }
        Ok(Ok(McpDistributedAdmissionOutcome::DuplicateRequestId)) => {
            McpDistributedAdmissionResolution::Duplicate
        }
        Ok(Ok(McpDistributedAdmissionOutcome::CancelledBeforeAdmission)) => {
            McpDistributedAdmissionResolution::CancelledBeforeOwnership
        }
        Ok(Ok(McpDistributedAdmissionOutcome::InvalidResponse)) | Ok(Err(_)) | Err(_) => {
            McpDistributedAdmissionResolution::CoordinationUnavailable
        }
    }
}

pub(super) fn mcp_cancel_marker_matches_generation(marker: &str, generation: Uuid) -> bool {
    marker == generation.to_string()
}

pub(super) fn mcp_distributed_cancel_markers_cancel_call(
    call_marker: Option<&str>,
    session_terminated: bool,
    generation: Uuid,
) -> bool {
    session_terminated
        || call_marker
            .is_some_and(|marker| mcp_cancel_marker_matches_generation(marker, generation))
}

impl McpDistributedToolCallLease {
    fn new(redis: redis::Client, key: &McpInFlightRequestKey, generation: Uuid) -> Self {
        Self { redis, keys: mcp_distributed_coordination_keys(key), generation }
    }

    async fn try_acquire(
        &self,
        ttl: Duration,
    ) -> Result<McpDistributedAdmissionOutcome, redis::RedisError> {
        let mut connection = self.redis.get_multiplexed_async_connection().await?;
        let code: i64 = redis::cmd("EVAL")
            .arg(MCP_DISTRIBUTED_OWNER_ACQUIRE_SCRIPT)
            .arg(3)
            .arg(&self.keys.owner)
            .arg(&self.keys.cancel)
            .arg(&self.keys.session_terminated)
            .arg(self.generation.to_string())
            .arg(MCP_TOOL_CALL_CANCEL_PENDING_MARKER)
            .arg(mcp_tool_call_redis_ttl_millis(ttl).to_string())
            .query_async(&mut connection)
            .await?;
        Ok(mcp_distributed_admission_outcome(code))
    }

    async fn wait_for_cancel(&self) -> McpDistributedCancelWaitOutcome {
        let Some(mut connection) = self.acquire_cancel_poll_connection().await else {
            return McpDistributedCancelWaitOutcome::CoordinationUnavailable;
        };
        loop {
            match self.poll_cancel_markers(&mut connection).await {
                McpDistributedCancelPollOutcome::Cancelled => {
                    return McpDistributedCancelWaitOutcome::Cancelled;
                }
                McpDistributedCancelPollOutcome::Pending => {}
                McpDistributedCancelPollOutcome::CoordinationUnavailable => {
                    return McpDistributedCancelWaitOutcome::CoordinationUnavailable;
                }
            }
            tokio::time::sleep(MCP_TOOL_CALL_CANCEL_POLL_INTERVAL).await;
        }
    }

    async fn acquire_cancel_poll_connection(&self) -> Option<redis::aio::MultiplexedConnection> {
        match tokio::time::timeout(
            MCP_TOOL_CALL_COORDINATION_TIMEOUT,
            self.redis.get_multiplexed_async_connection(),
        )
        .await
        {
            Ok(Ok(connection)) => Some(connection),
            Ok(Err(error)) => {
                log_cancel_poll_connection_failed(&error);
                None
            }
            Err(_) => {
                log_cancel_poll_connection_timed_out();
                None
            }
        }
    }

    async fn poll_cancel_markers(
        &self,
        connection: &mut redis::aio::MultiplexedConnection,
    ) -> McpDistributedCancelPollOutcome {
        let markers = tokio::time::timeout(
            MCP_TOOL_CALL_COORDINATION_TIMEOUT,
            redis::cmd("MGET")
                .arg(&self.keys.cancel)
                .arg(&self.keys.session_terminated)
                .query_async::<(Option<String>, Option<String>)>(connection),
        )
        .await;
        match markers {
            Ok(Ok((call_marker, session_marker)))
                if mcp_distributed_cancel_markers_cancel_call(
                    call_marker.as_deref(),
                    session_marker.is_some(),
                    self.generation,
                ) =>
            {
                McpDistributedCancelPollOutcome::Cancelled
            }
            Ok(Ok(_)) => McpDistributedCancelPollOutcome::Pending,
            Ok(Err(error)) => {
                log_cancel_poll_failed(&error);
                McpDistributedCancelPollOutcome::CoordinationUnavailable
            }
            Err(_) => {
                log_cancel_poll_timed_out();
                McpDistributedCancelPollOutcome::CoordinationUnavailable
            }
        }
    }

    async fn release(&self) -> Result<bool, redis::RedisError> {
        let mut connection = self.redis.get_multiplexed_async_connection().await?;
        let released: i64 = redis::cmd("EVAL")
            .arg(MCP_DISTRIBUTED_RELEASE_SCRIPT)
            .arg(2)
            .arg(&self.keys.owner)
            .arg(&self.keys.cancel)
            .arg(self.generation.to_string())
            .query_async(&mut connection)
            .await?;
        Ok(released > 0)
    }
}

/// The cancellation-poll `tracing` calls live in their own functions so their
/// expanded formatting machinery does not sit inside the branching bodies of
/// [`McpDistributedToolCallLease::acquire_cancel_poll_connection`] and
/// [`McpDistributedToolCallLease::poll_cancel_markers`].
fn log_cancel_poll_connection_failed(error: &redis::RedisError) {
    tracing::warn!(%error, "MCP distributed cancellation connection failed");
}

/// See [`log_cancel_poll_connection_failed`].
fn log_cancel_poll_connection_timed_out() {
    tracing::warn!("MCP distributed cancellation connection timed out");
}

/// See [`log_cancel_poll_connection_failed`].
fn log_cancel_poll_failed(error: &redis::RedisError) {
    tracing::warn!(%error, "MCP distributed cancellation poll failed");
}

/// See [`log_cancel_poll_connection_failed`].
fn log_cancel_poll_timed_out() {
    tracing::warn!("MCP distributed cancellation poll timed out");
}

pub(super) async fn mark_distributed_mcp_tool_call_cancelled(
    redis: &redis::Client,
    keys: &McpDistributedCoordinationKeys,
) -> Result<bool, redis::RedisError> {
    let mut connection = redis.get_multiplexed_async_connection().await?;
    let marked: i64 = redis::cmd("EVAL")
        .arg(MCP_DISTRIBUTED_CANCEL_SCRIPT)
        .arg(2)
        .arg(&keys.owner)
        .arg(&keys.cancel)
        .arg(MCP_TOOL_CALL_CANCEL_PENDING_MARKER)
        .arg(mcp_tool_call_redis_ttl_millis(MCP_TOOL_CALL_PENDING_CANCEL_TTL).to_string())
        .query_async(&mut connection)
        .await?;
    Ok(marked > 0)
}

fn tool_call_admission_error_response(
    response_id: Value,
    error: McpToolCallAdmissionError,
) -> McpJsonRpcResponse {
    match error {
        McpToolCallAdmissionError::DuplicateRequestId => error_response(
            Some(response_id),
            -32600,
            "duplicate request id",
            Some(json!({ "errorKind": "duplicate_request_id" })),
        ),
        McpToolCallAdmissionError::ProcessCapacity
        | McpToolCallAdmissionError::PrincipalProcessCapacity => error_response(
            Some(response_id),
            -32000,
            "tool call process capacity exceeded",
            Some(json!({ "errorKind": "tool_call_process_capacity_exceeded" })),
        ),
    }
}

fn tool_call_coordination_error_response(response_id: Value) -> McpJsonRpcResponse {
    error_response(
        Some(response_id),
        -32002,
        "tool call coordination unavailable",
        Some(json!({ "errorKind": "tool_call_coordination_unavailable" })),
    )
}

fn tool_call_deadline_error_response(response_id: Value) -> McpJsonRpcResponse {
    error_response(
        Some(response_id),
        -32001,
        "tool call deadline exceeded",
        Some(json!({ "errorKind": "tool_call_deadline_exceeded" })),
    )
}

fn tool_call_panic_error_response(response_id: Value) -> McpJsonRpcResponse {
    error_response(
        Some(response_id),
        -32603,
        "internal error",
        Some(json!({ "errorKind": "tool_call_panicked" })),
    )
}

#[cfg(test)]
pub(super) fn start_bounded_mcp_tool_call<F>(
    registry: Arc<McpInFlightToolCallRegistry>,
    key: McpInFlightRequestKey,
    response_id: Value,
    response_future: F,
    deadline: Duration,
) -> oneshot::Receiver<McpJsonRpcResponse>
where
    F: Future<Output = McpJsonRpcResponse> + Send + 'static,
{
    start_bounded_mcp_tool_call_with_mutation(
        registry,
        key,
        response_id,
        response_future,
        deadline,
        false,
    )
}

/// Test seam for the mutating half of the IRG-203 SITE-5 invariant. Routes a
/// call through the same [`McpRegisteredToolCall::spawn`] used by
/// [`start_distributed_bounded_mcp_tool_call`], so a test exercises the real
/// detach-and-hold-admission wiring rather than re-deriving it.
#[cfg(test)]
pub(super) fn start_bounded_mcp_tool_call_with_mutation<F>(
    registry: Arc<McpInFlightToolCallRegistry>,
    key: McpInFlightRequestKey,
    response_id: Value,
    response_future: F,
    deadline: Duration,
    mutating: bool,
) -> oneshot::Receiver<McpJsonRpcResponse>
where
    F: Future<Output = McpJsonRpcResponse> + Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    let registration = match registry.try_register(key) {
        Ok(registration) => registration,
        Err(error) => {
            let _ = sender.send(tool_call_admission_error_response(response_id, error));
            return receiver;
        }
    };
    let deadline_at = tokio::time::Instant::now() + deadline;
    McpRegisteredToolCall::from_dispatch(
        registration,
        None,
        McpToolCallDispatch { response_id, response_future, deadline, mutating },
        deadline_at,
        sender,
    )
    .spawn();
    receiver
}

pub(super) async fn start_distributed_bounded_mcp_tool_call<F>(
    admission: McpDistributedToolCallAdmission<F>,
) -> oneshot::Receiver<McpJsonRpcResponse>
where
    F: Future<Output = McpJsonRpcResponse> + Send + 'static,
{
    let McpDistributedToolCallAdmission { registry, redis, key, dispatch } = admission;
    let (sender, receiver) = oneshot::channel();
    let registration = match registry.try_register(key.clone()) {
        Ok(registration) => registration,
        Err(error) => {
            let _ = sender.send(tool_call_admission_error_response(dispatch.response_id, error));
            return receiver;
        }
    };
    let deadline_at = tokio::time::Instant::now() + dispatch.deadline;
    let lease = McpDistributedToolCallLease::new(redis, &key, registration.generation);
    let admission = tokio::time::timeout(
        MCP_TOOL_CALL_COORDINATION_TIMEOUT.min(dispatch.deadline),
        lease.try_acquire(dispatch.deadline),
    )
    .await;
    match mcp_distributed_admission_resolution(&admission) {
        McpDistributedAdmissionResolution::Owned => {
            McpRegisteredToolCall::from_dispatch(
                registration,
                Some(lease),
                dispatch,
                deadline_at,
                sender,
            )
            .spawn();
        }
        McpDistributedAdmissionResolution::Duplicate => {
            drop(registration);
            let _ = sender.send(tool_call_admission_error_response(
                dispatch.response_id,
                McpToolCallAdmissionError::DuplicateRequestId,
            ));
        }
        McpDistributedAdmissionResolution::CancelledBeforeOwnership => {
            drop(registration);
            drop(sender);
        }
        McpDistributedAdmissionResolution::CoordinationUnavailable => {
            release_uncertain_mcp_tool_call_admission(&lease).await;
            drop(registration);
            log_mcp_distributed_admission_unavailable(&admission);
            let _ = sender.send(tool_call_coordination_error_response(dispatch.response_id));
        }
    }
    receiver
}

pub(super) async fn handle_owned_mcp_tool_call(
    auth: AuthContext,
    state: AppState,
    request_id: String,
    id: Option<Value>,
    params: Option<Value>,
    surface: McpToolSurface,
) -> McpJsonRpcResponse {
    tools::handle_tools_call(&auth, &state, &request_id, id, params, surface).await
}

async fn release_uncertain_mcp_tool_call_admission(lease: &McpDistributedToolCallLease) {
    match tokio::time::timeout(MCP_TOOL_CALL_COORDINATION_TIMEOUT, lease.release()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => log_uncertain_mcp_tool_call_admission_cleanup_failed(&error),
        Err(_) => log_uncertain_mcp_tool_call_admission_cleanup_timed_out(),
    }
}

/// The `tracing` call lives in its own function so the warning's expanded
/// formatting machinery does not sit inside the branching body of
/// [`release_uncertain_mcp_tool_call_admission`].
fn log_uncertain_mcp_tool_call_admission_cleanup_failed(error: &redis::RedisError) {
    tracing::warn!(
        %error,
        "uncertain MCP distributed admission cleanup failed; TTL recovery remains active"
    );
}

/// See [`log_uncertain_mcp_tool_call_admission_cleanup_failed`].
fn log_uncertain_mcp_tool_call_admission_cleanup_timed_out() {
    tracing::warn!(
        "uncertain MCP distributed admission cleanup timed out; TTL recovery remains active"
    );
}

/// Logs the unusable-coordination branch of
/// [`mcp_distributed_admission_resolution`], keeping its distinct `tracing`
/// calls out of the admission match body.
fn log_mcp_distributed_admission_unavailable(admission: &McpDistributedAdmissionAttempt) {
    match admission {
        Ok(Ok(McpDistributedAdmissionOutcome::InvalidResponse)) => {
            log_mcp_distributed_admission_invalid_response();
        }
        Ok(Err(error)) => log_mcp_distributed_admission_failed(error),
        Err(_) => log_mcp_distributed_admission_timed_out(),
        Ok(Ok(_)) => {}
    }
}

/// See [`log_mcp_distributed_admission_unavailable`].
fn log_mcp_distributed_admission_invalid_response() {
    tracing::error!("invalid Redis response for MCP distributed tool-call admission");
}

/// See [`log_mcp_distributed_admission_unavailable`].
fn log_mcp_distributed_admission_failed(error: &redis::RedisError) {
    tracing::warn!(%error, "MCP distributed tool-call admission failed");
}

/// See [`log_mcp_distributed_admission_unavailable`].
fn log_mcp_distributed_admission_timed_out() {
    tracing::warn!("MCP distributed tool-call admission timed out");
}

/// Waits for a distributed cancellation of this call, or pends forever when
/// the call has no distributed lease (in-process only).
async fn wait_for_distributed_cancel(
    distributed_lease: Option<&McpDistributedToolCallLease>,
) -> McpDistributedCancelWaitOutcome {
    match distributed_lease {
        Some(lease) => lease.wait_for_cancel().await,
        None => std::future::pending::<McpDistributedCancelWaitOutcome>().await,
    }
}

/// Maps a distributed-cancel outcome to the response that ends the call, if
/// any. `Cancelled` ends it with no response (the cancellation notification or
/// session teardown already answered).
fn distributed_cancel_terminal_response(
    outcome: McpDistributedCancelWaitOutcome,
    response_id: Value,
) -> Option<McpJsonRpcResponse> {
    match outcome {
        McpDistributedCancelWaitOutcome::Cancelled => None,
        McpDistributedCancelWaitOutcome::CoordinationUnavailable => {
            Some(tool_call_coordination_error_response(response_id))
        }
    }
}

impl<F> McpRegisteredToolCall<F>
where
    F: Future<Output = McpJsonRpcResponse> + Send + 'static,
{
    /// Detaches the call from the caller's response body. A non-mutating
    /// dispatch is bounded inline; a mutating one is spawned detached so the
    /// deadline only cancels the wait, never the durable write, and its
    /// admission is held until that write reaches a terminal state
    /// (IRG-203 SITE-5).
    fn spawn(self) {
        let Self {
            registration,
            distributed_lease,
            response_id,
            response_future,
            deadline_at,
            sender,
            mutating,
        } = self;
        let cancellation = registration.cancellation.clone();
        tokio::spawn(async move {
            let guarded_future = AssertUnwindSafe(response_future).catch_unwind();
            let panic_response_id = response_id.clone();
            let deadline_response_id = response_id.clone();
            let coordination_response_id = response_id;
            let (terminal_response, detached_dispatch) = {
                let distributed_cancel = wait_for_distributed_cancel(distributed_lease.as_ref());
                tokio::pin!(distributed_cancel);
                if mutating {
                    // IRG-203 SITE-5: a mutating dispatch writes durable rows and
                    // must not be cancelled by the bounded `tools/call` deadline.
                    // Spawn it detached and keep its join handle in this outer
                    // scope, so the deadline only cancels the *wait*, never the
                    // task.
                    let mut dispatch = tokio::spawn(guarded_future);
                    let terminal_response = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => None,
                        outcome = &mut distributed_cancel => {
                            distributed_cancel_terminal_response(outcome, coordination_response_id)
                        }
                        joined = &mut dispatch => {
                            match joined {
                                Ok(Ok(response)) => Some(response),
                                Ok(Err(_)) => {
                                    tracing::error!("MCP mutating tool call panicked");
                                    Some(tool_call_panic_error_response(panic_response_id))
                                }
                                Err(join_error) => {
                                    tracing::error!(
                                        error = %join_error,
                                        "MCP mutating tool call task join failed"
                                    );
                                    Some(tool_call_panic_error_response(panic_response_id))
                                }
                            }
                        }
                        _ = tokio::time::sleep_until(deadline_at) => {
                            Some(tool_call_deadline_error_response(deadline_response_id))
                        }
                    };
                    (terminal_response, Some(dispatch))
                } else {
                    let terminal_response = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => None,
                        outcome = &mut distributed_cancel => {
                            distributed_cancel_terminal_response(outcome, coordination_response_id)
                        }
                        result = tokio::time::timeout_at(deadline_at, guarded_future) => {
                            match result {
                                Ok(Ok(response)) => Some(response),
                                Ok(Err(_)) => {
                                    tracing::error!("MCP tool call panicked");
                                    Some(tool_call_panic_error_response(panic_response_id))
                                }
                                Err(_) => Some(tool_call_deadline_error_response(deadline_response_id)),
                            }
                        }
                    };
                    (terminal_response, None)
                }
            };
            if let Some(response) = terminal_response {
                let _ = sender.send(response);
            }
            if let Some(dispatch) = detached_dispatch
                && !dispatch.is_finished()
            {
                // IRG-203 SITE-5 residual-free path: the terminal response has
                // been sent, but the detached mutation still owns this
                // request id. Hold the distributed lease and the in-process
                // registration until it reaches a terminal state, so a
                // same-request-id retry after `tool_call_deadline_exceeded`
                // cannot be admitted concurrently and apply the mutation
                // twice. The lease's Redis TTL remains the outer bound if the
                // mutation itself never returns.
                let _ = dispatch.await;
            }
            if let Some(lease) = distributed_lease {
                match tokio::time::timeout(MCP_TOOL_CALL_COORDINATION_TIMEOUT, lease.release())
                    .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "MCP distributed tool-call owner release failed");
                    }
                    Err(_) => {
                        tracing::warn!("MCP distributed tool-call owner release timed out");
                    }
                }
            }
            drop(registration);
        });
    }
}
