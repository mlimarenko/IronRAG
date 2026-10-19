use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sqlx::{AssertSqlSafe, PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    domains::webhook::{WebhookEvent, revision_ready_event_id},
    infra::{
        persistence::Persistence,
        repositories::{catalog_repository, ingest_repository, ops_repository},
    },
    services::maintenance::lease::MaintenanceClass,
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

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply authoritative migrations for ingest repository test")?;

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
            mutation_item_id: None,
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

fn graph_refresh_job_input(
    fixture: &IngestRepositoryFixture,
    document_id: Uuid,
    revision_id: Uuid,
    queue_state: &str,
    dedupe_key: &str,
) -> ingest_repository::NewIngestJob {
    graph_refresh_job_input_for_scope(
        fixture.workspace_id,
        fixture.library_id,
        document_id,
        revision_id,
        queue_state,
        dedupe_key,
    )
}

fn graph_refresh_job_input_for_scope(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    queue_state: &str,
    dedupe_key: &str,
) -> ingest_repository::NewIngestJob {
    ingest_repository::NewIngestJob {
        workspace_id,
        library_id,
        mutation_id: None,
        mutation_item_id: None,
        connector_id: None,
        async_operation_id: None,
        knowledge_document_id: Some(document_id),
        knowledge_revision_id: Some(revision_id),
        job_kind: "graph_refresh".to_string(),
        queue_state: queue_state.to_string(),
        priority: 200,
        dedupe_key: Some(dedupe_key.to_string()),
        queued_at: Some(Utc::now()),
        available_at: Some(Utc::now()),
        completed_at: if matches!(queue_state, "completed" | "failed" | "canceled") {
            Some(Utc::now())
        } else {
            None
        },
    }
}

async fn create_linked_operation(
    fixture: &IngestRepositoryFixture,
    subject_id: Uuid,
) -> Result<ops_repository::OpsAsyncOperationRow> {
    ops_repository::create_async_operation(
        &fixture.pool,
        &ops_repository::NewOpsAsyncOperation {
            workspace_id: fixture.workspace_id,
            library_id: Some(fixture.library_id),
            operation_kind: "content_mutation",
            surface_kind: "internal",
            requested_by_principal_id: None,
            status: "accepted",
            subject_kind: "content_mutation",
            subject_id: Some(subject_id),
            parent_async_operation_id: None,
            completed_at: None,
            failure_code: None,
        },
    )
    .await
    .context("failed to create linked async-operation fixture")
}

async fn create_inline_job_with_operation(
    fixture: &IngestRepositoryFixture,
    operation_id: Uuid,
    dedupe_key: &str,
) -> Result<ingest_repository::IngestJobRow> {
    ingest_repository::create_ingest_job(
        &fixture.pool,
        &ingest_repository::NewIngestJob {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            mutation_id: None,
            mutation_item_id: None,
            connector_id: None,
            async_operation_id: Some(operation_id),
            knowledge_document_id: None,
            knowledge_revision_id: None,
            job_kind: "content_mutation".to_string(),
            queue_state: "queued".to_string(),
            priority: 10,
            dedupe_key: Some(dedupe_key.to_string()),
            queued_at: Some(Utc::now()),
            available_at: Some(Utc::now()),
            completed_at: None,
        },
    )
    .await
    .context("failed to create linked inline job fixture")
}

fn new_inline_attempt(job_id: Uuid, lease_token: &str) -> ingest_repository::NewIngestAttempt {
    ingest_repository::NewIngestAttempt {
        job_id,
        attempt_number: 0,
        worker_principal_id: None,
        lease_token: Some(lease_token.to_string()),
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
    }
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
#[ignore = "requires local postgres with required extensions"]
async fn inline_attempt_lease_and_operation_transition_are_atomic() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let subject_id = Uuid::now_v7();
        let operation = create_linked_operation(&fixture, subject_id).await?;
        let job =
            create_inline_job_with_operation(&fixture, operation.id, "atomic-operation").await?;
        let attempt = ingest_repository::create_ingest_attempt_for_inline_lease(
            &fixture.pool,
            &new_inline_attempt(job.id, "inline-operation-success"),
            "inline-operation-success",
            "inline-test",
        )
        .await?
        .context("inline attempt was not leased")?;
        assert_eq!(attempt.attempt_number, 1);
        let operation = ops_repository::get_async_operation_by_id(&fixture.pool, operation.id)
            .await?
            .context("linked operation disappeared")?;
        assert_eq!(operation.status, "processing");
        assert!(operation.completed_at.is_none());
        assert!(operation.failure_code.is_none());

        let rejected_operation = create_linked_operation(&fixture, Uuid::now_v7()).await?;
        let rejected_job = create_inline_job_with_operation(
            &fixture,
            rejected_operation.id,
            "atomic-operation-rollback",
        )
        .await?;
        sqlx::raw_sql(
            "create function reject_async_operation_update() returns trigger
             language plpgsql as $$
             begin
                 raise exception 'injected async-operation update failure';
             end
             $$;
             create trigger reject_async_operation_update
             before update on ops_async_operation
             for each row execute function reject_async_operation_update();",
        )
        .execute(&fixture.pool)
        .await
        .context("failed to install async-operation rollback failpoint")?;

        let lease_result = ingest_repository::create_ingest_attempt_for_inline_lease(
            &fixture.pool,
            &new_inline_attempt(rejected_job.id, "inline-operation-failure"),
            "inline-operation-failure",
            "inline-test",
        )
        .await;
        assert!(lease_result.is_err(), "injected operation failure must abort the lease UoW");
        let attempts =
            ingest_repository::list_ingest_attempts_by_job(&fixture.pool, rejected_job.id).await?;
        assert!(attempts.is_empty(), "attempt insert escaped the rolled-back lease UoW");
        let rejected_job = ingest_repository::get_ingest_job_by_id(&fixture.pool, rejected_job.id)
            .await?
            .context("rolled-back inline job disappeared")?;
        assert_eq!(rejected_job.queue_state, "queued");
        assert!(rejected_job.queue_lease_token.is_none());
        let rejected_operation =
            ops_repository::get_async_operation_by_id(&fixture.pool, rejected_operation.id)
                .await?
                .context("rolled-back operation disappeared")?;
        assert_eq!(rejected_operation.status, "accepted");
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn terminal_linked_operation_is_not_reopened_by_inline_attempt_lease() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let operation = create_linked_operation(&fixture, Uuid::now_v7()).await?;
        let completed_at = Utc::now();
        ops_repository::update_async_operation(
            &fixture.pool,
            operation.id,
            &ops_repository::UpdateOpsAsyncOperation {
                status: "ready",
                completed_at: Some(completed_at),
                failure_code: None,
            },
        )
        .await?
        .context("terminal operation disappeared")?;
        let job =
            create_inline_job_with_operation(&fixture, operation.id, "terminal-operation").await?;

        let lease_result = ingest_repository::create_ingest_attempt_for_inline_lease(
            &fixture.pool,
            &new_inline_attempt(job.id, "terminal-operation-lease"),
            "terminal-operation-lease",
            "inline-test",
        )
        .await;
        anyhow::ensure!(
            lease_result.is_err(),
            "an attempt lease must reject a terminal linked operation"
        );
        let attempts =
            ingest_repository::list_ingest_attempts_by_job(&fixture.pool, job.id).await?;
        anyhow::ensure!(attempts.is_empty(), "a rejected terminal operation exposed an attempt");
        let job = ingest_repository::get_ingest_job_by_id(&fixture.pool, job.id)
            .await?
            .context("rejected terminal-operation job disappeared")?;
        anyhow::ensure!(job.queue_state == "queued", "a rejected lease changed the job state");
        anyhow::ensure!(job.queue_lease_token.is_none(), "a rejected lease retained a job token");
        let operation = ops_repository::get_async_operation_by_id(&fixture.pool, operation.id)
            .await?
            .context("rejected terminal operation disappeared")?;
        anyhow::ensure!(operation.status == "ready", "terminal operation was reopened");
        anyhow::ensure!(operation.completed_at.is_some(), "terminal completion time was cleared");
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn queue_lease_rechecks_job_library_after_parent_lock_wait() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let job = create_queue_job(&fixture, "leased", "library-lock-race").await?;
        set_job_queue_lease(&fixture.pool, job.id, Utc::now(), Some("library-lock-race-token"))
            .await?;
        let replacement_library = catalog_repository::create_library(
            &fixture.pool,
            fixture.workspace_id,
            &format!("replacement-library-{}", Uuid::now_v7().simple()),
            "Replacement Library",
            None,
            None,
        )
        .await?;

        let mut parent_lock = fixture.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>("select id from catalog_library where id = $1 for update")
            .bind(fixture.library_id)
            .fetch_one(&mut *parent_lock)
            .await?;

        let lease_pool = fixture.pool.clone();
        let attempt = new_inline_attempt(job.id, "library-lock-race-token");
        let lease = tokio::spawn(async move {
            ingest_repository::create_ingest_attempt_for_queue_lease(
                &lease_pool,
                &attempt,
                "library-lock-race-token",
            )
            .await
        });

        let mut observed_parent_lock_wait = false;
        for _ in 0..200 {
            let waiting: i64 = sqlx::query_scalar(
                "select count(*)::bigint
                 from pg_stat_activity
                 where datname = current_database()
                   and wait_event_type = 'Lock'
                   and query like '%from catalog_library%'
                   and query like '%for share%'",
            )
            .fetch_one(&fixture.pool)
            .await?;
            if waiting > 0 {
                observed_parent_lock_wait = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        anyhow::ensure!(
            observed_parent_lock_wait,
            "queue lease did not reach the controlled parent-library lock wait"
        );

        sqlx::query("update ingest_job set library_id = $2 where id = $1")
            .bind(job.id)
            .bind(replacement_library.id)
            .execute(&fixture.pool)
            .await?;
        parent_lock.commit().await?;

        let leased = tokio::time::timeout(std::time::Duration::from_secs(2), lease)
            .await
            .context("queue lease did not finish after releasing the parent-library lock")???;
        anyhow::ensure!(
            matches!(leased, ingest_repository::QueueLeaseOutcome::LeaseMoved),
            "queue lease used authority from a library the job no longer belongs to"
        );
        let attempts =
            ingest_repository::list_ingest_attempts_by_job(&fixture.pool, job.id).await?;
        anyhow::ensure!(attempts.is_empty(), "library drift exposed an ingest attempt");
        let moved_job = ingest_repository::get_ingest_job_by_id(&fixture.pool, job.id)
            .await?
            .context("moved queue job disappeared")?;
        anyhow::ensure!(
            moved_job.library_id == replacement_library.id,
            "test did not preserve the concurrent library move"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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
#[ignore = "requires local postgres with required extensions"]
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
            ingest_repository::recover_stale_ingest_leases(&recover_pool, Duration::minutes(5))
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
        assert_eq!(recovered.requeued, 0);
        assert_eq!(recovered.orphaned, 0);

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
#[ignore = "requires local postgres with required extensions"]
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
#[ignore = "requires local postgres with required extensions"]
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
        let mut terminal_attempt = None;
        for (job_id, lease_token) in
            [(active_stale.id, "active-attempt"), (terminal.id, "terminal-attempt")]
        {
            let attempt = ingest_repository::create_ingest_attempt(
                &fixture.pool,
                &ingest_repository::NewIngestAttempt {
                    job_id,
                    attempt_number: 1,
                    worker_principal_id: None,
                    lease_token: Some(lease_token.to_string()),
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
            .context("failed to create leased attempt")?;
            if job_id == terminal.id {
                terminal_attempt = Some(attempt);
            }
        }
        let terminal_attempt = terminal_attempt.context("terminal attempt not created")?;

        let recovered =
            ingest_repository::recover_stale_ingest_leases(&fixture.pool, Duration::minutes(5))
                .await
                .context("failed to recover stale leases")?;
        assert_eq!(recovered.requeued, 2);
        assert_eq!(recovered.orphaned, 1);

        let orphaned_attempt =
            ingest_repository::get_ingest_attempt_by_id(&fixture.pool, terminal_attempt.id)
                .await?
                .context("stale terminal attempt missing after recovery")?;
        assert_eq!(orphaned_attempt.attempt_state, "failed");
        assert_eq!(orphaned_attempt.failure_code.as_deref(), Some("stale_heartbeat"));

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
#[ignore = "requires local postgres with required extensions"]
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
            ingest_repository::recover_stale_ingest_leases(&fixture.pool, Duration::minutes(5))
                .await?;
        assert_eq!(recovered.requeued, 1);
        assert_eq!(recovered.orphaned, 0);

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
        assert!(matches!(delayed_attempt, ingest_repository::QueueLeaseOutcome::LeaseMoved));

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
#[ignore = "requires local postgres with required extensions"]
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
        let ready_revision_id = Uuid::now_v7();
        let ready_event = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: revision_ready_event_id(ready_revision_id),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: serde_json::json!({
                "document_id": Uuid::now_v7(),
                "revision_id": ready_revision_id,
                "library_id": fixture.library_id,
            }),
        };
        let source_truth_before =
            catalog_repository::get_library_source_truth_version(&fixture.pool, fixture.library_id)
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
            Some(&ready_event),
        )
        .await?
        .context("atomic finalization returned no row")?;
        assert_eq!(finalized.attempt_state, "succeeded");

        let recovered =
            ingest_repository::recover_stale_ingest_leases(&fixture.pool, Duration::minutes(5))
                .await?;
        assert_eq!(recovered.requeued, 0);
        assert_eq!(recovered.orphaned, 0);

        let finalized_job = ingest_repository::get_ingest_job_by_id(&fixture.pool, job.id)
            .await?
            .context("finalized job missing")?;
        assert_eq!(finalized_job.queue_state, "completed");
        assert!(finalized_job.queue_lease_token.is_none());
        let ready_outbox_count: i64 =
            sqlx::query_scalar("select count(*) from webhook_lifecycle_outbox where event_id = $1")
                .bind(&ready_event.event_id)
                .fetch_one(&fixture.pool)
                .await?;
        assert_eq!(ready_outbox_count, 1, "job success and ready event must commit together");
        let source_truth_after =
            catalog_repository::get_library_source_truth_version(&fixture.pool, fixture.library_id)
                .await?;
        assert!(
            source_truth_after > source_truth_before,
            "ready-event commit must advance the answer-cache source generation"
        );

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
        let stale_revision_id = Uuid::now_v7();
        let stale_event = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: revision_ready_event_id(stale_revision_id),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: serde_json::json!({
                "document_id": Uuid::now_v7(),
                "revision_id": stale_revision_id,
                "library_id": fixture.library_id,
            }),
        };
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
            Some(&stale_event),
        )
        .await?;
        assert!(stale_result.is_none());
        let source_truth_after_rollback =
            catalog_repository::get_library_source_truth_version(&fixture.pool, fixture.library_id)
                .await?;
        assert_eq!(
            source_truth_after_rollback, source_truth_after,
            "a lost finalization lease must roll back its source-generation update"
        );

        let rolled_back_attempt =
            ingest_repository::get_ingest_attempt_by_id(&fixture.pool, stale_attempt.id)
                .await?
                .context("rolled back attempt missing")?;
        assert_eq!(rolled_back_attempt.attempt_state, "leased");
        assert!(rolled_back_attempt.finished_at.is_none());
        let stale_outbox_count: i64 =
            sqlx::query_scalar("select count(*) from webhook_lifecycle_outbox where event_id = $1")
                .bind(&stale_event.event_id)
                .fetch_one(&fixture.pool)
                .await?;
        assert_eq!(stale_outbox_count, 0, "lost job lease must roll back the ready event");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn live_graph_refresh_job_suppresses_duplicate_enqueue() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let dedupe_key = format!("graph_reextract:test:{revision_id}");
        let live_job = ingest_repository::create_ingest_job(
            &fixture.pool,
            &graph_refresh_job_input(&fixture, document_id, revision_id, "queued", &dedupe_key),
        )
        .await
        .context("failed to create live graph refresh job")?;

        let duplicate = ingest_repository::enqueue_active_library_ingest_job_with_live_dedupe(
            &fixture.pool,
            &graph_refresh_job_input(&fixture, document_id, revision_id, "queued", &dedupe_key),
        )
        .await
        .context("failed to evaluate live graph refresh duplicate")?;
        assert!(duplicate.is_none(), "a live job must suppress a duplicate enqueue");

        let retained = ingest_repository::get_ingest_job_by_id(&fixture.pool, live_job.id)
            .await?
            .context("live graph refresh job disappeared")?;
        assert_eq!(retained.dedupe_key.as_deref(), Some(dedupe_key.as_str()));
        let job_count: i64 = sqlx::query_scalar(
            "select count(*) from ingest_job where library_id = $1 and knowledge_revision_id = $2",
        )
        .bind(fixture.library_id)
        .bind(revision_id)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(job_count, 1);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn automatic_graph_refresh_global_cap_is_linearized_across_libraries() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let second_library = catalog_repository::create_library(
            &fixture.pool,
            fixture.workspace_id,
            &format!("ingest-library-{}", Uuid::now_v7().simple()),
            "Concurrent Graph Repair Library",
            Some("global repair cap fixture"),
            None,
        )
        .await?;
        let first_revision_id = Uuid::now_v7();
        let second_revision_id = Uuid::now_v7();
        let mut first = graph_refresh_job_input_for_scope(
            fixture.workspace_id,
            fixture.library_id,
            Uuid::now_v7(),
            first_revision_id,
            "queued",
            &format!("graph_reextract:test:{first_revision_id}"),
        );
        first.priority = 250;
        let mut second = graph_refresh_job_input_for_scope(
            fixture.workspace_id,
            second_library.id,
            Uuid::now_v7(),
            second_revision_id,
            "queued",
            &format!("graph_reextract:test:{second_revision_id}"),
        );
        second.priority = 250;

        let (first_result, second_result) = tokio::join!(
            ingest_repository::enqueue_automatic_graph_refresh_job_with_live_dedupe(
                &fixture.pool,
                &first,
                1,
            ),
            ingest_repository::enqueue_automatic_graph_refresh_job_with_live_dedupe(
                &fixture.pool,
                &second,
                1,
            ),
        );
        let admitted = usize::from(first_result?.is_some()) + usize::from(second_result?.is_some());
        assert_eq!(admitted, 1, "concurrent libraries must share one automatic capacity slot");
        let live: i64 = sqlx::query_scalar(
            "select count(*)::bigint
             from ingest_job
             where job_kind = 'graph_refresh'::ingest_job_kind
               and queue_state in ('queued', 'leased', 'paused')",
        )
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(live, 1);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn live_job_kind_probe_is_kind_scoped_and_treats_paused_as_live() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        assert!(
            !ingest_repository::has_live_ingest_jobs_by_kind(
                &fixture.pool,
                fixture.library_id,
                "graph_refresh",
            )
            .await?
        );

        create_queue_job(&fixture, "queued", "unrelated-content-job").await?;
        assert!(
            !ingest_repository::has_live_ingest_jobs_by_kind(
                &fixture.pool,
                fixture.library_id,
                "graph_refresh",
            )
            .await?,
            "a different live job kind must not block graph recovery"
        );

        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let paused = ingest_repository::create_ingest_job(
            &fixture.pool,
            &graph_refresh_job_input(
                &fixture,
                document_id,
                revision_id,
                "paused",
                &format!("graph_reextract:test:{revision_id}"),
            ),
        )
        .await
        .context("failed to create paused graph-refresh job")?;
        assert!(
            ingest_repository::has_live_ingest_jobs_by_kind(
                &fixture.pool,
                fixture.library_id,
                "graph_refresh",
            )
            .await?,
            "an operator-paused batch must remain a scheduler blocker"
        );

        sqlx::query(
            "update ingest_job
             set queue_state = 'failed'::ingest_queue_state,
                 completed_at = now()
             where id = $1",
        )
        .bind(paused.id)
        .execute(&fixture.pool)
        .await
        .context("failed to terminalize paused graph-refresh fixture")?;
        assert!(
            !ingest_repository::has_live_ingest_jobs_by_kind(
                &fixture.pool,
                fixture.library_id,
                "graph_refresh",
            )
            .await?,
            "terminal graph-refresh jobs must release the next periodic batch"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn graph_reextract_admission_rotates_durably_and_recovers_expired_reservations() -> Result<()>
{
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let mut replica_a = PgPoolOptions::new()
            .max_connections(1)
            .connect(&fixture.temp_database.database_url)
            .await
            .context("failed to connect scheduler replica A")?;
        let mut replica_b = PgPoolOptions::new()
            .max_connections(1)
            .connect(&fixture.temp_database.database_url)
            .await
            .context("failed to connect scheduler replica B")?;
        let library_ids = (1..=100).map(Uuid::from_u128).collect::<Vec<_>>();
        let tick_interval = std::time::Duration::from_secs(3_600);
        let admission_class = MaintenanceClass::GraphReextractAdmission.as_str();

        let mut blocking_transaction = replica_a.begin().await?;
        sqlx::query(
            "select pg_advisory_xact_lock(
                hashtextextended('ironrag:graph-reextract-periodic-admission-tick'::text, 0)
             )",
        )
        .execute(&mut *blocking_transaction)
        .await?;
        let contended = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            ingest_repository::try_reserve_graph_reextract_admission_tick(
                &replica_b,
                &library_ids,
                tick_interval,
                32,
            ),
        )
        .await
        .context("contending scheduler blocked instead of skipping the tick")??;
        assert!(contended.is_none(), "an advisory-lock loser must skip without waiting");
        blocking_transaction.rollback().await?;

        let first = ingest_repository::try_reserve_graph_reextract_admission_tick(
            &replica_a,
            &library_ids,
            tick_interval,
            32,
        )
        .await?
        .context("first replica did not reserve an uncontended admission window")?;
        assert_eq!(first.start_index, 0);

        sqlx::query(
            "update maintenance_job_run
             set next_due_at = clock_timestamp()
             where class = $1 and scope_kind = 'instance' and scope_id is null",
        )
        .bind(admission_class)
        .execute(&fixture.pool)
        .await?;
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            ingest_repository::try_reserve_graph_reextract_admission_tick(
                &replica_b,
                &library_ids,
                tick_interval,
                32,
            ),
        )
        .await
        .context("second replica blocked behind an overdue in-flight reservation")??;
        assert!(
            second.is_none(),
            "an overdue tick must not supersede a still-live reservation token"
        );
        assert!(
            ingest_repository::complete_graph_reextract_admission_tick(
                &replica_a,
                first.token,
                library_ids.get(first.start_index).copied(),
            )
            .await?,
            "first replica failed to advance the durable cursor",
        );

        let mut observed_start_indices = vec![first.start_index];
        for expected_start_index in 1..100 {
            if expected_start_index == 50 {
                replica_a.close().await;
                replica_a = PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&fixture.temp_database.database_url)
                    .await
                    .context("failed to restart scheduler replica A")?;
            }
            if expected_start_index == 75 {
                replica_b.close().await;
                replica_b = PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&fixture.temp_database.database_url)
                    .await
                    .context("failed to restart scheduler replica B")?;
            }
            sqlx::query(
                "update maintenance_job_run
                 set next_due_at = clock_timestamp()
                 where class = $1 and scope_kind = 'instance' and scope_id is null",
            )
            .bind(admission_class)
            .execute(&fixture.pool)
            .await?;

            let replica = if expected_start_index % 2 == 0 { &replica_a } else { &replica_b };
            let reservation = ingest_repository::try_reserve_graph_reextract_admission_tick(
                replica,
                &library_ids,
                tick_interval,
                32,
            )
            .await?
            .context("forced-due admission window was not reserved")?;
            assert_eq!(reservation.start_index, expected_start_index);
            observed_start_indices.push(reservation.start_index);
            assert!(
                ingest_repository::complete_graph_reextract_admission_tick(
                    replica,
                    reservation.token,
                    library_ids.get(reservation.start_index).copied(),
                )
                .await?,
                "reservation owner failed to advance the durable cursor",
            );
        }

        assert_eq!(observed_start_indices.len(), library_ids.len());
        assert_eq!(observed_start_indices.last(), Some(&99));
        assert_eq!(
            observed_start_indices.into_iter().collect::<std::collections::BTreeSet<_>>().len(),
            library_ids.len(),
            "alternating replicas must cover every late library under one-library tick budgets",
        );

        sqlx::query(
            "update maintenance_job_run
             set next_due_at = clock_timestamp()
             where class = $1 and scope_kind = 'instance' and scope_id is null",
        )
        .bind(admission_class)
        .execute(&fixture.pool)
        .await?;
        let crashed = ingest_repository::try_reserve_graph_reextract_admission_tick(
            &replica_a,
            &library_ids,
            tick_interval,
            32,
        )
        .await?
        .context("crash simulation reservation was not granted")?;
        sqlx::query(
            "update maintenance_job_run
             set next_due_at = clock_timestamp(),
                 last_started_at = clock_timestamp() - interval '1 day'
             where class = $1 and scope_kind = 'instance' and scope_id is null",
        )
        .bind(admission_class)
        .execute(&fixture.pool)
        .await?;
        let replacement = ingest_repository::try_reserve_graph_reextract_admission_tick(
            &replica_b,
            &library_ids,
            tick_interval,
            32,
        )
        .await?
        .context("expired crashed reservation was not replaced")?;
        assert_eq!(
            replacement.start_index, crashed.start_index,
            "crash recovery must retry from the last completed cursor"
        );
        assert_ne!(replacement.token, crashed.token);
        assert!(
            !ingest_repository::complete_graph_reextract_admission_tick(
                &replica_a,
                crashed.token,
                library_ids.get(50).copied(),
            )
            .await?,
            "an expired reservation must not move the cursor after replacement",
        );
        assert!(
            ingest_repository::complete_graph_reextract_admission_tick(
                &replica_b,
                replacement.token,
                library_ids.get(replacement.start_index).copied(),
            )
            .await?,
            "replacement reservation failed to complete after rejecting the stale token",
        );

        replica_a.close().await;
        replica_b.close().await;
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn graph_reextract_admission_subtracts_cross_library_live_work_on_the_next_tick() -> Result<()>
{
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let second_library = catalog_repository::create_library(
            &fixture.pool,
            fixture.workspace_id,
            &format!("ingest-library-{}", Uuid::now_v7().simple()),
            "Second Ingest Repository Library",
            Some("cross-library scheduler fixture"),
            None,
        )
        .await
        .context("failed to create second scheduler library")?;
        let mut library_ids = vec![fixture.library_id, second_library.id];
        library_ids.sort_unstable();
        let tick_interval = std::time::Duration::from_secs(3_600);
        let admission_class = MaintenanceClass::GraphReextractAdmission.as_str();

        let foreground = create_queue_job(&fixture, "queued", "foreground-before-repair").await?;
        let first = ingest_repository::try_reserve_graph_reextract_admission_tick(
            &fixture.pool,
            &library_ids,
            tick_interval,
            4,
        )
        .await?
        .context("first global-capacity tick was not reserved")?;
        assert_eq!(first.remaining_capacity, 4);
        assert!(
            ingest_repository::complete_graph_reextract_admission_tick(
                &fixture.pool,
                first.token,
                library_ids.get(first.start_index).copied(),
            )
            .await?
        );

        for (index, (library_id, queue_state)) in [
            (fixture.library_id, "queued"),
            (fixture.library_id, "paused"),
            (second_library.id, "queued"),
            (second_library.id, "leased"),
        ]
        .into_iter()
        .enumerate()
        {
            let revision_id = Uuid::now_v7();
            ingest_repository::create_ingest_job(
                &fixture.pool,
                &graph_refresh_job_input_for_scope(
                    fixture.workspace_id,
                    library_id,
                    Uuid::now_v7(),
                    revision_id,
                    queue_state,
                    &format!("graph_reextract:test:{revision_id}:{index}"),
                ),
            )
            .await
            .with_context(|| format!("failed to create {queue_state} graph-refresh job"))?;
        }

        let claimed = ingest_repository::claim_next_queued_ingest_job(
            &fixture.pool,
            "foreground-order-token",
            "foreground-order-worker",
            16,
            16,
            16,
        )
        .await?
        .context("foreground queue job was not claimable")?;
        assert_eq!(
            claimed.id, foreground.id,
            "scheduler inserts must retain existing foreground queue_rank ordering"
        );

        sqlx::query(
            "update maintenance_job_run
             set next_due_at = clock_timestamp()
             where class = $1 and scope_kind = 'instance' and scope_id is null",
        )
        .bind(admission_class)
        .execute(&fixture.pool)
        .await?;
        let second = ingest_repository::try_reserve_graph_reextract_admission_tick(
            &fixture.pool,
            &library_ids,
            tick_interval,
            4,
        )
        .await?
        .context("second global-capacity tick was not reserved")?;
        assert_eq!(
            second.remaining_capacity, 0,
            "queued, leased, and paused graph-refresh work in every library must consume the next automatic wave"
        );
        assert!(
            ingest_repository::complete_graph_reextract_admission_tick(
                &fixture.pool,
                second.token,
                None,
            )
            .await?
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions and redis"]
async fn scheduler_control_lane_cannot_consume_the_heartbeat_reaper_reserve() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let mut settings = Settings::from_env()
            .context("failed to load settings for scheduler control lane test")?;
        settings.database_url = fixture.temp_database.database_url.clone();
        settings.database_max_connections = 4;
        settings.api_replicas = 0;
        settings.worker_replicas = 1;
        settings.service_role = "worker".to_string();
        let persistence = Persistence::connect(&settings)
            .await
            .context("failed to connect production worker pool layout")?;

        let active_job_heartbeat_slot = persistence
            .heartbeat_postgres
            .acquire()
            .await
            .context("failed to occupy the worker heartbeat slot")?;
        let active_scheduler_scan_slot = persistence
            .postgres
            .acquire()
            .await
            .context("failed to occupy the scheduler's main-pool scan slot")?;
        let mut scheduler_transaction = persistence.scheduler_postgres.begin().await?;
        sqlx::query(
            "select pg_advisory_xact_lock(
                hashtextextended('ironrag:graph-reextract-periodic-admission-tick'::text, 0)
             )",
        )
        .execute(&mut *scheduler_transaction)
        .await?;

        let reaper_probe = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sqlx::query_scalar::<_, i32>("select 1").fetch_one(&persistence.heartbeat_postgres),
        )
        .await
        .context("scheduler transaction starved the stale-reaper reserve")??;
        assert_eq!(reaper_probe, 1);

        scheduler_transaction.rollback().await?;
        drop(active_scheduler_scan_slot);
        drop(active_job_heartbeat_slot);
        persistence.scheduler_postgres.close().await;
        persistence.heartbeat_postgres.close().await;
        persistence.postgres.close().await;
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn terminal_graph_refresh_job_allows_retry_enqueue() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let dedupe_key = format!("graph_reextract:test:{revision_id}");
        let terminal_job = ingest_repository::create_ingest_job(
            &fixture.pool,
            &graph_refresh_job_input(&fixture, document_id, revision_id, "failed", &dedupe_key),
        )
        .await
        .context("failed to create terminal graph refresh job")?;

        let retried = ingest_repository::enqueue_active_library_ingest_job_with_live_dedupe(
            &fixture.pool,
            &graph_refresh_job_input(&fixture, document_id, revision_id, "queued", &dedupe_key),
        )
        .await
        .context("failed to retry terminal graph refresh job")?
        .context("terminal graph refresh job was incorrectly deduplicated")?;
        assert_ne!(retried.id, terminal_job.id);
        assert_eq!(retried.queue_state, "queued");
        assert_eq!(retried.dedupe_key.as_deref(), Some(dedupe_key.as_str()));

        let retired = ingest_repository::get_ingest_job_by_id(&fixture.pool, terminal_job.id)
            .await?
            .context("terminal graph refresh job disappeared")?;
        assert_eq!(retired.queue_state, "failed");
        assert!(retired.completed_at.is_some());
        assert!(
            retired
                .dedupe_key
                .as_deref()
                .is_some_and(|key| key.starts_with(&format!("{dedupe_key}:terminal:"))),
            "terminal graph-refresh history must retain its versioned repair identity"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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
                mutation_item_id: None,
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
                mutation_item_id: None,
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
                mutation_item_id: None,
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
#[ignore = "requires local postgres with required extensions"]
async fn ingest_attempts_and_stage_events_round_trip_with_ordered_queries() -> Result<()> {
    let fixture = IngestRepositoryFixture::create().await?;

    let result = async {
        let job = ingest_repository::create_ingest_job(
            &fixture.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                mutation_item_id: None,
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
