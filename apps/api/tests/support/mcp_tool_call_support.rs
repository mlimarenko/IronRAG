use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use ironrag_backend::interfaces::http::mcp::{
    MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER,
};

pub(crate) struct InitializedMcpSession {
    pub(crate) id: String,
    pub(crate) payload: Value,
}

pub(crate) async fn initialize_session(
    app: Router,
    endpoint: &str,
    token: &str,
    request_id: &str,
) -> Result<InitializedMcpSession> {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(endpoint)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
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

pub(crate) async fn post_session_json(
    app: Router,
    endpoint: &str,
    token: &str,
    session_id: &str,
    payload: Value,
) -> Result<Response> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(endpoint)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION)
            .header(MCP_SESSION_HEADER, session_id)
            .body(Body::from(payload.to_string()))
            .context("failed to build MCP session request")?,
    )
    .await
    .context("MCP session request failed")
}

pub(crate) async fn terminate_session(
    app: Router,
    endpoint: &str,
    token: &str,
    session_id: &str,
) -> Result<()> {
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(endpoint)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
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
    let session =
        initialize_session(app.clone(), endpoint, token, &format!("{request_id}-initialize"))
            .await?;
    if session.payload.get("error").is_some() {
        anyhow::bail!("MCP initialize returned a JSON-RPC error");
    }
    let response_result = post_session_json(
        app.clone(),
        endpoint,
        token,
        &session.id,
        json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }),
    )
    .await;
    let cleanup_result = terminate_session(app, endpoint, token, &session.id).await;
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

async fn response_json(response: Response) -> Result<Value> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .context("failed to collect MCP response body")?
        .to_bytes();
    serde_json::from_slice(&bytes).context("failed to decode MCP response JSON")
}
