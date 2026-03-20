use anyhow::Context;
use chrono::Duration as ChronoDuration;
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    infra::repositories::{
        self, ApiTokenRow, IngestionJobRow, ProjectRow, RuntimeGraphProgressCheckpointInput,
        RuntimeIngestionRunRow, WorkspaceRow,
    },
};

struct GraphThroughputFixture {
    workspace: WorkspaceRow,
    project: ProjectRow,
}

impl GraphThroughputFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            pool,
            &format!("graph-throughput-test-{suffix}"),
            "Graph Throughput Test",
        )
        .await
        .context("failed to create graph throughput test workspace")?;
        let project = repositories::create_project(
            pool,
            workspace.id,
            &format!("graph-throughput-library-{suffix}"),
            "Graph Throughput Library",
            Some("graph throughput pressure regression"),
        )
        .await
        .context("failed to create graph throughput test library")?;

        Ok(Self { workspace, project })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace.id)
            .execute(pool)
            .await
            .context("failed to delete graph throughput test workspace")?;
        Ok(())
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect graph throughput test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for graph throughput test")?;
    Ok(pool)
}

async fn create_processing_runtime_run(
    pool: &PgPool,
    project: &ProjectRow,
    file_stem: &str,
) -> anyhow::Result<RuntimeIngestionRunRow> {
    repositories::create_runtime_ingestion_run(
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
        "processing",
        "extracting_graph",
        "initial_upload",
        json!({}),
    )
    .await
    .with_context(|| format!("failed to create runtime run for {file_stem}"))
}

async fn create_queued_job(
    pool: &PgPool,
    project: &ProjectRow,
    runtime_run: &RuntimeIngestionRunRow,
    file_stem: &str,
) -> anyhow::Result<IngestionJobRow> {
    repositories::create_ingestion_job(
        pool,
        project.id,
        None,
        "runtime_upload",
        Some("graph-throughput-test"),
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
            "text": "graph throughput fixture",
            "file_kind": "md",
            "file_size_bytes": 256,
            "ingest_mode": "runtime_upload",
            "extra_metadata": {},
        }),
    )
    .await
    .with_context(|| format!("failed to create ingestion job for {file_stem}"))
}

async fn create_api_token(pool: &PgPool, workspace: &WorkspaceRow) -> anyhow::Result<ApiTokenRow> {
    repositories::create_api_token(
        pool,
        Some(workspace.id),
        "runtime",
        "graph-throughput-token",
        "hash",
        Some("rt-test"),
        json!({
            "workspaces": [workspace.id],
        }),
        None,
    )
    .await
    .context("failed to create graph throughput api token")
}

fn sample_checkpoint(
    runtime_run: &RuntimeIngestionRunRow,
    attempt_no: i32,
    processed_chunks: i64,
    total_chunks: i64,
    computed_at: chrono::DateTime<chrono::Utc>,
) -> RuntimeGraphProgressCheckpointInput {
    RuntimeGraphProgressCheckpointInput {
        ingestion_run_id: runtime_run.id,
        attempt_no,
        processed_chunks,
        total_chunks,
        progress_percent: Some(((processed_chunks as f64 / total_chunks as f64) * 100.0) as i32),
        provider_call_count: processed_chunks,
        avg_call_elapsed_ms: Some(1_500),
        avg_chunk_elapsed_ms: Some(3_000),
        avg_chars_per_second: Some(900.0),
        avg_tokens_per_second: Some(320.0),
        last_provider_call_at: Some(computed_at),
        next_checkpoint_eta_ms: Some(6_000),
        pressure_kind: Some("steady".to_string()),
        computed_at,
    }
}

#[tokio::test]
#[ignore = "requires local postgres services"]
async fn throttles_token_touch_and_heartbeat_without_cross_attempt_checkpoint_loss()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for graph throughput test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = GraphThroughputFixture::create(&pool).await?;

    let result = async {
        let token = create_api_token(&pool, &fixture.workspace).await?;
        assert!(
            repositories::touch_api_token_last_used(&pool, token.id, 3600)
                .await
                .context("failed to perform initial token touch")?
        );
        let after_first_touch = repositories::get_api_token_by_id(&pool, token.id)
            .await
            .context("failed to reload api token after first touch")?
            .context("api token disappeared after first touch")?;
        assert!(after_first_touch.last_used_at.is_some());

        assert!(
            !repositories::touch_api_token_last_used(&pool, token.id, 3600)
                .await
                .context("failed to perform throttled token touch")?
        );
        let after_second_touch = repositories::get_api_token_by_id(&pool, token.id)
            .await
            .context("failed to reload api token after throttled touch")?
            .context("api token disappeared after throttled touch")?;
        assert_eq!(after_second_touch.last_used_at, after_first_touch.last_used_at);

        let runtime_run =
            create_processing_runtime_run(&pool, &fixture.project, "heartbeat-source").await?;
        let queued_job =
            create_queued_job(&pool, &fixture.project, &runtime_run, "heartbeat-job").await?;
        let claimed_job = repositories::claim_next_ingestion_job(
            &pool,
            "graph-throughput-worker",
            ChronoDuration::seconds(300),
            4,
            1,
        )
        .await
        .context("failed to claim queued job")?
        .context("expected queued job to be claimable")?;
        assert_eq!(claimed_job.id, queued_job.id);

        let first_renewed = repositories::renew_ingestion_job_lease(
            &pool,
            claimed_job.id,
            "graph-throughput-worker",
            ChronoDuration::seconds(300),
            3600,
        )
        .await
        .context("failed to renew claimed job lease the first time")?;
        assert_eq!(first_renewed, repositories::LeaseRenewalOutcome::Renewed);
        let first_job_state = repositories::get_ingestion_job_by_id(&pool, claimed_job.id)
            .await
            .context("failed to reload claimed job after first renew")?
            .context("claimed job disappeared after first renew")?;
        let first_heartbeat_at =
            first_job_state.heartbeat_at.context("first renew did not set heartbeat_at")?;

        let second_renewed = repositories::renew_ingestion_job_lease(
            &pool,
            claimed_job.id,
            "graph-throughput-worker",
            ChronoDuration::seconds(300),
            3600,
        )
        .await
        .context("failed to renew claimed job lease the second time")?;
        assert_eq!(second_renewed, repositories::LeaseRenewalOutcome::Renewed);
        let second_job_state = repositories::get_ingestion_job_by_id(&pool, claimed_job.id)
            .await
            .context("failed to reload claimed job after second renew")?
            .context("claimed job disappeared after second renew")?;
        assert_eq!(second_job_state.heartbeat_at, Some(first_heartbeat_at));

        let first_checkpoint = repositories::upsert_runtime_graph_progress_checkpoint(
            &pool,
            &sample_checkpoint(&runtime_run, 1, 5, 20, chrono::Utc::now()),
        )
        .await
        .context("failed to persist attempt-1 checkpoint")?;
        let second_checkpoint = repositories::upsert_runtime_graph_progress_checkpoint(
            &pool,
            &sample_checkpoint(&runtime_run, 2, 3, 20, chrono::Utc::now()),
        )
        .await
        .context("failed to persist attempt-2 checkpoint")?;

        let reloaded_first =
            repositories::get_runtime_graph_progress_checkpoint(&pool, runtime_run.id, 1)
                .await
                .context("failed to reload attempt-1 checkpoint")?
                .context("attempt-1 checkpoint disappeared")?;
        let reloaded_second =
            repositories::get_runtime_graph_progress_checkpoint(&pool, runtime_run.id, 2)
                .await
                .context("failed to reload attempt-2 checkpoint")?
                .context("attempt-2 checkpoint disappeared")?;

        assert_eq!(reloaded_first.processed_chunks, first_checkpoint.processed_chunks);
        assert_eq!(reloaded_second.processed_chunks, second_checkpoint.processed_chunks);
        assert_ne!(reloaded_first.attempt_no, reloaded_second.attempt_no);

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
