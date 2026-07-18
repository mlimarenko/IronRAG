use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{catalog_repository, content_repository},
    services::ingest::extract::{
        CheckpointResumeCursorCommand, ExtractService, MaterializeChunkResultCommand,
        NewEdgeCandidate, NewNodeCandidate,
    },
};

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("extract_graph_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect extract_graph admin postgres")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {database_name}"))?;
        admin_pool.close().await;

        Ok(Self {
            name: database_name.clone(),
            admin_url,
            database_url: replace_database_name(base_database_url, &database_name)?,
        })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect extract_graph admin postgres for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct ExtractGraphFixture {
    temp_database: TempDatabase,
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    attempt_id: Uuid,
    node_id: Uuid,
}

impl ExtractGraphFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for extract_graph test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect extract_graph postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply extract_graph migrations")?;

        let state = build_test_state(settings, postgres).await?;
        let fixture = Self {
            temp_database,
            state,
            workspace_id: Uuid::nil(),
            library_id: Uuid::nil(),
            document_id: Uuid::nil(),
            revision_id: Uuid::nil(),
            chunk_id: Uuid::nil(),
            attempt_id: Uuid::nil(),
            node_id: Uuid::nil(),
        };
        fixture.seed().await
    }

    async fn seed(mut self) -> Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &self.state.persistence.postgres,
            &format!("extract-graph-workspace-{suffix}"),
            "Extract Graph Workspace",
            None,
        )
        .await
        .context("failed to create workspace fixture")?;
        let library = catalog_repository::create_library(
            &self.state.persistence.postgres,
            workspace.id,
            &format!("extract-graph-library-{suffix}"),
            "Extract Graph Library",
            Some("extract graph test library"),
            None,
        )
        .await
        .context("failed to create library fixture")?;

        let document = content_repository::create_document_with_projection(
            &self.state.persistence.postgres,
            &content_repository::NewContentDocument {
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: &format!("extract-graph-doc-{suffix}"),
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
            Some("extract-graph.txt"),
        )
        .await
        .context("failed to create content document")?;
        let revision = match content_repository::create_revision_with_projection(
            &self.state.persistence.postgres,
            &content_repository::NewContentRevisionProjection {
                document_id: document.id,
                workspace_id: workspace.id,
                library_id: library.id,
                content_source_kind: "upload",
                checksum: "sha256:extract-graph",
                mime_type: "text/plain",
                byte_size: 96,
                title: Some("Extract Graph Fixture"),
                language_code: Some("en"),
                source_uri: None,
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            },
        )
        .await
        .context("failed to create content revision projection")?
        {
            content_repository::CreateContentRevisionOutcome::Created(revision) => *revision,
            outcome => anyhow::bail!("content revision fixture was not created: {outcome:?}"),
        };
        let chunk = content_repository::create_chunk(
            &self.state.persistence.postgres,
            &content_repository::NewContentChunk {
                revision_id: revision.id,
                chunk_index: 0,
                start_offset: 0,
                end_offset: 96,
                token_count: Some(16),
                normalized_text: "Readable extracted text for the canonical greenfield test.",
                text_checksum: "sha256:chunk",
                occurred_at: None,
                occurred_until: None,
            },
        )
        .await
        .context("failed to create content chunk")?;
        let attempt = sqlx::query_scalar::<_, Uuid>(
            "insert into ingest_job (
                workspace_id,
                library_id,
                job_kind,
                queue_state,
                priority,
                queued_at,
                available_at
            )
            values ($1, $2, 'content_mutation', 'queued', 100, now(), now())
            returning id",
        )
        .bind(workspace.id)
        .bind(library.id)
        .fetch_one(&self.state.persistence.postgres)
        .await
        .context("failed to insert ingest job")?;
        let attempt = sqlx::query_scalar::<_, Uuid>(
            "insert into ingest_attempt (
                job_id,
                attempt_number,
                attempt_state,
                current_stage,
                started_at
            )
            values ($1, 1, 'running', 'extracting_graph', now())
            returning id",
        )
        .bind(attempt)
        .fetch_one(&self.state.persistence.postgres)
        .await
        .context("failed to insert ingest attempt")?;

        let extract_service = ExtractService::new();
        let _ = self
            .state
            .canonical_services
            .knowledge
            .set_revision_extract_state(
                &self.state,
                revision.id,
                "ready",
                Some("Readable extracted text for the canonical greenfield test."),
                Some("sha256:extract-graph"),
            )
            .await
            .context("failed to persist extract content")?;
        let _chunk_result = extract_service
            .materialize_chunk_result(
                &self.state,
                MaterializeChunkResultCommand {
                    chunk_id: chunk.id,
                    attempt_id: attempt,
                    extract_state: "ready".to_string(),
                    provider_call_id: None,
                    finished_at: Some(Utc::now()),
                    failure_code: None,
                    node_candidates: vec![
                        NewNodeCandidate {
                            canonical_key: "entity:greenfield-test".to_string(),
                            node_kind: "entity".to_string(),
                            display_label: "Greenfield Test".to_string(),
                            summary: Some("Typed node candidate".to_string()),
                        },
                        NewNodeCandidate {
                            canonical_key: "entity:greenfield-other".to_string(),
                            node_kind: "entity".to_string(),
                            display_label: "Greenfield Other".to_string(),
                            summary: None,
                        },
                    ],
                    edge_candidates: vec![NewEdgeCandidate {
                        canonical_key: "entity:greenfield-test--mentions--entity:greenfield-other"
                            .to_string(),
                        edge_kind: "mentions".to_string(),
                        from_display_label: "Greenfield Test".to_string(),
                        from_canonical_key: "entity:greenfield-test".to_string(),
                        to_display_label: "Greenfield Other".to_string(),
                        to_canonical_key: "entity:greenfield-other".to_string(),
                        summary: Some("Typed edge candidate".to_string()),
                    }],
                },
            )
            .await
            .context("failed to materialize chunk extraction result")?;
        let cursor = extract_service
            .checkpoint_resume_cursor(
                &self.state,
                CheckpointResumeCursorCommand {
                    attempt_id: attempt,
                    last_completed_chunk_index: 0,
                },
            )
            .await
            .context("failed to checkpoint resume cursor")?;

        self.workspace_id = workspace.id;
        self.library_id = library.id;
        self.document_id = document.id;
        self.revision_id = revision.id;
        self.chunk_id = chunk.id;
        self.attempt_id = cursor.attempt_id;
        self.node_id = Uuid::now_v7();
        Ok(self)
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

async fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let redis = redis::Client::open(settings.redis_url.clone())
        .context("failed to create redis client for extract_graph test state")?;
    let persistence = ironrag_backend::infra::persistence::Persistence::for_tests(postgres, redis);
    AppState::from_dependencies(settings, persistence)
}

fn replace_database_name(database_url: &str, new_database: &str) -> Result<String> {
    let (without_query, query_suffix) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash_index = without_query
        .rfind('/')
        .with_context(|| format!("database url is missing database name: {database_url}"))?;
    let mut rebuilt = format!("{}{new_database}", &without_query[..=slash_index]);
    if let Some(query) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(query);
    }
    Ok(rebuilt)
}

async fn terminate_database_connections(postgres: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(postgres)
    .await
    .with_context(|| format!("failed to terminate connections for {database_name}"))?;
    Ok(())
}

async fn assert_legacy_truth_tables_absent(pool: &PgPool) -> Result<()> {
    for table in ["entity", "relation", "runtime_vector_target", "extract_content"] {
        let exists = sqlx::query_scalar::<_, Option<String>>("select to_regclass($1)::text")
            .bind(table)
            .fetch_one(pool)
            .await
            .with_context(|| format!("failed to inspect legacy table {table}"))?;
        assert!(exists.is_none(), "legacy table {table} should not exist");
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn extract_flow_preserves_readable_text_chunk_results_and_resume_cursors() -> Result<()> {
    let fixture = ExtractGraphFixture::create().await?;

    let result = async {
        let extract_service = ExtractService::new();

        let content = extract_service
            .get_extract_content(&fixture.state, fixture.revision_id)
            .await
            .context("failed to load extract content")?;
        assert_eq!(content.extract_state, "ready");
        assert_eq!(
            content.normalized_text.as_deref(),
            Some("Readable extracted text for the canonical greenfield test."),
        );

        let chunk_results = extract_service
            .list_chunk_results(&fixture.state, fixture.attempt_id)
            .await
            .context("failed to list chunk results")?;
        assert_eq!(chunk_results.len(), 1);
        assert_eq!(chunk_results[0].chunk_id, fixture.chunk_id);
        assert_eq!(chunk_results[0].extract_state, "ready");

        let node_candidates = extract_service
            .list_node_candidates(&fixture.state, chunk_results[0].id)
            .await
            .context("failed to list node candidates")?;
        assert_eq!(node_candidates.len(), 2);
        assert_eq!(
            node_candidates
                .iter()
                .map(|candidate| candidate.canonical_key.as_str())
                .collect::<Vec<_>>(),
            vec!["entity:greenfield_other", "entity:greenfield_test"]
        );

        let edge_candidates = extract_service
            .list_edge_candidates(&fixture.state, chunk_results[0].id)
            .await
            .context("failed to list edge candidates")?;
        assert_eq!(edge_candidates.len(), 1);
        assert_eq!(
            edge_candidates[0].canonical_key,
            "entity:greenfield_test--mentions--entity:greenfield_other",
        );

        let cursor = extract_service
            .get_resume_cursor(&fixture.state, fixture.attempt_id)
            .await
            .context("failed to load resume cursor")?
            .context("missing resume cursor")?;
        assert_eq!(cursor.last_completed_chunk_index, 0);
        assert_eq!(cursor.replay_count, 0);
        assert_eq!(cursor.downgrade_level, 0);

        let replay = extract_service
            .increment_replay_count(&fixture.state, fixture.attempt_id)
            .await
            .context("failed to increment replay count")?;
        assert_eq!(replay.replay_count, 1);
        let downgrade = extract_service
            .increment_downgrade_level(&fixture.state, fixture.attempt_id)
            .await
            .context("failed to increment downgrade level")?;
        assert_eq!(downgrade.downgrade_level, 1);

        assert_legacy_truth_tables_absent(&fixture.state.persistence.postgres).await
    }
    .await;

    fixture.cleanup().await?;
    result
}
