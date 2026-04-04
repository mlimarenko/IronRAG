use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{self, NewMcpMutationReceipt},
    interfaces::http::{auth::hash_token, router},
};

struct McpAuditFixture {
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    foreign_workspace_id: Uuid,
}

impl McpAuditFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-audit-{suffix}"),
            "MCP Audit Test",
        )
        .await
        .context("failed to create mcp audit workspace")?;
        let library = repositories::create_project(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-audit-library-{suffix}"),
            "MCP Audit Library",
            Some("mcp audit test library"),
        )
        .await
        .context("failed to create mcp audit library")?;

        let foreign_workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-audit-foreign-{suffix}"),
            "MCP Audit Foreign Test",
        )
        .await
        .context("failed to create foreign audit workspace")?;
        let _foreign_library = repositories::create_project(
            &state.persistence.postgres,
            foreign_workspace.id,
            &format!("mcp-audit-foreign-library-{suffix}"),
            "MCP Audit Foreign Library",
            Some("mcp audit foreign library"),
        )
        .await
        .context("failed to create foreign audit library")?;

        Ok(Self {
            state,
            workspace_id: workspace.id,
            library_id: library.id,
            foreign_workspace_id: foreign_workspace.id,
        })
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = any($1)")
            .bind([self.workspace_id, self.foreign_workspace_id].as_slice())
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete audit test workspaces")?;
        Ok(())
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn bearer_token(
        &self,
        workspace_id: Option<Uuid>,
        token_kind: &str,
        scopes: &[&str],
        label: &str,
    ) -> anyhow::Result<(Uuid, String)> {
        let plaintext = format!("mcp-audit-{}-{}", label, Uuid::now_v7());
        let token = repositories::create_api_token(
            &self.state.persistence.postgres,
            workspace_id,
            token_kind,
            label,
            &hash_token(&plaintext),
            Some("mcp-audit-token"),
            json!(scopes),
            None,
        )
        .await
        .with_context(|| format!("failed to create audit token for {label}"))?;
        Ok((token.id, plaintext))
    }

    async fn mcp_tool_call(
        &self,
        token: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<(HeaderMap, Value)> {
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
                            "id": "audit-test",
                            "method": "tools/call",
                            "params": {
                                "name": tool_name,
                                "arguments": arguments,
                            },
                        })
                        .to_string(),
                    ))
                    .expect("build mcp audit request"),
            )
            .await
            .with_context(|| format!("MCP audit tool call {tool_name} failed"))?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for tool {tool_name}", response.status());
        }

        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect mcp audit body")?
            .to_bytes();
        let json = serde_json::from_slice(&bytes).context("failed to decode mcp audit json")?;
        Ok((headers, json))
    }

    async fn get_json(
        &self,
        token: &str,
        path: &str,
    ) -> anyhow::Result<(StatusCode, HeaderMap, Value)> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("build audit get request"),
            )
            .await
            .with_context(|| format!("GET {path} failed"))?;

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .with_context(|| format!("failed to collect body for {path}"))?
            .to_bytes();
        let json = serde_json::from_slice(&bytes).context("failed to decode audit get body")?;
        Ok((status, headers, json))
    }

    async fn create_readable_document(
        &self,
        external_key: &str,
        content: &str,
    ) -> anyhow::Result<Uuid> {
        let document = repositories::create_document(
            &self.state.persistence.postgres,
            self.library_id,
            None,
            external_key,
            Some(external_key),
            Some("text/plain"),
            Some("mcp-audit-checksum"),
        )
        .await
        .with_context(|| format!("failed to create audit document {external_key}"))?;
        let revision = repositories::create_document_revision(
            &self.state.persistence.postgres,
            document.id,
            1,
            "initial_upload",
            None,
            &format!("{external_key}.txt"),
            Some("text/plain"),
            Some(i64::try_from(content.len()).unwrap_or(i64::MAX)),
            None,
            Some("mcp-audit-revision"),
        )
        .await
        .with_context(|| format!("failed to create audit revision for {external_key}"))?;
        repositories::activate_document_revision(
            &self.state.persistence.postgres,
            document.id,
            revision.id,
        )
        .await
        .context("failed to activate audit revision")?;
        repositories::update_document_current_revision(
            &self.state.persistence.postgres,
            document.id,
            Some(revision.id),
            "active",
            None,
            None,
        )
        .await
        .context("failed to mark audit document active")?;
        repositories::create_chunk(
            &self.state.persistence.postgres,
            document.id,
            self.library_id,
            0,
            content,
            Some(i32::try_from(content.split_whitespace().count()).unwrap_or(i32::MAX)),
            json!({ "source": "mcp_audit_test" }),
        )
        .await
        .context("failed to create audit chunk")?;
        let runtime_run = repositories::create_runtime_ingestion_run(
            &self.state.persistence.postgres,
            self.library_id,
            Some(document.id),
            Some(revision.id),
            None,
            &format!("audit-track-{external_key}-{}", Uuid::now_v7()),
            &format!("{external_key}.txt"),
            "txt",
            Some("text/plain"),
            Some(i64::try_from(content.len()).unwrap_or(i64::MAX)),
            "ready",
            "completed",
            "mcp_audit_fixture",
            json!({}),
        )
        .await
        .context("failed to create audit runtime run")?;
        repositories::update_runtime_ingestion_run_status(
            &self.state.persistence.postgres,
            runtime_run.id,
            "ready",
            "completed",
            Some(100),
            None,
        )
        .await
        .context("failed to mark audit run ready")?;
        repositories::upsert_runtime_extracted_content(
            &self.state.persistence.postgres,
            runtime_run.id,
            Some(document.id),
            "normalized_text",
            Some(content),
            None,
            Some(i32::try_from(content.chars().count()).unwrap_or(i32::MAX)),
            json!([]),
            json!({}),
            None,
            None,
            None,
        )
        .await
        .context("failed to write audit extracted content")?;
        Ok(document.id)
    }

    async fn insert_broken_receipt(&self, token_id: Uuid) -> anyhow::Result<Uuid> {
        let receipt = repositories::create_mcp_mutation_receipt(
            &self.state.persistence.postgres,
            &NewMcpMutationReceipt {
                token_id,
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                document_id: None,
                operation_kind: "broken_operation".to_string(),
                idempotency_key: format!("broken-receipt-{}", Uuid::now_v7()),
                payload_identity: Some("broken-payload".to_string()),
                status: "accepted".to_string(),
                failure_kind: None,
            },
        )
        .await
        .context("failed to create broken receipt")?;
        Ok(receipt.id)
    }
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn successful_mcp_actions_persist_audit_rows_with_expected_action_kinds() -> anyhow::Result<()>
{
    let settings =
        Settings::from_env().context("failed to load settings for audit success test")?;
    let fixture = McpAuditFixture::create(settings).await?;

    let result = async {
        let (token_id, token) = fixture
            .bearer_token(
                Some(fixture.workspace_id),
                "workspace",
                &["workspace:admin", "documents:read", "documents:write"],
                "audit-success",
            )
            .await?;
        let document_id = fixture
            .create_readable_document("audit-memory", "beacon audit memory for read and search")
            .await?;

        let _ = fixture.get_json(&token, "/v1/mcp/capabilities").await?;
        let _ = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({ "query": "beacon", "libraryIds": [fixture.library_id] }),
            )
            .await?;
        let _ = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        let _ = fixture
            .mcp_tool_call(
                &token,
                "upload_documents",
                json!({
                    "libraryId": fixture.library_id,
                    "documents": [{
                        "fileName": "audit-upload.txt",
                        "contentBase64": "QXVkaXQgdXBsb2FkIG1lbW9yeS4=",
                        "mimeType": "text/plain"
                    }]
                }),
            )
            .await?;
        let _ = fixture
            .mcp_tool_call(
                &token,
                "update_document",
                json!({
                    "libraryId": fixture.library_id,
                    "documentId": document_id,
                    "operationKind": "append",
                    "appendedText": " and append follow-up"
                }),
            )
            .await?;

        let rows = repositories::list_mcp_audit_events(
            &fixture.state.persistence.postgres,
            Some(fixture.workspace_id),
            Some(token_id),
            32,
        )
        .await
        .context("failed to list persisted audit rows")?;
        let action_kinds = rows.iter().map(|row| row.action_kind.as_str()).collect::<Vec<_>>();
        assert!(action_kinds.contains(&"capability_snapshot"));
        assert!(action_kinds.contains(&"search_documents"));
        assert!(action_kinds.contains(&"read_document"));
        assert!(action_kinds.contains(&"upload_documents"));
        assert!(action_kinds.contains(&"update_document"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn rejected_and_failed_mcp_actions_persist_sanitized_audit_rows() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for audit error test")?;
    let fixture = McpAuditFixture::create(settings).await?;

    let result = async {
        let (_, read_only_token) = fixture
            .bearer_token(
                Some(fixture.workspace_id),
                "workspace",
                &["documents:read"],
                "audit-rejected",
            )
            .await?;
        let (admin_token_id, admin_token) = fixture
            .bearer_token(
                Some(fixture.workspace_id),
                "workspace",
                &["workspace:admin", "documents:read", "documents:write"],
                "audit-failed",
            )
            .await?;

        let (rejected_headers, rejected) = fixture
            .mcp_tool_call(
                &read_only_token,
                "upload_documents",
                json!({
                    "libraryId": fixture.library_id,
                    "documents": [{
                        "fileName": "forbidden.txt",
                        "contentBase64": "Rm9yYmlkZGVuIG1lbW9yeS4=",
                        "mimeType": "text/plain"
                    }]
                }),
            )
            .await?;
        assert_eq!(rejected["result"]["isError"], json!(true));
        assert_eq!(rejected["result"]["structuredContent"]["errorKind"], json!("unauthorized"));
        let rejected_request_id = rejected_headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .context("missing rejected request id header")?;

        let broken_receipt_id = fixture.insert_broken_receipt(admin_token_id).await?;
        let (failed_headers, failed) = fixture
            .mcp_tool_call(
                &admin_token,
                "get_mutation_status",
                json!({ "receiptId": broken_receipt_id }),
            )
            .await?;
        assert_eq!(failed["result"]["isError"], json!(true));
        assert_eq!(failed["result"]["structuredContent"]["errorKind"], json!("internal"));
        let failed_request_id = failed_headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .context("missing failed request id header")?;

        let rejected_row = sqlx::query_as::<_, repositories::McpAuditEventRow>(
            "select id, request_id, token_id, token_kind, action_kind, workspace_id, library_id, document_id,
                    status, error_kind, metadata_json, created_at
             from mcp_audit_event
             where request_id = $1",
        )
        .bind(rejected_request_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to load rejected audit row")?;
        assert_eq!(rejected_row.action_kind, "upload_documents");
        assert_eq!(rejected_row.status, "rejected");
        assert_eq!(rejected_row.error_kind.as_deref(), Some("unauthorized"));
        assert!(rejected_row.workspace_id.is_none());
        assert!(rejected_row.library_id.is_none());
        assert!(rejected_row.document_id.is_none());

        let failed_row = sqlx::query_as::<_, repositories::McpAuditEventRow>(
            "select id, request_id, token_id, token_kind, action_kind, workspace_id, library_id, document_id,
                    status, error_kind, metadata_json, created_at
             from mcp_audit_event
             where request_id = $1",
        )
        .bind(failed_request_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to load failed audit row")?;
        assert_eq!(failed_row.action_kind, "get_mutation_status");
        assert_eq!(failed_row.status, "failed");
        assert_eq!(failed_row.error_kind.as_deref(), Some("internal"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn request_ids_appear_in_mcp_responses_and_persisted_audit_rows() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for audit request id test")?;
    let fixture = McpAuditFixture::create(settings).await?;

    let result = async {
        let (_, token) = fixture
            .bearer_token(
                Some(fixture.workspace_id),
                "workspace",
                &["documents:read"],
                "audit-request-id",
            )
            .await?;
        let _ = fixture
            .create_readable_document("audit-request-id-memory", "request id beacon memory")
            .await?;

        let (headers, response) = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({ "query": "beacon", "libraryIds": [fixture.library_id] }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));

        let request_id = headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .context("missing request id header")?;
        let row = sqlx::query_as::<_, repositories::McpAuditEventRow>(
            "select id, request_id, token_id, token_kind, action_kind, workspace_id, library_id, document_id,
                    status, error_kind, metadata_json, created_at
             from mcp_audit_event
             where request_id = $1",
        )
        .bind(request_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to load request-id audit row")?;
        assert_eq!(row.request_id, request_id);
        assert_eq!(row.action_kind, "search_documents");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn audit_review_is_limited_to_authorized_callers_and_workspace_scope() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for audit review test")?;
    let fixture = McpAuditFixture::create(settings).await?;

    let result = async {
        let (_, read_only_token) = fixture
            .bearer_token(
                Some(fixture.workspace_id),
                "workspace",
                &["documents:read"],
                "audit-review-denied",
            )
            .await?;
        let (_, admin_token) = fixture
            .bearer_token(
                Some(fixture.workspace_id),
                "workspace",
                &["workspace:admin", "documents:read"],
                "audit-review-admin",
            )
            .await?;

        let _ = fixture.get_json(&admin_token, "/v1/mcp/capabilities").await?;

        let (denied_status, _, denied_body) =
            fixture.get_json(&read_only_token, "/v1/mcp/audit").await?;
        assert_eq!(denied_status, StatusCode::UNAUTHORIZED);
        assert_eq!(denied_body["errorKind"], json!("unauthorized"));

        let (ok_status, _, ok_body) = fixture.get_json(&admin_token, "/v1/mcp/audit").await?;
        assert_eq!(ok_status, StatusCode::OK);
        assert!(ok_body["events"].as_array().is_some_and(|events| !events.is_empty()));

        let (foreign_status, _, foreign_body) = fixture
            .get_json(
                &admin_token,
                &format!("/v1/mcp/audit?workspaceId={}", fixture.foreign_workspace_id),
            )
            .await?;
        assert_eq!(foreign_status, StatusCode::UNAUTHORIZED);
        assert_eq!(foreign_body["errorKind"], json!("unauthorized"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
