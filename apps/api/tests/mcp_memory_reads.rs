#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/iam_token_support.rs"]
mod iam_token_support;
#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::{sync::broadcast, time};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{catalog_repository, content_repository},
    interfaces::http::router,
    services::ingest::worker,
};

struct McpReadFixture {
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    library_ref: String,
}

impl McpReadFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-read-{suffix}"),
            "MCP Read Test",
            None,
        )
        .await
        .context("failed to create mcp read workspace")?;
        let library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-read-library-{suffix}"),
            "MCP Read Library",
            Some("mcp read test library"),
            None,
        )
        .await
        .context("failed to create mcp read library")?;

        Ok(Self {
            state,
            workspace_id: workspace.id,
            library_id: library.id,
            library_ref: format!("{}/{}", workspace.slug, library.slug),
        })
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace_id)
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete mcp read test workspace")?;
        Ok(())
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn bearer_token(&self, _scopes: &[&str], label: &str) -> anyhow::Result<String> {
        iam_token_support::mint_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            _scopes,
        )
        .await
        .map(|token| token.plaintext)
        .with_context(|| format!("failed to create mcp read token for {label}"))
    }

    async fn mcp_tool_call(
        &self,
        token: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
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
                            "id": "read-test",
                            "method": "tools/call",
                            "params": {
                                "name": tool_name,
                                "arguments": arguments,
                            },
                        })
                        .to_string(),
                    ))
                    .expect("build mcp read request"),
            )
            .await
            .with_context(|| format!("MCP read tool call {tool_name} failed"))?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for tool {tool_name}", response.status());
        }

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect mcp read response body")?
            .to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode mcp read response json")
    }

    async fn create_document_state(
        &self,
        external_key: &str,
        status: &str,
        extracted_text: Option<&str>,
        error_message: Option<&str>,
    ) -> anyhow::Result<(Uuid, Uuid)> {
        let document = content_repository::create_document(
            &self.state.persistence.postgres,
            &content_repository::NewContentDocument {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                external_key,
                document_state: "active",
                created_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to create read document {external_key}"))?;
        let revision = content_repository::create_revision(
            &self.state.persistence.postgres,
            &content_repository::NewContentRevision {
                document_id: document.id,
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "initial_upload",
                checksum: "mcp-read-revision",
                mime_type: "text/plain",
                byte_size: i64::try_from(extracted_text.unwrap_or_default().len())
                    .unwrap_or(i64::MAX),
                title: Some(external_key),
                language_code: None,
                source_uri: Some(&format!("{external_key}.txt")),
                storage_key: None,
                created_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to create read revision for {external_key}"))?;
        content_repository::upsert_document_head(
            &self.state.persistence.postgres,
            &content_repository::NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: Some(revision.id),
                readable_revision_id: Some(revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to upsert read document head")?;

        let _ = (status, extracted_text, error_message);

        Ok((document.id, revision.id))
    }
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn full_document_reads_return_complete_content_and_stable_revision_identity()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for full read test")?;
    let fixture = McpReadFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-full").await?;
        let content = "alpha beta gamma delta".to_string();
        let (document_id, revision_id) =
            fixture.create_document_state("full-read", "ready", Some(&content), None).await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));
        let payload = &response["result"]["structuredContent"];
        assert_eq!(payload["documentId"], json!(document_id));
        assert_eq!(payload["libraryId"], json!(fixture.library_id));
        assert_eq!(payload["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(payload["latestRevisionId"], json!(revision_id));
        assert_eq!(payload["readMode"], json!("full"));
        assert_eq!(payload["readabilityState"], json!("readable"));
        assert_eq!(payload["content"], json!(content));
        assert_eq!(payload["sliceStartOffset"], json!(0));
        assert_eq!(payload["sliceEndOffset"], json!(content.chars().count()));
        assert_eq!(payload["totalContentLength"], json!(content.chars().count()));
        assert_eq!(payload["hasMore"], json!(false));
        assert!(payload["continuationToken"].is_null());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn excerpt_reads_respect_requested_window_and_offsets() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for excerpt read test")?;
    let fixture = McpReadFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-excerpt").await?;
        let content = "abcdefghijklmnopqrstuvwxyz".to_string();
        let (document_id, revision_id) =
            fixture.create_document_state("excerpt-read", "ready", Some(&content), None).await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({
                    "documentId": document_id,
                    "mode": "excerpt",
                    "startOffset": 5,
                    "length": 8
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));
        let payload = &response["result"]["structuredContent"];
        assert_eq!(payload["latestRevisionId"], json!(revision_id));
        assert_eq!(payload["readMode"], json!("excerpt"));
        assert_eq!(payload["content"], json!("fghijklm"));
        assert_eq!(payload["sliceStartOffset"], json!(5));
        assert_eq!(payload["sliceEndOffset"], json!(13));
        assert_eq!(payload["totalContentLength"], json!(26));
        assert_eq!(payload["hasMore"], json!(true));
        assert!(payload["continuationToken"].is_string());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn continuation_reads_reconstruct_large_documents_without_gaps_or_duplicates()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for continuation read test")?;
    let fixture = McpReadFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-continuation").await?;
        let content = (0..12).map(|index| format!("segment-{index}|")).collect::<String>();
        let (document_id, _) = fixture
            .create_document_state("continuation-read", "ready", Some(&content), None)
            .await?;

        let mut reconstructed = String::new();
        let mut next_arguments = json!({
            "documentId": document_id,
            "mode": "full",
            "length": 9
        });
        let mut expected_start_offset = 0usize;

        for _ in 0..32 {
            let response = fixture.mcp_tool_call(&token, "read_document", next_arguments).await?;
            assert_eq!(response["result"]["isError"], json!(false));
            let payload = &response["result"]["structuredContent"];
            assert_eq!(payload["readMode"], json!("full"));

            let slice_start_offset = usize::try_from(
                payload["sliceStartOffset"].as_u64().context("sliceStartOffset missing")?,
            )
            .context("sliceStartOffset overflow")?;
            let slice_end_offset = usize::try_from(
                payload["sliceEndOffset"].as_u64().context("sliceEndOffset missing")?,
            )
            .context("sliceEndOffset overflow")?;
            assert_eq!(slice_start_offset, expected_start_offset);

            let slice = payload["content"].as_str().context("content slice missing")?;
            reconstructed.push_str(slice);
            expected_start_offset = slice_end_offset;

            if payload["hasMore"] == json!(true) {
                let continuation_token =
                    payload["continuationToken"].as_str().context("continuation token missing")?;
                next_arguments = json!({ "continuationToken": continuation_token });
                continue;
            }

            assert!(payload["continuationToken"].is_null());
            break;
        }

        assert_eq!(reconstructed, content);
        assert_eq!(expected_start_offset, content.chars().count());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn invalid_continuation_tokens_return_explicit_mcp_error_kind() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for invalid continuation test")?;
    let fixture = McpReadFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-invalid-token").await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "continuationToken": "this-is-not-a-valid-continuation-token" }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(true));
        assert_eq!(
            response["result"]["structuredContent"]["errorKind"],
            json!("invalid_continuation_token")
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn unreadable_documents_return_honest_processing_failed_and_unavailable_reasons()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for unreadable read test")?;
    let fixture = McpReadFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-unavailable").await?;
        let (processing_document_id, _) =
            fixture.create_document_state("processing-read", "processing", None, None).await?;
        let (failed_document_id, _) = fixture
            .create_document_state("failed-read", "failed", None, Some("extractor timeout"))
            .await?;
        let (unavailable_document_id, _) =
            fixture.create_document_state("unavailable-read", "ready_no_graph", None, None).await?;

        let processing = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": processing_document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(processing["result"]["isError"], json!(false));
        assert_eq!(
            processing["result"]["structuredContent"]["readabilityState"],
            json!("processing")
        );
        assert_eq!(
            processing["result"]["structuredContent"]["statusReason"],
            json!("document is still being processed")
        );
        assert!(processing["result"]["structuredContent"]["content"].is_null());
        assert!(processing["result"]["structuredContent"]["totalContentLength"].is_null());
        assert_eq!(processing["result"]["structuredContent"]["hasMore"], json!(false));

        let failed = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": failed_document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(failed["result"]["isError"], json!(false));
        assert_eq!(failed["result"]["structuredContent"]["readabilityState"], json!("failed"));
        assert_eq!(
            failed["result"]["structuredContent"]["statusReason"],
            json!("extractor timeout")
        );
        assert!(failed["result"]["structuredContent"]["content"].is_null());
        assert!(failed["result"]["structuredContent"]["totalContentLength"].is_null());
        assert_eq!(failed["result"]["structuredContent"]["hasMore"], json!(false));

        let unavailable = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": unavailable_document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(unavailable["result"]["isError"], json!(false));
        assert_eq!(
            unavailable["result"]["structuredContent"]["readabilityState"],
            json!("unavailable")
        );
        assert_eq!(
            unavailable["result"]["structuredContent"]["statusReason"],
            json!("document finished without normalized extracted text")
        );
        assert!(unavailable["result"]["structuredContent"]["content"].is_null());
        assert!(unavailable["result"]["structuredContent"]["totalContentLength"].is_null());
        assert_eq!(unavailable["result"]["structuredContent"]["hasMore"], json!(false));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn processing_documents_with_extracted_text_are_readable_before_terminal_ready_status()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for early readable read test")?;
    let fixture = McpReadFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-early-readable").await?;
        let content =
            "Early readable extracted text is available while graph extraction continues.";
        let (document_id, revision_id) = fixture
            .create_document_state("early-readable-read", "processing", Some(content), None)
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));
        assert_eq!(response["result"]["structuredContent"]["documentId"], json!(document_id));
        assert_eq!(response["result"]["structuredContent"]["latestRevisionId"], json!(revision_id));
        assert_eq!(response["result"]["structuredContent"]["readabilityState"], json!("readable"));
        assert!(response["result"]["structuredContent"]["statusReason"].is_null());
        assert_eq!(response["result"]["structuredContent"]["content"], json!(content));
        assert_eq!(
            response["result"]["structuredContent"]["totalContentLength"],
            json!(content.chars().count())
        );
        assert_eq!(response["result"]["structuredContent"]["hasMore"], json!(false));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn failed_documents_with_extracted_text_remain_readable_for_memory_reads()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for failed-readable read test")?;
    let fixture = McpReadFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-failed-readable").await?;
        let content =
            "Graph projection failed later, but this extracted memory must remain readable.";
        let (document_id, revision_id) = fixture
            .create_document_state(
                "failed-readable-read",
                "failed",
                Some(content),
                Some("failed to refresh the canonical graph view"),
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));
        assert_eq!(response["result"]["structuredContent"]["documentId"], json!(document_id));
        assert_eq!(response["result"]["structuredContent"]["latestRevisionId"], json!(revision_id));
        assert_eq!(response["result"]["structuredContent"]["readabilityState"], json!("readable"));
        assert!(response["result"]["structuredContent"]["statusReason"].is_null());
        assert_eq!(response["result"]["structuredContent"]["content"], json!(content));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn web_ingest_documents_are_readable_through_mcp_read_document() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web-ingest read test")?;
    let fixture = McpReadFixture::create(settings).await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let token =
            fixture.bearer_token(&["documents:read", "documents:write"], "read-web-ingest").await?;

        let submit = fixture
            .mcp_tool_call(
                &token,
                "submit_web_ingest_run",
                json!({
                    "library": fixture.library_ref.clone(),
                    "seedUrl": server.url("/seed"),
                    "mode": "single_page",
                }),
            )
            .await?;
        assert_eq!(submit["result"]["isError"], json!(false));
        assert_eq!(submit["result"]["structuredContent"]["runState"], json!("processing"));

        let run_id: Uuid =
            serde_json::from_value(submit["result"]["structuredContent"]["runId"].clone())
                .context("run id missing")?;
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);
        let deadline = time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let run = fixture
                .state
                .canonical_services
                .web_ingest
                .get_run(&fixture.state, run_id)
                .await
                .context("failed to poll MCP web ingest run")?;
            if run.run_state == "completed" {
                break;
            }
            if time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for MCP web ingest run {run_id} to complete");
            }
            time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let pages = fixture
            .mcp_tool_call(&token, "list_web_ingest_run_pages", json!({ "runId": run_id }))
            .await?;
        let page_items = pages["result"]["structuredContent"]["pages"]
            .as_array()
            .context("pages payload missing")?;
        assert_eq!(page_items.len(), 1);
        let document_id: Uuid = serde_json::from_value(page_items[0]["documentId"].clone())
            .context("document id missing")?;
        assert_eq!(page_items[0]["candidateState"], json!("processed"));
        assert_eq!(page_items[0]["classificationReason"], json!("seed_accepted"));

        let read = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        assert_eq!(read["result"]["isError"], json!(false));
        assert_eq!(read["result"]["structuredContent"]["documentId"], json!(document_id));
        assert_eq!(read["result"]["structuredContent"]["readabilityState"], json!("readable"));
        assert_eq!(read["result"]["structuredContent"]["documentTitle"], json!("Seed Page"));
        let content = read["result"]["structuredContent"]["content"]
            .as_str()
            .context("web-ingest read content missing")?;
        assert!(content.contains("Canonical single-page ingest should keep only this page"));

        let _ = shutdown_tx.send(());
        let _ = time::timeout(std::time::Duration::from_secs(5), worker_handle).await;

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}
