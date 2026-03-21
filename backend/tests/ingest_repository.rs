use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    infra::repositories::{catalog_repository, ingest_repository},
};

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("ingest_repository_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect admin postgres for ingest repository test")?;

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
            .context("failed to reconnect admin postgres for ingest repository cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct IngestRepositoryFixture {
    pool: PgPool,
    temp_database: TempDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl IngestRepositoryFixture {
    async fn create() -> Result<Self> {
        let settings =
            Settings::from_env().context("failed to load settings for ingest repository test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&temp_database.database_url)
            .await
            .context("failed to connect ingest repository test postgres")?;

        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&pool)
            .await
            .context("failed to apply canonical 0001_init.sql for ingest repository test")?;

        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("ingest-workspace-{}", Uuid::now_v7().simple()),
            "Ingest Repository Workspace",
            None,
        )
        .await
        .context("failed to create workspace fixture")?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("ingest-library-{}", Uuid::now_v7().simple()),
            "Ingest Repository Library",
            Some("repository test fixture"),
            None,
        )
        .await
        .context("failed to create library fixture")?;

        Ok(Self { pool, temp_database, workspace_id: workspace.id, library_id: library.id })
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
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
#[ignore = "requires local postgres with canonical extensions"]
async fn ingest_job_crud_and_ordering_round_trip() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let now = Utc::now();
        let high_priority = ingest_repository::create_ingest_job(
            &fixture.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "content_mutation".to_string(),
                queue_state: "queued".to_string(),
                priority: 10,
                dedupe_key: Some("job-high".to_string()),
                queued_at: Some(now),
                available_at: Some(now),
                completed_at: None,
            },
        )
        .await
        .context("failed to create high priority ingest job")?;
        let delayed = ingest_repository::create_ingest_job(
            &fixture.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "reindex".to_string(),
                queue_state: "queued".to_string(),
                priority: 10,
                dedupe_key: Some("job-delayed".to_string()),
                queued_at: Some(now + Duration::seconds(1)),
                available_at: Some(now + Duration::minutes(5)),
                completed_at: None,
            },
        )
        .await
        .context("failed to create delayed ingest job")?;
        let low_priority = ingest_repository::create_ingest_job(
            &fixture.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "graph_refresh".to_string(),
                queue_state: "queued".to_string(),
                priority: 100,
                dedupe_key: Some("job-low".to_string()),
                queued_at: Some(now + Duration::seconds(2)),
                available_at: Some(now),
                completed_at: None,
            },
        )
        .await
        .context("failed to create low priority ingest job")?;

        let ordered =
            ingest_repository::list_ingest_jobs(&fixture.pool, Some(fixture.workspace_id), None)
                .await
                .context("failed to list ordered ingest jobs")?;
        let ordered_ids: Vec<Uuid> = ordered.into_iter().map(|row| row.id).collect();
        assert_eq!(ordered_ids, vec![high_priority.id, delayed.id, low_priority.id]);

        let deduped = ingest_repository::get_ingest_job_by_dedupe_key(
            &fixture.pool,
            fixture.library_id,
            "job-high",
        )
        .await
        .context("failed to resolve ingest job by dedupe key")?
        .context("missing dedupe-matched ingest job")?;
        assert_eq!(deduped.id, high_priority.id);

        let completed_at = now + Duration::minutes(10);
        let updated = ingest_repository::update_ingest_job(
            &fixture.pool,
            high_priority.id,
            &ingest_repository::UpdateIngestJob {
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "content_mutation".to_string(),
                queue_state: "completed".to_string(),
                priority: 5,
                dedupe_key: Some("job-high".to_string()),
                available_at: now,
                completed_at: Some(completed_at),
            },
        )
        .await
        .context("failed to update ingest job")?
        .context("updated ingest job missing")?;
        assert_eq!(updated.queue_state, "completed");
        assert_eq!(updated.priority, 5);
        assert_eq!(updated.completed_at, Some(completed_at));

        let reloaded = ingest_repository::get_ingest_job_by_id(&fixture.pool, high_priority.id)
            .await
            .context("failed to reload ingest job")?
            .context("reloaded ingest job missing")?;
        assert_eq!(reloaded.queue_state, "completed");
        assert_eq!(reloaded.completed_at, Some(completed_at));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn ingest_attempts_and_stage_events_round_trip_with_ordered_queries() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let job = ingest_repository::create_ingest_job(
            &fixture.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "content_mutation".to_string(),
                queue_state: "leased".to_string(),
                priority: 10,
                dedupe_key: Some("attempt-job".to_string()),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        )
        .await
        .context("failed to create job for attempt/event test")?;

        let attempt_two = ingest_repository::create_ingest_attempt(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 2,
                worker_principal_id: None,
                lease_token: Some("lease-2".to_string()),
                knowledge_generation_id: None,
                attempt_state: "running".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(Utc::now() + Duration::seconds(5)),
                heartbeat_at: None,
                finished_at: None,
                failure_class: None,
                failure_code: None,
                retryable: false,
            },
        )
        .await
        .context("failed to create second attempt")?;
        let attempt_one = ingest_repository::create_ingest_attempt(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some("lease-1".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("queued".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: None,
                finished_at: None,
                failure_class: None,
                failure_code: None,
                retryable: true,
            },
        )
        .await
        .context("failed to create first attempt")?;

        let attempts = ingest_repository::list_ingest_attempts_by_job(&fixture.pool, job.id)
            .await
            .context("failed to list attempts")?;
        let ordered_attempt_numbers: Vec<i32> =
            attempts.into_iter().map(|row| row.attempt_number).collect();
        assert_eq!(ordered_attempt_numbers, vec![1, 2]);

        let updated_attempt = ingest_repository::update_ingest_attempt(
            &fixture.pool,
            attempt_one.id,
            &ingest_repository::UpdateIngestAttempt {
                worker_principal_id: None,
                lease_token: Some("lease-1b".to_string()),
                knowledge_generation_id: None,
                attempt_state: "failed".to_string(),
                current_stage: Some("extract_text".to_string()),
                heartbeat_at: Some(Utc::now() + Duration::seconds(30)),
                finished_at: Some(Utc::now() + Duration::seconds(60)),
                failure_class: Some("upstream_timeout".to_string()),
                failure_code: Some("timeout".to_string()),
                retryable: true,
            },
        )
        .await
        .context("failed to update attempt")?
        .context("updated attempt missing")?;
        assert_eq!(updated_attempt.attempt_state, "failed");
        assert_eq!(updated_attempt.failure_code.as_deref(), Some("timeout"));

        let latest = ingest_repository::get_latest_ingest_attempt_by_job(&fixture.pool, job.id)
            .await
            .context("failed to load latest attempt")?
            .context("latest attempt missing")?;
        assert_eq!(latest.id, attempt_two.id);

        let attempt_two_event_b = ingest_repository::create_ingest_stage_event(
            &fixture.pool,
            &ingest_repository::NewIngestStageEvent {
                attempt_id: attempt_two.id,
                stage_name: "extract_graph".to_string(),
                stage_state: "completed".to_string(),
                ordinal: 2,
                message: Some("graph extraction complete".to_string()),
                details_json: serde_json::json!({ "chunks": 3 }),
                recorded_at: Some(Utc::now() + Duration::seconds(90)),
            },
        )
        .await
        .context("failed to create second stage event")?;
        let attempt_two_event_a = ingest_repository::create_ingest_stage_event(
            &fixture.pool,
            &ingest_repository::NewIngestStageEvent {
                attempt_id: attempt_two.id,
                stage_name: "extract_text".to_string(),
                stage_state: "started".to_string(),
                ordinal: 1,
                message: Some("text extraction started".to_string()),
                details_json: serde_json::json!({ "chunks": 3 }),
                recorded_at: Some(Utc::now() + Duration::seconds(80)),
            },
        )
        .await
        .context("failed to create first stage event")?;
        let attempt_one_event = ingest_repository::create_ingest_stage_event(
            &fixture.pool,
            &ingest_repository::NewIngestStageEvent {
                attempt_id: attempt_one.id,
                stage_name: "extract_text".to_string(),
                stage_state: "failed".to_string(),
                ordinal: 1,
                message: Some("first attempt failed".to_string()),
                details_json: serde_json::json!({ "reason": "timeout" }),
                recorded_at: Some(Utc::now() + Duration::seconds(40)),
            },
        )
        .await
        .context("failed to create attempt one stage event")?;

        let attempt_two_events =
            ingest_repository::list_ingest_stage_events_by_attempt(&fixture.pool, attempt_two.id)
                .await
                .context("failed to list stage events by attempt")?;
        let attempt_two_ordinals: Vec<i32> =
            attempt_two_events.iter().map(|row| row.ordinal).collect();
        assert_eq!(attempt_two_ordinals, vec![1, 2]);
        assert_eq!(attempt_two_events[0].id, attempt_two_event_a.id);
        assert_eq!(attempt_two_events[1].id, attempt_two_event_b.id);

        let job_events = ingest_repository::list_ingest_stage_events_by_job(&fixture.pool, job.id)
            .await
            .context("failed to list stage events by job")?;
        let job_event_ids: Vec<Uuid> = job_events.iter().map(|row| row.id).collect();
        assert_eq!(
            job_event_ids,
            vec![attempt_one_event.id, attempt_two_event_a.id, attempt_two_event_b.id]
        );

        let fetched_event =
            ingest_repository::get_ingest_stage_event_by_id(&fixture.pool, attempt_two_event_b.id)
                .await
                .context("failed to get stage event by id")?
                .context("stage event by id missing")?;
        assert_eq!(fetched_event.message.as_deref(), Some("graph extraction complete"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
