use std::{sync::Arc, time::Duration as StdDuration};

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::{
            bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
            client::ArangoClient,
        },
        persistence::Persistence,
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        ingest_service::{
            AdmitIngestJobCommand, FinalizeAttemptCommand, LeaseAttemptCommand,
            RecordStageEventCommand,
        },
        knowledge_service::{
            CreateKnowledgeDocumentCommand, CreateKnowledgeRevisionCommand,
            PromoteKnowledgeDocumentCommand, RefreshKnowledgeLibraryGenerationCommand,
        },
        ops_service::CreateAsyncOperationCommand,
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

struct TempArangoDatabase {
    base_url: String,
    username: String,
    password: String,
    name: String,
    http: reqwest::Client,
}

impl TempArangoDatabase {
    async fn create(settings: &Settings) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("ingest_attempts_{}", Uuid::now_v7().simple());
        let http = reqwest::Client::builder()
            .timeout(StdDuration::from_secs(settings.arangodb_request_timeout_seconds.max(1)))
            .build()
            .context("failed to build ArangoDB admin http client")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .context("failed to create temp ArangoDB database for ingest_attempts")?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to create temp ArangoDB database {}: status {}",
                name,
                response.status()
            ));
        }

        Ok(Self {
            base_url,
            username: settings.arangodb_username.clone(),
            password: settings.arangodb_password.clone(),
            name,
            http,
        })
    }

    async fn drop(self) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/_api/database/{}", self.base_url, self.name))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to drop temp ArangoDB database for ingest_attempts")?;
        if response.status() != reqwest::StatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to drop temp ArangoDB database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }
}

struct IngestAttemptsFixture {
    state: AppState,
    temp_database: TempDatabase,
    temp_arango: TempArangoDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    generation_id: Uuid,
}

impl IngestAttemptsFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for ingest_attempts test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let temp_arango = TempArangoDatabase::create(&settings).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.arangodb_database = temp_arango.name.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect ingest_attempts postgres")?;

        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&postgres)
            .await
            .context("failed to apply canonical 0001_init.sql for ingest_attempts test")?;

        let arango_client = Arc::new(
            ArangoClient::from_settings(&settings).context("failed to build arango client")?,
        );
        arango_client.ping().await.context("failed to ping temp ArangoDB for ingest_attempts")?;
        bootstrap_knowledge_plane(
            &arango_client,
            &ArangoBootstrapOptions {
                collections: true,
                views: false,
                graph: true,
                vector_indexes: false,
                vector_dimensions: 3072,
                vector_index_n_lists: 100,
                vector_index_default_n_probe: 8,
                vector_index_training_iterations: 25,
            },
        )
        .await
        .context("failed to bootstrap Arango knowledge plane for ingest_attempts")?;

        let persistence = Persistence {
            postgres,
            redis: redis::Client::open(settings.redis_url.clone())
                .context("failed to create redis client for ingest_attempts test state")?,
        };
        let state = AppState::from_dependencies(settings, persistence, arango_client);
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

        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        state
            .canonical_services
            .knowledge
            .create_document_shell(
                &state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    external_key: format!("ingest-attempts-doc-{}", Uuid::now_v7().simple()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create ingest_attempts document shell")?;
        state
            .canonical_services
            .knowledge
            .write_revision(
                &state,
                CreateKnowledgeRevisionCommand {
                    revision_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    document_id,
                    revision_number: 1,
                    revision_state: "active".to_string(),
                    revision_kind: "upload".to_string(),
                    storage_ref: Some(format!("memory://ingest-attempts/{revision_id}")),
                    source_uri: Some(format!("memory://ingest-attempts/source/{revision_id}")),
                    mime_type: "text/plain".to_string(),
                    checksum: format!("checksum-{revision_id}"),
                    byte_size: 128,
                    title: Some("Ingest Attempts Fixture".to_string()),
                    normalized_text: Some(
                        "Ingest attempts fixture text for readiness and async operation proof."
                            .to_string(),
                    ),
                    text_checksum: Some(format!("text-checksum-{revision_id}")),
                    text_state: "readable".to_string(),
                    vector_state: "pending".to_string(),
                    graph_state: "pending".to_string(),
                    text_readable_at: Some(Utc::now()),
                    vector_ready_at: None,
                    graph_ready_at: None,
                    superseded_by_revision_id: None,
                },
            )
            .await
            .context("failed to write ingest_attempts revision")?;
        state
            .canonical_services
            .knowledge
            .promote_document(
                &state,
                PromoteKnowledgeDocumentCommand {
                    document_id,
                    document_state: "active".to_string(),
                    active_revision_id: Some(revision_id),
                    readable_revision_id: Some(revision_id),
                    latest_revision_no: Some(1),
                    deleted_at: None,
                },
            )
            .await
            .context("failed to promote ingest_attempts document")?;

        let generation_id = Uuid::now_v7();
        state
            .canonical_services
            .knowledge
            .refresh_library_generation(
                &state,
                RefreshKnowledgeLibraryGenerationCommand {
                    generation_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    active_text_generation: 1,
                    active_vector_generation: 0,
                    active_graph_generation: 0,
                    degraded_state: "extracting".to_string(),
                },
            )
            .await
            .context("failed to refresh ingest_attempts knowledge generation")?;

        Ok(Self {
            state,
            temp_database,
            temp_arango,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id,
            revision_id,
            generation_id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_arango.drop().await?;
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
async fn canonical_ingest_attempts_preserve_queue_state_retry_and_stage_ordering() -> Result<()> {
    let fixture = IngestAttemptsFixture::create().await?;

    let result = async {
        let ingest = &fixture.state.canonical_services.ingest;
        let dedupe_key = format!("ingest-job-{}", Uuid::now_v7());
        let mutation_id = Some(Uuid::now_v7());
        let async_operation = fixture
            .state
            .canonical_services
            .ops
            .create_async_operation(
                &fixture.state,
                CreateAsyncOperationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "content_mutation".to_string(),
                    surface_kind: "rest".to_string(),
                    requested_by_principal_id: None,
                    status: "accepted".to_string(),
                    subject_kind: "knowledge_revision".to_string(),
                    subject_id: Some(fixture.revision_id),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await
            .context("failed to create async operation for ingest attempts")?;

        let job = ingest
            .admit_job(
                &fixture.state,
                AdmitIngestJobCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    mutation_id,
                    connector_id: None,
                    async_operation_id: Some(async_operation.id),
                    knowledge_document_id: Some(fixture.document_id),
                    knowledge_revision_id: Some(fixture.revision_id),
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
        assert_eq!(job.async_operation_id, Some(async_operation.id));
        assert_eq!(job.knowledge_document_id, Some(fixture.document_id));
        assert_eq!(job.knowledge_revision_id, Some(fixture.revision_id));

        let admitted_handle = ingest
            .get_job_handle(&fixture.state, job.id)
            .await
            .context("failed to load admitted ingest job handle")?;
        assert_eq!(admitted_handle.job.id, job.id);
        assert_eq!(
            admitted_handle.async_operation.as_ref().map(|operation| operation.status.as_str()),
            Some("accepted")
        );

        let deduped = ingest
            .admit_job(
                &fixture.state,
                AdmitIngestJobCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    mutation_id,
                    connector_id: None,
                    async_operation_id: Some(async_operation.id),
                    knowledge_document_id: Some(fixture.document_id),
                    knowledge_revision_id: Some(fixture.revision_id),
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
                    knowledge_generation_id: Some(fixture.generation_id),
                    current_stage: Some("queued".to_string()),
                },
            )
            .await
            .context("failed to lease first attempt")?;
        assert_eq!(first_attempt.attempt_number, 1);
        assert_eq!(first_attempt.worker_principal_id, Some(worker_a));
        assert_eq!(first_attempt.attempt_state, "leased");
        assert_eq!(first_attempt.knowledge_generation_id, Some(fixture.generation_id));

        let leased_handle = ingest
            .get_attempt_handle(&fixture.state, first_attempt.id)
            .await
            .context("failed to load leased attempt handle")?;
        assert_eq!(leased_handle.job.id, job.id);
        assert_eq!(leased_handle.attempt.knowledge_generation_id, Some(fixture.generation_id));
        assert_eq!(
            leased_handle.async_operation.as_ref().map(|operation| operation.status.as_str()),
            Some("processing")
        );

        let _ = ingest
            .heartbeat_attempt(
                &fixture.state,
                rustrag_backend::services::ingest_service::HeartbeatAttemptCommand {
                    attempt_id: first_attempt.id,
                    knowledge_generation_id: Some(fixture.generation_id),
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
                    knowledge_generation_id: Some(fixture.generation_id),
                    attempt_state: "failed".to_string(),
                    current_stage: Some("extracting".to_string()),
                    failure_class: Some("lease_lost".to_string()),
                    failure_code: Some("lease_lost".to_string()),
                    retryable: true,
                },
            )
            .await
            .context("failed to finalize retryable first attempt")?;
        assert_eq!(first_attempt.attempt_state, "failed");
        assert_eq!(first_attempt.failure_class.as_deref(), Some("lease_lost"));
        assert!(first_attempt.retryable);

        let queued_job = ingest.get_job(&fixture.state, job.id).await?;
        assert_eq!(queued_job.queue_state, "queued");
        assert!(queued_job.completed_at.is_none());
        assert_eq!(queued_job.knowledge_document_id, Some(fixture.document_id));
        assert_eq!(queued_job.knowledge_revision_id, Some(fixture.revision_id));

        let reaccepted_handle = ingest
            .get_job_handle(&fixture.state, job.id)
            .await
            .context("failed to reload requeued ingest job handle")?;
        assert_eq!(
            reaccepted_handle.async_operation.as_ref().map(|operation| operation.status.as_str()),
            Some("accepted")
        );

        let worker_b = Uuid::now_v7();
        let second_attempt = ingest
            .lease_attempt(
                &fixture.state,
                LeaseAttemptCommand {
                    job_id: job.id,
                    worker_principal_id: Some(worker_b),
                    lease_token: Some("lease-b".to_string()),
                    knowledge_generation_id: Some(fixture.generation_id),
                    current_stage: Some("extracting".to_string()),
                },
            )
            .await
            .context("failed to lease second attempt")?;
        assert_eq!(second_attempt.attempt_number, 2);
        assert_eq!(second_attempt.worker_principal_id, Some(worker_b));
        assert_eq!(second_attempt.knowledge_generation_id, Some(fixture.generation_id));

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
                    knowledge_generation_id: Some(fixture.generation_id),
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
        assert_eq!(completed_job.knowledge_document_id, Some(fixture.document_id));
        assert_eq!(completed_job.knowledge_revision_id, Some(fixture.revision_id));

        let completed_handle = ingest
            .get_attempt_handle(&fixture.state, second_attempt.id)
            .await
            .context("failed to load completed attempt handle")?;
        assert_eq!(
            completed_handle.async_operation.as_ref().map(|operation| operation.status.as_str()),
            Some("ready")
        );
        assert!(completed_handle
            .async_operation
            .as_ref()
            .and_then(|operation| operation.completed_at)
            .is_some());
        assert_eq!(completed_handle.attempt.knowledge_generation_id, Some(fixture.generation_id));

        let requeued = ingest
            .retry_job(&fixture.state, job.id, Some(Utc::now() + Duration::seconds(5)))
            .await
            .context("failed to requeue completed job for explicit retry request")?;
        assert_eq!(requeued.queue_state, "queued");
        assert!(requeued.available_at > Utc::now());

        let retried_handle = ingest
            .get_job_handle(&fixture.state, job.id)
            .await
            .context("failed to load retried ingest job handle")?;
        assert_eq!(
            retried_handle.async_operation.as_ref().map(|operation| operation.status.as_str()),
            Some("accepted")
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango"]
async fn ingest_attempt_emits_stage_events_for_worker_progress() -> Result<()> {
    let fixture = IngestAttemptsFixture::create().await?;

    let result = async {
        let ingest = &fixture.state.canonical_services.ingest;
        let job = ingest
            .admit_job(
                &fixture.state,
                AdmitIngestJobCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    mutation_id: None,
                    connector_id: None,
                    async_operation_id: None,
                    knowledge_document_id: Some(fixture.document_id),
                    knowledge_revision_id: Some(fixture.revision_id),
                    job_kind: "content_mutation".to_string(),
                    priority: 50,
                    dedupe_key: Some(format!("stage-events-{}", Uuid::now_v7())),
                    available_at: None,
                },
            )
            .await
            .context("failed to admit stage-events ingest job")?;
        let attempt = ingest
            .lease_attempt(
                &fixture.state,
                LeaseAttemptCommand {
                    job_id: job.id,
                    worker_principal_id: None,
                    lease_token: Some("stage-events-lease".to_string()),
                    knowledge_generation_id: Some(fixture.generation_id),
                    current_stage: Some("queued".to_string()),
                },
            )
            .await
            .context("failed to lease stage-events attempt")?;

        for (stage_name, stage_state) in [("queued", "started"), ("extracting", "started")] {
            let _ = ingest
                .record_stage_event(
                    &fixture.state,
                    RecordStageEventCommand {
                        attempt_id: attempt.id,
                        stage_name: stage_name.to_string(),
                        stage_state: stage_state.to_string(),
                        message: Some(format!("{stage_name} {stage_state}")),
                        details_json: serde_json::json!({
                            "stage": stage_name,
                            "state": stage_state,
                        }),
                    },
                )
                .await
                .with_context(|| {
                    format!("failed to record stage event {stage_name}/{stage_state}")
                })?;
        }

        let events = ingest
            .list_stage_events(&fixture.state, attempt.id)
            .await
            .context("failed to list emitted stage events")?;
        assert!(events.len() >= 2);
        assert!(events.iter().any(|event| event.stage_name == "queued"));
        assert!(events.iter().any(|event| event.stage_name == "extracting"));
        assert!(events.iter().all(|event| event.attempt_id == attempt.id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
