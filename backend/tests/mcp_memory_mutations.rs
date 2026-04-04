#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::Engine as _;
use http_body_util::BodyExt;
use reqwest::Url;
use serde_json::{Value, json};
use tokio::time::{Duration, sleep};
use tower::ServiceExt;
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{self, NewMcpMutationReceipt},
    interfaces::http::{auth::hash_token, router},
};

struct McpMutationFixture {
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl McpMutationFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-mutation-test-{suffix}"),
            "MCP Mutation Test",
        )
        .await
        .context("failed to create mcp mutation workspace")?;
        let library = repositories::create_project(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-mutation-library-{suffix}"),
            "MCP Mutation Library",
            Some("mcp mutation route test fixture"),
        )
        .await
        .context("failed to create mcp mutation library")?;

        Ok(Self { state, workspace_id: workspace.id, library_id: library.id })
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        sqlx::query(
            "delete from mcp_audit_event
             where workspace_id = $1
                or token_id in (select id from api_token where workspace_id = $1)",
        )
        .bind(self.workspace_id)
        .execute(&self.state.persistence.postgres)
        .await
        .context("failed to delete mcp audit events for mutation test workspace")?;
        sqlx::query("delete from mcp_mutation_receipt where workspace_id = $1")
            .bind(self.workspace_id)
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete mcp mutation receipts for mutation test workspace")?;
        sqlx::query("delete from api_token where workspace_id = $1")
            .bind(self.workspace_id)
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete api tokens for mutation test workspace")?;
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace_id)
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete mcp mutation test workspace")?;
        Ok(())
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn bearer_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        let plaintext = format!("mcp-test-{}-{}", label, Uuid::now_v7());
        repositories::create_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            &hash_token(&plaintext),
            Some("mcp-test-token"),
            json!(scopes),
            None,
        )
        .await
        .with_context(|| format!("failed to create token for {label}"))?;
        Ok(plaintext)
    }

    async fn mcp_tool_call(
        &self,
        token: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
        let (status, response_json) = self
            .raw_mcp_request(
                token,
                json!({
                    "jsonrpc": "2.0",
                    "id": "test",
                    "method": "tools/call",
                    "params": {
                        "name": tool_name,
                        "arguments": arguments,
                    },
                })
                .to_string(),
            )
            .await
            .with_context(|| format!("MCP tool call {tool_name} failed"))?;

        if status != StatusCode::OK {
            anyhow::bail!("unexpected status {status} for tool {tool_name}");
        }

        Ok(response_json)
    }

    async fn raw_mcp_request(
        &self,
        token: &str,
        body: String,
    ) -> anyhow::Result<(StatusCode, Value)> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/mcp")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("build mcp tool call request"),
            )
            .await
            .context("raw MCP request failed")?;

        let status = response.status();

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect mcp response body")?
            .to_bytes();
        let response_json = serde_json::from_slice(&bytes).context("failed to decode mcp json")?;
        Ok((status, response_json))
    }

    async fn mutation_receipt_count(&self) -> anyhow::Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from mcp_mutation_receipt where workspace_id = $1",
        )
        .bind(self.workspace_id)
        .fetch_one(&self.state.persistence.postgres)
        .await
        .context("failed to count mcp mutation receipts")
    }

    async fn create_document_with_status(
        &self,
        external_key: &str,
        content: &str,
        status: &str,
    ) -> anyhow::Result<(Uuid, String)> {
        let document = repositories::create_document(
            &self.state.persistence.postgres,
            self.library_id,
            None,
            external_key,
            Some(external_key),
            Some("text/plain"),
            Some("mcp-readable-checksum"),
        )
        .await
        .with_context(|| format!("failed to create readable document {external_key}"))?;
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
            Some("mcp-readable-hash"),
        )
        .await
        .with_context(|| format!("failed to create readable revision for {external_key}"))?;
        repositories::activate_document_revision(
            &self.state.persistence.postgres,
            document.id,
            revision.id,
        )
        .await
        .context("failed to activate readable revision")?;
        repositories::update_document_current_revision(
            &self.state.persistence.postgres,
            document.id,
            Some(revision.id),
            "active",
            None,
            None,
        )
        .await
        .context("failed to mark readable document active")?;

        let track_id = format!("readable-track-{}", Uuid::now_v7());
        let runtime_run = repositories::create_runtime_ingestion_run(
            &self.state.persistence.postgres,
            self.library_id,
            Some(document.id),
            Some(revision.id),
            None,
            &track_id,
            &format!("{external_key}.txt"),
            "txt",
            Some("text/plain"),
            Some(i64::try_from(content.len()).unwrap_or(i64::MAX)),
            status,
            match status {
                "ready" | "ready_no_graph" => "completed",
                "failed" => "failed",
                _ => "extracting",
            },
            "initial_upload",
            json!({}),
        )
        .await
        .with_context(|| format!("failed to create runtime run for {external_key}"))?;
        if matches!(status, "ready" | "ready_no_graph" | "failed") {
            repositories::update_runtime_ingestion_run_status(
                &self.state.persistence.postgres,
                runtime_run.id,
                status,
                match status {
                    "ready" | "ready_no_graph" => "completed",
                    "failed" => "failed",
                    _ => "extracting",
                },
                Some(100),
                None,
            )
            .await
            .with_context(|| format!("failed to update runtime run status for {external_key}"))?;
        }
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
        .context("failed to persist readable extracted content")?;

        Ok((document.id, track_id))
    }

    async fn create_readable_document(
        &self,
        external_key: &str,
        content: &str,
    ) -> anyhow::Result<(Uuid, String)> {
        self.create_document_with_status(external_key, content, "ready").await
    }
}

async fn receipt_row_count_for_id(state: &AppState, receipt_id: Uuid) -> anyhow::Result<i64> {
    sqlx::query_scalar::<_, i64>("select count(*) from mcp_mutation_receipt where id = $1")
        .bind(receipt_id)
        .fetch_one(&state.persistence.postgres)
        .await
        .context("failed to count receipt row by id")
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn read_only_tokens_cannot_create_mutation_receipts_via_mcp() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp mutation test")?;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "mcp-read-only").await?;

        let upload = fixture
            .mcp_tool_call(
                &token,
                "upload_documents",
                json!({
                    "libraryId": fixture.library_id,
                    "documents": [{
                        "fileName": "memory.txt",
                        "contentBase64": "bWVtb3J5Cg==",
                        "mimeType": "text/plain"
                    }]
                }),
            )
            .await?;
        assert_eq!(upload["result"]["isError"], json!(true));
        assert_eq!(upload["result"]["structuredContent"]["errorKind"], json!("unauthorized"));

        let update = fixture
            .mcp_tool_call(
                &token,
                "update_document",
                json!({
                    "libraryId": fixture.library_id,
                    "documentId": Uuid::now_v7(),
                    "operationKind": "append",
                    "appendedText": "forbidden"
                }),
            )
            .await?;
        assert_eq!(update["result"]["isError"], json!(true));
        assert_eq!(update["result"]["structuredContent"]["errorKind"], json!("unauthorized"));

        assert_eq!(fixture.mutation_receipt_count().await?, 0);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn authorized_upload_returns_receipt_but_document_remains_unreadable_until_processing_finishes()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp upload test")?;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token =
            fixture.bearer_token(&["documents:read", "documents:write"], "mcp-upload").await?;

        let upload = fixture
            .mcp_tool_call(
                &token,
                "upload_documents",
                json!({
                    "libraryId": fixture.library_id,
                    "documents": [{
                        "fileName": "draft-memory.txt",
                        "contentBase64": "QWdlbnQgbWVtb3J5IGRyYWZ0IHRleHQu",
                        "mimeType": "text/plain",
                        "title": "Draft Memory"
                    }]
                }),
            )
            .await?;
        assert_eq!(upload["result"]["isError"], json!(false));
        let receipt = &upload["result"]["structuredContent"]["receipts"][0];
        let receipt_id: Uuid =
            serde_json::from_value(receipt["receiptId"].clone()).context("receipt id missing")?;
        let document_id: Uuid =
            serde_json::from_value(receipt["documentId"].clone()).context("document id missing")?;
        assert_eq!(receipt["operationKind"], json!("upload"));
        assert_eq!(receipt["status"], json!("accepted"));
        assert!(receipt.get("runtimeTrackingId").is_none());
        assert_eq!(receipt_row_count_for_id(&fixture.state, receipt_id).await?, 1);

        let status = fixture
            .mcp_tool_call(&token, "get_mutation_status", json!({ "receiptId": receipt_id }))
            .await?;
        assert_eq!(status["result"]["isError"], json!(false));
        assert!(matches!(
            status["result"]["structuredContent"]["status"].as_str(),
            Some("accepted" | "processing")
        ));

        let read = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(read["result"]["isError"], json!(false));
        assert_eq!(read["result"]["structuredContent"]["readabilityState"], json!("processing"));
        assert!(read["result"]["structuredContent"]["content"].is_null());

        let search = fixture
            .mcp_tool_call(&token, "search_documents", json!({ "query": "Agent memory draft" }))
            .await?;
        assert_eq!(search["result"]["isError"], json!(false));
        assert_eq!(search["result"]["structuredContent"]["hits"], json!([]));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn append_and_replace_mutations_preserve_logical_document_identity() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp mutation test")?;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token =
            fixture.bearer_token(&["documents:read", "documents:write"], "mcp-update").await?;
        let (document_id, _) = fixture
            .create_readable_document(
                "memory-anchor",
                "This memory document is ready for append and replace mutations.",
            )
            .await?;

        let append = fixture
            .mcp_tool_call(
                &token,
                "update_document",
                json!({
                    "libraryId": fixture.library_id,
                    "documentId": document_id,
                    "operationKind": "append",
                    "idempotencyKey": "append-once",
                    "appendedText": " Additional agent memory."
                }),
            )
            .await?;
        assert_eq!(append["result"]["isError"], json!(false));
        assert_eq!(append["result"]["structuredContent"]["documentId"], json!(document_id));
        assert_eq!(append["result"]["structuredContent"]["operationKind"], json!("append"));
        assert!(append["result"]["structuredContent"].get("runtimeTrackingId").is_none());

        let replace = fixture
            .mcp_tool_call(
                &token,
                "update_document",
                json!({
                    "libraryId": fixture.library_id,
                    "documentId": document_id,
                    "operationKind": "replace",
                    "idempotencyKey": "replace-once",
                    "replacementFileName": "memory-anchor-v2.txt",
                    "replacementContentBase64": "UmVwbGFjZWQgbWVtb3J5IGRvY3VtZW50Lg==",
                    "replacementMimeType": "text/plain"
                }),
            )
            .await?;
        assert_eq!(replace["result"]["isError"], json!(false));
        assert_eq!(replace["result"]["structuredContent"]["documentId"], json!(document_id));
        assert_eq!(replace["result"]["structuredContent"]["operationKind"], json!("replace"));
        assert!(replace["result"]["structuredContent"].get("runtimeTrackingId").is_none());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn readable_processing_documents_still_reject_overlapping_mutations() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp mutation test")?;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token = fixture
            .bearer_token(&["documents:read", "documents:write"], "mcp-update-early-readable")
            .await?;
        let (document_id, _) = fixture
            .create_document_with_status(
                "memory-early-readable",
                "Existing extracted memory is available before graph extraction finishes.",
                "processing",
            )
            .await?;

        let append = fixture
            .mcp_tool_call(
                &token,
                "update_document",
                json!({
                    "libraryId": fixture.library_id,
                    "documentId": document_id,
                    "operationKind": "append",
                    "idempotencyKey": "append-while-processing-but-readable",
                    "appendedText": " Additional memory after readable extraction."
                }),
            )
            .await?;
        assert_eq!(append["result"]["isError"], json!(true));
        assert_eq!(
            append["result"]["structuredContent"]["errorKind"],
            json!("conflicting_mutation")
        );
        assert!(
            append["result"]["structuredContent"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("document is still processing"))
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn repeated_upload_idempotency_reuses_the_same_receipt() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for idempotency test")?;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token =
            fixture.bearer_token(&["documents:read", "documents:write"], "mcp-idempotency").await?;

        let first = fixture
            .mcp_tool_call(
                &token,
                "upload_documents",
                json!({
                    "libraryId": fixture.library_id,
                    "idempotencyKey": "same-upload",
                    "documents": [{
                        "fileName": "dedupe.txt",
                        "contentBase64": "RGVkdXBsaWNhdGUgbWUu",
                        "mimeType": "text/plain"
                    }]
                }),
            )
            .await?;
        let second = fixture
            .mcp_tool_call(
                &token,
                "upload_documents",
                json!({
                    "libraryId": fixture.library_id,
                    "idempotencyKey": "same-upload",
                    "documents": [{
                        "fileName": "dedupe.txt",
                        "contentBase64": "RGVkdXBsaWNhdGUgbWUu",
                        "mimeType": "text/plain"
                    }]
                }),
            )
            .await?;

        let first_receipt = &first["result"]["structuredContent"]["receipts"][0];
        let second_receipt = &second["result"]["structuredContent"]["receipts"][0];
        assert_eq!(first_receipt["receiptId"], second_receipt["receiptId"]);
        assert_eq!(fixture.mutation_receipt_count().await?, 1);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn failed_but_readable_documents_can_still_accept_append_mutations() -> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for failed-readable mutation test")?;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token = fixture
            .bearer_token(&["documents:read", "documents:write"], "mcp-update-failed-readable")
            .await?;
        let (document_id, _) = fixture
            .create_document_with_status(
                "memory-failed-readable",
                "Readable memory survived a later graph projection failure.",
                "failed",
            )
            .await?;

        let append = fixture
            .mcp_tool_call(
                &token,
                "update_document",
                json!({
                    "libraryId": fixture.library_id,
                    "documentId": document_id,
                    "operationKind": "append",
                    "idempotencyKey": "append-after-graph-failure",
                    "appendedText": " Additional memory must still be accepted."
                }),
            )
            .await?;
        assert_eq!(append["result"]["isError"], json!(false));
        assert_eq!(append["result"]["structuredContent"]["documentId"], json!(document_id));
        assert_eq!(append["result"]["structuredContent"]["operationKind"], json!("append"));
        assert!(append["result"]["structuredContent"].get("runtimeTrackingId").is_none());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn mutation_status_reports_ready_when_failed_runtime_run_already_exposes_memory()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for failed-readable receipt test")?;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token = fixture
            .bearer_token(&["documents:read", "documents:write"], "mcp-receipt-failed-readable")
            .await?;
        let token_row = repositories::find_api_token_by_hash(
            &fixture.state.persistence.postgres,
            &hash_token(&token),
        )
        .await
        .context("failed to reload token for failed-readable receipt test")?
        .context("failed-readable receipt token missing")?;
        let (document_id, _track_id) = fixture
            .create_document_with_status(
                "memory-failed-receipt",
                "Readable memory survived a later graph projection failure.",
                "failed",
            )
            .await?;
        let receipt = repositories::create_mcp_mutation_receipt(
            &fixture.state.persistence.postgres,
            &NewMcpMutationReceipt {
                token_id: token_row.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: Some(document_id),
                operation_kind: "upload".to_string(),
                idempotency_key: "receipt-after-graph-failure".to_string(),
                payload_identity: Some("sha256:failed-readable".to_string()),
                status: "accepted".to_string(),
                failure_kind: None,
            },
        )
        .await
        .context("failed to create failed-readable receipt")?;

        let status = fixture
            .mcp_tool_call(&token, "get_mutation_status", json!({ "receiptId": receipt.id }))
            .await?;
        assert_eq!(status["result"]["isError"], json!(false));
        assert_eq!(status["result"]["structuredContent"]["status"], json!("ready"));
        assert_eq!(status["result"]["structuredContent"]["documentId"], json!(document_id));
        assert!(status["result"]["structuredContent"]["failureKind"].is_null());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn upload_documents_rejects_decoded_payloads_over_mcp_upload_limit() -> anyhow::Result<()> {
    let mut settings =
        Settings::from_env().context("failed to load settings for mcp mutation test")?;
    settings.upload_max_size_mb = 1;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:write"], "mcp-upload-too-large").await?;
        let oversized_body =
            base64::engine::general_purpose::STANDARD.encode(vec![b'a'; 1_200_000]);

        let response = fixture
            .mcp_tool_call(
                &token,
                "upload_documents",
                json!({
                    "libraryId": fixture.library_id,
                    "documents": [{
                        "fileName": "too-large.txt",
                        "mimeType": "text/plain",
                        "contentBase64": oversized_body,
                    }],
                }),
            )
            .await?;

        assert_eq!(response["result"]["isError"], json!(true));
        assert_eq!(
            response["result"]["structuredContent"]["errorKind"],
            json!("upload_limit_exceeded")
        );
        assert_eq!(fixture.mutation_receipt_count().await?, 0);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn replace_document_rejects_replacement_payloads_over_mcp_upload_limit() -> anyhow::Result<()>
{
    let mut settings =
        Settings::from_env().context("failed to load settings for mcp mutation test")?;
    settings.upload_max_size_mb = 1;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:write"], "mcp-replace-too-large").await?;
        let (document_id, _) = fixture
            .create_document_with_status("oversized-replace", "ready content", "ready")
            .await?;
        let oversized_body =
            base64::engine::general_purpose::STANDARD.encode(vec![b'b'; 1_200_000]);

        let response = fixture
            .mcp_tool_call(
                &token,
                "update_document",
                json!({
                    "libraryId": fixture.library_id,
                    "documentId": document_id,
                    "operationKind": "replace",
                    "replacementFileName": "replace.txt",
                    "replacementMimeType": "text/plain",
                    "replacementContentBase64": oversized_body,
                }),
            )
            .await?;

        assert_eq!(response["result"]["isError"], json!(true));
        assert_eq!(
            response["result"]["structuredContent"]["errorKind"],
            json!("upload_limit_exceeded")
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn mcp_route_rejects_oversized_request_bodies_with_structured_limit_error()
-> anyhow::Result<()> {
    let mut settings =
        Settings::from_env().context("failed to load settings for mcp mutation test")?;
    settings.upload_max_size_mb = 1;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:write"], "mcp-body-too-large").await?;
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": "body-too-large",
            "method": "tools/call",
            "params": {
                "name": "upload_documents",
                "arguments": {
                    "libraryId": fixture.library_id,
                    "documents": [{
                        "fileName": "oversized-body.txt",
                        "contentBase64": "A".repeat(3 * 1024 * 1024),
                    }],
                },
            },
        })
        .to_string();

        let (status, response) = fixture.raw_mcp_request(&token, request_body).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["error"]["code"], json!(-32600));
        assert_eq!(response["error"]["data"]["errorKind"], json!("upload_limit_exceeded"));
        assert_eq!(response["error"]["data"]["details"]["uploadLimitMb"], json!(1));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango"]
async fn mcp_upload_receipt_status_read_lifecycle_is_stable() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp mutation test")?;
    let fixture = McpMutationFixture::create(settings).await?;

    let result = async {
        let token =
            fixture.bearer_token(&["documents:read", "documents:write"], "lifecycle").await?;

        let content = "stable lifecycle content for MCP upload/read";
        let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());

        let upload = fixture
            .mcp_tool_call(
                &token,
                "upload_documents",
                json!({
                    "libraryId": fixture.library_id,
                    "documents": [{
                        "fileName": "lifecycle.txt",
                        "contentBase64": encoded,
                        "mimeType": "text/plain"
                    }]
                }),
            )
            .await?;
        assert_eq!(upload["result"]["isError"], json!(false));

        let receipt = &upload["result"]["structuredContent"]["receipts"][0];
        let receipt_id: Uuid =
            serde_json::from_value(receipt["receiptId"].clone()).context("receipt id missing")?;
        let document_id: Uuid =
            serde_json::from_value(receipt["documentId"].clone()).context("document id missing")?;

        let mut ready = false;
        for _ in 0..60 {
            let status = fixture
                .mcp_tool_call(&token, "get_mutation_status", json!({ "receiptId": receipt_id }))
                .await?;
            assert_eq!(status["result"]["isError"], json!(false));

            if let Some(status_document_id) = status["result"]["structuredContent"]
                .get("documentId")
                .and_then(|value| value.as_str())
            {
                assert_eq!(status_document_id, document_id.to_string());
            }

            let state = status["result"]["structuredContent"]["status"]
                .as_str()
                .context("mutation status missing")?;

            match state {
                "ready" => {
                    ready = true;
                    break;
                }
                "failed" => {
                    anyhow::bail!("mutation status reported failed");
                }
                _ => sleep(Duration::from_millis(500)).await,
            }
        }

        assert!(ready, "timed out waiting for receipt to reach ready status");

        let read = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(read["result"]["isError"], json!(false));
        assert_eq!(read["result"]["structuredContent"]["readabilityState"], json!("readable"));
        let returned = read["result"]["structuredContent"]["content"]
            .as_str()
            .context("read content missing")?;
        assert!(returned.contains("stable lifecycle content"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn mcp_single_page_web_ingest_submit_and_inspect_run() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp mutation test")?;
    let fixture = McpMutationFixture::create(settings).await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let token =
            fixture.bearer_token(&["documents:read", "documents:write"], "mcp-web-ingest").await?;

        let submit = fixture
            .mcp_tool_call(
                &token,
                "submit_web_ingest_run",
                json!({
                    "libraryId": fixture.library_id,
                    "seedUrl": server.url("/seed"),
                    "mode": "single_page",
                }),
            )
            .await?;
        assert_eq!(submit["result"]["isError"], json!(false));
        let receipt = &submit["result"]["structuredContent"];
        let run_id: Uuid =
            serde_json::from_value(receipt["runId"].clone()).context("run id missing")?;
        assert_eq!(receipt["mode"], json!("single_page"));
        assert_eq!(receipt["runState"], json!("completed"));
        assert_eq!(receipt["counts"]["discovered"], json!(1));
        assert_eq!(receipt["counts"]["processed"], json!(1));
        assert_eq!(receipt["failureCode"], json!(null));
        assert_eq!(receipt["cancelRequestedAt"], json!(null));

        let run =
            fixture.mcp_tool_call(&token, "get_web_ingest_run", json!({ "runId": run_id })).await?;
        assert_eq!(run["result"]["isError"], json!(false));
        assert_eq!(run["result"]["structuredContent"]["runId"], json!(run_id));
        assert_eq!(run["result"]["structuredContent"]["mode"], json!("single_page"));
        assert_eq!(run["result"]["structuredContent"]["seedUrl"], json!(server.url("/seed")));
        assert_eq!(run["result"]["structuredContent"]["counts"]["discovered"], json!(1));
        assert_eq!(run["result"]["structuredContent"]["counts"]["processed"], json!(1));
        assert_eq!(run["result"]["structuredContent"]["counts"]["failed"], json!(0));

        let pages = fixture
            .mcp_tool_call(&token, "list_web_ingest_run_pages", json!({ "runId": run_id }))
            .await?;
        assert_eq!(pages["result"]["isError"], json!(false));
        let page_items = pages["result"]["structuredContent"]["pages"]
            .as_array()
            .context("pages payload missing")?;
        assert_eq!(page_items.len(), 1);
        assert_eq!(page_items[0]["normalizedUrl"], json!(server.url("/seed")));
        assert_eq!(page_items[0]["candidateState"], json!("processed"));
        assert_eq!(page_items[0]["classificationReason"], json!("seed_accepted"));
        assert!(page_items[0]["documentId"].is_string());
        assert!(page_items[0]["resultRevisionId"].is_string());

        let cancel = fixture
            .mcp_tool_call(&token, "cancel_web_ingest_run", json!({ "runId": run_id }))
            .await?;
        assert_eq!(cancel["result"]["isError"], json!(false));
        assert_eq!(cancel["result"]["structuredContent"]["runId"], json!(run_id));
        assert_eq!(cancel["result"]["structuredContent"]["mode"], json!("single_page"));
        assert_eq!(cancel["result"]["structuredContent"]["runState"], json!("completed"));
        assert_eq!(cancel["result"]["structuredContent"]["counts"]["processed"], json!(1));
        assert_eq!(cancel["result"]["structuredContent"]["failureCode"], json!(null));
        assert_eq!(cancel["result"]["structuredContent"]["cancelRequestedAt"], json!(null));

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn mcp_recursive_web_ingest_defaults_same_host_and_lists_discovered_pages()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp mutation test")?;
    let fixture = McpMutationFixture::create(settings).await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let token = fixture
            .bearer_token(&["documents:read", "documents:write"], "mcp-web-recursive")
            .await?;
        let external_url = server.url("/child").replace("127.0.0.1", "localhost");
        let seed_url = Url::parse_with_params(
            &server.url("/recursive/seed"),
            &[("external", external_url.as_str())],
        )
        .context("failed to build recursive seed url")?
        .to_string();

        let submit = fixture
            .mcp_tool_call(
                &token,
                "submit_web_ingest_run",
                json!({
                    "libraryId": fixture.library_id,
                    "seedUrl": seed_url,
                    "mode": "recursive_crawl",
                }),
            )
            .await?;
        assert_eq!(submit["result"]["isError"], json!(false));
        let receipt = &submit["result"]["structuredContent"];
        let run_id: Uuid =
            serde_json::from_value(receipt["runId"].clone()).context("run id missing")?;
        assert_eq!(receipt["mode"], json!("recursive_crawl"));
        assert_eq!(receipt["runState"], json!("accepted"));
        assert_eq!(receipt["counts"]["discovered"], json!(1));
        assert_eq!(receipt["counts"]["eligible"], json!(1));
        assert_eq!(receipt["failureCode"], json!(null));
        assert_eq!(receipt["cancelRequestedAt"], json!(null));

        fixture
            .state
            .canonical_services
            .web_ingest
            .execute_recursive_discovery_job(&fixture.state, run_id)
            .await
            .context("failed to execute recursive discovery job for MCP coverage")?;

        let run =
            fixture.mcp_tool_call(&token, "get_web_ingest_run", json!({ "runId": run_id })).await?;
        assert_eq!(run["result"]["isError"], json!(false));
        let structured = &run["result"]["structuredContent"];
        assert_eq!(structured["runId"], json!(run_id));
        assert_eq!(structured["mode"], json!("recursive_crawl"));
        assert_eq!(structured["boundaryPolicy"], json!("same_host"));
        assert_eq!(structured["maxDepth"], json!(3));
        assert_eq!(structured["maxPages"], json!(100));
        assert_eq!(structured["runState"], json!("processing"));
        assert!(structured["counts"]["queued"].as_i64().unwrap_or_default() > 0);
        assert!(structured["counts"]["excluded"].as_i64().unwrap_or_default() >= 1);

        let pages = fixture
            .mcp_tool_call(&token, "list_web_ingest_run_pages", json!({ "runId": run_id }))
            .await?;
        assert_eq!(pages["result"]["isError"], json!(false));
        let page_items = pages["result"]["structuredContent"]["pages"]
            .as_array()
            .context("pages payload missing")?;
        assert!(page_items.len() >= 4, "expected seed plus recursive discoveries");
        assert!(page_items.iter().any(|page| {
            page["normalizedUrl"] == json!(seed_url)
                && page["candidateState"] == json!("queued")
                && page["depth"] == json!(0)
        }));
        assert!(page_items.iter().any(|page| {
            page["normalizedUrl"] == json!(server.url("/recursive/first"))
                && page["candidateState"] == json!("queued")
                && page["depth"] == json!(1)
        }));
        assert!(page_items.iter().any(|page| {
            page["hostClassification"] == json!("external")
                && page["candidateState"] == json!("excluded")
                && page["classificationReason"] == json!("outside_boundary_policy")
        }));

        let cancel = fixture
            .mcp_tool_call(&token, "cancel_web_ingest_run", json!({ "runId": run_id }))
            .await?;
        assert_eq!(cancel["result"]["isError"], json!(false));
        assert_eq!(cancel["result"]["structuredContent"]["runId"], json!(run_id));
        assert_eq!(cancel["result"]["structuredContent"]["runState"], json!("canceled"));
        assert!(cancel["result"]["structuredContent"]["cancelRequestedAt"].is_string());
        assert_eq!(cancel["result"]["structuredContent"]["failureCode"], json!(null));
        assert!(
            cancel["result"]["structuredContent"]["counts"]["canceled"]
                .as_i64()
                .unwrap_or_default()
                > 0
        );

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}
