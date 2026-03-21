use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories,
    interfaces::http::{auth::hash_token, router},
};

struct McpReadFixture {
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl McpReadFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-read-{suffix}"),
            "MCP Read Test",
        )
        .await
        .context("failed to create mcp read workspace")?;
        let library = repositories::create_project(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-read-library-{suffix}"),
            "MCP Read Library",
            Some("mcp read test library"),
        )
        .await
        .context("failed to create mcp read library")?;

        Ok(Self { state, workspace_id: workspace.id, library_id: library.id })
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

    async fn bearer_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        let plaintext = format!("mcp-read-{}-{}", label, Uuid::now_v7());
        repositories::create_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            &hash_token(&plaintext),
            Some("mcp-read-token"),
            json!(scopes),
            None,
        )
        .await
        .with_context(|| format!("failed to create mcp read token for {label}"))?;
        Ok(plaintext)
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
        let document = repositories::create_document(
            &self.state.persistence.postgres,
            self.library_id,
            None,
            external_key,
            Some(external_key),
            Some("text/plain"),
            Some("mcp-read-checksum"),
        )
        .await
        .with_context(|| format!("failed to create read document {external_key}"))?;
        let revision = repositories::create_document_revision(
            &self.state.persistence.postgres,
            document.id,
            1,
            "initial_upload",
            None,
            &format!("{external_key}.txt"),
            Some("text/plain"),
            Some(i64::try_from(extracted_text.unwrap_or_default().len()).unwrap_or(i64::MAX)),
            None,
            Some("mcp-read-revision"),
        )
        .await
        .with_context(|| format!("failed to create read revision for {external_key}"))?;
        repositories::activate_document_revision(
            &self.state.persistence.postgres,
            document.id,
            revision.id,
        )
        .await
        .context("failed to activate read revision")?;
        repositories::update_document_current_revision(
            &self.state.persistence.postgres,
            document.id,
            Some(revision.id),
            "active",
            None,
            None,
        )
        .await
        .context("failed to update read document current revision")?;

        let runtime_run = repositories::create_runtime_ingestion_run(
            &self.state.persistence.postgres,
            self.library_id,
            Some(document.id),
            Some(revision.id),
            None,
            &format!("read-track-{external_key}-{}", Uuid::now_v7()),
            &format!("{external_key}.txt"),
            "txt",
            Some("text/plain"),
            Some(i64::try_from(extracted_text.unwrap_or_default().len()).unwrap_or(i64::MAX)),
            status,
            match status {
                "ready" | "ready_no_graph" => "completed",
                "failed" => "failed",
                _ => "extracting",
            },
            "mcp_read_fixture",
            json!({}),
        )
        .await
        .with_context(|| format!("failed to create runtime run for {external_key}"))?;

        if matches!(status, "ready" | "ready_no_graph" | "failed") || error_message.is_some() {
            repositories::update_runtime_ingestion_run_status(
                &self.state.persistence.postgres,
                runtime_run.id,
                status,
                match status {
                    "ready" | "ready_no_graph" => "completed",
                    "failed" => "failed",
                    _ => "extracting",
                },
                Some(if matches!(status, "ready" | "ready_no_graph" | "failed") {
                    100
                } else {
                    50
                }),
                error_message,
            )
            .await
            .with_context(|| format!("failed to update runtime run status for {external_key}"))?;
        }

        if let Some(extracted_text) = extracted_text {
            repositories::upsert_runtime_extracted_content(
                &self.state.persistence.postgres,
                runtime_run.id,
                Some(document.id),
                "normalized_text",
                Some(extracted_text),
                None,
                Some(i32::try_from(extracted_text.chars().count()).unwrap_or(i32::MAX)),
                json!([]),
                json!({}),
                None,
                None,
                None,
            )
            .await
            .with_context(|| {
                format!("failed to upsert read extracted content for {external_key}")
            })?;
        }

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
