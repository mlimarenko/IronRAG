#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/iam_token_support.rs"]
mod iam_token_support;

use axum::response::IntoResponse;
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::json;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::catalog_repository,
    interfaces::http::router,
    interfaces::http::router_support::ApiError,
    mcp_types::{
        McpCapabilitySnapshot, McpDocumentHit, McpMutationOperationKind, McpMutationReceipt,
        McpMutationReceiptStatus, McpReadDocumentResponse, McpReadMode, McpReadabilityState,
        McpSearchDocumentsResponse,
    },
};

use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;

#[test]
fn capability_snapshot_omits_absent_workspace_scope_and_keeps_tool_list() {
    let value = serde_json::to_value(McpCapabilitySnapshot {
        token_id: Some(Uuid::nil()),
        token_kind: "instance_admin".to_string(),
        workspace_scope: None,
        visible_workspace_count: 2,
        visible_library_count: 4,
        tools: vec!["list_workspaces".to_string(), "search_documents".to_string()],
        generated_at: Some(Utc::now()),
    })
    .unwrap();

    assert!(value.get("workspaceScope").is_none());
    assert_eq!(value.get("tools"), Some(&json!(["list_workspaces", "search_documents"])));
}

#[test]
fn read_response_preserves_nullability_for_unreadable_payloads() {
    let value = serde_json::to_value(McpReadDocumentResponse {
        document_id: Uuid::nil(),
        document_title: "Unreadable memory".to_string(),
        library_id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        latest_revision_id: None,
        read_mode: McpReadMode::Excerpt,
        readability_state: McpReadabilityState::Processing,
        readiness_kind: "processing".to_string(),
        graph_coverage_kind: "processing".to_string(),
        status_reason: Some("document is still being processed".to_string()),
        mime_type: None,
        source_uri: None,
        source_access: None,
        visual_description: None,
        content: None,
        slice_start_offset: 0,
        slice_end_offset: 0,
        total_content_length: None,
        continuation_token: None,
        has_more: false,
        chunk_references: Vec::new(),
        technical_fact_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    })
    .unwrap();

    assert!(value.get("latestRevisionId").is_none());
    assert!(value.get("content").is_none());
    assert!(value.get("totalContentLength").is_none());
    assert!(value.get("continuationToken").is_none());
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
        accepted_at: Utc::now(),
        last_status_at: Utc::now(),
        failure_kind: None,
    })
    .unwrap();

    assert!(value.get("documentId").is_some_and(serde_json::Value::is_null));
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
        libraries: vec!["default/default-library".to_string()],
        hits: vec![
            McpDocumentHit {
                document_id: readable_document_id,
                library_id: Uuid::nil(),
                workspace_id: Uuid::nil(),
                document_title: "Readable memory".to_string(),
                latest_revision_id: Some(Uuid::now_v7()),
                score: 4.0,
                excerpt: Some("memory excerpt".to_string()),
                excerpt_start_offset: Some(12),
                excerpt_end_offset: Some(26),
                suggested_start_offset: None,
                readability_state: McpReadabilityState::Readable,
                readiness_kind: "readable".to_string(),
                graph_coverage_kind: "graph_ready".to_string(),
                status_reason: None,
                chunk_references: Vec::new(),
                technical_fact_references: Vec::new(),
                entity_references: Vec::new(),
                relation_references: Vec::new(),
                evidence_references: Vec::new(),
            },
            McpDocumentHit {
                document_id: unavailable_document_id,
                library_id: Uuid::nil(),
                workspace_id: Uuid::nil(),
                document_title: "Unavailable memory".to_string(),
                latest_revision_id: None,
                score: 1.0,
                excerpt: None,
                excerpt_start_offset: None,
                excerpt_end_offset: None,
                suggested_start_offset: None,
                readability_state: McpReadabilityState::Unavailable,
                readiness_kind: "failed".to_string(),
                graph_coverage_kind: "failed".to_string(),
                status_reason: Some(
                    "document finished without normalized extracted text".to_string(),
                ),
                chunk_references: Vec::new(),
                technical_fact_references: Vec::new(),
                entity_references: Vec::new(),
                relation_references: Vec::new(),
                evidence_references: Vec::new(),
            },
        ],
    })
    .unwrap();

    let hits = value.get("hits").and_then(serde_json::Value::as_array).unwrap();
    assert_eq!(hits[0].get("documentId"), Some(&json!(readable_document_id)));
    assert_eq!(hits[1].get("documentId"), Some(&json!(unavailable_document_id)));
    assert!(hits[1].get("latestRevisionId").is_none());
    assert!(hits[1].get("excerpt").is_none());
    assert!(hits[1].get("excerptStartOffset").is_none());
    assert!(hits[1].get("excerptEndOffset").is_none());
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
    workspace_ref: String,
}

impl McpDiscoveryContractFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-contracts-empty-{suffix}"),
            "MCP Empty Discovery Contract",
            None,
        )
        .await
        .context("failed to create mcp empty discovery workspace")?;
        Ok(Self { state, workspace_id: workspace.id, workspace_ref: workspace.slug })
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        sqlx::query("delete from catalog_workspace where id = $1")
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
        iam_token_support::mint_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            scopes,
        )
        .await
        .map(|token| token.plaintext)
        .with_context(|| format!("failed to create mcp contracts token for {label}"))
    }

    async fn instance_admin_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        iam_token_support::mint_api_token(
            &self.state.persistence.postgres,
            None,
            "instance_admin",
            label,
            scopes,
        )
        .await
        .map(|token| token.plaintext)
        .with_context(|| format!("failed to create mcp contracts instance-admin token for {label}"))
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
#[ignore = "requires local postgres, redis, and arango services"]
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
#[ignore = "requires local postgres, redis, and arango services"]
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
#[ignore = "requires local postgres, redis, and arango services"]
async fn resource_discovery_methods_are_not_supported() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp resource contracts test")?;
    let fixture = McpDiscoveryContractFixture::create(settings).await?;

    let result = async {
        let token = fixture.token(&["documents:read"], "resource-discovery").await?;

        let initialize = fixture.rpc_call(&token, "initialize", json!({})).await?;
        assert_eq!(initialize["result"]["protocolVersion"], json!("2025-06-18"));
        assert_eq!(initialize["result"]["capabilities"]["tools"], json!({ "listChanged": false }));
        assert!(
            initialize["result"]["capabilities"].get("resources").is_none(),
            "initialize must not advertise an empty resources surface"
        );

        let resources = fixture.rpc_call(&token, "resources/list", json!({})).await?;
        assert_eq!(resources["error"]["code"], json!(-32601));
        assert_eq!(resources["error"]["data"]["errorKind"], json!("unsupported_method"));

        let templates = fixture.rpc_call(&token, "resources/templates/list", json!({})).await?;
        assert_eq!(templates["error"]["code"], json!(-32601));
        assert_eq!(templates["error"]["data"]["errorKind"], json!("unsupported_method"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
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
        assert_eq!(workspace_tool["inputSchema"]["required"], json!(["workspace"]));

        let library_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("create_library"))
            .context("create_library tool missing from tools/list")?;
        assert_eq!(library_tool["inputSchema"]["required"], json!(["library"]));

        let workspace_response = fixture
            .tool_call(
                &token,
                "create_workspace",
                json!({ "workspace": "agent-workspace", "title": "Agent Workspace ++" }),
            )
            .await?;
        let created_workspace_id =
            workspace_response["result"]["structuredContent"]["workspace"]["workspaceId"]
                .as_str()
                .context("create_workspace must return workspaceId")?
                .parse::<Uuid>()
                .context("create_workspace returned invalid workspaceId")?;
        assert_eq!(
            workspace_response["result"]["structuredContent"]["workspace"]["ref"],
            json!("agent-workspace")
        );

        let library_response = fixture
            .tool_call(
                &token,
                "create_library",
                json!({
                    "library": format!("{}/agent-library", fixture.workspace_ref),
                    "title": "Agent Library ++",
                }),
            )
            .await?;
        assert_eq!(
            library_response["result"]["structuredContent"]["library"]["ref"],
            json!(format!("{}/agent-library", fixture.workspace_ref))
        );
        assert_eq!(
            library_response["result"]["structuredContent"]["library"]["ingestionReadiness"]["ready"],
            json!(false)
        );
        assert_eq!(
            library_response["result"]["structuredContent"]["library"]["ingestionReadiness"]["missingBindingPurposes"],
            json!(["extract_graph"])
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

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn web_ingest_tools_advertise_recursive_defaults_and_page_listing_contracts()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp web-ingest contract test")?;
    let fixture = McpDiscoveryContractFixture::create(settings).await?;

    let result = async {
        let token =
            fixture.token(&["documents:read", "documents:write"], "web-ingest-contracts").await?;

        let capabilities = fixture.capabilities(&token).await?;
        let tools =
            capabilities["tools"].as_array().context("capabilities tools must be an array")?;
        assert!(tools.iter().any(|tool| tool == "submit_web_ingest_run"));
        assert!(tools.iter().any(|tool| tool == "get_web_ingest_run"));
        assert!(tools.iter().any(|tool| tool == "list_web_ingest_run_pages"));
        assert!(tools.iter().any(|tool| tool == "cancel_web_ingest_run"));
        assert!(tools.iter().any(|tool| tool == "get_runtime_execution"));
        assert!(tools.iter().any(|tool| tool == "get_runtime_execution_trace"));

        let tool_list = fixture.rpc_call(&token, "tools/list", json!({})).await?;
        let tool_items = tool_list["result"]["tools"]
            .as_array()
            .context("tools/list must return a tools array")?;

        let submit_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("submit_web_ingest_run"))
            .context("submit_web_ingest_run tool missing from tools/list")?;
        assert_eq!(submit_tool["inputSchema"]["required"], json!(["library", "seedUrl", "mode"]));
        assert_eq!(
            submit_tool["inputSchema"]["properties"]["mode"]["enum"],
            json!(["single_page", "recursive_crawl"])
        );
        assert_eq!(
            submit_tool["inputSchema"]["properties"]["boundaryPolicy"]["enum"],
            json!(["same_host", "allow_external"])
        );
        assert!(
            submit_tool["inputSchema"]["properties"]["maxDepth"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("defaults to 3")
        );
        assert!(
            submit_tool["inputSchema"]["properties"]["maxPages"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("Optional crawl budget")
        );

        let get_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("get_web_ingest_run"))
            .context("get_web_ingest_run tool missing from tools/list")?;
        assert_eq!(get_tool["inputSchema"]["required"], json!(["runId"]));

        let pages_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("list_web_ingest_run_pages"))
            .context("list_web_ingest_run_pages tool missing from tools/list")?;
        assert_eq!(pages_tool["inputSchema"]["required"], json!(["runId"]));
        assert!(
            pages_tool["description"]
                .as_str()
                .unwrap_or_default()
                .contains("candidate pages and outcomes")
        );

        let cancel_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("cancel_web_ingest_run"))
            .context("cancel_web_ingest_run tool missing from tools/list")?;
        assert_eq!(cancel_tool["inputSchema"]["required"], json!(["runId"]));

        let runtime_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("get_runtime_execution"))
            .context("get_runtime_execution tool missing from tools/list")?;
        assert_eq!(runtime_tool["inputSchema"]["required"], json!(["runtimeExecutionId"]));

        let trace_tool = tool_items
            .iter()
            .find(|tool| tool["name"] == json!("get_runtime_execution_trace"))
            .context("get_runtime_execution_trace tool missing from tools/list")?;
        assert_eq!(trace_tool["inputSchema"]["required"], json!(["runtimeExecutionId"]));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
