use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        graph_store::{
            GraphProjectionData, GraphProjectionEdgeWrite, GraphProjectionNodeWrite,
            GraphProjectionWriteError, GraphStore,
        },
        persistence::Persistence,
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        ingest_service::{
            AdmitIngestJobCommand, FinalizeAttemptCommand, LeaseAttemptCommand,
            RecordStageEventCommand,
        },
    },
};

struct NoopGraphStore;

#[async_trait]
impl GraphStore for NoopGraphStore {
    fn backend_name(&self) -> &'static str {
        "noop"
    }

    async fn ping(&self) -> Result<()> {
        Ok(())
    }

    async fn replace_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphProjectionNodeWrite],
        _edges: &[GraphProjectionEdgeWrite],
    ) -> Result<(), GraphProjectionWriteError> {
        Ok(())
    }

    async fn refresh_library_projection_targets(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _remove_node_ids: &[Uuid],
        _remove_edge_ids: &[Uuid],
        _nodes: &[GraphProjectionNodeWrite],
        _edges: &[GraphProjectionEdgeWrite],
    ) -> Result<(), GraphProjectionWriteError> {
        Ok(())
    }

    async fn load_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
    ) -> Result<GraphProjectionData> {
        Ok(GraphProjectionData::default())
    }
}

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("ingest_attempts_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect admin postgres for ingest_attempts test")?;

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
            .context("failed to reconnect admin postgres for ingest_attempts cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct IngestAttemptsFixture {
    state: AppState,
    temp_database: TempDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl IngestAttemptsFixture {
    async fn create() -> Result<Self> {
        let settings =
            Settings::from_env().context("failed to load settings for ingest_attempts test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&temp_database.database_url)
            .await
            .context("failed to connect ingest_attempts postgres")?;

        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&postgres)
            .await
            .context("failed to apply canonical 0001_init.sql for ingest_attempts test")?;

        let state = build_test_state(settings, postgres)?;
        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("ingest-workspace-{}", Uuid::now_v7().simple())),
                    display_name: "Ingest Attempts Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create ingest_attempts workspace")?;
        let library = state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("ingest-library-{}", Uuid::now_v7().simple())),
                    display_name: "Ingest Attempts Library".to_string(),
                    description: Some("canonical ingest attempt test fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create ingest_attempts library")?;

        Ok(Self { state, temp_database, workspace_id: workspace.id, library_id: library.id })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let persistence = Persistence {
        postgres,
        redis: redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for ingest_attempts test state")?,
    };
    Ok(AppState::from_dependencies(settings, persistence, Arc::new(NoopGraphStore)))
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
async fn canonical_ingest_attempts_preserve_queue_state_retry_and_stage_ordering() -> Result<()> {
    let fixture = IngestAttemptsFixture::create().await?;

    let result = async {
        let ingest = &fixture.state.canonical_services.ingest;
        let dedupe_key = format!("ingest-job-{}", Uuid::now_v7());
        let mutation_id = Some(Uuid::now_v7());

        let job = ingest
            .admit_job(
                &fixture.state,
                AdmitIngestJobCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    mutation_id,
                    connector_id: None,
                    job_kind: "content_mutation".to_string(),
                    priority: 100,
                    dedupe_key: Some(dedupe_key.clone()),
                    available_at: None,
                },
            )
            .await
            .context("failed to admit ingest job")?;
        assert_eq!(job.queue_state, "queued");
        assert_eq!(job.priority, 100);
        assert_eq!(job.mutation_id, mutation_id);

        let deduped = ingest
            .admit_job(
                &fixture.state,
                AdmitIngestJobCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    mutation_id,
                    connector_id: None,
                    job_kind: "content_mutation".to_string(),
                    priority: 5,
                    dedupe_key: Some(dedupe_key),
                    available_at: None,
                },
            )
            .await
            .context("failed to re-admit deduped ingest job")?;
        assert_eq!(deduped.id, job.id);

        let worker_a = Uuid::now_v7();
        let first_attempt = ingest
            .lease_attempt(
                &fixture.state,
                LeaseAttemptCommand {
                    job_id: job.id,
                    worker_principal_id: Some(worker_a),
                    lease_token: Some("lease-a".to_string()),
                    current_stage: Some("queued".to_string()),
                },
            )
            .await
            .context("failed to lease first attempt")?;
        assert_eq!(first_attempt.attempt_number, 1);
        assert_eq!(first_attempt.worker_principal_id, Some(worker_a));
        assert_eq!(first_attempt.attempt_state, "leased");

        let _ = ingest
            .heartbeat_attempt(
                &fixture.state,
                rustrag_backend::services::ingest_service::HeartbeatAttemptCommand {
                    attempt_id: first_attempt.id,
                    current_stage: Some("extracting".to_string()),
                },
            )
            .await
            .context("failed to heartbeat first attempt")?;

        for (stage_name, stage_state, message) in [
            ("queued", "started", Some("job admitted")),
            ("extracting", "started", Some("worker started extraction")),
            ("extracting", "failed", Some("lease lost before completion")),
        ] {
            let _ = ingest
                .record_stage_event(
                    &fixture.state,
                    RecordStageEventCommand {
                        attempt_id: first_attempt.id,
                        stage_name: stage_name.to_string(),
                        stage_state: stage_state.to_string(),
                        message: message.map(ToString::to_string),
                        details_json: serde_json::json!({ "stage": stage_name, "state": stage_state }),
                    },
                )
                .await
                .with_context(|| format!("failed to record stage event {stage_name}/{stage_state}"))?;
        }

        let stages = ingest
            .list_stage_events(&fixture.state, first_attempt.id)
            .await
            .context("failed to list first-attempt stages")?;
        assert_eq!(stages.len(), 3);
        assert_eq!(stages[0].ordinal, 1);
        assert_eq!(stages[1].ordinal, 2);
        assert_eq!(stages[2].ordinal, 3);
        assert_eq!(stages[0].stage_name, "queued");
        assert_eq!(stages[1].stage_name, "extracting");
        assert_eq!(stages[2].stage_state, "failed");

        let first_attempt = ingest
            .finalize_attempt(
                &fixture.state,
                FinalizeAttemptCommand {
                    attempt_id: first_attempt.id,
                    attempt_state: "abandoned".to_string(),
                    current_stage: Some("extracting".to_string()),
                    failure_class: Some("lease_lost".to_string()),
                    failure_code: Some("lease_lost".to_string()),
                    retryable: true,
                },
            )
            .await
            .context("failed to finalize abandoned first attempt")?;
        assert_eq!(first_attempt.attempt_state, "abandoned");
        assert_eq!(first_attempt.failure_class.as_deref(), Some("lease_lost"));
        assert!(first_attempt.retryable);

        let queued_job = ingest.get_job(&fixture.state, job.id).await?;
        assert_eq!(queued_job.queue_state, "queued");
        assert!(queued_job.completed_at.is_none());

        let worker_b = Uuid::now_v7();
        let second_attempt = ingest
            .lease_attempt(
                &fixture.state,
                LeaseAttemptCommand {
                    job_id: job.id,
                    worker_principal_id: Some(worker_b),
                    lease_token: Some("lease-b".to_string()),
                    current_stage: Some("extracting".to_string()),
                },
            )
            .await
            .context("failed to lease second attempt")?;
        assert_eq!(second_attempt.attempt_number, 2);
        assert_eq!(second_attempt.worker_principal_id, Some(worker_b));

        let _ = ingest
            .record_stage_event(
                &fixture.state,
                RecordStageEventCommand {
                    attempt_id: second_attempt.id,
                    stage_name: "extracting".to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("retry worker completed extraction".to_string()),
                    details_json: serde_json::json!({ "worker": worker_b }),
                },
            )
            .await
            .context("failed to record retry completion stage")?;

        let second_attempt = ingest
            .finalize_attempt(
                &fixture.state,
                FinalizeAttemptCommand {
                    attempt_id: second_attempt.id,
                    attempt_state: "succeeded".to_string(),
                    current_stage: Some("finalizing".to_string()),
                    failure_class: None,
                    failure_code: None,
                    retryable: false,
                },
            )
            .await
            .context("failed to finalize successful retry attempt")?;
        assert_eq!(second_attempt.attempt_state, "succeeded");
        assert_eq!(second_attempt.worker_principal_id, Some(worker_b));
        assert!(second_attempt.finished_at.is_some());

        let attempts = ingest
            .list_attempts(&fixture.state, job.id)
            .await
            .context("failed to list attempts")?;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].attempt_number, 2);
        assert_eq!(attempts[1].attempt_number, 1);
        assert_eq!(attempts[1].failure_class.as_deref(), Some("lease_lost"));

        let completed_job = ingest.get_job(&fixture.state, job.id).await?;
        assert_eq!(completed_job.queue_state, "completed");
        assert!(completed_job.completed_at.is_some());

        let requeued = ingest
            .retry_job(&fixture.state, job.id, Some(Utc::now() + Duration::seconds(5)))
            .await
            .context("failed to requeue completed job for explicit retry request")?;
        assert_eq!(requeued.queue_state, "queued");
        assert!(requeued.available_at > Utc::now());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
