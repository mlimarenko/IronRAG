use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    infra::repositories::{catalog_repository, extract_repository},
};

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("extract_repository_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect extract repository admin postgres")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(&format!("drop database if exists \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(&format!("create database \"{database_name}\""))
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
            .context("failed to reconnect extract repository admin postgres for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct ExtractRepositoryFixture {
    temp_database: TempDatabase,
    postgres: PgPool,
    revision_id: Uuid,
    chunk_id: Uuid,
    attempt_id: Uuid,
}

impl ExtractRepositoryFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for extract repository test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect extract repository postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply extract repository migrations")?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &postgres,
            &format!("extract-repository-workspace-{suffix}"),
            "Extract Repository Workspace",
            None,
        )
        .await
        .context("failed to create extract repository workspace")?;
        let library = catalog_repository::create_library(
            &postgres,
            workspace.id,
            &format!("extract-repository-library-{suffix}"),
            "Extract Repository Library",
            Some("extract repository test library"),
            None,
        )
        .await
        .context("failed to create extract repository library")?;

        let document_id = sqlx::query_scalar::<_, Uuid>(
            "insert into content_document (
                workspace_id,
                library_id,
                external_key,
                document_state,
                created_at
            )
            values ($1, $2, $3, 'active', now())
            returning id",
        )
        .bind(workspace.id)
        .bind(library.id)
        .bind(format!("extract-repository-doc-{suffix}"))
        .fetch_one(&postgres)
        .await
        .context("failed to insert content_document")?;

        let revision_id = sqlx::query_scalar::<_, Uuid>(
            "insert into content_revision (
                document_id,
                workspace_id,
                library_id,
                revision_number,
                content_source_kind,
                checksum,
                mime_type,
                byte_size,
                title,
                created_at
            )
            values ($1, $2, $3, 1, 'upload', $4, 'text/plain', $5, $6, now())
            returning id",
        )
        .bind(document_id)
        .bind(workspace.id)
        .bind(library.id)
        .bind("sha256:extract-repository")
        .bind(128_i64)
        .bind("Extract Repository Document")
        .fetch_one(&postgres)
        .await
        .context("failed to insert content_revision")?;

        let chunk_id = sqlx::query_scalar::<_, Uuid>(
            "insert into content_chunk (
                revision_id,
                chunk_index,
                start_offset,
                end_offset,
                token_count,
                normalized_text,
                text_checksum
            )
            values ($1, 0, 0, 32, 6, $2, $3)
            returning id",
        )
        .bind(revision_id)
        .bind("Extract repository candidate chunk text")
        .bind("sha256:chunk")
        .fetch_one(&postgres)
        .await
        .context("failed to insert content_chunk")?;

        let job_id = sqlx::query_scalar::<_, Uuid>(
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
        .fetch_one(&postgres)
        .await
        .context("failed to insert ingest_job")?;

        let attempt_id = sqlx::query_scalar::<_, Uuid>(
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
        .bind(job_id)
        .fetch_one(&postgres)
        .await
        .context("failed to insert ingest_attempt")?;

        Ok(Self { temp_database, postgres, revision_id, chunk_id, attempt_id })
    }

    async fn cleanup(self) -> Result<()> {
        self.postgres.close().await;
        self.temp_database.drop().await
    }
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

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn extract_repository_crud_queries_round_trip_greenfield_extract_state() -> Result<()> {
    let fixture = ExtractRepositoryFixture::create().await?;

    let result = async {
        let processing = extract_repository::upsert_extract_content(
            &fixture.postgres,
            fixture.revision_id,
            Some(fixture.attempt_id),
            "processing",
            None,
            None,
            1,
        )
        .await
        .context("failed to upsert processing extract_content")?;
        assert_eq!(processing.extract_state, "processing");
        assert_eq!(processing.warning_count, 1);
        assert_eq!(processing.attempt_id, Some(fixture.attempt_id));

        let ready = extract_repository::upsert_extract_content(
            &fixture.postgres,
            fixture.revision_id,
            Some(fixture.attempt_id),
            "ready",
            Some("Normalized readable text for the revision"),
            Some("sha256:text"),
            2,
        )
        .await
        .context("failed to upsert ready extract_content")?;
        assert_eq!(ready.extract_state, "ready");
        assert_eq!(
            ready.normalized_text.as_deref(),
            Some("Normalized readable text for the revision")
        );
        assert_eq!(ready.text_checksum.as_deref(), Some("sha256:text"));

        let loaded = extract_repository::get_extract_content_by_revision_id(
            &fixture.postgres,
            fixture.revision_id,
        )
        .await
        .context("failed to load extract_content by revision")?
        .context("missing extract_content by revision")?;
        assert_eq!(loaded.extract_state, "ready");

        let by_attempt = extract_repository::list_extract_content_by_attempt(
            &fixture.postgres,
            fixture.attempt_id,
        )
        .await
        .context("failed to list extract_content by attempt")?;
        assert_eq!(by_attempt.len(), 1);
        assert_eq!(by_attempt[0].revision_id, fixture.revision_id);

        let chunk_result = extract_repository::create_extract_chunk_result(
            &fixture.postgres,
            fixture.chunk_id,
            fixture.attempt_id,
            "processing",
            None,
            None,
            None,
            None,
        )
        .await
        .context("failed to create extract_chunk_result")?;
        assert_eq!(chunk_result.extract_state, "processing");
        assert_eq!(chunk_result.chunk_id, fixture.chunk_id);

        let chunk_by_lookup = extract_repository::get_extract_chunk_result_by_chunk_and_attempt(
            &fixture.postgres,
            fixture.chunk_id,
            fixture.attempt_id,
        )
        .await
        .context("failed to load extract_chunk_result by chunk and attempt")?
        .context("missing extract_chunk_result by chunk and attempt")?;
        assert_eq!(chunk_by_lookup.id, chunk_result.id);

        let updated_chunk_result = extract_repository::update_extract_chunk_result(
            &fixture.postgres,
            chunk_result.id,
            "ready",
            None,
            Some(chrono::Utc::now()),
            None,
        )
        .await
        .context("failed to update extract_chunk_result")?
        .context("missing updated extract_chunk_result")?;
        assert_eq!(updated_chunk_result.extract_state, "ready");
        assert!(updated_chunk_result.finished_at.is_some());

        let node_rows = extract_repository::replace_extract_node_candidates(
            &fixture.postgres,
            chunk_result.id,
            &[
                extract_repository::NewExtractNodeCandidate {
                    canonical_key: "node:alpha",
                    node_kind: "concept",
                    display_label: "Alpha",
                    summary: Some("alpha summary"),
                },
                extract_repository::NewExtractNodeCandidate {
                    canonical_key: "node:beta",
                    node_kind: "concept",
                    display_label: "Beta",
                    summary: None,
                },
            ],
        )
        .await
        .context("failed to replace node candidates")?;
        assert_eq!(node_rows.len(), 2);

        let listed_node_rows = extract_repository::list_extract_node_candidates_by_chunk_result(
            &fixture.postgres,
            chunk_result.id,
        )
        .await
        .context("failed to list node candidates")?;
        assert_eq!(listed_node_rows.len(), 2);
        assert_eq!(listed_node_rows[0].canonical_key, "node:alpha");

        let edge_rows = extract_repository::replace_extract_edge_candidates(
            &fixture.postgres,
            chunk_result.id,
            &[extract_repository::NewExtractEdgeCandidate {
                canonical_key: "edge:alpha-beta",
                edge_kind: "relates_to",
                from_canonical_key: "node:alpha",
                to_canonical_key: "node:beta",
                summary: Some("edge summary"),
            }],
        )
        .await
        .context("failed to replace edge candidates")?;
        assert_eq!(edge_rows.len(), 1);

        let listed_edge_rows = extract_repository::list_extract_edge_candidates_by_chunk_result(
            &fixture.postgres,
            chunk_result.id,
        )
        .await
        .context("failed to list edge candidates")?;
        assert_eq!(listed_edge_rows.len(), 1);
        assert_eq!(listed_edge_rows[0].from_canonical_key, "node:alpha");
        assert_eq!(listed_edge_rows[0].to_canonical_key, "node:beta");

        let checkpointed = extract_repository::checkpoint_extract_resume_cursor(
            &fixture.postgres,
            fixture.attempt_id,
            3,
        )
        .await
        .context("failed to checkpoint resume cursor")?;
        assert_eq!(checkpointed.last_completed_chunk_index, 3);
        assert_eq!(checkpointed.replay_count, 0);
        assert_eq!(checkpointed.downgrade_level, 0);

        let checkpointed_again = extract_repository::checkpoint_extract_resume_cursor(
            &fixture.postgres,
            fixture.attempt_id,
            2,
        )
        .await
        .context("failed to checkpoint resume cursor with lower index")?;
        assert_eq!(checkpointed_again.last_completed_chunk_index, 3);

        let replayed = extract_repository::increment_extract_resume_replay_count(
            &fixture.postgres,
            fixture.attempt_id,
        )
        .await
        .context("failed to increment resume replay count")?;
        assert_eq!(replayed.replay_count, 1);

        let downgraded = extract_repository::increment_extract_resume_downgrade_level(
            &fixture.postgres,
            fixture.attempt_id,
        )
        .await
        .context("failed to increment resume downgrade level")?;
        assert_eq!(downgraded.downgrade_level, 1);

        let cursor = extract_repository::upsert_extract_resume_cursor(
            &fixture.postgres,
            fixture.attempt_id,
            7,
            4,
            2,
        )
        .await
        .context("failed to upsert resume cursor")?;
        assert_eq!(cursor.last_completed_chunk_index, 7);
        assert_eq!(cursor.replay_count, 4);
        assert_eq!(cursor.downgrade_level, 2);

        let loaded_cursor = extract_repository::get_extract_resume_cursor_by_attempt_id(
            &fixture.postgres,
            fixture.attempt_id,
        )
        .await
        .context("failed to load resume cursor by attempt")?
        .context("missing resume cursor by attempt")?;
        assert_eq!(loaded_cursor.last_completed_chunk_index, 7);
        assert_eq!(loaded_cursor.replay_count, 4);
        assert_eq!(loaded_cursor.downgrade_level, 2);

        let listed_chunk_results = extract_repository::list_extract_chunk_results_by_attempt(
            &fixture.postgres,
            fixture.attempt_id,
        )
        .await
        .context("failed to list chunk results by attempt")?;
        assert_eq!(listed_chunk_results.len(), 1);
        assert_eq!(listed_chunk_results[0].id, chunk_result.id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
