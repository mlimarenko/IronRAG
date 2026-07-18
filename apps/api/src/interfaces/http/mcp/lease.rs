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
        let connection = tokio::time::timeout(
            MCP_TOOL_CALL_COORDINATION_TIMEOUT,
            self.redis.get_multiplexed_async_connection(),
        )
        .await;
        let mut connection = match connection {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                tracing::warn!(%error, "MCP distributed cancellation connection failed");
                return McpDistributedCancelWaitOutcome::CoordinationUnavailable;
            }
            Err(_) => {
                tracing::warn!("MCP distributed cancellation connection timed out");
                return McpDistributedCancelWaitOutcome::CoordinationUnavailable;
            }
        };

        loop {
            let markers = tokio::time::timeout(
                MCP_TOOL_CALL_COORDINATION_TIMEOUT,
                redis::cmd("MGET")
                    .arg(&self.keys.cancel)
                    .arg(&self.keys.session_terminated)
                    .query_async::<(Option<String>, Option<String>)>(&mut connection),
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
                    return McpDistributedCancelWaitOutcome::Cancelled;
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "MCP distributed cancellation poll failed");
                    return McpDistributedCancelWaitOutcome::CoordinationUnavailable;
                }
                Err(_) => {
                    tracing::warn!("MCP distributed cancellation poll timed out");
                    return McpDistributedCancelWaitOutcome::CoordinationUnavailable;
                }
            }
            tokio::time::sleep(MCP_TOOL_CALL_CANCEL_POLL_INTERVAL).await;
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
    let (sender, receiver) = oneshot::channel();
    let registration = match registry.try_register(key) {
        Ok(registration) => registration,
        Err(error) => {
            let _ = sender.send(tool_call_admission_error_response(response_id, error));
            return receiver;
        }
    };
    let deadline_at = tokio::time::Instant::now() + deadline;
    spawn_registered_mcp_tool_call(
        registration,
        None,
        response_id,
        response_future,
        deadline_at,
        sender,
    );
    receiver
}

pub(super) async fn start_distributed_bounded_mcp_tool_call<F>(
    registry: Arc<McpInFlightToolCallRegistry>,
    redis: redis::Client,
    key: McpInFlightRequestKey,
    response_id: Value,
    response_future: F,
    deadline: Duration,
) -> oneshot::Receiver<McpJsonRpcResponse>
where
    F: Future<Output = McpJsonRpcResponse> + Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    let registration = match registry.try_register(key.clone()) {
        Ok(registration) => registration,
        Err(error) => {
            let _ = sender.send(tool_call_admission_error_response(response_id, error));
            return receiver;
        }
    };
    let deadline_at = tokio::time::Instant::now() + deadline;
    let lease = McpDistributedToolCallLease::new(redis, &key, registration.generation);
    let admission = tokio::time::timeout(
        MCP_TOOL_CALL_COORDINATION_TIMEOUT.min(deadline),
        lease.try_acquire(deadline),
    )
    .await;
    match admission {
        Ok(Ok(McpDistributedAdmissionOutcome::Acquired)) => {
            spawn_registered_mcp_tool_call(
                registration,
                Some(lease),
                response_id,
                response_future,
                deadline_at,
                sender,
            );
        }
        Ok(Ok(McpDistributedAdmissionOutcome::DuplicateRequestId)) => {
            drop(registration);
            let _ = sender.send(tool_call_admission_error_response(
                response_id,
                McpToolCallAdmissionError::DuplicateRequestId,
            ));
        }
        Ok(Ok(McpDistributedAdmissionOutcome::CancelledBeforeAdmission)) => {
            drop(registration);
            drop(sender);
        }
        Ok(Ok(McpDistributedAdmissionOutcome::InvalidResponse)) => {
            release_uncertain_mcp_tool_call_admission(&lease).await;
            drop(registration);
            tracing::error!("invalid Redis response for MCP distributed tool-call admission");
            let _ = sender.send(tool_call_coordination_error_response(response_id));
        }
        Ok(Err(error)) => {
            release_uncertain_mcp_tool_call_admission(&lease).await;
            drop(registration);
            tracing::warn!(%error, "MCP distributed tool-call admission failed");
            let _ = sender.send(tool_call_coordination_error_response(response_id));
        }
        Err(_) => {
            release_uncertain_mcp_tool_call_admission(&lease).await;
            drop(registration);
            tracing::warn!("MCP distributed tool-call admission timed out");
            let _ = sender.send(tool_call_coordination_error_response(response_id));
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
        Ok(Err(error)) => {
            tracing::warn!(
                %error,
                "uncertain MCP distributed admission cleanup failed; TTL recovery remains active"
            );
        }
        Err(_) => {
            tracing::warn!(
                "uncertain MCP distributed admission cleanup timed out; TTL recovery remains active"
            );
        }
    }
}

fn spawn_registered_mcp_tool_call<F>(
    registration: McpInFlightToolCallRegistration,
    distributed_lease: Option<McpDistributedToolCallLease>,
    response_id: Value,
    response_future: F,
    deadline_at: tokio::time::Instant,
    sender: oneshot::Sender<McpJsonRpcResponse>,
) where
    F: Future<Output = McpJsonRpcResponse> + Send + 'static,
{
    let cancellation = registration.cancellation.clone();
    tokio::spawn(async move {
        let guarded_future = AssertUnwindSafe(response_future).catch_unwind();
        let panic_response_id = response_id.clone();
        let deadline_response_id = response_id.clone();
        let coordination_response_id = response_id;
        let terminal_response = {
            let distributed_cancel = async {
                match distributed_lease.as_ref() {
                    Some(lease) => lease.wait_for_cancel().await,
                    None => std::future::pending::<McpDistributedCancelWaitOutcome>().await,
                }
            };
            tokio::pin!(distributed_cancel);
            tokio::select! {
                biased;
                () = cancellation.cancelled() => None,
                outcome = &mut distributed_cancel => {
                    match outcome {
                        McpDistributedCancelWaitOutcome::Cancelled => None,
                        McpDistributedCancelWaitOutcome::CoordinationUnavailable => {
                            Some(tool_call_coordination_error_response(coordination_response_id))
                        }
                    }
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
            }
        };
        if let Some(response) = terminal_response {
            let _ = sender.send(response);
        }
        if let Some(lease) = distributed_lease {
            match tokio::time::timeout(MCP_TOOL_CALL_COORDINATION_TIMEOUT, lease.release()).await {
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
