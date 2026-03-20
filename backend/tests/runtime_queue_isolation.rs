use anyhow::Context;
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    infra::repositories::{
        self, IngestionJobRow, ProjectRow, RuntimeIngestionRunRow, WorkspaceRow,
    },
};

struct QueueIsolationFixture {
    workspace: WorkspaceRow,
    older_project: ProjectRow,
    newer_project: ProjectRow,
}

impl QueueIsolationFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            pool,
            &format!("queue-isolation-test-{suffix}"),
            "Queue Isolation Test",
        )
        .await
        .context("failed to create queue isolation workspace")?;
        let older_project = repositories::create_project(
            pool,
            workspace.id,
            &format!("older-library-{suffix}"),
            "Older Library",
            Some("older backlog library"),
        )
        .await
        .context("failed to create older test library")?;
        let newer_project = repositories::create_project(
            pool,
            workspace.id,
            &format!("newer-library-{suffix}"),
            "Newer Library",
            Some("new library that should get the reserved slot"),
        )
        .await
        .context("failed to create newer test library")?;

        Ok(Self { workspace, older_project, newer_project })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace.id)
            .execute(pool)
            .await
            .context("failed to delete queue isolation test workspace")?;
        Ok(())
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect queue isolation test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for queue isolation test")?;
    Ok(pool)
}

async fn create_queued_runtime_job(
    pool: &PgPool,
    project: &ProjectRow,
    file_stem: &str,
) -> anyhow::Result<(RuntimeIngestionRunRow, IngestionJobRow)> {
    let runtime_run = repositories::create_runtime_ingestion_run(
        pool,
        project.id,
        None,
        None,
        None,
        &format!("track-{file_stem}"),
        &format!("{file_stem}.md"),
        "md",
        Some("text/markdown"),
        Some(256),
        "queued",
        "accepted",
        "initial_upload",
        json!({}),
    )
    .await
    .with_context(|| format!("failed to create runtime run for {file_stem}"))?;
    let ingestion_job = repositories::create_ingestion_job(
        pool,
        project.id,
        None,
        "runtime_upload",
        Some("queue-isolation-test"),
        None,
        None,
        None,
        json!({
            "project_id": project.id,
            "runtime_ingestion_run_id": runtime_run.id,
            "source_id": null,
            "external_key": file_stem,
            "title": file_stem,
            "mime_type": "text/markdown",
            "text": "queue isolation fixture",
            "file_kind": "md",
            "file_size_bytes": 256,
            "ingest_mode": "runtime_upload",
            "extra_metadata": {},
        }),
    )
    .await
    .with_context(|| format!("failed to create ingestion job for {file_stem}"))?;

    Ok((runtime_run, ingestion_job))
}

#[tokio::test]
#[ignore = "requires local postgres services"]
async fn retry_jobs_created_with_attempt_count_are_claimed_before_first_attempt_backlog()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for retry-priority test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = QueueIsolationFixture::create(&pool).await?;

    let result = async {
        let (_, first_attempt_job) =
            create_queued_runtime_job(&pool, &fixture.older_project, "first-attempt").await?;

        let retried_job = repositories::create_ingestion_job(
            &pool,
            fixture.older_project.id,
            None,
            "runtime_upload",
            Some("queue-isolation-test"),
            Some(first_attempt_job.id),
            None,
            Some(2),
            json!({
                "project_id": fixture.older_project.id,
                "external_key": "retry-priority",
                "title": "retry-priority",
                "mime_type": "text/markdown",
                "text": "retry priority fixture",
                "file_kind": "md",
                "file_size_bytes": 128,
                "ingest_mode": "runtime_requeue",
                "extra_metadata": {},
            }),
        )
        .await
        .context("failed to create retried ingestion job")?;

        let claimed = repositories::claim_next_ingestion_job(
            &pool,
            "retry-priority-worker",
            chrono::Duration::seconds(300),
            4,
            1,
        )
        .await
        .context("failed to claim queued retry job")?
        .context("expected a queued job to be claimable")?;

        assert_eq!(claimed.id, retried_job.id);
        assert_eq!(claimed.attempt_count, 3);
        assert_eq!(claimed.project_id, fixture.older_project.id);

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres services"]
async fn reserved_slot_stays_available_for_quiet_project_work_when_busy_project_has_retries()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for reserved-slot quiet-project preference test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = QueueIsolationFixture::create(&pool).await?;

    let result = async {
        for index in 0..3 {
            let (_, job) =
                create_queued_runtime_job(&pool, &fixture.older_project, &format!("older-{index}"))
                    .await?;
            let claimed = repositories::claim_next_ingestion_job(
                &pool,
                &format!("older-worker-{index}"),
                chrono::Duration::seconds(300),
                4,
                1,
            )
            .await
            .context("failed to claim older backlog job")?
            .context("expected older backlog job to be claimable")?;
            assert_eq!(claimed.id, job.id);
        }

        let (_, quiet_project_job) =
            create_queued_runtime_job(&pool, &fixture.newer_project, "quiet-project").await?;

        let retried_job = repositories::create_ingestion_job(
            &pool,
            fixture.older_project.id,
            None,
            "runtime_upload",
            Some("queue-isolation-test"),
            None,
            None,
            Some(2),
            json!({
                "project_id": fixture.older_project.id,
                "external_key": "retry-priority-with-running-project",
                "title": "retry-priority-with-running-project",
                "mime_type": "text/markdown",
                "text": "retry priority fixture",
                "file_kind": "md",
                "file_size_bytes": 128,
                "ingest_mode": "runtime_requeue",
                "extra_metadata": {},
            }),
        )
        .await
        .context("failed to create retried ingestion job")?;

        let claimed = repositories::claim_next_ingestion_job(
            &pool,
            "reserved-slot-worker",
            chrono::Duration::seconds(300),
            4,
            1,
        )
        .await
        .context("failed to claim reserved slot")?
        .context("expected reserved slot to claim one queued job")?;

        assert_eq!(claimed.id, quiet_project_job.id);
        assert_eq!(claimed.project_id, fixture.newer_project.id);
        assert_eq!(claimed.attempt_count, 1);

        let retried_job = repositories::get_ingestion_job_by_id(&pool, retried_job.id)
            .await
            .context("failed to reload retried queued job")?
            .context("retried queued job missing unexpectedly")?;
        assert_eq!(retried_job.status, "queued");

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres services"]
async fn higher_attempt_retries_are_claimed_before_lower_attempt_retries() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for retry-attempt ordering test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = QueueIsolationFixture::create(&pool).await?;

    let result = async {
        let lower_retry = repositories::create_ingestion_job(
            &pool,
            fixture.older_project.id,
            None,
            "runtime_upload",
            Some("queue-isolation-test"),
            None,
            None,
            Some(1),
            json!({
                "project_id": fixture.older_project.id,
                "external_key": "lower-retry",
                "title": "lower-retry",
                "mime_type": "text/markdown",
                "text": "lower retry fixture",
                "file_kind": "md",
                "file_size_bytes": 128,
                "ingest_mode": "runtime_requeue",
                "extra_metadata": {},
            }),
        )
        .await
        .context("failed to create lower retry ingestion job")?;

        let higher_retry = repositories::create_ingestion_job(
            &pool,
            fixture.older_project.id,
            None,
            "runtime_upload",
            Some("queue-isolation-test"),
            None,
            None,
            Some(2),
            json!({
                "project_id": fixture.older_project.id,
                "external_key": "higher-retry",
                "title": "higher-retry",
                "mime_type": "text/markdown",
                "text": "higher retry fixture",
                "file_kind": "md",
                "file_size_bytes": 128,
                "ingest_mode": "runtime_requeue",
                "extra_metadata": {},
            }),
        )
        .await
        .context("failed to create higher retry ingestion job")?;

        let claimed = repositories::claim_next_ingestion_job(
            &pool,
            "retry-attempt-ordering-worker",
            chrono::Duration::seconds(300),
            4,
            1,
        )
        .await
        .context("failed to claim retry-priority job")?
        .context("expected a queued retry job to be claimable")?;

        assert_eq!(claimed.id, higher_retry.id);
        assert_eq!(claimed.attempt_count, 3);

        let lower_still_queued = repositories::get_ingestion_job_by_id(&pool, lower_retry.id)
            .await
            .context("failed to reload lower retry job")?
            .context("lower retry job missing unexpectedly")?;
        assert_eq!(lower_still_queued.status, "queued");

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres services"]
async fn mcp_upload_jobs_are_claimed_before_bulk_retries() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp-priority ordering test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = QueueIsolationFixture::create(&pool).await?;

    let result = async {
        let bulk_retry = repositories::create_ingestion_job(
            &pool,
            fixture.older_project.id,
            None,
            "runtime_upload",
            Some("queue-isolation-test"),
            None,
            None,
            Some(2),
            json!({
                "project_id": fixture.older_project.id,
                "external_key": "bulk-retry",
                "title": "bulk-retry",
                "mime_type": "text/markdown",
                "text": "bulk retry fixture",
                "file_kind": "md",
                "file_size_bytes": 128,
                "ingest_mode": "runtime_requeue",
                "extra_metadata": {},
            }),
        )
        .await
        .context("failed to create bulk retry ingestion job")?;

        let interactive_job = repositories::create_ingestion_job(
            &pool,
            fixture.newer_project.id,
            None,
            "mcp_upload",
            Some("queue-isolation-test"),
            None,
            None,
            None,
            json!({
                "project_id": fixture.newer_project.id,
                "external_key": "mcp-priority",
                "title": "mcp-priority",
                "mime_type": "text/markdown",
                "text": "mcp priority fixture",
                "file_kind": "md",
                "file_size_bytes": 128,
                "ingest_mode": "runtime_upload",
                "extra_metadata": {},
            }),
        )
        .await
        .context("failed to create interactive mcp ingestion job")?;

        let claimed = repositories::claim_next_ingestion_job(
            &pool,
            "mcp-priority-ordering-worker",
            chrono::Duration::seconds(300),
            4,
            1,
        )
        .await
        .context("failed to claim priority ingestion job")?
        .context("expected a queued job to be claimable")?;

        assert_eq!(claimed.id, interactive_job.id);
        assert_eq!(claimed.trigger_kind, "mcp_upload");
        assert_eq!(claimed.attempt_count, 1);

        let bulk_still_queued = repositories::get_ingestion_job_by_id(&pool, bulk_retry.id)
            .await
            .context("failed to reload bulk retry job")?
            .context("bulk retry job missing unexpectedly")?;
        assert_eq!(bulk_still_queued.status, "queued");

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres services"]
async fn reserved_slot_allows_interactive_mcp_work_even_when_project_is_already_running()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp reserved-slot test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = QueueIsolationFixture::create(&pool).await?;

    let result = async {
        for index in 0..3 {
            let (_, job) =
                create_queued_runtime_job(&pool, &fixture.older_project, &format!("older-{index}"))
                    .await?;
            let claimed = repositories::claim_next_ingestion_job(
                &pool,
                &format!("older-worker-{index}"),
                chrono::Duration::seconds(300),
                4,
                1,
            )
            .await
            .context("failed to claim older backlog job")?
            .context("expected older backlog job to be claimable")?;
            assert_eq!(claimed.id, job.id);
            assert_eq!(claimed.project_id, fixture.older_project.id);
        }

        let interactive_job = repositories::create_ingestion_job(
            &pool,
            fixture.older_project.id,
            None,
            "mcp_append",
            Some("queue-isolation-test"),
            None,
            None,
            None,
            json!({
                "project_id": fixture.older_project.id,
                "external_key": "mcp-reserved-slot",
                "title": "mcp-reserved-slot",
                "mime_type": "text/markdown",
                "text": "interactive mcp fixture",
                "file_kind": "md",
                "file_size_bytes": 128,
                "ingest_mode": "runtime_append",
                "extra_metadata": {},
            }),
        )
        .await
        .context("failed to create interactive mcp append job")?;

        let claimed = repositories::claim_next_ingestion_job(
            &pool,
            "reserved-slot-worker",
            chrono::Duration::seconds(300),
            4,
            1,
        )
        .await
        .context("failed to claim interactive reserved slot")?
        .context("expected reserved slot to claim interactive mcp job")?;

        assert_eq!(claimed.id, interactive_job.id);
        assert_eq!(claimed.trigger_kind, "mcp_append");
        assert_eq!(claimed.project_id, fixture.older_project.id);
        assert_eq!(claimed.attempt_count, 1);

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres services"]
async fn newly_uploaded_library_claims_reserved_slot_before_older_backlog() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for queue isolation test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = QueueIsolationFixture::create(&pool).await?;

    let result = async {
        let mut older_jobs = Vec::new();
        for index in 0..4 {
            let (_, job) =
                create_queued_runtime_job(&pool, &fixture.older_project, &format!("older-{index}"))
                    .await?;
            older_jobs.push(job);
        }

        for worker_index in 0..3 {
            let claimed = repositories::claim_next_ingestion_job(
                &pool,
                &format!("older-worker-{worker_index}"),
                chrono::Duration::seconds(300),
                4,
                1,
            )
            .await
            .context("failed to claim older backlog job")?
            .context("expected older backlog job to be claimable")?;
            assert_eq!(claimed.project_id, fixture.older_project.id);
        }

        let (_, newer_job) =
            create_queued_runtime_job(&pool, &fixture.newer_project, "newer-queued").await?;

        let claimed = repositories::claim_next_ingestion_job(
            &pool,
            "reserved-slot-worker",
            chrono::Duration::seconds(300),
            4,
            1,
        )
        .await
        .context("failed to claim reserved slot")?
        .context("expected reserved slot to claim one queued job")?;
        assert_eq!(claimed.project_id, fixture.newer_project.id);
        assert_eq!(claimed.id, newer_job.id);

        let older_backlog_job = repositories::get_ingestion_job_by_id(&pool, older_jobs[3].id)
            .await
            .context("failed to reload older backlog job")?
            .context("older backlog job disappeared unexpectedly")?;
        assert_eq!(older_backlog_job.status, "queued");

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
