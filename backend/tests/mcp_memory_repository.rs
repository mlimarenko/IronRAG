mod mcp_memory_support;

use anyhow::Context;
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use mcp_memory_support::sample_mcp_token;
use rustrag_backend::{
    app::config::Settings,
    infra::repositories::{
        self, DocumentRow, NewMcpAuditEvent, NewMcpMutationReceipt, ProjectRow,
        RuntimeIngestionRunRow, WorkspaceRow,
    },
};

struct McpMemoryFixture {
    workspace: WorkspaceRow,
    primary_library: ProjectRow,
    secondary_library: ProjectRow,
    token_id: Uuid,
}

impl McpMemoryFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            pool,
            &format!("mcp-memory-{suffix}"),
            "MCP Memory Repository Test",
        )
        .await
        .context("failed to create mcp memory workspace")?;
        let primary_library = repositories::create_project(
            pool,
            workspace.id,
            &format!("mcp-primary-{suffix}"),
            "Primary MCP Library",
            Some("primary repository test library"),
        )
        .await
        .context("failed to create primary mcp library")?;
        let secondary_library = repositories::create_project(
            pool,
            workspace.id,
            &format!("mcp-secondary-{suffix}"),
            "Secondary MCP Library",
            Some("secondary repository test library"),
        )
        .await
        .context("failed to create secondary mcp library")?;

        let token_template = sample_mcp_token(&["documents:read", "documents:write"]);
        let token = repositories::create_api_token(
            pool,
            Some(workspace.id),
            &token_template.token_kind,
            &token_template.label,
            &format!("hash-{}", Uuid::now_v7()),
            token_template.token_preview.as_deref(),
            token_template.scope_json,
            None,
        )
        .await
        .context("failed to create api token for mcp repository test")?;

        Ok(Self { workspace, primary_library, secondary_library, token_id: token.id })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace.id)
            .execute(pool)
            .await
            .context("failed to delete mcp repository test workspace")?;
        Ok(())
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect mcp repository test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for mcp repository test")?;
    Ok(pool)
}

async fn create_document_state(
    pool: &PgPool,
    library: &ProjectRow,
    external_key: &str,
    status: &str,
    extracted_text: Option<&str>,
    matching_chunks: &[&str],
    error_message: Option<&str>,
) -> anyhow::Result<(DocumentRow, RuntimeIngestionRunRow)> {
    let document = repositories::create_document(
        pool,
        library.id,
        None,
        external_key,
        Some(external_key),
        Some("text/plain"),
        Some("mcp-test-checksum"),
    )
    .await
    .with_context(|| format!("failed to create document state for {external_key}"))?;

    for (ordinal, chunk) in matching_chunks.iter().enumerate() {
        repositories::create_chunk(
            pool,
            document.id,
            library.id,
            i32::try_from(ordinal).unwrap_or(i32::MAX),
            chunk,
            Some(i32::try_from(chunk.split_whitespace().count()).unwrap_or(i32::MAX)),
            json!({ "source": "mcp_repository_test" }),
        )
        .await
        .with_context(|| format!("failed to create chunk {ordinal} for {external_key}"))?;
    }

    let runtime_run = repositories::create_runtime_ingestion_run(
        pool,
        library.id,
        Some(document.id),
        None,
        None,
        &format!("track-{external_key}-{}", Uuid::now_v7()),
        &format!("{external_key}.txt"),
        "txt",
        Some("text/plain"),
        Some(256),
        status,
        match status {
            "ready" | "ready_no_graph" => "completed",
            "failed" => "failed",
            _ => "extracting",
        },
        "runtime_upload",
        json!({}),
    )
    .await
    .with_context(|| format!("failed to create runtime run for {external_key}"))?;

    let runtime_run = if error_message.is_some() || matches!(status, "ready" | "ready_no_graph") {
        repositories::update_runtime_ingestion_run_status(
            pool,
            runtime_run.id,
            status,
            match status {
                "ready" | "ready_no_graph" => "completed",
                "failed" => "failed",
                _ => "extracting",
            },
            Some(if matches!(status, "ready" | "ready_no_graph" | "failed") { 100 } else { 50 }),
            error_message,
        )
        .await
        .with_context(|| format!("failed to update runtime run status for {external_key}"))?
    } else {
        runtime_run
    };

    if let Some(extracted_text) = extracted_text {
        repositories::upsert_runtime_extracted_content(
            pool,
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

    Ok((document, runtime_run))
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn mcp_repository_helpers_persist_and_project_memory_truth() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = McpMemoryFixture::create(&pool).await?;

    let result = async {
        let (readable_document, readable_run) = create_document_state(
            &pool,
            &fixture.primary_library,
            "readable-memory",
            "ready",
            Some("Memory systems preserve recall. Memory systems keep agents aligned."),
            &["Memory systems preserve recall.", "Memory systems keep agents aligned."],
            None,
        )
        .await?;
        let (processing_readable_document, processing_readable_run) = create_document_state(
            &pool,
            &fixture.primary_library,
            "processing-memory",
            "processing",
            Some("Processing work already extracted into readable memory."),
            &["Processing work already extracted into readable memory."],
            None,
        )
        .await?;
        let (failed_but_readable_document, failed_but_readable_run) = create_document_state(
            &pool,
            &fixture.primary_library,
            "failed-readable-memory",
            "failed",
            Some(
                "Graph projection failed later, but readable memory still contains memory anchor.",
            ),
            &[],
            Some("failed to refresh the canonical graph view"),
        )
        .await?;
        create_document_state(
            &pool,
            &fixture.primary_library,
            "failed-memory",
            "failed",
            None,
            &["Failed work still leaves a searchable chunk trail."],
            Some("extractor timeout"),
        )
        .await?;
        create_document_state(
            &pool,
            &fixture.secondary_library,
            "secondary-memory",
            "ready",
            Some("Secondary library memory remains searchable across scopes."),
            &["Secondary library memory remains searchable across scopes."],
            None,
        )
        .await?;

        let audit = repositories::create_mcp_audit_event(
            &pool,
            &NewMcpAuditEvent {
                request_id: Uuid::now_v7().to_string(),
                token_id: fixture.token_id,
                token_kind: "workspace".to_string(),
                action_kind: "search_documents".to_string(),
                workspace_id: Some(fixture.workspace.id),
                library_id: Some(fixture.primary_library.id),
                document_id: Some(readable_document.id),
                status: "succeeded".to_string(),
                error_kind: None,
                metadata_json: json!({ "query": "memory" }),
            },
        )
        .await
        .context("failed to persist mcp audit row")?;
        let receipt = repositories::create_mcp_mutation_receipt(
            &pool,
            &NewMcpMutationReceipt {
                token_id: fixture.token_id,
                workspace_id: fixture.workspace.id,
                library_id: fixture.primary_library.id,
                document_id: Some(readable_document.id),
                operation_kind: "upload".to_string(),
                idempotency_key: "mcp-upload-1".to_string(),
                payload_identity: Some("sha256:abc123".to_string()),
                status: "accepted".to_string(),
                failure_kind: None,
            },
        )
        .await
        .context("failed to persist mcp mutation receipt")?;

        let audit_rows = repositories::list_mcp_audit_events(
            &pool,
            Some(fixture.workspace.id),
            Some(fixture.token_id),
            10,
        )
        .await
        .context("failed to list mcp audit rows")?;
        assert_eq!(audit_rows.len(), 1);
        assert_eq!(audit_rows[0].id, audit.id);
        assert_eq!(audit_rows[0].action_kind, "search_documents");

        let receipt_by_key = repositories::find_mcp_mutation_receipt_by_idempotency(
            &pool,
            fixture.token_id,
            "upload",
            fixture.primary_library.id,
            Some(readable_document.id),
            "mcp-upload-1",
        )
        .await
        .context("failed to load mcp mutation receipt by idempotency")?
        .context("missing mcp mutation receipt by idempotency")?;
        let _receipt_by_id = repositories::get_mcp_mutation_receipt_by_id(&pool, receipt.id)
            .await
            .context("failed to load mcp mutation receipt by id")?
            .context("missing mcp mutation receipt by id")?;
        assert_eq!(receipt_by_key.id, receipt.id);

        let libraries =
            repositories::list_visible_libraries_with_counts(&pool, fixture.workspace.id)
                .await
                .context("failed to list visible libraries with counts")?;
        let primary = libraries
            .iter()
            .find(|row| row.library_id == fixture.primary_library.id)
            .context("missing primary library counts")?;
        assert_eq!(primary.document_count, 4);
        assert_eq!(primary.readable_document_count, 3);
        assert_eq!(primary.processing_document_count, 0);
        assert_eq!(primary.failed_document_count, 1);

        let hits = repositories::search_document_memory_by_library_scope(
            &pool,
            &[fixture.primary_library.id, fixture.secondary_library.id],
            "memory",
            10,
        )
        .await
        .context("failed to search document memory across libraries")?;
        assert!(hits.len() >= 2);
        assert_eq!(hits[0].document_id, readable_document.id);
        assert_eq!(hits[0].chunk_match_count, 2);
        assert_eq!(hits[0].readability_state, "readable");
        assert!(hits.iter().any(|hit| {
            hit.document_id == processing_readable_document.id
                && hit.readability_state == "readable"
                && hit.excerpt.as_deref().is_some_and(|excerpt| excerpt.contains("extracted"))
        }));
        assert!(hits.iter().any(|hit| {
            hit.document_id == failed_but_readable_document.id
                && hit.readability_state == "readable"
                && hit.excerpt.as_deref().is_some_and(|excerpt| excerpt.contains("memory anchor"))
        }));
        assert!(hits.iter().any(|hit| hit.library_id == fixture.secondary_library.id));

        let latest_state =
            repositories::get_latest_readable_runtime_document_state(&pool, readable_document.id)
                .await
                .context("failed to load latest readable runtime document state")?
                .context("missing latest readable runtime document state")?;
        assert_eq!(latest_state.readability_state, "readable");
        assert_eq!(latest_state.ingestion_run_id, Some(readable_run.id));
        assert!(latest_state.content_text.as_deref().is_some_and(|text| text.contains("agents")));

        let processing_state = repositories::get_latest_readable_runtime_document_state(
            &pool,
            processing_readable_document.id,
        )
        .await
        .context("failed to load processing readable runtime document state")?
        .context("missing processing readable runtime document state")?;
        assert_eq!(processing_state.ingestion_run_id, Some(processing_readable_run.id));
        assert_eq!(processing_state.runtime_status.as_deref(), Some("processing"));
        assert_eq!(processing_state.readability_state, "readable");
        assert!(
            processing_state
                .content_text
                .as_deref()
                .is_some_and(|text| text.contains("already extracted"))
        );

        let failed_but_readable_state = repositories::get_latest_readable_runtime_document_state(
            &pool,
            failed_but_readable_document.id,
        )
        .await
        .context("failed to load failed-readable runtime document state")?
        .context("missing failed-readable runtime document state")?;
        assert_eq!(failed_but_readable_state.ingestion_run_id, Some(failed_but_readable_run.id));
        assert_eq!(failed_but_readable_state.runtime_status.as_deref(), Some("failed"));
        assert_eq!(failed_but_readable_state.readability_state, "readable");
        assert!(
            failed_but_readable_state
                .content_text
                .as_deref()
                .is_some_and(|text| text.contains("memory anchor"))
        );

        let read_slice =
            repositories::load_runtime_document_read_slice(&pool, readable_document.id, 7, 12)
                .await
                .context("failed to load runtime document read slice")?
                .context("missing runtime document read slice")?;
        assert_eq!(read_slice.document_id, readable_document.id);
        assert_eq!(read_slice.slice_start_offset, 7);
        assert!(read_slice.slice_end_offset > read_slice.slice_start_offset);
        assert_eq!(read_slice.content, "systems pres");

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
