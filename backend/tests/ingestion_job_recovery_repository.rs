use anyhow::Context;
use chrono::{Duration, Utc};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    infra::repositories::{self, ProjectRow, WorkspaceRow},
};

struct IngestionJobRecoveryFixture {
    workspace: WorkspaceRow,
    project: ProjectRow,
}

impl IngestionJobRecoveryFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            pool,
            &format!("ingestion-recovery-{suffix}"),
            "Ingestion Recovery Repository",
        )
        .await
        .context("failed to create ingestion recovery test workspace")?;
        let project = repositories::create_project(
            pool,
            workspace.id,
            &format!("recovery-library-{suffix}"),
            "Ingestion Recovery Repository Library",
            Some("ingestion recovery repository regression fixture"),
        )
        .await
        .context("failed to create ingestion recovery test project")?;

        Ok(Self { workspace, project })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace.id)
            .execute(pool)
            .await
            .context("failed to delete ingestion recovery test workspace")?;
        Ok(())
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect ingestion recovery test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for ingestion recovery test")?;
    Ok(pool)
}

async fn seed_running_job(
    pool: &PgPool,
    project_id: Uuid,
    worker_id: &str,
    stage: &str,
    lease_expires_at: Option<chrono::DateTime<Utc>>,
    heartbeat_at: Option<chrono::DateTime<Utc>>,
) -> anyhow::Result<repositories::IngestionJobRow> {
    let job = repositories::create_ingestion_job(
        pool,
        project_id,
        None,
        "runtime_upload",
        Some("recovery-test"),
        None,
        None,
        None,
        serde_json::json!({}),
    )
    .await
    .context("failed to create ingestion job")?;

    repositories::record_ingestion_job_attempt_claim(pool, job.id, 1, worker_id, stage)
        .await
        .context("failed to create ingestion job attempt claim")?;

    sqlx::query(
        "update ingestion_job
         set status = 'running',
             stage = $2,
             started_at = now() - interval '2 minutes',
             updated_at = now() - interval '1 minute',
             attempt_count = 1,
             worker_id = $3,
             lease_expires_at = $4,
             heartbeat_at = $5
         where id = $1",
    )
    .bind(job.id)
    .bind(stage)
    .bind(worker_id)
    .bind(lease_expires_at)
    .bind(heartbeat_at)
    .execute(pool)
    .await
    .context("failed to mark ingestion job as running")?;

    repositories::get_ingestion_job_by_id(pool, job.id)
        .await
        .context("failed to reload seeded ingestion job")?
        .context("seeded ingestion job missing after update")
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn recover_expired_leases_returns_previous_worker_state_and_allows_attempt_failure()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for ingestion recovery test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = IngestionJobRecoveryFixture::create(&pool).await?;

    let result = async {
        let job = seed_running_job(
            &pool,
            fixture.project.id,
            "worker-alpha",
            "extracting_content",
            Some(Utc::now() - Duration::minutes(1)),
            Some(Utc::now() - Duration::seconds(30)),
        )
        .await?;

        let recovered = repositories::recover_expired_ingestion_job_leases(&pool)
            .await
            .context("failed to recover expired ingestion job leases")?;
        assert_eq!(recovered.len(), 1);

        let recovered_job = &recovered[0];
        assert_eq!(recovered_job.id, job.id);
        assert_eq!(recovered_job.previous_status, "running");
        assert_eq!(recovered_job.previous_stage, "extracting_content");
        assert_eq!(recovered_job.previous_worker_id.as_deref(), Some("worker-alpha"));
        assert_eq!(recovered_job.status, "queued");
        assert_eq!(recovered_job.stage, "requeued_after_lease_expiry");
        assert!(recovered_job.worker_id.is_none());
        assert!(recovered_job.lease_expires_at.is_none());

        repositories::fail_ingestion_job_attempt(
            &pool,
            recovered_job.id,
            recovered_job.attempt_count,
            recovered_job.attempt_worker_id("lease-recovery"),
            "lease_expired",
            "job lease expired before completion; requeued for retry",
        )
        .await
        .context("failed to mark recovered ingestion job attempt as retryable_failed")?;

        let attempts = repositories::list_ingestion_job_attempts(&pool, recovered_job.id)
            .await
            .context("failed to list ingestion job attempts")?;
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, "retryable_failed");
        assert_eq!(attempts[0].stage, "lease_expired");
        assert_eq!(attempts[0].worker_id.as_deref(), Some("worker-alpha"));

        let queued_job = repositories::get_ingestion_job_by_id(&pool, recovered_job.id)
            .await
            .context("failed to reload recovered ingestion job")?
            .context("recovered ingestion job missing")?;
        assert_eq!(queued_job.status, "queued");
        assert_eq!(queued_job.stage, "requeued_after_lease_expiry");
        assert!(queued_job.worker_id.is_none());

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn recover_stale_heartbeats_returns_previous_running_state() -> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for ingestion recovery stale heartbeat test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = IngestionJobRecoveryFixture::create(&pool).await?;

    let result = async {
        let job = seed_running_job(
            &pool,
            fixture.project.id,
            "worker-beta",
            "projecting_graph",
            Some(Utc::now() + Duration::minutes(5)),
            Some(Utc::now() - Duration::minutes(10)),
        )
        .await?;
        let stale_before = Utc::now() - Duration::minutes(5);

        let recovered = repositories::recover_stale_ingestion_job_heartbeats(&pool, stale_before)
            .await
            .context("failed to recover stale ingestion job heartbeats")?;
        assert_eq!(recovered.len(), 1);

        let recovered_job = &recovered[0];
        assert_eq!(recovered_job.id, job.id);
        assert_eq!(recovered_job.previous_status, "running");
        assert_eq!(recovered_job.previous_stage, "projecting_graph");
        assert_eq!(recovered_job.previous_worker_id.as_deref(), Some("worker-beta"));
        assert_eq!(recovered_job.status, "queued");
        assert_eq!(recovered_job.stage, "requeued_after_stale_heartbeat");
        assert!(recovered_job.worker_id.is_none());
        assert!(recovered_job.lease_expires_at.is_none());
        assert!(
            recovered_job.previous_heartbeat_at.is_some_and(|heartbeat| heartbeat < stale_before)
        );

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
