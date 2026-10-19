use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::state::AppState,
    infra::repositories::iam_repository::{self, SystemRole},
    interfaces::http::{
        auth::{
            build_session_cookie_value, hash_session_secret, hash_token,
            mint_plaintext_session_secret,
        },
        mcp::{MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER},
    },
};

use crate::ingest_ai_support;

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled by suites without admin-session fixtures"
)]
pub(crate) async fn create_admin_session_cookie(
    state: &AppState,
    label: &str,
    display_name: &str,
) -> Result<String> {
    let suffix = Uuid::now_v7().simple();
    let user = iam_repository::create_user(
        &state.persistence.postgres,
        &format!("{label}-{suffix}"),
        &format!("{label}-{suffix}@example.invalid"),
        display_name,
        &hash_token(&format!("password-{suffix}")),
        SystemRole::Admin,
    )
    .await
    .with_context(|| format!("failed to create admin user for {label}"))?;
    let secret = mint_plaintext_session_secret();
    let session = iam_repository::create_session(
        &state.persistence.postgres,
        user.principal_id,
        &hash_session_secret(&secret),
        Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .with_context(|| format!("failed to create admin session for {label}"))?;
    Ok(format!(
        "{}={}",
        state.ui_session_cookie.name,
        build_session_cookie_value(session.id, &secret)
    ))
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled by suites without ingestion fixtures"
)]
pub(crate) async fn configure_empty_graph_ingest_ai(
    state: &mut AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    account_label: &str,
    credential_ref: &str,
    fixture_name: &'static str,
) -> Result<()> {
    ingest_ai_support::configure_ingest_ai_fixture(
        state,
        workspace_id,
        library_id,
        ingest_ai_support::IngestAiFixtureConfig {
            account_label,
            account_kind: "provider",
            credential_ref,
            fixture_name,
            graph_output: r#"{"entities":[],"relations":[]}"#,
            invalid_dimensions_subject: Some(fixture_name),
            missing_catalog_subject: fixture_name,
            io_delay: None,
            panic_on: None,
        },
    )
    .await
}

pub(crate) struct InitializedMcpSession {
    pub(crate) id: String,
    pub(crate) payload: Value,
}

#[derive(Clone, Copy)]
enum McpTestCredential<'a> {
    Bearer(&'a str),
    Cookie(&'a str),
}

fn with_credential(
    builder: axum::http::request::Builder,
    credential: McpTestCredential<'_>,
) -> axum::http::request::Builder {
    match credential {
        McpTestCredential::Bearer(token) => {
            builder.header(header::AUTHORIZATION, format!("Bearer {token}"))
        }
        McpTestCredential::Cookie(cookie) => builder.header(header::COOKIE, cookie),
    }
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled by suites that only use high-level RPC helpers"
)]
pub(crate) async fn initialize_session(
    app: Router,
    endpoint: &str,
    token: &str,
    request_id: &str,
) -> Result<InitializedMcpSession> {
    initialize_session_with_credential(app, endpoint, McpTestCredential::Bearer(token), request_id)
        .await
}

async fn initialize_session_with_credential(
    app: Router,
    endpoint: &str,
    credential: McpTestCredential<'_>,
    request_id: &str,
) -> Result<InitializedMcpSession> {
    let response = app
        .oneshot(
            with_credential(Request::builder().method("POST").uri(endpoint), credential)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": MCP_PROTOCOL_VERSION,
                            "capabilities": {},
                            "clientInfo": { "name": "ironrag-integration-test", "version": "1" },
                        },
                    })
                    .to_string(),
                ))
                .context("failed to build MCP initialize request")?,
        )
        .await
        .context("MCP initialize failed")?;
    if response.status() != StatusCode::OK {
        anyhow::bail!("unexpected MCP initialize status {}", response.status());
    }
    let session_id = response
        .headers()
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .context("MCP initialize response omitted the session id")?
        .to_string();
    let payload = response_json(response).await?;
    Ok(InitializedMcpSession { id: session_id, payload })
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled by suites that only use high-level RPC helpers"
)]
pub(crate) async fn post_session_json(
    app: Router,
    endpoint: &str,
    token: &str,
    session_id: &str,
    payload: Value,
) -> Result<Response> {
    post_session_json_with_credential(
        app,
        endpoint,
        McpTestCredential::Bearer(token),
        session_id,
        payload,
    )
    .await
}

async fn post_session_json_with_credential(
    app: Router,
    endpoint: &str,
    credential: McpTestCredential<'_>,
    session_id: &str,
    payload: Value,
) -> Result<Response> {
    app.oneshot(
        with_credential(Request::builder().method("POST").uri(endpoint), credential)
            .header(header::CONTENT_TYPE, "application/json")
            .header(MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION)
            .header(MCP_SESSION_HEADER, session_id)
            .body(Body::from(payload.to_string()))
            .context("failed to build MCP session request")?,
    )
    .await
    .context("MCP session request failed")
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled by suites that only use high-level RPC helpers"
)]
pub(crate) async fn terminate_session(
    app: Router,
    endpoint: &str,
    token: &str,
    session_id: &str,
) -> Result<()> {
    terminate_session_with_credential(app, endpoint, McpTestCredential::Bearer(token), session_id)
        .await
}

async fn terminate_session_with_credential(
    app: Router,
    endpoint: &str,
    credential: McpTestCredential<'_>,
    session_id: &str,
) -> Result<()> {
    let response = app
        .oneshot(
            with_credential(Request::builder().method("DELETE").uri(endpoint), credential)
                .header(MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION)
                .header(MCP_SESSION_HEADER, session_id)
                .body(Body::empty())
                .context("failed to build MCP session cleanup request")?,
        )
        .await
        .context("MCP session cleanup failed")?;
    if response.status() != StatusCode::OK {
        anyhow::bail!("unexpected MCP session cleanup status {}", response.status());
    }
    Ok(())
}

pub(crate) async fn call_rpc(
    app: Router,
    endpoint: &str,
    token: &str,
    request_id: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    call_rpc_with_credential(
        app,
        endpoint,
        McpTestCredential::Bearer(token),
        request_id,
        method,
        params,
    )
    .await
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled by suites that only use bearer auth"
)]
pub(crate) async fn call_rpc_with_cookie(
    app: Router,
    endpoint: &str,
    cookie: &str,
    request_id: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    call_rpc_with_credential(
        app,
        endpoint,
        McpTestCredential::Cookie(cookie),
        request_id,
        method,
        params,
    )
    .await
}

async fn call_rpc_with_credential(
    app: Router,
    endpoint: &str,
    credential: McpTestCredential<'_>,
    request_id: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    let session = initialize_session_with_credential(
        app.clone(),
        endpoint,
        credential,
        &format!("{request_id}-initialize"),
    )
    .await?;
    if session.payload.get("error").is_some() {
        anyhow::bail!("MCP initialize returned a JSON-RPC error");
    }
    let response_result = post_session_json_with_credential(
        app.clone(),
        endpoint,
        credential,
        &session.id,
        json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }),
    )
    .await;
    let cleanup_result =
        terminate_session_with_credential(app, endpoint, credential, &session.id).await;
    let response = response_result?;
    cleanup_result?;
    if response.status() != StatusCode::OK {
        anyhow::bail!("unexpected MCP {method} status {}", response.status());
    }
    response_json(response).await
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled by crates that only use call_rpc"
)]
pub(crate) async fn call_tool(
    app: Router,
    endpoint: &str,
    token: &str,
    request_id: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<Value> {
    call_rpc(
        app,
        endpoint,
        token,
        request_id,
        "tools/call",
        json!({ "name": tool_name, "arguments": arguments }),
    )
    .await
}

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled by suites that only use bearer auth"
)]
pub(crate) async fn call_tool_with_cookie(
    app: Router,
    endpoint: &str,
    cookie: &str,
    request_id: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<Value> {
    call_rpc_with_cookie(
        app,
        endpoint,
        cookie,
        request_id,
        "tools/call",
        json!({ "name": tool_name, "arguments": arguments }),
    )
    .await
}

async fn response_json(response: Response) -> Result<Value> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .context("failed to collect MCP response body")?
        .to_bytes();
    serde_json::from_slice(&bytes).context("failed to decode MCP response JSON")
}
