use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::Engine as _;
use http_body_util::BodyExt;
use serde_json::{Value, json};
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
        assert!(receipt["runtimeTrackingId"].is_string());
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
        let append_track_id = append["result"]["structuredContent"]["runtimeTrackingId"]
            .as_str()
            .context("append runtime tracking id missing")?;
        let append_run = repositories::get_runtime_ingestion_run_by_track_id(
            &fixture.state.persistence.postgres,
            append_track_id,
        )
        .await
        .context("failed to reload append runtime run")?
        .context("append runtime run missing")?;
        assert_eq!(append_run.document_id, Some(document_id));

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
        let replace_track_id = replace["result"]["structuredContent"]["runtimeTrackingId"]
            .as_str()
            .context("replace runtime tracking id missing")?;
        let replace_run = repositories::get_runtime_ingestion_run_by_track_id(
            &fixture.state.persistence.postgres,
            replace_track_id,
        )
        .await
        .context("failed to reload replace runtime run")?
        .context("replace runtime run missing")?;
        assert_eq!(replace_run.document_id, Some(document_id));

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
        assert!(append["result"]["structuredContent"]["runtimeTrackingId"].is_string());

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
        let (document_id, track_id) = fixture
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
                runtime_tracking_id: Some(track_id),
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
