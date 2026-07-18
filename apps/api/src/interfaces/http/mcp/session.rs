use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::http::HeaderMap;
use redis::RedisError;
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::interfaces::http::auth::AuthContext;

use super::lease::update_mcp_coordination_hash_frame;
use super::{MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER};

pub(super) const MCP_SESSION_TTL: Duration = Duration::from_mins(30);
pub(super) const MCP_SESSION_TERMINATION_TTL: Duration = Duration::from_mins(5);
pub(super) const MCP_SESSION_COORDINATION_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const MCP_GET_STREAM_MAX_LIFETIME: Duration = Duration::from_mins(10);
// Active-session memory is capped per API process and per principal. The
// deployment-wide bound is the configured replica maximum; Redis ownership
// remains TTL-bounded rather than using cross-slot global counters.
pub(super) const MCP_SESSION_PROCESS_LIMIT: usize = 1_024;
pub(super) const MCP_SESSION_PER_PRINCIPAL_PROCESS_LIMIT: usize = 32;
pub(super) const MCP_GET_STREAM_PROCESS_LIMIT: usize = 64;
pub(super) const MCP_GET_STREAM_PER_PRINCIPAL_PROCESS_LIMIT: usize = 4;

const MCP_SESSION_WIRE_MAX_BYTES: usize = 64;
const MCP_SESSION_COORDINATION_KEY_PREFIX: &str = "ironrag:mcp:session:v1";

pub(super) const MCP_SESSION_REGISTER_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[2]) == 1 then
    return 0
end
local registered = redis.call('SET', KEYS[1], ARGV[1], 'NX', 'PX', ARGV[2])
if registered then
    return 1
end
return 0
";

pub(super) const MCP_SESSION_VALIDATE_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[2]) == 1 then
    return -1
end
if redis.call('GET', KEYS[1]) == ARGV[1] then
    redis.call('PEXPIRE', KEYS[1], ARGV[2])
    return 1
end
return 0
";

pub(super) const MCP_SESSION_TERMINATE_SCRIPT: &str = r"
local owner = redis.call('GET', KEYS[1])
local terminated_owner = redis.call('GET', KEYS[2])
if owner == ARGV[1] or terminated_owner == ARGV[1] then
    redis.call('SET', KEYS[2], ARGV[1], 'PX', ARGV[2])
    redis.call('DEL', KEYS[1])
    return 1
end
return 0
";

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub(super) struct McpSessionId([u8; 32]);

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub(super) struct McpSessionOwner {
    principal_id: Uuid,
    token_id: Uuid,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct McpSessionRedisKeys {
    pub(super) owner: String,
    pub(super) terminated: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpSessionHeaderError {
    MissingOrInvalid,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpProtocolHeaderError {
    MissingOrInvalid,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpSessionRegistrationOutcome {
    Registered,
    Collision,
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpSessionValidationOutcome {
    Active,
    MissingOrForeign,
    Terminated,
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpSessionTerminationOutcome {
    Terminated,
    MissingOrForeign,
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpStreamAdmissionError {
    SessionMissingOrForeign,
    SessionProcessCapacity,
    SessionPrincipalProcessCapacity,
    ProcessCapacity,
    PrincipalProcessCapacity,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum McpLocalSessionAdmissionOutcome {
    Accepted,
    MissingOrForeign,
    ProcessCapacity,
    PrincipalProcessCapacity,
}

impl McpSessionId {
    fn from_wire(wire: &str) -> Option<Self> {
        if wire.is_empty() || wire.len() > MCP_SESSION_WIRE_MAX_BYTES || wire.trim() != wire {
            return None;
        }
        let parsed = Uuid::parse_str(wire).ok()?;
        if parsed.is_nil() || parsed.as_hyphenated().to_string() != wire {
            return None;
        }
        Some(Self(Sha256::digest(wire.as_bytes()).into()))
    }

    pub(super) const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub(super) const fn digest(self) -> [u8; 32] {
        self.0
    }

    fn redis_hash_tag(self) -> String {
        hex::encode(self.0)
    }
}

impl McpSessionOwner {
    pub(super) const fn from_auth(auth: &AuthContext) -> Self {
        Self { principal_id: auth.principal_id, token_id: auth.token_id }
    }

    pub(super) const fn principal_id(self) -> Uuid {
        self.principal_id
    }

    pub(super) const fn token_id(self) -> Uuid {
        self.token_id
    }

    fn redis_value(self) -> String {
        let mut hasher = Sha256::new();
        update_mcp_coordination_hash_frame(&mut hasher, b"ironrag-mcp-session-owner-v1");
        update_mcp_coordination_hash_frame(&mut hasher, self.principal_id.as_bytes());
        update_mcp_coordination_hash_frame(&mut hasher, self.token_id.as_bytes());
        hex::encode(hasher.finalize())
    }
}

pub(super) fn issue_session_id() -> (String, McpSessionId) {
    let wire = Uuid::now_v7().as_hyphenated().to_string();
    let session_id = McpSessionId(Sha256::digest(wire.as_bytes()).into());
    (wire, session_id)
}

pub(super) fn optional_session_id(
    headers: &HeaderMap,
) -> Result<Option<McpSessionId>, McpSessionHeaderError> {
    let mut values = headers.get_all(MCP_SESSION_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(McpSessionHeaderError::MissingOrInvalid);
    }
    let wire = value.to_str().map_err(|_| McpSessionHeaderError::MissingOrInvalid)?;
    McpSessionId::from_wire(wire).map(Some).ok_or(McpSessionHeaderError::MissingOrInvalid)
}

pub(super) fn required_session_id(
    headers: &HeaderMap,
) -> Result<McpSessionId, McpSessionHeaderError> {
    optional_session_id(headers)?.ok_or(McpSessionHeaderError::MissingOrInvalid)
}

pub(super) fn required_protocol_version(headers: &HeaderMap) -> Result<(), McpProtocolHeaderError> {
    let mut values = headers.get_all(MCP_PROTOCOL_HEADER).iter();
    let value = values.next().ok_or(McpProtocolHeaderError::MissingOrInvalid)?;
    if values.next().is_some() || value.as_bytes() != MCP_PROTOCOL_VERSION.as_bytes() {
        return Err(McpProtocolHeaderError::MissingOrInvalid);
    }
    Ok(())
}

pub(super) fn session_redis_keys(session_id: McpSessionId) -> McpSessionRedisKeys {
    let hash_tag = session_id.redis_hash_tag();
    let base = format!("{MCP_SESSION_COORDINATION_KEY_PREFIX}:{{{hash_tag}}}");
    McpSessionRedisKeys { owner: format!("{base}:owner"), terminated: format!("{base}:terminated") }
}

fn duration_millis(ttl: Duration) -> u128 {
    ttl.as_millis().max(1)
}

pub(super) async fn register_session(
    redis: &redis::Client,
    owner: McpSessionOwner,
    session_id: McpSessionId,
    ttl: Duration,
) -> Result<McpSessionRegistrationOutcome, RedisError> {
    let keys = session_redis_keys(session_id);
    let mut connection = redis.get_multiplexed_async_connection().await?;
    let code: i64 = redis::cmd("EVAL")
        .arg(MCP_SESSION_REGISTER_SCRIPT)
        .arg(2)
        .arg(keys.owner)
        .arg(keys.terminated)
        .arg(owner.redis_value())
        .arg(duration_millis(ttl).to_string())
        .query_async(&mut connection)
        .await?;
    Ok(match code {
        1 => McpSessionRegistrationOutcome::Registered,
        0 => McpSessionRegistrationOutcome::Collision,
        _ => McpSessionRegistrationOutcome::InvalidResponse,
    })
}

pub(super) async fn validate_session(
    redis: &redis::Client,
    owner: McpSessionOwner,
    session_id: McpSessionId,
    ttl: Duration,
) -> Result<McpSessionValidationOutcome, RedisError> {
    let keys = session_redis_keys(session_id);
    let mut connection = redis.get_multiplexed_async_connection().await?;
    let code: i64 = redis::cmd("EVAL")
        .arg(MCP_SESSION_VALIDATE_SCRIPT)
        .arg(2)
        .arg(keys.owner)
        .arg(keys.terminated)
        .arg(owner.redis_value())
        .arg(duration_millis(ttl).to_string())
        .query_async(&mut connection)
        .await?;
    Ok(match code {
        1 => McpSessionValidationOutcome::Active,
        0 => McpSessionValidationOutcome::MissingOrForeign,
        -1 => McpSessionValidationOutcome::Terminated,
        _ => McpSessionValidationOutcome::InvalidResponse,
    })
}

pub(super) async fn terminate_session(
    redis: &redis::Client,
    owner: McpSessionOwner,
    session_id: McpSessionId,
    termination_ttl: Duration,
) -> Result<McpSessionTerminationOutcome, RedisError> {
    let keys = session_redis_keys(session_id);
    let mut connection = redis.get_multiplexed_async_connection().await?;
    let code: i64 = redis::cmd("EVAL")
        .arg(MCP_SESSION_TERMINATE_SCRIPT)
        .arg(2)
        .arg(keys.owner)
        .arg(keys.terminated)
        .arg(owner.redis_value())
        .arg(duration_millis(termination_ttl).to_string())
        .query_async(&mut connection)
        .await?;
    Ok(match code {
        1 => McpSessionTerminationOutcome::Terminated,
        0 => McpSessionTerminationOutcome::MissingOrForeign,
        _ => McpSessionTerminationOutcome::InvalidResponse,
    })
}

struct McpLocalSessionEntry {
    owner: McpSessionOwner,
    expires_at: Instant,
    cancellation: CancellationToken,
}

#[derive(Default)]
struct McpLocalSessionState {
    sessions: HashMap<McpSessionId, McpLocalSessionEntry>,
    principal_sessions: HashMap<Uuid, usize>,
    active_streams: usize,
    principal_streams: HashMap<Uuid, usize>,
}

pub(super) struct McpLocalSessionRegistry {
    state: Mutex<McpLocalSessionState>,
    process_session_limit: usize,
    per_principal_session_limit: usize,
    process_stream_limit: usize,
    per_principal_stream_limit: usize,
}

impl McpLocalSessionRegistry {
    pub(super) fn new(
        process_session_limit: usize,
        per_principal_session_limit: usize,
        process_stream_limit: usize,
        per_principal_stream_limit: usize,
    ) -> Self {
        Self {
            state: Mutex::new(McpLocalSessionState::default()),
            process_session_limit: process_session_limit.max(1),
            per_principal_session_limit: per_principal_session_limit.max(1),
            process_stream_limit: process_stream_limit.max(1),
            per_principal_stream_limit: per_principal_stream_limit.max(1),
        }
    }

    pub(super) fn accept_validated(
        &self,
        owner: McpSessionOwner,
        session_id: McpSessionId,
    ) -> McpLocalSessionAdmissionOutcome {
        let now = Instant::now();
        let mut state = self.lock_state();
        Self::remove_expired(&mut state, now);
        self.accept_validated_locked(&mut state, owner, session_id, now)
    }

    fn accept_validated_locked(
        &self,
        state: &mut McpLocalSessionState,
        owner: McpSessionOwner,
        session_id: McpSessionId,
        now: Instant,
    ) -> McpLocalSessionAdmissionOutcome {
        match state.sessions.get_mut(&session_id) {
            Some(entry) if entry.owner == owner => {
                entry.expires_at = now + MCP_SESSION_TTL;
                McpLocalSessionAdmissionOutcome::Accepted
            }
            Some(_) => McpLocalSessionAdmissionOutcome::MissingOrForeign,
            None => {
                if state.sessions.len() >= self.process_session_limit {
                    return McpLocalSessionAdmissionOutcome::ProcessCapacity;
                }
                let principal_sessions =
                    state.principal_sessions.get(&owner.principal_id).copied().unwrap_or(0);
                if principal_sessions >= self.per_principal_session_limit {
                    return McpLocalSessionAdmissionOutcome::PrincipalProcessCapacity;
                }
                state.sessions.insert(
                    session_id,
                    McpLocalSessionEntry {
                        owner,
                        expires_at: now + MCP_SESSION_TTL,
                        cancellation: CancellationToken::new(),
                    },
                );
                state.principal_sessions.insert(owner.principal_id, principal_sessions + 1);
                McpLocalSessionAdmissionOutcome::Accepted
            }
        }
    }

    pub(super) fn try_open_validated_stream(
        self: &Arc<Self>,
        owner: McpSessionOwner,
        session_id: McpSessionId,
    ) -> Result<(McpSessionStreamGuard, CancellationToken), McpStreamAdmissionError> {
        let now = Instant::now();
        let mut state = self.lock_state();
        Self::remove_expired(&mut state, now);

        match self.accept_validated_locked(&mut state, owner, session_id, now) {
            McpLocalSessionAdmissionOutcome::Accepted => {}
            McpLocalSessionAdmissionOutcome::MissingOrForeign => {
                return Err(McpStreamAdmissionError::SessionMissingOrForeign);
            }
            McpLocalSessionAdmissionOutcome::ProcessCapacity => {
                return Err(McpStreamAdmissionError::SessionProcessCapacity);
            }
            McpLocalSessionAdmissionOutcome::PrincipalProcessCapacity => {
                return Err(McpStreamAdmissionError::SessionPrincipalProcessCapacity);
            }
        }
        let cancellation = state
            .sessions
            .get(&session_id)
            .map(|entry| entry.cancellation.clone())
            .ok_or(McpStreamAdmissionError::SessionMissingOrForeign)?;

        if state.active_streams >= self.process_stream_limit {
            return Err(McpStreamAdmissionError::ProcessCapacity);
        }
        let principal_streams =
            state.principal_streams.get(&owner.principal_id).copied().unwrap_or(0);
        if principal_streams >= self.per_principal_stream_limit {
            return Err(McpStreamAdmissionError::PrincipalProcessCapacity);
        }
        state.active_streams += 1;
        state.principal_streams.insert(owner.principal_id, principal_streams + 1);
        drop(state);

        Ok((
            McpSessionStreamGuard {
                registry: Arc::clone(self),
                principal_id: owner.principal_id,
                released: false,
            },
            cancellation,
        ))
    }

    pub(super) fn terminate_owned(&self, owner: McpSessionOwner, session_id: McpSessionId) -> bool {
        let cancellation = {
            let mut state = self.lock_state();
            let entry = state
                .sessions
                .get(&session_id)
                .is_some_and(|entry| entry.owner == owner)
                .then(|| state.sessions.remove(&session_id))
                .flatten();
            if let Some(entry) = entry {
                Self::release_session_count(&mut state, entry.owner.principal_id);
                let cancellation = Some(entry.cancellation);
                drop(state);
                cancellation
            } else {
                drop(state);
                None
            }
        };
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
            true
        } else {
            false
        }
    }

    fn release_stream(&self, principal_id: Uuid) {
        let mut state = self.lock_state();
        state.active_streams = state.active_streams.saturating_sub(1);
        if let Some(principal_streams) = state.principal_streams.get_mut(&principal_id) {
            *principal_streams = principal_streams.saturating_sub(1);
            if *principal_streams == 0 {
                state.principal_streams.remove(&principal_id);
            }
        }
    }

    fn remove_expired(state: &mut McpLocalSessionState, now: Instant) {
        let expired = state
            .sessions
            .iter()
            .filter_map(|(session_id, entry)| (entry.expires_at <= now).then_some(*session_id))
            .collect::<Vec<_>>();
        for session_id in expired {
            if let Some(entry) = state.sessions.remove(&session_id) {
                Self::release_session_count(state, entry.owner.principal_id);
                entry.cancellation.cancel();
            }
        }
    }

    fn release_session_count(state: &mut McpLocalSessionState, principal_id: Uuid) {
        if let Some(principal_sessions) = state.principal_sessions.get_mut(&principal_id) {
            *principal_sessions = principal_sessions.saturating_sub(1);
            if *principal_sessions == 0 {
                state.principal_sessions.remove(&principal_id);
            }
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, McpLocalSessionState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    fn active_streams(&self) -> usize {
        self.lock_state().active_streams
    }

    #[cfg(test)]
    fn active_sessions(&self) -> usize {
        self.lock_state().sessions.len()
    }

    #[cfg(test)]
    fn expire_session(&self, session_id: McpSessionId) {
        if let Some(entry) = self.lock_state().sessions.get_mut(&session_id) {
            entry.expires_at = Instant::now();
        }
    }
}

pub(super) struct McpSessionStreamGuard {
    registry: Arc<McpLocalSessionRegistry>,
    principal_id: Uuid,
    released: bool,
}

impl Drop for McpSessionStreamGuard {
    fn drop(&mut self) {
        if !self.released {
            self.registry.release_stream(self.principal_id);
            self.released = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::{HeaderMap, HeaderValue};
    use uuid::Uuid;

    use super::{
        MCP_GET_STREAM_MAX_LIFETIME, MCP_GET_STREAM_PER_PRINCIPAL_PROCESS_LIMIT,
        MCP_GET_STREAM_PROCESS_LIMIT, MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION,
        MCP_SESSION_HEADER, MCP_SESSION_PER_PRINCIPAL_PROCESS_LIMIT, MCP_SESSION_PROCESS_LIMIT,
        MCP_SESSION_REGISTER_SCRIPT, MCP_SESSION_TERMINATE_SCRIPT, MCP_SESSION_TERMINATION_TTL,
        MCP_SESSION_TTL, MCP_SESSION_VALIDATE_SCRIPT, McpLocalSessionAdmissionOutcome,
        McpLocalSessionRegistry, McpProtocolHeaderError, McpSessionHeaderError, McpSessionId,
        McpSessionOwner, McpStreamAdmissionError, issue_session_id, optional_session_id,
        required_protocol_version, required_session_id, session_redis_keys,
    };

    fn owner(principal: u128, token: u128) -> McpSessionOwner {
        McpSessionOwner {
            principal_id: Uuid::from_u128(principal),
            token_id: Uuid::from_u128(token),
        }
    }

    #[test]
    fn session_header_accepts_only_one_nonempty_server_shaped_identifier() {
        let wire = Uuid::now_v7().as_hyphenated().to_string();
        let mut headers = HeaderMap::new();
        headers.insert(MCP_SESSION_HEADER, HeaderValue::from_str(&wire).expect("session header"));

        let parsed = required_session_id(&headers).expect("issued-shaped session id");
        assert_ne!(parsed.digest(), [0; 32]);
        assert_eq!(optional_session_id(&headers).expect("optional session"), Some(parsed));

        let missing = HeaderMap::new();
        assert_eq!(optional_session_id(&missing), Ok(None));
        assert_eq!(required_session_id(&missing), Err(McpSessionHeaderError::MissingOrInvalid));

        for invalid in
            ["", "   ", "arbitrary-client-session", "00000000-0000-0000-0000-000000000000-extra"]
        {
            let mut invalid_headers = HeaderMap::new();
            invalid_headers.insert(
                MCP_SESSION_HEADER,
                HeaderValue::from_str(invalid).expect("ASCII invalid session header"),
            );
            assert_eq!(
                required_session_id(&invalid_headers),
                Err(McpSessionHeaderError::MissingOrInvalid),
                "invalid wire value must fail closed: {invalid:?}"
            );
        }

        let mut duplicate = HeaderMap::new();
        duplicate.append(MCP_SESSION_HEADER, HeaderValue::from_str(&wire).expect("first header"));
        duplicate.append(MCP_SESSION_HEADER, HeaderValue::from_str(&wire).expect("second header"));
        assert_eq!(required_session_id(&duplicate), Err(McpSessionHeaderError::MissingOrInvalid));
    }

    #[test]
    fn protocol_header_requires_one_exact_supported_version() {
        let mut headers = HeaderMap::new();
        headers.insert(MCP_PROTOCOL_HEADER, HeaderValue::from_static(MCP_PROTOCOL_VERSION));
        assert_eq!(required_protocol_version(&headers), Ok(()));

        assert_eq!(
            required_protocol_version(&HeaderMap::new()),
            Err(McpProtocolHeaderError::MissingOrInvalid)
        );
        for invalid in ["2025-06-18", " 2025-11-25", "2025-11-25 "] {
            let mut invalid_headers = HeaderMap::new();
            invalid_headers.insert(
                MCP_PROTOCOL_HEADER,
                HeaderValue::from_str(invalid).expect("ASCII invalid protocol version"),
            );
            assert_eq!(
                required_protocol_version(&invalid_headers),
                Err(McpProtocolHeaderError::MissingOrInvalid)
            );
        }

        let mut duplicate = HeaderMap::new();
        duplicate.append(MCP_PROTOCOL_HEADER, HeaderValue::from_static(MCP_PROTOCOL_VERSION));
        duplicate.append(MCP_PROTOCOL_HEADER, HeaderValue::from_static(MCP_PROTOCOL_VERSION));
        assert_eq!(
            required_protocol_version(&duplicate),
            Err(McpProtocolHeaderError::MissingOrInvalid)
        );
    }

    #[test]
    fn issued_session_keys_and_owners_never_contain_raw_identity_values() {
        let (wire, session_id) = issue_session_id();
        let owner = owner(101, 202);
        let keys = session_redis_keys(session_id);
        let combined = format!("{} {} {}", keys.owner, keys.terminated, owner.redis_value());

        assert_ne!(keys.owner, keys.terminated);
        assert!(keys.owner.starts_with("ironrag:mcp:session:v1:"));
        assert!(!combined.contains(&wire));
        assert!(!combined.contains(&owner.principal_id.to_string()));
        assert!(!combined.contains(&owner.token_id.to_string()));
    }

    #[test]
    fn distributed_session_scripts_are_owned_ttl_bounded_and_scan_free() {
        assert_eq!(MCP_GET_STREAM_PROCESS_LIMIT, 64);
        assert_eq!(MCP_GET_STREAM_PER_PRINCIPAL_PROCESS_LIMIT, 4);
        assert_eq!(MCP_SESSION_PROCESS_LIMIT, 1_024);
        assert_eq!(MCP_SESSION_PER_PRINCIPAL_PROCESS_LIMIT, 32);
        assert!(MCP_GET_STREAM_MAX_LIFETIME <= MCP_SESSION_TTL);
        assert!(MCP_SESSION_TERMINATION_TTL >= super::super::MCP_TOOL_CALL_DEADLINE);
        assert!(MCP_SESSION_REGISTER_SCRIPT.contains("NX"));
        assert!(MCP_SESSION_REGISTER_SCRIPT.contains("PX"));
        assert!(MCP_SESSION_VALIDATE_SCRIPT.contains("PEXPIRE"));
        assert!(MCP_SESSION_VALIDATE_SCRIPT.contains("EXISTS"));
        assert!(MCP_SESSION_TERMINATE_SCRIPT.contains("terminated_owner == ARGV[1]"));
        assert!(MCP_SESSION_TERMINATE_SCRIPT.contains("DEL"));
        for script in
            [MCP_SESSION_REGISTER_SCRIPT, MCP_SESSION_VALIDATE_SCRIPT, MCP_SESSION_TERMINATE_SCRIPT]
        {
            assert!(!script.contains("KEYS "));
            assert!(!script.contains("SCAN"));
        }
    }

    #[test]
    fn stream_registry_enforces_caps_and_raii_releases_capacity() {
        let registry = Arc::new(McpLocalSessionRegistry::new(8, 4, 2, 1));
        let owner_a = owner(1, 11);
        let owner_b = owner(2, 22);
        let (_, session_a) = issue_session_id();
        let (_, session_b) = issue_session_id();
        let (_, session_c) = issue_session_id();

        let (guard_a, _) =
            registry.try_open_validated_stream(owner_a, session_a).expect("first principal stream");
        assert_eq!(registry.active_streams(), 1);
        assert!(matches!(
            registry.try_open_validated_stream(owner_a, session_a),
            Err(McpStreamAdmissionError::PrincipalProcessCapacity)
        ));
        let (guard_b, _) = registry
            .try_open_validated_stream(owner_b, session_b)
            .expect("second principal stream");
        assert_eq!(registry.active_streams(), 2);
        assert!(matches!(
            registry.try_open_validated_stream(owner(3, 33), session_c),
            Err(McpStreamAdmissionError::ProcessCapacity)
        ));

        drop(guard_a);
        assert_eq!(registry.active_streams(), 1);
        let (replacement, _) = registry
            .try_open_validated_stream(owner_a, session_a)
            .expect("RAII drop releases principal and process capacity");
        drop(replacement);
        drop(guard_b);
        assert_eq!(registry.active_streams(), 0);
    }

    #[tokio::test]
    async fn owned_session_termination_cancels_streams_and_rejects_foreign_owner() {
        let registry = Arc::new(McpLocalSessionRegistry::new(8, 4, 2, 2));
        let session_owner = owner(9, 10);
        let foreign = owner(9, 99);
        let (_, session_id) = issue_session_id();
        let (_guard, cancellation) =
            registry.try_open_validated_stream(session_owner, session_id).expect("owned stream");

        assert_eq!(
            registry.accept_validated(foreign, session_id),
            McpLocalSessionAdmissionOutcome::MissingOrForeign
        );
        assert!(!registry.terminate_owned(foreign, session_id));
        assert!(!cancellation.is_cancelled());
        assert!(registry.terminate_owned(session_owner, session_id));
        cancellation.cancelled().await;
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn repeated_session_admission_enforces_process_and_principal_caps_and_reaps_expiry() {
        let registry = McpLocalSessionRegistry::new(2, 1, 4, 4);
        let owner_a = owner(41, 51);
        let owner_b = owner(42, 52);
        let owner_c = owner(43, 53);
        let (_, session_a) = issue_session_id();
        let (_, session_b) = issue_session_id();
        let (_, session_c) = issue_session_id();

        assert_eq!(
            registry.accept_validated(owner_a, session_a),
            McpLocalSessionAdmissionOutcome::Accepted
        );
        assert_eq!(
            registry.accept_validated(owner_a, session_a),
            McpLocalSessionAdmissionOutcome::Accepted,
            "re-validating the same session must refresh rather than consume another slot"
        );
        assert_eq!(
            registry.accept_validated(owner_a, session_b),
            McpLocalSessionAdmissionOutcome::PrincipalProcessCapacity
        );
        assert_eq!(
            registry.accept_validated(owner_b, session_b),
            McpLocalSessionAdmissionOutcome::Accepted
        );
        assert_eq!(registry.active_sessions(), 2);
        assert_eq!(
            registry.accept_validated(owner_c, session_c),
            McpLocalSessionAdmissionOutcome::ProcessCapacity
        );

        registry.expire_session(session_a);
        assert_eq!(
            registry.accept_validated(owner_c, session_c),
            McpLocalSessionAdmissionOutcome::Accepted,
            "expired entries must be reaped before applying session caps"
        );
        assert_eq!(registry.active_sessions(), 2);
        assert!(registry.terminate_owned(owner_b, session_b));
        assert_eq!(registry.active_sessions(), 1);
    }

    #[test]
    fn digest_constructor_preserves_session_scope_without_raw_header() {
        let digest = [7_u8; 32];
        assert_eq!(McpSessionId::from_digest(digest).digest(), digest);
    }
}
