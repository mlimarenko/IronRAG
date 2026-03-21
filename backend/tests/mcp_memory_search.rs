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

struct McpSearchFixture {
    state: AppState,
    workspace_id: Uuid,
    primary_library_id: Uuid,
    secondary_library_id: Uuid,
    foreign_workspace_id: Uuid,
    foreign_library_id: Uuid,
}

impl McpSearchFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();

        let workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-search-{suffix}"),
            "MCP Search Test",
        )
        .await
        .context("failed to create mcp search workspace")?;
        let primary_library = repositories::create_project(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-search-primary-{suffix}"),
            "Primary Search Library",
            Some("primary mcp search test library"),
        )
        .await
        .context("failed to create primary search library")?;
        let secondary_library = repositories::create_project(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-search-secondary-{suffix}"),
            "Secondary Search Library",
            Some("secondary mcp search test library"),
        )
        .await
        .context("failed to create secondary search library")?;

        let foreign_workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-search-foreign-{suffix}"),
            "MCP Search Foreign Test",
        )
        .await
        .context("failed to create foreign search workspace")?;
        let foreign_library = repositories::create_project(
            &state.persistence.postgres,
            foreign_workspace.id,
            &format!("mcp-search-foreign-library-{suffix}"),
            "Foreign Search Library",
            Some("foreign mcp search test library"),
        )
        .await
        .context("failed to create foreign search library")?;

        Ok(Self {
            state,
            workspace_id: workspace.id,
            primary_library_id: primary_library.id,
            secondary_library_id: secondary_library.id,
            foreign_workspace_id: foreign_workspace.id,
            foreign_library_id: foreign_library.id,
        })
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = any($1)")
            .bind([self.workspace_id, self.foreign_workspace_id].as_slice())
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete mcp search test workspaces")?;
        Ok(())
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn bearer_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        let plaintext = format!("mcp-search-{}-{}", label, Uuid::now_v7());
        repositories::create_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            &hash_token(&plaintext),
            Some("mcp-search-token"),
            json!(scopes),
            None,
        )
        .await
        .with_context(|| format!("failed to create mcp search token for {label}"))?;
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
                            "id": "search-test",
                            "method": "tools/call",
                            "params": {
                                "name": tool_name,
                                "arguments": arguments,
                            },
                        })
                        .to_string(),
                    ))
                    .expect("build mcp search request"),
            )
            .await
            .with_context(|| format!("MCP search tool call {tool_name} failed"))?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for tool {tool_name}", response.status());
        }

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect mcp search response body")?
            .to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode mcp search response json")
    }

    async fn create_document_state(
        &self,
        library_id: Uuid,
        external_key: &str,
        status: &str,
        extracted_text: Option<&str>,
        matching_chunks: &[&str],
        error_message: Option<&str>,
    ) -> anyhow::Result<(Uuid, Uuid)> {
        let document = repositories::create_document(
            &self.state.persistence.postgres,
            library_id,
            None,
            external_key,
            Some(external_key),
            Some("text/plain"),
            Some("mcp-search-checksum"),
        )
        .await
        .with_context(|| format!("failed to create search document {external_key}"))?;
        let revision = repositories::create_document_revision(
            &self.state.persistence.postgres,
            document.id,
            1,
            "initial_upload",
            None,
            &format!("{external_key}.txt"),
            Some("text/plain"),
            Some(512),
            None,
            Some("mcp-search-revision"),
        )
        .await
        .with_context(|| format!("failed to create search revision for {external_key}"))?;
        repositories::activate_document_revision(
            &self.state.persistence.postgres,
            document.id,
            revision.id,
        )
        .await
        .context("failed to activate search revision")?;
        repositories::update_document_current_revision(
            &self.state.persistence.postgres,
            document.id,
            Some(revision.id),
            "active",
            None,
            None,
        )
        .await
        .context("failed to update search document current revision")?;

        for (ordinal, chunk) in matching_chunks.iter().enumerate() {
            repositories::create_chunk(
                &self.state.persistence.postgres,
                document.id,
                library_id,
                i32::try_from(ordinal).unwrap_or(i32::MAX),
                chunk,
                Some(i32::try_from(chunk.split_whitespace().count()).unwrap_or(i32::MAX)),
                json!({ "source": "mcp_search_test" }),
            )
            .await
            .with_context(|| format!("failed to create chunk {ordinal} for {external_key}"))?;
        }

        let runtime_run = repositories::create_runtime_ingestion_run(
            &self.state.persistence.postgres,
            library_id,
            Some(document.id),
            Some(revision.id),
            None,
            &format!("search-track-{external_key}-{}", Uuid::now_v7()),
            &format!("{external_key}.txt"),
            "txt",
            Some("text/plain"),
            Some(512),
            status,
            match status {
                "ready" | "ready_no_graph" => "completed",
                "failed" => "failed",
                _ => "extracting",
            },
            "mcp_search_fixture",
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
            .with_context(|| format!("failed to upsert extracted content for {external_key}"))?;
        }

        Ok((document.id, revision.id))
    }
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn authorized_search_spans_multiple_libraries_and_returns_stable_scope_ids()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp search test")?;
    let fixture = McpSearchFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-multi").await?;
        let (primary_document_id, primary_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "primary-beacon",
                "ready",
                Some("beacon-signal alpha beacon-signal beta beacon-signal gamma"),
                &["beacon-signal alpha", "beacon-signal beta", "beacon-signal gamma"],
                None,
            )
            .await?;
        let (secondary_document_id, secondary_revision_id) = fixture
            .create_document_state(
                fixture.secondary_library_id,
                "secondary-beacon",
                "ready",
                Some("beacon-signal delta beacon-signal epsilon"),
                &["beacon-signal delta", "beacon-signal epsilon"],
                None,
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "beacon-signal",
                    "libraryIds": [fixture.primary_library_id, fixture.secondary_library_id],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));
        assert_eq!(
            response["result"]["structuredContent"]["libraryIds"],
            json!([fixture.primary_library_id, fixture.secondary_library_id])
        );

        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("search hits missing")?;
        assert_eq!(hits.len(), 2);

        assert_eq!(hits[0]["documentId"], json!(primary_document_id));
        assert_eq!(hits[0]["logicalDocumentId"], json!(primary_document_id));
        assert_eq!(hits[0]["libraryId"], json!(fixture.primary_library_id));
        assert_eq!(hits[0]["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(hits[0]["latestRevisionId"], json!(primary_revision_id));
        assert_eq!(hits[0]["readabilityState"], json!("readable"));
        assert!(
            hits[0]["excerpt"].as_str().is_some_and(|excerpt| excerpt.contains("beacon-signal"))
        );

        assert_eq!(hits[1]["documentId"], json!(secondary_document_id));
        assert_eq!(hits[1]["logicalDocumentId"], json!(secondary_document_id));
        assert_eq!(hits[1]["libraryId"], json!(fixture.secondary_library_id));
        assert_eq!(hits[1]["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(hits[1]["latestRevisionId"], json!(secondary_revision_id));
        assert_eq!(hits[1]["readabilityState"], json!("readable"));
        assert!(
            hits[1]["excerpt"].as_str().is_some_and(|excerpt| excerpt.contains("beacon-signal"))
        );

        let first_score = hits[0]["score"].as_f64().context("first score missing")?;
        let second_score = hits[1]["score"].as_f64().context("second score missing")?;
        assert!(first_score > second_score);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn empty_search_results_return_explicit_no_match_payload() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for empty search test")?;
    let fixture = McpSearchFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-empty").await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "string-that-does-not-exist",
                    "limit": 3
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));
        assert_eq!(
            response["result"]["structuredContent"]["query"],
            json!("string-that-does-not-exist")
        );
        assert_eq!(response["result"]["structuredContent"]["hits"], json!([]));
        assert_eq!(
            response["result"]["structuredContent"]["libraryIds"],
            json!([fixture.primary_library_id, fixture.secondary_library_id])
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn processing_and_failed_documents_surface_honest_readability_metadata_in_hits()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for search state test")?;
    let fixture = McpSearchFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-states").await?;
        let (processing_document_id, _) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "processing-beacon",
                "processing",
                None,
                &["status-signal pending work"],
                None,
            )
            .await?;
        let (failed_document_id, _) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "failed-beacon",
                "failed",
                None,
                &["status-signal failed once", "status-signal failed twice"],
                Some("extractor timeout"),
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "status-signal",
                    "libraryIds": [fixture.primary_library_id],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));

        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("stateful search hits missing")?;
        assert_eq!(hits.len(), 2);

        let processing_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(processing_document_id))
            .context("processing search hit missing")?;
        assert_eq!(processing_hit["readabilityState"], json!("processing"));
        assert!(processing_hit["excerpt"].is_null());
        assert!(processing_hit["excerptStartOffset"].is_null());
        assert!(processing_hit["excerptEndOffset"].is_null());
        assert_eq!(processing_hit["statusReason"], json!("document is still being processed"));

        let failed_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(failed_document_id))
            .context("failed search hit missing")?;
        assert_eq!(failed_hit["readabilityState"], json!("failed"));
        assert!(failed_hit["excerpt"].is_null());
        assert!(failed_hit["excerptStartOffset"].is_null());
        assert!(failed_hit["excerptEndOffset"].is_null());
        assert_eq!(failed_hit["statusReason"], json!("extractor timeout"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn processing_documents_with_extracted_text_surface_as_readable_hits() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for search state test")?;
    let fixture = McpSearchFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-early-readable").await?;
        let (document_id, revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "processing-readable-beacon",
                "processing",
                Some(
                    "status-signal extracted memory is already readable before graph extraction finishes",
                ),
                &["status-signal extracted memory is already readable"],
                None,
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "status-signal",
                    "libraryIds": [fixture.primary_library_id],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));

        let hit = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("search hits missing")?
            .iter()
            .find(|item| item["documentId"] == json!(document_id))
            .context("processing readable hit missing")?;
        assert_eq!(hit["latestRevisionId"], json!(revision_id));
        assert_eq!(hit["readabilityState"], json!("readable"));
        assert!(hit["statusReason"].is_null());
        assert!(hit["excerpt"].as_str().is_some_and(|excerpt| excerpt.contains("status-signal")));
        assert!(hit["excerptStartOffset"].as_i64().is_some());
        assert!(hit["excerptEndOffset"].as_i64().is_some());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn extracted_text_matches_are_searchable_without_chunk_rows_and_after_graph_failure()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for direct text search test")?;
    let fixture = McpSearchFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-direct-text").await?;
        let (ready_document_id, ready_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "direct-readable-beacon",
                "ready",
                Some("search-direct-anchor is present in extracted text before chunk indexing"),
                &[],
                None,
            )
            .await?;
        let (failed_document_id, failed_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "failed-readable-beacon",
                "failed",
                Some("search-direct-anchor also survives a later graph projection failure"),
                &[],
                Some("failed to refresh the canonical graph view"),
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "search-direct-anchor",
                    "libraryIds": [fixture.primary_library_id],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));

        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("direct text search hits missing")?;
        assert_eq!(hits.len(), 2);

        let ready_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(ready_document_id))
            .context("ready direct-text hit missing")?;
        assert_eq!(ready_hit["latestRevisionId"], json!(ready_revision_id));
        assert_eq!(ready_hit["readabilityState"], json!("readable"));
        assert!(
            ready_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("search-direct-anchor"))
        );

        let failed_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(failed_document_id))
            .context("failed direct-text hit missing")?;
        assert_eq!(failed_hit["latestRevisionId"], json!(failed_revision_id));
        assert_eq!(failed_hit["readabilityState"], json!("readable"));
        assert!(failed_hit["statusReason"].is_null());
        assert!(
            failed_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("search-direct-anchor"))
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn inaccessible_library_filters_reject_search_instead_of_returning_partial_results()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for search auth test")?;
    let fixture = McpSearchFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-authz").await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "beacon-signal",
                    "libraryIds": [fixture.primary_library_id, fixture.foreign_library_id],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(true));
        assert_eq!(response["result"]["structuredContent"]["errorKind"], json!("unauthorized"));
        assert_eq!(response["result"]["structuredContent"]["message"], json!("unauthorized"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
