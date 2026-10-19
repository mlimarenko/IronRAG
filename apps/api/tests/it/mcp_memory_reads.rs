use crate::{iam_token_support, mcp_tool_call_support, temp_postgres_database, web_ingest_support};

#[cfg(feature = "test-support")]
use anyhow::Context;
use axum::Router;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    infra::repositories::{
        catalog_repository, content_document_repository, content_revision_repository,
    },
    interfaces::http::router,
    services::knowledge::service::{
        CreateKnowledgeDocumentCommand, CreateKnowledgeRevisionCommand,
    },
};

#[cfg(feature = "test-support")]
use ironrag_backend::services::ingest::worker;
#[cfg(feature = "test-support")]
use tokio::{sync::broadcast, time};

struct McpReadFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
    _content_storage_root: tempfile::TempDir,
    workspace_id: Uuid,
    library_id: Uuid,
    #[cfg(feature = "test-support")]
    library_ref: String,
}

fn knowledge_document_command(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    external_key: &str,
) -> CreateKnowledgeDocumentCommand {
    CreateKnowledgeDocumentCommand {
        external_key: external_key.to_string(),
        document_state: "active".to_string(),
        title: Some(external_key.to_string()),
        document_id,
        library_id,
        workspace_id,
        file_name: None,
    }
}

struct KnowledgeRevisionSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    external_key: &'a str,
    status: &'a str,
    extracted_text: Option<&'a str>,
}

fn knowledge_revision_command(seed: KnowledgeRevisionSeed<'_>) -> CreateKnowledgeRevisionCommand {
    let KnowledgeRevisionSeed {
        workspace_id,
        library_id,
        document_id,
        revision_id,
        external_key,
        status,
        extracted_text,
    } = seed;
    let has_readable_text = extracted_text.is_some();
    let text_state = if has_readable_text {
        "text_readable"
    } else if status == "failed" {
        "failed"
    } else {
        "extracting_text"
    };
    CreateKnowledgeRevisionCommand {
        revision_id,
        document_id,
        workspace_id,
        library_id,
        title: Some(external_key.to_string()),
        source_uri: Some(format!("{external_key}.txt")),
        mime_type: "text/plain".to_string(),
        checksum: format!("sha256:{}", "7".repeat(64)),
        byte_size: i64::try_from(extracted_text.unwrap_or_default().len()).unwrap_or(i64::MAX),
        normalized_text: extracted_text.map(ToString::to_string),
        text_checksum: extracted_text.map(|_| "sha256:mcp-read-text".to_string()),
        text_state: text_state.to_string(),
        text_readable_at: has_readable_text.then(Utc::now),
        revision_number: 1,
        revision_state: "active".to_string(),
        revision_kind: "upload".to_string(),
        storage_ref: None,
        document_hint: None,
        vector_state: "accepted".to_string(),
        vector_ready_at: None,
        graph_state: "accepted".to_string(),
        graph_ready_at: None,
        superseded_by_revision_id: None,
    }
}

fn assert_full_readable_response(
    response: &Value,
    document_id: Uuid,
    revision_id: Uuid,
    content: &str,
) {
    let structured = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"], json!(false));
    assert_eq!(structured["documentId"], json!(document_id));
    assert_eq!(structured["latestRevisionId"], json!(revision_id));
    assert_eq!(structured["readabilityState"], json!("readable"));
    assert_eq!(
        structured["statusReason"],
        json!(
            "document text is readable, but structured preparation and graph extraction are still processing"
        )
    );
    assert_eq!(structured["content"], json!(content));
    assert_eq!(structured["totalContentLength"], json!(content.chars().count()));
    assert_eq!(structured["hasMore"], json!(false));
}

impl McpReadFixture {
    async fn create(mut settings: Settings) -> anyhow::Result<Self> {
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        // This suite used to run straight against the configured database and
        // never migrated it, so it only worked when some earlier suite had
        // already created the schema — an ordering dependency the Makefile
        // never promised.
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "mcp_memory_reads").await?;
        settings.database_url = temp_database.database_url.clone();
        let content_storage_root =
            tempfile::tempdir().context("failed to create mcp read storage root")?;
        settings.dependency_object_storage_mode = "disabled".to_string();
        settings.content_storage_provider = "filesystem".to_string();
        settings.content_storage_topology = "single_node".to_string();
        settings.content_storage_key_prefix.clear();
        settings.content_storage_root = content_storage_root.path().to_string_lossy().into_owned();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect mcp read postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply mcp read migrations")?;
        seed_clean_v1_ai_catalog(&postgres).await.context("failed to seed mcp read AI catalog")?;
        postgres.close().await;
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
            temp_database,
            _content_storage_root: content_storage_root,
            workspace_id: workspace.id,
            library_id: library.id,
            #[cfg(feature = "test-support")]
            library_ref: format!("{}/{}", workspace.slug, library.slug),
        })
    }

    async fn cleanup(self) -> anyhow::Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }

    #[cfg(feature = "test-support")]
    async fn configure_ingest_ai(&mut self) -> anyhow::Result<()> {
        mcp_tool_call_support::configure_empty_graph_ingest_ai(
            &mut self.state,
            self.workspace_id,
            self.library_id,
            "MCP web-ingest fixture",
            "secret://mcp-read/ingest-provider",
            "MCP web-ingest",
        )
        .await
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
        mcp_tool_call_support::call_tool(
            self.app(),
            "/v1/mcp",
            token,
            "read-test",
            tool_name,
            arguments,
        )
        .await
    }

    async fn mcp_diagnostics_tool_call(
        &self,
        token: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
        mcp_tool_call_support::call_tool(
            self.app(),
            "/v1/mcp/diagnostics",
            token,
            "read-test-diagnostics",
            tool_name,
            arguments,
        )
        .await
    }

    async fn create_document_state(
        &self,
        external_key: &str,
        status: &str,
        extracted_text: Option<&str>,
    ) -> anyhow::Result<(Uuid, Uuid)> {
        let document = content_document_repository::create_document(
            &self.state.persistence.postgres,
            &content_document_repository::NewContentDocument {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                external_key,
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .with_context(|| format!("failed to create read document {external_key}"))?;
        let revision = crate::content_repository::create_imported_fixture_revision(
            &self.state.persistence.postgres,
            &content_revision_repository::NewContentRevision {
                admission_identity: None,
                document_id: document.id,
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: &format!("sha256:{}", "7".repeat(64)),
                mime_type: "text/plain",
                byte_size: i64::try_from(extracted_text.unwrap_or_default().len())
                    .unwrap_or(i64::MAX),
                title: Some(external_key),
                language_code: None,
                source_uri: Some(&format!("{external_key}.txt")),
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            },
            &format!("{external_key}.txt"),
        )
        .await
        .with_context(|| format!("failed to create read revision for {external_key}"))?;
        crate::content_lifecycle_support::seed_extracted_revision_head(
            &self.state.persistence.postgres,
            document.id,
            revision.id,
            &format!("{external_key}.txt"),
            extracted_text,
        )
        .await?;

        // `read_document` resolves the document through `document_store`, i.e.
        // the knowledge plane. This fixture only ever wrote content-plane rows
        // and discarded the three arguments below, so every read answered
        // "document not found".
        self.state
            .application_services
            .knowledge
            .create_document_shell(
                &self.state,
                knowledge_document_command(
                    self.workspace_id,
                    self.library_id,
                    document.id,
                    external_key,
                ),
            )
            .await
            .with_context(|| format!("failed to create knowledge shell for {external_key}"))?;
        self.state
            .application_services
            .knowledge
            .write_revision(
                &self.state,
                // Text becomes readable the moment extraction succeeds, even
                // when a later stage is still running or has failed — that
                // independence is what several tests here exist to pin down.
                knowledge_revision_command(KnowledgeRevisionSeed {
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    document_id: document.id,
                    revision_id: revision.id,
                    external_key,
                    status,
                    extracted_text,
                }),
            )
            .await
            .with_context(|| format!("failed to write knowledge revision for {external_key}"))?;
        crate::content_lifecycle_support::promote_fixture_document(
            &self.state,
            document.id,
            extracted_text.map(|_| revision.id),
        )
        .await?;
        Ok((document.id, revision.id))
    }
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn full_document_reads_return_complete_content_and_stable_revision_identity()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for full read test")?;
    let fixture = Box::pin(McpReadFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-full").await?;
        let content = "alpha beta gamma delta".to_string();
        let (document_id, revision_id) =
            fixture.create_document_state("full-read", "ready", Some(&content)).await?;

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
#[ignore = "requires local postgres and redis services"]
async fn excerpt_reads_respect_requested_window_and_offsets() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for excerpt read test")?;
    let fixture = Box::pin(McpReadFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-excerpt").await?;
        let content = "abcdefghijklmnopqrstuvwxyz".to_string();
        let (document_id, revision_id) =
            fixture.create_document_state("excerpt-read", "ready", Some(&content)).await?;

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
#[ignore = "requires local postgres and redis services"]
async fn continuation_reads_reconstruct_large_documents_without_gaps_or_duplicates()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for continuation read test")?;
    let fixture = Box::pin(McpReadFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-continuation").await?;
        let content = (0..12).map(|index| format!("segment-{index}|")).collect::<String>();
        let (document_id, _) =
            fixture.create_document_state("continuation-read", "ready", Some(&content)).await?;

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
#[ignore = "requires local postgres and redis services"]
async fn invalid_continuation_tokens_return_explicit_mcp_error_kind() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for invalid continuation test")?;
    let fixture = Box::pin(McpReadFixture::create(settings)).await?;

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
#[ignore = "requires local postgres and redis services"]
async fn unreadable_documents_return_honest_empty_and_failed_reasons() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for unreadable read test")?;
    let fixture = Box::pin(McpReadFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-unavailable").await?;
        let (processing_document_id, _) =
            fixture.create_document_state("processing-read", "processing", None).await?;
        let (failed_document_id, _) =
            fixture.create_document_state("failed-read", "failed", None).await?;

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
            json!("unavailable")
        );
        assert_eq!(
            processing["result"]["structuredContent"]["statusReason"],
            json!("document has no readable revision yet")
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
        // The tool reports a fixed reason for a failed extraction; it never
        // echoes the extractor's own message back to a caller.
        assert_eq!(
            failed["result"]["structuredContent"]["statusReason"],
            json!("latest readable revision extraction failed")
        );
        assert!(failed["result"]["structuredContent"]["content"].is_null());
        assert!(failed["result"]["structuredContent"]["totalContentLength"].is_null());
        assert_eq!(failed["result"]["structuredContent"]["hasMore"], json!(false));

        for response in [&processing, &failed] {
            let metadata = &response["result"]["structuredContent"];
            for field in [
                "latestRevisionId",
                "mimeType",
                "sourceUri",
                "sourceAccess",
                "visualDescription",
                "continuationToken",
            ] {
                assert!(metadata[field].is_null(), "unpublished {field} leaked: {metadata}");
            }
            let decoded: ironrag_backend::mcp_types::McpReadDocumentResponse =
                serde_json::from_value(metadata.clone())?;
            assert!(decoded.chunk_references.is_empty(), "unpublished chunks leaked");
            assert!(decoded.technical_fact_references.is_empty(), "unpublished facts leaked");
            assert!(decoded.entity_references.is_empty(), "unpublished entities leaked");
            assert!(decoded.relation_references.is_empty(), "unpublished relations leaked");
            assert!(decoded.evidence_references.is_empty(), "unpublished evidence leaked");
        }

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn processing_documents_with_extracted_text_are_readable_before_terminal_ready_status()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for early readable read test")?;
    let fixture = Box::pin(McpReadFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-early-readable").await?;
        let content =
            "Early readable extracted text is available while graph extraction continues.";
        let (document_id, revision_id) = fixture
            .create_document_state("early-readable-read", "processing", Some(content))
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        // Readable text plus unfinished downstream stages is not a silent
        // state any more: the tool says which stages are still running.
        assert_full_readable_response(&response, document_id, revision_id, content);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn failed_documents_with_extracted_text_remain_readable_for_memory_reads()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for failed-readable read test")?;
    let fixture = Box::pin(McpReadFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "read-failed-readable").await?;
        let content =
            "Graph projection failed later, but this extracted memory must remain readable.";
        let (document_id, revision_id) =
            fixture.create_document_state("failed-readable-read", "failed", Some(content)).await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": document_id, "mode": "full" }),
            )
            .await?;
        // Readable text plus unfinished downstream stages is not a silent
        // state any more: the tool says which stages are still running.
        assert_full_readable_response(&response, document_id, revision_id, content);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn web_ingest_documents_are_readable_through_mcp_read_document() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web-ingest read test")?;
    let mut fixture = Box::pin(McpReadFixture::create(settings)).await?;
    fixture.configure_ingest_ai().await?;
    web_ingest_support::enable_loopback_test_transport(&mut fixture.state);
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let token = fixture
            .bearer_token(
                &["projects:write", "documents:read", "documents:write"],
                "read-web-ingest",
            )
            .await?;

        let submit = fixture
            .mcp_diagnostics_tool_call(
                &token,
                "submit_web_run",
                json!({
                    "library": fixture.library_ref.clone(),
                    "seedUrl": server.url("/seed"),
                    "mode": "single_page",
                }),
            )
            .await?;
        assert_eq!(
            submit["result"]["isError"],
            json!(false),
            "submit_web_run returned an MCP tool error: {submit}",
        );
        assert_eq!(submit["result"]["structuredContent"]["runState"], json!("accepted"));

        let run_id: Uuid =
            serde_json::from_value(submit["result"]["structuredContent"]["runId"].clone())
                .context("run id missing")?;
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);
        let deadline = time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let run = fixture
                .state
                .application_services
                .web_ingest
                .get_run(&fixture.state, run_id)
                .await
                .context("failed to poll MCP web ingest run")?;
            if run.run_state == "completed" {
                break;
            }
            if matches!(run.run_state.as_str(), "completed_partial" | "failed" | "canceled") {
                let failure_diagnostics = sqlx::query_scalar::<_, Value>(
                    "select jsonb_build_object(
                        'candidateState', page.candidate_state::text,
                        'classificationReason', page.classification_reason,
                        'itemState', item.item_state::text,
                        'itemMessage', item.message,
                        'queueState', job.queue_state::text,
                        'attemptState', attempt.attempt_state::text,
                        'currentStage', attempt.current_stage,
                        'failureClass', attempt.failure_class,
                        'failureCode', attempt.failure_code,
                        'failureMessage', attempt.failure_message
                     )
                     from content_web_discovered_page page
                     left join content_mutation_item item on item.id = page.mutation_item_id
                     left join ingest_job job on job.mutation_item_id = item.id
                     left join lateral (
                        select *
                        from ingest_attempt candidate_attempt
                        where candidate_attempt.job_id = job.id
                        order by candidate_attempt.attempt_number desc
                        limit 1
                     ) attempt on true
                     where page.run_id = $1
                     order by page.discovered_at
                     limit 1",
                )
                .bind(run_id)
                .fetch_optional(&fixture.state.persistence.postgres)
                .await
                .context("failed to inspect terminal MCP web ingest run")?;
                anyhow::bail!(
                    "MCP web ingest run {run_id} terminated as {} with failure {:?}: {:?}",
                    run.run_state,
                    run.failure_code,
                    failure_diagnostics,
                );
            }
            if time::Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for MCP web ingest run {run_id} to complete from state {}",
                    run.run_state,
                );
            }
            time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let pages = fixture
            .mcp_diagnostics_tool_call(&token, "list_web_run_pages", json!({ "runId": run_id }))
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
        assert!(content.contains("Authoritative single-page ingest should keep only this page"));

        let _ = shutdown_tx.send(());
        let _ = time::timeout(std::time::Duration::from_secs(5), worker_handle).await;

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}
