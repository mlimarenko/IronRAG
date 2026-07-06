use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sqlx::{AssertSqlSafe, PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
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
        sqlx::query(AssertSqlSafe(format!("drop database if exists \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(AssertSqlSafe(format!("create database \"{database_name}\"")))
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
        sqlx::query(AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
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
        sqlx::raw_sql(include_str!("../migrations/0002_retrieval_config.sql"))
            .execute(&pool)
            .await
            .context("failed to apply 0002_retrieval_config.sql for ingest repository test")?;
        sqlx::raw_sql(include_str!("../migrations/0003_minimax_provider_catalog.sql"))
            .execute(&pool)
            .await
            .context(
                "failed to apply 0003_minimax_provider_catalog.sql for ingest repository test",
            )?;
        sqlx::raw_sql(include_str!("../migrations/0004_ai_config_simplification.sql"))
            .execute(&pool)
            .await
            .context(
                "failed to apply 0004_ai_config_simplification.sql for ingest repository test",
            )?;
        sqlx::raw_sql(include_str!("../migrations/0005_ingest_queue_lease_metadata.sql"))
            .execute(&pool)
            .await
            .context(
                "failed to apply 0005_ingest_queue_lease_metadata.sql for ingest repository test",
            )?;

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

async fn create_queue_job(
    fixture: &IngestRepositoryFixture,
    queue_state: &str,
    dedupe_key: &str,
) -> Result<ingest_repository::IngestJobRow> {
    ingest_repository::create_ingest_job(
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
            queue_state: queue_state.to_string(),
            priority: 10,
            dedupe_key: Some(dedupe_key.to_string()),
            queued_at: Some(Utc::now()),
            available_at: Some(Utc::now()),
            completed_at: if matches!(queue_state, "completed" | "failed" | "canceled") {
                Some(Utc::now())
            } else {
                None
            },
        },
    )
    .await
    .with_context(|| format!("failed to create {queue_state} queue job"))
}

async fn set_job_queue_lease(
    pool: &PgPool,
    job_id: Uuid,
    leased_at: chrono::DateTime<Utc>,
    token: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "update ingest_job
         set queue_leased_at = $2,
             queue_lease_token = $3,
             queue_lease_owner = 'test-worker'
         where id = $1",
    )
    .bind(job_id)
    .bind(leased_at)
    .bind(token)
    .execute(pool)
    .await
    .context("failed to set queue lease metadata")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn concurrent_cap_one_claim_allows_only_one_queue_lease() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        create_queue_job(&fixture, "queued", "concurrent-cap-one-a").await?;
        create_queue_job(&fixture, "queued", "concurrent-cap-one-b").await?;

        let pool_a = fixture.pool.clone();
        let pool_b = fixture.pool.clone();
        let claim_a = tokio::spawn(async move {
            ingest_repository::claim_next_queued_ingest_job(
                &pool_a,
                "concurrent-cap-token-a",
                "concurrent-cap-worker-a",
                1,
                1,
                1,
            )
            .await
        });
        let claim_b = tokio::spawn(async move {
            ingest_repository::claim_next_queued_ingest_job(
                &pool_b,
                "concurrent-cap-token-b",
                "concurrent-cap-worker-b",
                1,
                1,
                1,
            )
            .await
        });

        let (claimed_a, claimed_b) = tokio::join!(claim_a, claim_b);
        let claimed_a = claimed_a.context("claim task A panicked")??;
        let claimed_b = claimed_b.context("claim task B panicked")??;
        assert_eq!(claimed_a.is_some() as u8 + claimed_b.is_some() as u8, 1);
        let claimed = claimed_a.or(claimed_b).context("one claim should lease a job")?;
        assert!(claimed.queue_leased_at.is_some());
        assert!(claimed.queue_lease_token.is_some());
        assert!(claimed.queue_lease_owner.is_some());

        let leased_count = sqlx::query_scalar::<_, i64>(
            "select count(*) from ingest_job where queue_state = 'leased'",
        )
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count leased jobs")?;
        assert_eq!(leased_count, 1);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn recovery_race_keeps_job_leased_when_fresh_attempt_appears_under_lock() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let stale_time = Utc::now() - Duration::minutes(10);
        let job = create_queue_job(&fixture, "leased", "recover-race-fresh-attempt").await?;
        set_job_queue_lease(&fixture.pool, job.id, stale_time, Some("recover-race-token")).await?;

        let mut tx = fixture.pool.begin().await.context("failed to begin race tx")?;
        sqlx::query("select id from ingest_job where id = $1 for update")
            .bind(job.id)
            .fetch_one(&mut *tx)
            .await
            .context("failed to lock race job")?;

        let recover_pool = fixture.pool.clone();
        let recover = tokio::spawn(async move {
            ingest_repository::recover_stale_canonical_leases(&recover_pool, Duration::minutes(5))
                .await
        });
        tokio::task::yield_now().await;

        sqlx::query(
            "insert into ingest_attempt (
                id,
                job_id,
                attempt_number,
                attempt_state,
                current_stage,
                started_at,
                heartbeat_at,
                progress_percent,
                retryable
            ) values (
                $1,
                $2,
                1,
                'leased'::ingest_attempt_state,
                'extract_content',
                now(),
                now(),
                0,
                false
            )",
        )
        .bind(Uuid::now_v7())
        .bind(job.id)
        .execute(&mut *tx)
        .await
        .context("failed to insert fresh active attempt under race lock")?;
        tx.commit().await.context("failed to commit race tx")?;

        let recovered = recover.await.context("recover task panicked")??;
        assert_eq!(recovered, 0);

        let reloaded = ingest_repository::get_ingest_job_by_id(&fixture.pool, job.id)
            .await?
            .context("race job missing")?;
        assert_eq!(reloaded.queue_state, "leased");
        assert_eq!(reloaded.queue_lease_token.as_deref(), Some("recover-race-token"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn retry_race_returns_none_when_fresh_attempt_appears_under_lock() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let stale_time = Utc::now() - Duration::minutes(10);
        let job = create_queue_job(&fixture, "leased", "retry-race-fresh-attempt").await?;
        set_job_queue_lease(&fixture.pool, job.id, stale_time, Some("retry-race-token")).await?;

        let mut tx = fixture.pool.begin().await.context("failed to begin retry race tx")?;
        sqlx::query("select id from ingest_job where id = $1 for update")
            .bind(job.id)
            .fetch_one(&mut *tx)
            .await
            .context("failed to lock retry race job")?;

        let retry_pool = fixture.pool.clone();
        let retry = tokio::spawn(async move {
            ingest_repository::retry_or_requeue_ingest_job(
                &retry_pool,
                job.id,
                Duration::minutes(5),
                Utc::now(),
            )
            .await
        });
        tokio::task::yield_now().await;

        sqlx::query(
            "insert into ingest_attempt (
                id,
                job_id,
                attempt_number,
                attempt_state,
                current_stage,
                started_at,
                heartbeat_at,
                progress_percent,
                retryable
            ) values (
                $1,
                $2,
                1,
                'leased'::ingest_attempt_state,
                'extract_content',
                now(),
                now(),
                0,
                false
            )",
        )
        .bind(Uuid::now_v7())
        .bind(job.id)
        .execute(&mut *tx)
        .await
        .context("failed to insert retry race active attempt")?;
        tx.commit().await.context("failed to commit retry race tx")?;

        let retried = retry.await.context("retry task panicked")??;
        assert!(retried.is_none());

        let reloaded = ingest_repository::get_ingest_job_by_id(&fixture.pool, job.id)
            .await?
            .context("retry race job missing")?;
        assert_eq!(reloaded.queue_state, "leased");
        assert_eq!(reloaded.queue_lease_token.as_deref(), Some("retry-race-token"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn stale_orphaned_queue_lease_recovers_but_fresh_active_and_terminal_do_not() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let stale_orphan = create_queue_job(&fixture, "leased", "stale-orphan").await?;
        let fresh_orphan = create_queue_job(&fixture, "leased", "fresh-orphan").await?;
        let active_stale = create_queue_job(&fixture, "leased", "active-stale").await?;
        let terminal = create_queue_job(&fixture, "completed", "terminal-completed").await?;

        let stale_time = Utc::now() - Duration::minutes(10);
        set_job_queue_lease(&fixture.pool, stale_orphan.id, stale_time, Some("stale-token"))
            .await?;
        set_job_queue_lease(&fixture.pool, fresh_orphan.id, Utc::now(), Some("fresh-token"))
            .await?;
        set_job_queue_lease(&fixture.pool, active_stale.id, stale_time, Some("active-token"))
            .await?;
        ingest_repository::create_ingest_attempt(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: active_stale.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some("active-attempt".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("extract_content".to_string()),
                started_at: Some(stale_time),
                heartbeat_at: Some(stale_time),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 1,
                retryable: false,
            },
        )
        .await
        .context("failed to create active attempt")?;

        let recovered =
            ingest_repository::recover_stale_canonical_leases(&fixture.pool, Duration::minutes(5))
                .await
                .context("failed to recover stale leases")?;
        assert_eq!(recovered, 2);

        let stale_orphan = ingest_repository::get_ingest_job_by_id(&fixture.pool, stale_orphan.id)
            .await?
            .context("stale orphan missing")?;
        assert_eq!(stale_orphan.queue_state, "queued");
        assert!(stale_orphan.queue_lease_token.is_none());

        let fresh_orphan = ingest_repository::get_ingest_job_by_id(&fixture.pool, fresh_orphan.id)
            .await?
            .context("fresh orphan missing")?;
        assert_eq!(fresh_orphan.queue_state, "leased");
        assert_eq!(fresh_orphan.queue_lease_token.as_deref(), Some("fresh-token"));

        let active_stale = ingest_repository::get_ingest_job_by_id(&fixture.pool, active_stale.id)
            .await?
            .context("active stale missing")?;
        assert_eq!(active_stale.queue_state, "queued");
        assert!(active_stale.queue_leased_at.is_none());
        assert!(active_stale.queue_lease_token.is_none());
        assert!(active_stale.queue_lease_owner.is_none());

        let terminal = ingest_repository::get_ingest_job_by_id(&fixture.pool, terminal.id)
            .await?
            .context("terminal missing")?;
        assert_eq!(terminal.queue_state, "completed");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn delayed_worker_cannot_create_attempt_after_queue_lease_token_changes() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let job = create_queue_job(&fixture, "queued", "token-race").await?;
        let claimed_a = ingest_repository::claim_next_queued_ingest_job(
            &fixture.pool,
            "queue-token-a",
            "worker-a",
            1,
            1,
            1,
        )
        .await?
        .context("worker A failed to claim")?;
        assert_eq!(claimed_a.id, job.id);

        set_job_queue_lease(
            &fixture.pool,
            job.id,
            Utc::now() - Duration::minutes(10),
            Some("queue-token-a"),
        )
        .await?;
        let recovered =
            ingest_repository::recover_stale_canonical_leases(&fixture.pool, Duration::minutes(5))
                .await?;
        assert_eq!(recovered, 1);

        let claimed_b = ingest_repository::claim_next_queued_ingest_job(
            &fixture.pool,
            "queue-token-b",
            "worker-b",
            1,
            1,
            1,
        )
        .await?
        .context("worker B failed to reclaim")?;
        assert_eq!(claimed_b.id, job.id);

        let delayed_attempt = ingest_repository::create_ingest_attempt_for_queue_lease(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 0,
                worker_principal_id: None,
                lease_token: Some("attempt-a".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("extract_content".to_string()),
                started_at: None,
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 0,
                retryable: false,
            },
            "queue-token-a",
        )
        .await?;
        assert!(delayed_attempt.is_none());

        let attempts =
            ingest_repository::list_ingest_attempts_by_job(&fixture.pool, job.id).await?;
        assert!(attempts.is_empty());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn atomic_attempt_finalization_does_not_expose_stale_orphan_job() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let stale_time = Utc::now() - Duration::minutes(10);
        let job = create_queue_job(&fixture, "leased", "atomic-finalize").await?;
        set_job_queue_lease(&fixture.pool, job.id, stale_time, Some("queue-token")).await?;
        let attempt = ingest_repository::create_ingest_attempt(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some("attempt-token".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("extract_content".to_string()),
                started_at: Some(stale_time),
                heartbeat_at: Some(stale_time),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 80,
                retryable: false,
            },
        )
        .await?;

        let finalized = ingest_repository::finalize_leased_ingest_attempt_and_update_job(
            &fixture.pool,
            attempt.id,
            &ingest_repository::UpdateIngestAttempt {
                worker_principal_id: attempt.worker_principal_id,
                lease_token: attempt.lease_token,
                knowledge_generation_id: attempt.knowledge_generation_id,
                attempt_state: "succeeded".to_string(),
                current_stage: Some("embed_chunk".to_string()),
                heartbeat_at: Some(Utc::now()),
                finished_at: Some(Utc::now()),
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 100,
                retryable: false,
            },
            &ingest_repository::UpdateIngestJob {
                mutation_id: job.mutation_id,
                connector_id: job.connector_id,
                async_operation_id: job.async_operation_id,
                knowledge_document_id: job.knowledge_document_id,
                knowledge_revision_id: job.knowledge_revision_id,
                job_kind: job.job_kind.clone(),
                queue_state: "completed".to_string(),
                priority: job.priority,
                dedupe_key: job.dedupe_key.clone(),
                available_at: job.available_at,
                completed_at: Some(Utc::now()),
            },
        )
        .await?
        .context("atomic finalization returned no row")?;
        assert_eq!(finalized.attempt_state, "succeeded");

        let recovered =
            ingest_repository::recover_stale_canonical_leases(&fixture.pool, Duration::minutes(5))
                .await?;
        assert_eq!(recovered, 0);

        let finalized_job = ingest_repository::get_ingest_job_by_id(&fixture.pool, job.id)
            .await?
            .context("finalized job missing")?;
        assert_eq!(finalized_job.queue_state, "completed");
        assert!(finalized_job.queue_lease_token.is_none());

        let stale_job = create_queue_job(&fixture, "leased", "atomic-rollback").await?;
        set_job_queue_lease(&fixture.pool, stale_job.id, stale_time, Some("rollback-token"))
            .await?;
        let stale_attempt = ingest_repository::create_ingest_attempt(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: stale_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some("rollback-attempt-token".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("chunk_content".to_string()),
                started_at: Some(stale_time),
                heartbeat_at: Some(stale_time),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 40,
                retryable: false,
            },
        )
        .await?;
        ingest_repository::update_ingest_job(
            &fixture.pool,
            stale_job.id,
            &ingest_repository::UpdateIngestJob {
                mutation_id: stale_job.mutation_id,
                connector_id: stale_job.connector_id,
                async_operation_id: stale_job.async_operation_id,
                knowledge_document_id: stale_job.knowledge_document_id,
                knowledge_revision_id: stale_job.knowledge_revision_id,
                job_kind: stale_job.job_kind.clone(),
                queue_state: "queued".to_string(),
                priority: stale_job.priority,
                dedupe_key: stale_job.dedupe_key.clone(),
                available_at: stale_job.available_at,
                completed_at: None,
            },
        )
        .await?
        .context("failed to move rollback job out of leased state")?;

        let stale_result = ingest_repository::finalize_leased_ingest_attempt_and_update_job(
            &fixture.pool,
            stale_attempt.id,
            &ingest_repository::UpdateIngestAttempt {
                worker_principal_id: stale_attempt.worker_principal_id,
                lease_token: stale_attempt.lease_token,
                knowledge_generation_id: stale_attempt.knowledge_generation_id,
                attempt_state: "failed".to_string(),
                current_stage: stale_attempt.current_stage,
                heartbeat_at: Some(Utc::now()),
                finished_at: Some(Utc::now()),
                failure_class: Some("queue_state".to_string()),
                failure_code: Some("lease_lost".to_string()),
                failure_message: Some("linked job lease was lost".to_string()),
                progress_percent: stale_attempt.progress_percent,
                retryable: true,
            },
            &ingest_repository::UpdateIngestJob {
                mutation_id: stale_job.mutation_id,
                connector_id: stale_job.connector_id,
                async_operation_id: stale_job.async_operation_id,
                knowledge_document_id: stale_job.knowledge_document_id,
                knowledge_revision_id: stale_job.knowledge_revision_id,
                job_kind: stale_job.job_kind,
                queue_state: "queued".to_string(),
                priority: stale_job.priority,
                dedupe_key: stale_job.dedupe_key,
                available_at: stale_job.available_at,
                completed_at: None,
            },
        )
        .await?;
        assert!(stale_result.is_none());

        let rolled_back_attempt =
            ingest_repository::get_ingest_attempt_by_id(&fixture.pool, stale_attempt.id)
                .await?
                .context("rolled back attempt missing")?;
        assert_eq!(rolled_back_attempt.attempt_state, "leased");
        assert!(rolled_back_attempt.finished_at.is_none());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn migration_0005_backfills_legacy_leased_rows_and_is_idempotent() -> Result<()> {
    let settings = Settings::from_env().context("failed to load settings")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect migration test postgres")?;

    let result = async {
        sqlx::raw_sql(include_str!("../migrations/0001_init.sql")).execute(&pool).await?;
        sqlx::raw_sql(include_str!("../migrations/0002_retrieval_config.sql"))
            .execute(&pool)
            .await?;
        sqlx::raw_sql(include_str!("../migrations/0003_minimax_provider_catalog.sql"))
            .execute(&pool)
            .await?;
        sqlx::raw_sql(include_str!("../migrations/0004_ai_config_simplification.sql"))
            .execute(&pool)
            .await?;

        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("migration-workspace-{}", Uuid::now_v7().simple()),
            "Migration Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("migration-library-{}", Uuid::now_v7().simple()),
            "Migration Library",
            None,
            None,
        )
        .await?;
        let job = ingest_repository::create_ingest_job(
            &pool,
            &ingest_repository::NewIngestJob {
                workspace_id: workspace.id,
                library_id: library.id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "content_mutation".to_string(),
                queue_state: "leased".to_string(),
                priority: 10,
                dedupe_key: Some("legacy-null-metadata".to_string()),
                queued_at: Some(Utc::now() - Duration::minutes(30)),
                available_at: Some(Utc::now() - Duration::minutes(30)),
                completed_at: None,
            },
        )
        .await?;

        sqlx::raw_sql(include_str!("../migrations/0005_ingest_queue_lease_metadata.sql"))
            .execute(&pool)
            .await?;
        sqlx::raw_sql(include_str!("../migrations/0005_ingest_queue_lease_metadata.sql"))
            .execute(&pool)
            .await?;

        let row = ingest_repository::get_ingest_job_by_id(&pool, job.id)
            .await?
            .context("migrated job missing")?;
        assert_eq!(row.queue_state, "leased");
        assert!(row.queue_leased_at.is_some());
        assert_eq!(row.queue_lease_owner.as_deref(), Some("legacy-migration"));
        let expected_token = format!("legacy-{}", job.id);
        assert_eq!(row.queue_lease_token.as_deref(), Some(expected_token.as_str()));

        Ok::<(), anyhow::Error>(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
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

        let ordered = ingest_repository::list_ingest_jobs(
            &fixture.pool,
            Some(fixture.workspace_id),
            None,
            None,
            None,
        )
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
        assert_eq!(
            updated.completed_at.map(|value| value.timestamp_micros()),
            Some(completed_at.timestamp_micros())
        );

        let reloaded = ingest_repository::get_ingest_job_by_id(&fixture.pool, high_priority.id)
            .await
            .context("failed to reload ingest job")?
            .context("reloaded ingest job missing")?;
        assert_eq!(reloaded.queue_state, "completed");
        assert_eq!(
            reloaded.completed_at.map(|value| value.timestamp_micros()),
            Some(completed_at.timestamp_micros())
        );

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
                failure_message: None,
                progress_percent: 80,
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
                failure_message: None,
                progress_percent: 10,
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
                failure_message: Some("upstream timed out".to_string()),
                progress_percent: 25,
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
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: None,
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
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: None,
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
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: None,
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
