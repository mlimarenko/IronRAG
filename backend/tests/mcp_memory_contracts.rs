use axum::response::IntoResponse;
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::json;
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    domains::mcp_memory::{
        McpCapabilitySnapshot, McpDocumentHit, McpMutationOperationKind, McpMutationReceipt,
        McpMutationReceiptStatus, McpReadDocumentResponse, McpReadMode, McpReadabilityState,
        McpSearchDocumentsResponse,
    },
    infra::repositories,
    interfaces::http::router_support::ApiError,
    interfaces::http::{auth::hash_token, router},
};

use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;

#[test]
fn capability_snapshot_serializes_null_workspace_scope_and_tool_list() {
    let value = serde_json::to_value(McpCapabilitySnapshot {
        token_id: Uuid::nil(),
        token_kind: "instance_admin".to_string(),
        workspace_scope: None,
        visible_workspace_count: 2,
        visible_library_count: 4,
        tools: vec!["list_workspaces".to_string(), "search_documents".to_string()],
        generated_at: Utc::now(),
    })
    .unwrap();

    assert!(value.get("workspaceScope").is_some_and(serde_json::Value::is_null));
    assert_eq!(value.get("tools"), Some(&json!(["list_workspaces", "search_documents"])));
}

#[test]
fn read_response_preserves_nullability_for_unreadable_payloads() {
    let value = serde_json::to_value(McpReadDocumentResponse {
        document_id: Uuid::nil(),
        library_id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        latest_revision_id: None,
        read_mode: McpReadMode::Excerpt,
        readability_state: McpReadabilityState::Processing,
        status_reason: Some("document is still being processed".to_string()),
        content: None,
        slice_start_offset: 0,
        slice_end_offset: 0,
        total_content_length: None,
        continuation_token: None,
        has_more: false,
    })
    .unwrap();

    assert!(value.get("latestRevisionId").is_some_and(serde_json::Value::is_null));
    assert!(value.get("content").is_some_and(serde_json::Value::is_null));
    assert!(value.get("totalContentLength").is_some_and(serde_json::Value::is_null));
    assert!(value.get("continuationToken").is_some_and(serde_json::Value::is_null));
    assert_eq!(value.get("readabilityState"), Some(&json!("processing")));
}

#[test]
fn mutation_receipt_serializes_optional_runtime_and_failure_fields() {
    let value = serde_json::to_value(McpMutationReceipt {
        receipt_id: Uuid::nil(),
        token_id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        document_id: None,
        operation_kind: McpMutationOperationKind::Upload,
        idempotency_key: "mcp-upload-1".to_string(),
        status: McpMutationReceiptStatus::Accepted,
        runtime_tracking_id: None,
        accepted_at: Utc::now(),
        last_status_at: Utc::now(),
        failure_kind: None,
    })
    .unwrap();

    assert!(value.get("documentId").is_some_and(serde_json::Value::is_null));
    assert!(value.get("runtimeTrackingId").is_some_and(serde_json::Value::is_null));
    assert!(value.get("failureKind").is_some_and(serde_json::Value::is_null));
    assert_eq!(value.get("operationKind"), Some(&json!("upload")));
    assert_eq!(value.get("status"), Some(&json!("accepted")));
}

#[test]
fn search_responses_preserve_hit_order_and_nullability_for_unavailable_hits() {
    let readable_document_id = Uuid::now_v7();
    let unavailable_document_id = Uuid::now_v7();
    let value = serde_json::to_value(McpSearchDocumentsResponse {
        query: "memory".to_string(),
        limit: 2,
        library_ids: vec![Uuid::nil()],
        hits: vec![
            McpDocumentHit {
                document_id: readable_document_id,
                logical_document_id: readable_document_id,
                library_id: Uuid::nil(),
                workspace_id: Uuid::nil(),
                document_title: "Readable memory".to_string(),
                latest_revision_id: Some(Uuid::now_v7()),
                score: 4.0,
                excerpt: Some("memory excerpt".to_string()),
                excerpt_start_offset: Some(12),
                excerpt_end_offset: Some(26),
                readability_state: McpReadabilityState::Readable,
                status_reason: None,
            },
            McpDocumentHit {
                document_id: unavailable_document_id,
                logical_document_id: unavailable_document_id,
                library_id: Uuid::nil(),
                workspace_id: Uuid::nil(),
                document_title: "Unavailable memory".to_string(),
                latest_revision_id: None,
                score: 1.0,
                excerpt: None,
                excerpt_start_offset: None,
                excerpt_end_offset: None,
                readability_state: McpReadabilityState::Unavailable,
                status_reason: Some(
                    "document finished without normalized extracted text".to_string(),
                ),
            },
        ],
    })
    .unwrap();

    let hits = value.get("hits").and_then(serde_json::Value::as_array).unwrap();
    assert_eq!(hits[0].get("documentId"), Some(&json!(readable_document_id)));
    assert_eq!(hits[1].get("documentId"), Some(&json!(unavailable_document_id)));
    assert!(hits[1].get("latestRevisionId").is_some_and(serde_json::Value::is_null));
    assert!(hits[1].get("excerpt").is_some_and(serde_json::Value::is_null));
    assert!(hits[1].get("excerptStartOffset").is_some_and(serde_json::Value::is_null));
    assert!(hits[1].get("excerptEndOffset").is_some_and(serde_json::Value::is_null));
    assert_eq!(hits[1].get("readabilityState"), Some(&json!("unavailable")));
}

#[tokio::test]
async fn mcp_specific_api_errors_emit_contract_error_kinds() {
    let scenarios = [
        (ApiError::invalid_mcp_tool_call("unsupported MCP tool"), "invalid_mcp_tool_call"),
        (
            ApiError::invalid_continuation_token("invalid continuation token"),
            "invalid_continuation_token",
        ),
        (ApiError::unreadable_document("document is not readable yet"), "unreadable_document"),
        (
            ApiError::idempotency_conflict("payload changed for the same idempotency key"),
            "idempotency_conflict",
        ),
    ];

    for (error, expected_kind) in scenarios {
        let response = error.into_response();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value.get("errorKind"), Some(&json!(expected_kind)));
    }
}

struct McpDiscoveryContractFixture {
    state: AppState,
    workspace_id: Uuid,
}

impl McpDiscoveryContractFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-contracts-empty-{suffix}"),
            "MCP Empty Discovery Contract",
        )
        .await
        .context("failed to create mcp empty discovery workspace")?;
        Ok(Self { state, workspace_id: workspace.id })
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace_id)
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete mcp empty discovery workspace")?;
        Ok(())
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        let plaintext = format!("mcp-contracts-{}-{}", label, Uuid::now_v7());
        repositories::create_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            &hash_token(&plaintext),
            Some("mcp-contracts-token"),
            json!(scopes),
            None,
        )
        .await
        .with_context(|| format!("failed to create mcp contracts token for {label}"))?;
        Ok(plaintext)
    }

    async fn instance_admin_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        let plaintext = format!("mcp-contracts-instance-{}-{}", label, Uuid::now_v7());
        repositories::create_api_token(
            &self.state.persistence.postgres,
            None,
            "instance_admin",
            label,
            &hash_token(&plaintext),
            Some("mcp-contracts-instance-token"),
            json!(scopes),
            None,
        )
        .await
        .with_context(|| {
            format!("failed to create mcp contracts instance-admin token for {label}")
        })?;
        Ok(plaintext)
    }

    async fn capabilities(&self, token: &str) -> anyhow::Result<serde_json::Value> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/mcp/capabilities")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("build mcp contracts capabilities request"),
            )
            .await
            .context("mcp contracts capabilities request failed")?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for capabilities", response.status());
        }

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect capabilities body")?
            .to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode capabilities json")
    }

    async fn tool_call(
        &self,
        token: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/mcp")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": "contracts-empty-discovery",
                            "method": "tools/call",
                            "params": {
                                "name": tool_name,
                                "arguments": arguments,
                            },
                        })
                        .to_string(),
                    ))
                    .expect("build mcp contracts tools/call request"),
            )
            .await
            .with_context(|| format!("mcp contracts tool call {tool_name} failed"))?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for tool {tool_name}", response.status());
        }

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect tools/call body")?
            .to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode tools/call json")
    }

    async fn rpc_call(
        &self,
        token: &str,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/mcp")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": "contracts-rpc-call",
                            "method": method,
                            "params": params,
                        })
                        .to_string(),
                    ))
                    .expect("build mcp contracts rpc request"),
            )
            .await
            .with_context(|| format!("mcp contracts rpc call {method} failed"))?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for method {method}", response.status());
        }

        let bytes =
            response.into_body().collect().await.context("failed to collect rpc body")?.to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode rpc json")
    }

    async fn notification(
        &self,
        token: &str,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<(StatusCode, Vec<u8>)> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/mcp")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "method": method,
                            "params": params,
                        })
                        .to_string(),
                    ))
                    .expect("build mcp contracts notification request"),
            )
            .await
            .with_context(|| format!("mcp contracts notification {method} failed"))?;

        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect notification body")?
            .to_bytes()
            .to_vec();
        Ok((status, bytes))
    }
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and neo4j services"]
async fn no_access_discovery_returns_explicit_zero_counts_and_empty_arrays() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp discovery contracts test")?;
    let fixture = McpDiscoveryContractFixture::create(settings).await?;

    let result = async {
        let token = fixture.token(&["documents:read"], "empty-discovery").await?;

        let capabilities = fixture.capabilities(&token).await?;
        assert_eq!(capabilities["workspaceScope"], json!(fixture.workspace_id));
        assert_eq!(capabilities["visibleWorkspaceCount"], json!(1));
        assert_eq!(capabilities["visibleLibraryCount"], json!(0));

        let workspaces = fixture.tool_call(&token, "list_workspaces", json!({})).await?;
        let workspace_items = workspaces["result"]["structuredContent"]["workspaces"]
            .as_array()
            .context("workspaces payload must be an array")?;
        assert_eq!(workspace_items.len(), 1);
        assert_eq!(workspace_items[0]["visibleLibraryCount"], json!(0));

        let libraries = fixture.tool_call(&token, "list_libraries", json!({})).await?;
        let library_items = libraries["result"]["structuredContent"]["libraries"]
            .as_array()
            .context("libraries payload must be an array")?;
        assert!(library_items.is_empty());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and neo4j services"]
async fn initialized_notifications_are_accepted_without_jsonrpc_error_bodies() -> anyhow::Result<()>
{
    let settings = Settings::from_env()
        .context("failed to load settings for mcp notification contracts test")?;
    let fixture = McpDiscoveryContractFixture::create(settings).await?;

    let result = async {
        let token = fixture.token(&["documents:read"], "notification-accept").await?;
        let (status, body) =
            fixture.notification(&token, "notifications/initialized", json!({})).await?;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.is_empty(), "notification responses must not include a JSON-RPC body");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and neo4j services"]
async fn resource_discovery_methods_return_empty_lists_instead_of_method_errors()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp resource contracts test")?;
    let fixture = McpDiscoveryContractFixture::create(settings).await?;

    let result = async {
        let token = fixture.token(&["documents:read"], "resource-discovery").await?;

        let resources = fixture.rpc_call(&token, "resources/list", json!({})).await?;
        assert_eq!(resources["result"]["resources"], json!([]));

        let templates = fixture.rpc_call(&token, "resources/templates/list", json!({})).await?;
        assert_eq!(templates["result"]["resourceTemplates"], json!([]));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and neo4j services"]
async fn create_tools_allow_omitting_slug_and_advertise_optional_slug_inputs() -> anyhow::Result<()>
{
    let settings = Settings::from_env()
        .context("failed to load settings for mcp create-tool contracts test")?;
    let fixture = McpDiscoveryContractFixture::create(settings).await?;

    let result = async {
        let token = fixture
            .instance_admin_token(
                &[
                    "instance_admin",
                    "workspace:admin",
                    "projects:write",
                    "documents:read",
                    "documents:write",
                ],
                "optional-slug",
            )
            .await?;

        let tools = fixture.rpc_call(&token, "tools/list", json!({})).await?;
        let tool_items =
            tools["result"]["tools"].as_array().context("tools/list must return a tools array")?;

        let workspace_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("create_workspace"))
            .context("create_workspace tool missing from tools/list")?;
        assert_eq!(workspace_tool["inputSchema"]["required"], json!(["name"]));

        let library_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("create_library"))
            .context("create_library tool missing from tools/list")?;
        assert_eq!(library_tool["inputSchema"]["required"], json!(["workspaceId", "name"]));

        let workspace_response = fixture
            .tool_call(&token, "create_workspace", json!({ "name": "Agent Workspace ++" }))
            .await?;
        let created_workspace_id =
            workspace_response["result"]["structuredContent"]["workspace"]["workspaceId"]
                .as_str()
                .context("create_workspace must return workspaceId")?
                .parse::<Uuid>()
                .context("create_workspace returned invalid workspaceId")?;
        assert_eq!(
            workspace_response["result"]["structuredContent"]["workspace"]["slug"],
            json!("agent-workspace")
        );

        let library_response = fixture
            .tool_call(
                &token,
                "create_library",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "name": "Agent Library ++",
                }),
            )
            .await?;
        assert_eq!(
            library_response["result"]["structuredContent"]["library"]["slug"],
            json!("agent-library")
        );

        sqlx::query("delete from workspace where id = $1")
            .bind(created_workspace_id)
            .execute(&fixture.state.persistence.postgres)
            .await
            .context("failed to delete created workspace from optional-slug contract test")?;

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
