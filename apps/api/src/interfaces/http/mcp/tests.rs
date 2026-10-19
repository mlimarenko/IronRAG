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
    McpDistributedAdmissionOutcome, McpDistributedAdmissionResolution,
    McpDistributedToolCallAdmission, McpInFlightRequestKey, McpInFlightToolCallRegistry,
    McpJsonRpcResponse, McpSessionScope, McpToolCallDispatch, accept_preference,
    cancel_mcp_in_flight_tool_call, finalize_mcp_response,
    finalize_mcp_tool_call_sse_response_with_interval,
    grounded_answer_must_preserve_spans_for_evidence,
    grounded_answer_must_preserve_spans_for_source_titles, mcp_cancel_marker_matches_generation,
    mcp_distributed_admission_outcome, mcp_distributed_admission_resolution,
    mcp_distributed_cancel_markers_cancel_call, mcp_distributed_coordination_keys,
    mcp_initialize_result, mcp_request_id_key, mcp_scope_digest, mcp_tool_call_redis_ttl_millis,
    ok_tool_result, start_bounded_mcp_tool_call, start_bounded_mcp_tool_call_with_mutation,
    start_distributed_bounded_mcp_tool_call, success_response, valid_initialize_params,
};

struct DropProbe(Arc<AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn ok_tool_result_text_carries_the_payload_for_text_only_clients() {
    let payload = json!({ "workspaces": [{ "ref": "ide", "status": "active" }] });
    let result = ok_tool_result("Visible workspaces loaded.", payload.clone());
    assert!(!result.is_error);
    assert_eq!(result.structured_content, payload);
    assert_eq!(result.content.len(), 1);
    let text = &result.content[0].text;
    assert!(text.contains("Visible workspaces loaded."));
    assert!(text.contains("\"ref\": \"ide\""));
    assert!(text.contains("\"status\": \"active\""));
}

#[test]
fn ok_tool_result_with_null_payload_keeps_summary_text_only() {
    let result = ok_tool_result("Workspace created.", Value::Null);
    assert_eq!(result.content[0].text, "Workspace created.");
    assert_eq!(result.structured_content, Value::Null);
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

    assert_eq!(payload["instructions"], json!(crate::services::mcp::agent_policy::instructions()));
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
    let valid_params = json!({
        "protocolVersion": super::MCP_PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "test-client", "version": "1" },
    });
    assert!(valid_initialize_params(Some(&valid_params)));
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
    assert!(
        !mcp_cancel_marker_matches_generation(MCP_TOOL_CALL_CANCEL_PENDING_MARKER, generation,)
    );
    assert!(!mcp_cancel_marker_matches_generation(&Uuid::from_u128(334).to_string(), generation));
    assert!(mcp_distributed_cancel_markers_cancel_call(None, true, generation));
    assert!(mcp_distributed_cancel_markers_cancel_call(
        Some(&generation.to_string()),
        false,
        generation,
    ));
    assert!(!mcp_distributed_cancel_markers_cancel_call(None, false, generation));
}

/// The distributed admission branch table is pure: every coded outcome maps to
/// the caller's reaction, and every unusable coordination result — a Redis
/// error, an invalid reply, or a timeout — collapses to the cleanup-and-answer
/// path without touching Redis here.
#[test]
fn distributed_admission_resolution_classifies_outcomes_without_redis() {
    let acquired = Ok(Ok(McpDistributedAdmissionOutcome::Acquired));
    let duplicate = Ok(Ok(McpDistributedAdmissionOutcome::DuplicateRequestId));
    let cancelled = Ok(Ok(McpDistributedAdmissionOutcome::CancelledBeforeAdmission));
    let invalid = Ok(Ok(McpDistributedAdmissionOutcome::InvalidResponse));
    let redis_error = Ok(Err(redis::RedisError::from((
        redis::ErrorKind::Extension,
        "synthetic admission transport failure",
    ))));

    assert_eq!(
        mcp_distributed_admission_resolution(&acquired),
        McpDistributedAdmissionResolution::Owned
    );
    assert_eq!(
        mcp_distributed_admission_resolution(&duplicate),
        McpDistributedAdmissionResolution::Duplicate
    );
    assert_eq!(
        mcp_distributed_admission_resolution(&cancelled),
        McpDistributedAdmissionResolution::CancelledBeforeOwnership
    );
    assert_eq!(
        mcp_distributed_admission_resolution(&invalid),
        McpDistributedAdmissionResolution::CoordinationUnavailable
    );
    assert_eq!(
        mcp_distributed_admission_resolution(&redis_error),
        McpDistributedAdmissionResolution::CoordinationUnavailable
    );
}

/// The timeout arm is a real `Elapsed`, produced by tokio's timer rather than
/// hand-built; it must classify as coordination-unavailable too.
#[tokio::test(start_paused = true)]
async fn distributed_admission_resolution_classifies_timeout_as_unavailable() {
    let timed_out = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
        .await
        .expect_err("a zero timeout on a pending future must expire");

    assert_eq!(
        mcp_distributed_admission_resolution(&Err(timed_out)),
        McpDistributedAdmissionResolution::CoordinationUnavailable
    );
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
    let receiver = start_distributed_bounded_mcp_tool_call(McpDistributedToolCallAdmission {
        registry: Arc::clone(&registry),
        redis: redis.clone(),
        key,
        dispatch: McpToolCallDispatch {
            response_id,
            response_future: std::future::pending::<McpJsonRpcResponse>(),
            deadline: Duration::from_secs(3),
            mutating: false,
        },
    })
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
    assert_eq!(next_body_data(&mut body).await.expect("keepalive").as_ref(), b": keep-alive\n\n");
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

/// IRG-203 SITE-5: this drives the real chokepoint
/// (`start_bounded_mcp_tool_call_with_mutation` → `McpRegisteredToolCall::spawn`,
/// the same path used by `start_distributed_bounded_mcp_tool_call` in
/// production) for a mutating tool, not a hand-rolled spawn. The mutation must
/// survive the bounded deadline, and its admission must stay held until it
/// finishes — which is what closes the same-request-id double-apply window.
#[tokio::test(start_paused = true)]
async fn mutating_bounded_dispatch_outlives_deadline_and_holds_admission_until_completion() {
    let completed = Arc::new(AtomicBool::new(false));
    let completed_in_tool = Arc::clone(&completed);
    let registry = Arc::new(McpInFlightToolCallRegistry::new(4, 2));
    let response_id = json!(113);
    let receiver = start_bounded_mcp_tool_call_with_mutation(
        Arc::clone(&registry),
        in_flight_key(45, 46, "session-mutating-detach", &response_id),
        response_id,
        async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            completed_in_tool.store(true, Ordering::SeqCst);
            success_response(Some(json!(113)), json!({ "status": "done" }))
        },
        Duration::from_secs(1),
        true,
    );

    tokio::time::advance(Duration::from_secs(2)).await;
    let response = receiver.await.expect("bounded admission must emit a terminal response");
    let payload = serde_json::to_value(response).expect("serialize deadline response");
    assert_eq!(payload["error"]["data"]["errorKind"], json!("tool_call_deadline_exceeded"));
    assert!(
        !completed.load(Ordering::SeqCst),
        "the mutation must still be in flight when the deadline response is sent"
    );
    assert_eq!(
        registry.len(),
        1,
        "a mutating dispatch must hold its admission until the mutation completes"
    );

    tokio::time::advance(Duration::from_secs(20)).await;
    for _ in 0..64 {
        if completed.load(Ordering::SeqCst) && registry.len() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        completed.load(Ordering::SeqCst),
        "the detached mutating dispatch must complete after the bounded caller is dropped"
    );
    assert_eq!(registry.len(), 0, "the admission is released once the mutation completes");
}

/// The contrast: a non-mutating bounded dispatch stays inline, so the deadline
/// drops (cancels) it and releases its admission immediately. This pins the
/// mutating/non-mutating split the test above depends on.
#[tokio::test(start_paused = true)]
async fn non_mutating_bounded_dispatch_is_cancelled_and_released_at_deadline() {
    let completed = Arc::new(AtomicBool::new(false));
    let completed_in_tool = Arc::clone(&completed);
    let probe = DropProbe(Arc::new(AtomicBool::new(false)));
    let registry = Arc::new(McpInFlightToolCallRegistry::new(4, 2));
    let response_id = json!(114);
    let receiver = start_bounded_mcp_tool_call_with_mutation(
        Arc::clone(&registry),
        in_flight_key(47, 48, "session-inline", &response_id),
        response_id,
        async move {
            let _probe = probe;
            tokio::time::sleep(Duration::from_secs(10)).await;
            completed_in_tool.store(true, Ordering::SeqCst);
            success_response(Some(json!(114)), json!({ "status": "done" }))
        },
        Duration::from_secs(1),
        false,
    );

    tokio::time::advance(Duration::from_secs(2)).await;
    let response = receiver.await.expect("bounded admission must emit a terminal response");
    let payload = serde_json::to_value(response).expect("serialize deadline response");
    assert_eq!(payload["error"]["data"]["errorKind"], json!("tool_call_deadline_exceeded"));
    assert_eq!(registry.len(), 0, "a cancelled non-mutating dispatch releases its admission");

    tokio::time::advance(Duration::from_secs(200)).await;
    tokio::task::yield_now().await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "an inline non-mutating bounded dispatch must be cancelled at the deadline"
    );
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
        serde_json::to_value(principal_rejected).expect("serialize capacity")["error"]["data"]["errorKind"],
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
        spans.iter().filter(|span| span.as_str() == "Alpha flow includes completed action").count(),
        1
    );
}
