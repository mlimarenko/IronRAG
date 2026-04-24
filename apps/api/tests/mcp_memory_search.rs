#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/iam_token_support.rs"]
mod iam_token_support;

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

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{catalog_repository, content_repository},
    interfaces::http::router,
};

struct McpSearchFixture {
    state: AppState,
    workspace_id: Uuid,
    primary_library_id: Uuid,
    primary_library_ref: String,
    secondary_library_id: Uuid,
    secondary_library_ref: String,
    foreign_workspace_id: Uuid,
    foreign_library_ref: String,
}

impl McpSearchFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();

        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-search-{suffix}"),
            "MCP Search Test",
            None,
        )
        .await
        .context("failed to create mcp search workspace")?;
        let primary_library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-search-primary-{suffix}"),
            "Primary Search Library",
            Some("primary mcp search test library"),
            None,
        )
        .await
        .context("failed to create primary search library")?;
        let secondary_library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-search-secondary-{suffix}"),
            "Secondary Search Library",
            Some("secondary mcp search test library"),
            None,
        )
        .await
        .context("failed to create secondary search library")?;

        let foreign_workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-search-foreign-{suffix}"),
            "MCP Search Foreign Test",
            None,
        )
        .await
        .context("failed to create foreign search workspace")?;
        let foreign_library = catalog_repository::create_library(
            &state.persistence.postgres,
            foreign_workspace.id,
            &format!("mcp-search-foreign-library-{suffix}"),
            "Foreign Search Library",
            Some("foreign mcp search test library"),
            None,
        )
        .await
        .context("failed to create foreign search library")?;

        Ok(Self {
            state,
            workspace_id: workspace.id,
            primary_library_id: primary_library.id,
            primary_library_ref: format!("{}/{}", workspace.slug, primary_library.slug),
            secondary_library_id: secondary_library.id,
            secondary_library_ref: format!("{}/{}", workspace.slug, secondary_library.slug),
            foreign_workspace_id: foreign_workspace.id,
            foreign_library_ref: format!("{}/{}", foreign_workspace.slug, foreign_library.slug),
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
        iam_token_support::mint_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            scopes,
        )
        .await
        .map(|token| token.plaintext)
        .with_context(|| format!("failed to create mcp search token for {label}"))
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
        let document = content_repository::create_document(
            &self.state.persistence.postgres,
            &content_repository::NewContentDocument {
                workspace_id: self.workspace_id,
                library_id,
                external_key,
                document_state: "active",
                created_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to create search document {external_key}"))?;
        let revision = content_repository::create_revision(
            &self.state.persistence.postgres,
            &content_repository::NewContentRevision {
                document_id: document.id,
                workspace_id: self.workspace_id,
                library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "initial_upload",
                checksum: "mcp-search-revision",
                mime_type: "text/plain",
                byte_size: 512,
                title: Some(external_key),
                language_code: None,
                source_uri: Some(&format!("{external_key}.txt")),
                storage_key: None,
                created_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to create search revision for {external_key}"))?;
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
        .context("failed to upsert search document head")?;

        let mut start_offset = 0i32;
        for (ordinal, chunk) in matching_chunks.iter().enumerate() {
            let chunk_length = i32::try_from(chunk.chars().count()).unwrap_or(i32::MAX);
            let end_offset = start_offset.saturating_add(chunk_length);
            content_repository::create_chunk(
                &self.state.persistence.postgres,
                &content_repository::NewContentChunk {
                    revision_id: revision.id,
                    chunk_index: i32::try_from(ordinal).unwrap_or(i32::MAX),
                    start_offset,
                    end_offset,
                    token_count: Some(
                        i32::try_from(chunk.split_whitespace().count()).unwrap_or(i32::MAX),
                    ),
                    normalized_text: chunk,
                    text_checksum: "mcp-search-chunk",
                },
            )
            .await
            .with_context(|| format!("failed to create chunk {ordinal} for {external_key}"))?;
            start_offset = end_offset;
        }

        let _ = (status, extracted_text, error_message);

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
                    "libraries": [fixture.primary_library_ref, fixture.secondary_library_ref],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));
        assert_eq!(
            response["result"]["structuredContent"]["libraries"],
            json!([fixture.primary_library_ref, fixture.secondary_library_ref])
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
            response["result"]["structuredContent"]["libraries"],
            json!([fixture.primary_library_ref, fixture.secondary_library_ref])
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
                    "libraries": [fixture.primary_library_ref],
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
                    "libraries": [fixture.primary_library_ref.clone()],
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
                    "libraries": [fixture.primary_library_ref.clone()],
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
                    "libraries": [
                        fixture.primary_library_ref.clone(),
                        fixture.foreign_library_ref.clone(),
                    ],
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

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn search_documents_degrades_to_lexical_hits_when_vector_path_is_unavailable()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp fallback search test")?;
    let fixture = McpSearchFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-fallback").await?;
        let _ = fixture
            .create_document_state(
                fixture.primary_library_id,
                "fallback-beacon",
                "ready",
                Some("fallback-anchor remains searchable through lexical-only path"),
                &["fallback-anchor remains searchable through lexical-only path"],
                None,
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "fallback-anchor",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(false));
        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("fallback search hits missing")?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["readabilityState"], json!("readable"));
        assert!(
            hits[0]["excerpt"].as_str().is_some_and(|excerpt| excerpt.contains("fallback-anchor"))
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango"]
async fn search_documents_returns_structured_content_with_stable_envelope() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp search test")?;
    let fixture = McpSearchFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-envelope").await?;
        let _ = fixture
            .create_document_state(
                fixture.primary_library_id,
                "envelope-beacon",
                "ready",
                Some("envelope-anchor text is searchable"),
                &["envelope-anchor text"],
                None,
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "envelope-anchor",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 3
                }),
            )
            .await?;

        let result_value = response.get("result").context("missing result field")?;
        assert_eq!(result_value["isError"], json!(false));

        let has_structured_content =
            result_value.get("structuredContent").is_some_and(|value| !value.is_null());
        let has_content = result_value.get("content").is_some_and(|value| !value.is_null());
        assert!(has_structured_content || has_content);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
