#[cfg(feature = "test-support")]
#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sqlx::{AssertSqlSafe, PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{persistence::Persistence, repositories::ingest_repository},
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::{CreateDocumentCommand, CreateRevisionCommand, PromoteHeadCommand},
        ingest::service::{
            AdmitIngestJobCommand, FinalizeAttemptCommand, INGEST_STAGE_CHUNK_CONTENT,
            INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_FINALIZING, LeaseAttemptCommand,
            RecordStageEventCommand,
        },
        knowledge::service::CreateKnowledgeRevisionCommand,
    },
};

#[cfg(feature = "test-support")]
use ironrag_backend::{
    infra::repositories::ingest_repository::NewWebDiscoveredPage,
    services::ingest::web::CreateWebIngestRunCommand,
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
            .context("failed to reconnect admin postgres for ingest_attempts cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
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
    document_id: Uuid,
    revision_id: Uuid,
    generation_id: Uuid,
}

impl IngestAttemptsFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for ingest_attempts test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect ingest_attempts postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply canonical baseline migrations for ingest_attempts")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for ingest_attempts test state")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = AppState::from_dependencies(settings, persistence)?;
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

        let document = state
            .canonical_services
            .content
            .create_document(
                &state,
                CreateDocumentCommand {
                    workspace_id: workspace.id,
                    library_id: library.id,
                    external_key: Some(format!("ingest-attempts-doc-{}", Uuid::now_v7().simple())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create ingest_attempts content document")?;
        let document_id = document.id;
        let content_revision = state
            .canonical_services
            .content
            .create_revision(
                &state,
                CreateRevisionCommand {
                    document_id,
                    content_source_kind: "upload".to_string(),
                    checksum: format!("checksum-{document_id}"),
                    mime_type: "text/plain".to_string(),
                    byte_size: 128,
                    title: Some("Ingest Attempts Fixture".to_string()),
                    language_code: None,
                    source_uri: Some(format!("memory://ingest-attempts/source/{document_id}")),
                    document_hint: None,
                    storage_key: Some(format!("memory://ingest-attempts/{document_id}")),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create ingest_attempts content revision")?;
        let revision_id = content_revision.id;
        let generation_id = Uuid::now_v7();
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
                    document_hint: None,
                    mime_type: "text/plain".to_string(),
                    checksum: format!("checksum-{revision_id}"),
                    byte_size: 128,
                    title: Some("Ingest Attempts Fixture".to_string()),
                    normalized_text: Some(
                        "Ingest attempts fixture text for readiness and async operation proof."
                            .to_string(),
                    ),
                    text_checksum: Some(format!("text-checksum-{revision_id}")),
                    text_state: "text_readable".to_string(),
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
            .content
            .promote_document_head(
                &state,
                PromoteHeadCommand {
                    document_id,
                    active_revision_id: Some(revision_id),
                    readable_revision_id: Some(revision_id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote ingest_attempts document")?;

        Ok(Self {
            state,
            temp_database,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id,
            revision_id,
            generation_id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
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
        let mutation_id = None;

        let job = ingest
            .admit_job(
                &fixture.state,
                AdmitIngestJobCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    mutation_id,
                    mutation_item_id: None,
                    connector_id: None,
                    async_operation_id: None,
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
        assert_eq!(job.async_operation_id, None);
        assert_eq!(job.knowledge_document_id, Some(fixture.document_id));
        assert_eq!(job.knowledge_revision_id, Some(fixture.revision_id));

        let admitted_handle = ingest
            .get_job_handle(&fixture.state, job.id)
            .await
            .context("failed to load admitted ingest job handle")?;
        assert_eq!(admitted_handle.job.id, job.id);

        let deduped = ingest
            .admit_job(
                &fixture.state,
                AdmitIngestJobCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    mutation_id,
                    mutation_item_id: None,
                    connector_id: None,
                    async_operation_id: None,
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

        let queue_claim_token_a = format!("ingest-attempts-claim-a-{}", Uuid::now_v7().simple());
        let claimed_first_job = ingest_repository::claim_next_queued_ingest_job(
            &fixture.state.persistence.postgres,
            &queue_claim_token_a,
            "ingest-attempts-test",
            10,
            10,
            10,
        )
        .await
        .context("failed to claim first ingest job")?
        .context("expected to claim first ingest job")?;
        assert_eq!(claimed_first_job.id, job.id);
        let first_queue_lease_token = claimed_first_job
            .queue_lease_token
            .context("claimed first ingest job missing queue lease token")?;

        let first_attempt = ingest
            .lease_attempt(
                &fixture.state,
                LeaseAttemptCommand {
                    job_id: job.id,
                    worker_principal_id: None,
                    lease_token: Some("lease-a".to_string()),
                    expected_queue_lease_token: Some(first_queue_lease_token),
                    knowledge_generation_id: Some(fixture.generation_id),
                    current_stage: Some(INGEST_STAGE_EXTRACT_CONTENT.to_string()),
                },
            )
            .await
            .context("failed to lease first attempt")?;
        assert_eq!(first_attempt.attempt_number, 1);
        assert_eq!(first_attempt.worker_principal_id, None);
        assert_eq!(first_attempt.attempt_state, "leased");
        assert_eq!(first_attempt.knowledge_generation_id, Some(fixture.generation_id));

        let leased_handle = ingest
            .get_attempt_handle(&fixture.state, first_attempt.id)
            .await
            .context("failed to load leased attempt handle")?;
        assert_eq!(leased_handle.job.id, job.id);
        assert_eq!(leased_handle.attempt.knowledge_generation_id, Some(fixture.generation_id));

        let _ = ingest
            .heartbeat_attempt(
                &fixture.state,
                ironrag_backend::services::ingest::service::HeartbeatAttemptCommand {
                    attempt_id: first_attempt.id,
                    knowledge_generation_id: Some(fixture.generation_id),
                    current_stage: Some(INGEST_STAGE_CHUNK_CONTENT.to_string()),
                },
            )
            .await
            .context("failed to heartbeat first attempt")?;

        for (stage_name, stage_state, message) in [
            (INGEST_STAGE_EXTRACT_CONTENT, "started", Some("job admitted")),
            (INGEST_STAGE_CHUNK_CONTENT, "started", Some("worker started chunking")),
            (INGEST_STAGE_CHUNK_CONTENT, "failed", Some("lease lost before completion")),
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
        assert_eq!(stages[0].stage_name, INGEST_STAGE_EXTRACT_CONTENT);
        assert_eq!(stages[1].stage_name, INGEST_STAGE_CHUNK_CONTENT);
        assert_eq!(stages[2].stage_state, "failed");

        let first_attempt = ingest
            .finalize_attempt(
                &fixture.state,
                FinalizeAttemptCommand {
                    attempt_id: first_attempt.id,
                    knowledge_generation_id: Some(fixture.generation_id),
                    attempt_state: "failed".to_string(),
                    current_stage: Some(INGEST_STAGE_CHUNK_CONTENT.to_string()),
                    failure_class: Some("lease_lost".to_string()),
                    failure_code: Some("lease_lost".to_string()),
                    failure_message: Some("lease lost during extraction".to_string()),
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

        sqlx::query("update ingest_job set available_at = now() where id = $1")
            .bind(job.id)
            .execute(&fixture.state.persistence.postgres)
            .await
            .context("failed to make requeued ingest job immediately claimable")?;
        let queued_job = ingest
            .get_job(&fixture.state, job.id)
            .await
            .context("failed to reload requeued ingest job after availability update")?;
        assert_eq!(queued_job.queue_state, "queued");
        assert!(queued_job.available_at <= Utc::now());

        let reaccepted_handle = ingest
            .get_job_handle(&fixture.state, job.id)
            .await
            .context("failed to reload requeued ingest job handle")?;
        assert_eq!(reaccepted_handle.job.queue_state, "queued");

        let queue_claim_token_b = format!("ingest-attempts-claim-b-{}", Uuid::now_v7().simple());
        let claimed_second_job = ingest_repository::claim_next_queued_ingest_job(
            &fixture.state.persistence.postgres,
            &queue_claim_token_b,
            "ingest-attempts-test",
            10,
            10,
            10,
        )
        .await
        .context("failed to claim second ingest job")?
        .context("expected to claim second ingest job")?;
        assert_eq!(claimed_second_job.id, job.id);
        let second_queue_lease_token = claimed_second_job
            .queue_lease_token
            .context("claimed second ingest job missing queue lease token")?;

        let worker_b = Uuid::now_v7();
        let second_attempt = ingest
            .lease_attempt(
                &fixture.state,
                LeaseAttemptCommand {
                    job_id: job.id,
                    worker_principal_id: None,
                    lease_token: Some("lease-b".to_string()),
                    expected_queue_lease_token: Some(second_queue_lease_token),
                    knowledge_generation_id: Some(fixture.generation_id),
                    current_stage: Some(INGEST_STAGE_EXTRACT_CONTENT.to_string()),
                },
            )
            .await
            .context("failed to lease second attempt")?;
        assert_eq!(second_attempt.attempt_number, 2);
        assert_eq!(second_attempt.worker_principal_id, None);
        assert_eq!(second_attempt.knowledge_generation_id, Some(fixture.generation_id));

        let _ = ingest
            .record_stage_event(
                &fixture.state,
                RecordStageEventCommand {
                    attempt_id: second_attempt.id,
                    stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("retry worker completed extraction".to_string()),
                    details_json: serde_json::json!({ "worker": worker_b }),
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
            .context("failed to record retry completion stage")?;

        let second_attempt = ingest
            .finalize_attempt(
                &fixture.state,
                FinalizeAttemptCommand {
                    attempt_id: second_attempt.id,
                    knowledge_generation_id: Some(fixture.generation_id),
                    attempt_state: "succeeded".to_string(),
                    current_stage: Some(INGEST_STAGE_FINALIZING.to_string()),
                    failure_class: None,
                    failure_code: None,
                    failure_message: None,
                    retryable: false,
                },
            )
            .await
            .context("failed to finalize successful retry attempt")?;
        assert_eq!(second_attempt.attempt_state, "succeeded");
        assert_eq!(second_attempt.worker_principal_id, None);
        assert!(second_attempt.finished_at.is_some());

        let attempts = ingest
            .list_attempts(&fixture.state, job.id)
            .await
            .context("failed to list attempts")?;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].attempt_number, 1);
        assert_eq!(attempts[0].failure_class.as_deref(), Some("lease_lost"));
        assert_eq!(attempts[1].attempt_number, 2);

        let completed_job = ingest.get_job(&fixture.state, job.id).await?;
        assert_eq!(completed_job.queue_state, "completed");
        assert!(completed_job.completed_at.is_some());
        assert_eq!(completed_job.knowledge_document_id, Some(fixture.document_id));
        assert_eq!(completed_job.knowledge_revision_id, Some(fixture.revision_id));

        let completed_handle = ingest
            .get_attempt_handle(&fixture.state, second_attempt.id)
            .await
            .context("failed to load completed attempt handle")?;
        assert_eq!(completed_handle.attempt.knowledge_generation_id, Some(fixture.generation_id));

        let retry_result = ingest
            .retry_job(&fixture.state, job.id, Some(Utc::now() + Duration::seconds(5)))
            .await;
        assert!(retry_result.is_err(), "completed jobs must not be requeued from the queue");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_web_ingest_jobs_queue_page_materialization_only_after_discovery() -> Result<()> {
    let mut fixture = IngestAttemptsFixture::create().await?;
    web_ingest_support::enable_loopback_test_transport(&mut fixture.state);
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = Box::pin(async {
        let web_policy = ironrag_backend::shared::web::ingest::default_web_ingest_policy();
        let run = fixture
            .state
            .canonical_services
            .web_ingest
            .create_run(
                &fixture.state,
                CreateWebIngestRunCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    seed_url: server.url("/recursive/seed"),
                    mode: "recursive_crawl".to_string(),
                    boundary_policy: Some("same_host".to_string()),
                    max_depth: Some(1),
                    max_pages: Some(20),
                    crawl_filter: web_policy.crawl_filter,
                    materialization_filter: web_policy.materialization_filter,
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    idempotency_key: None,
                },
            )
            .await
            .context("failed to submit recursive web ingest run for queue-order test")?;

        let admitted_jobs = fixture
            .state
            .canonical_services
            .ingest
            .list_jobs(&fixture.state, Some(fixture.workspace_id), Some(fixture.library_id))
            .await
            .context("failed to list admitted canonical jobs")?;
        assert_eq!(admitted_jobs.len(), 1);
        assert_eq!(admitted_jobs[0].job_kind, "web_discovery");

        let discovered_page_url = server.url("/recursive/first");
        ingest_repository::create_web_discovered_page(
            &fixture.state.persistence.postgres,
            &NewWebDiscoveredPage {
                id: Uuid::now_v7(),
                run_id: run.run_id,
                discovered_url: Some(discovered_page_url.as_str()),
                normalized_url: discovered_page_url.as_str(),
                final_url: Some(discovered_page_url.as_str()),
                canonical_url: Some(discovered_page_url.as_str()),
                depth: 1,
                referrer_candidate_id: None,
                host_classification: "same_host",
                candidate_state: "eligible",
                classification_reason: Some("seed_accepted"),
                classification_detail: None,
                content_type: Some("text/html; charset=utf-8"),
                http_status: Some(200),
                snapshot_storage_key: None,
                discovered_at: None,
                updated_at: None,
                document_id: None,
                result_revision_id: None,
                mutation_item_id: None,
            },
        )
        .await
        .context("failed to preseed eligible discovered web page")?;

        Box::pin(
            fixture
                .state
                .canonical_services
                .web_ingest
                .execute_recursive_discovery_job(&fixture.state, run.run_id),
        )
        .await
        .context("failed to execute recursive discovery job directly")?;

        let queued_jobs = fixture
            .state
            .canonical_services
            .ingest
            .list_jobs(&fixture.state, Some(fixture.workspace_id), Some(fixture.library_id))
            .await
            .context("failed to list canonical jobs after discovery")?;
        let discovery_jobs =
            queued_jobs.iter().filter(|job| job.job_kind == "web_discovery").collect::<Vec<_>>();
        let page_jobs = queued_jobs
            .iter()
            .filter(|job| job.job_kind == "web_materialize_page")
            .collect::<Vec<_>>();

        assert_eq!(discovery_jobs.len(), 1);
        assert!(!page_jobs.is_empty());
        assert!(page_jobs.iter().all(|job| job.queued_at >= discovery_jobs[0].queued_at));

        let refreshed_run = fixture
            .state
            .canonical_services
            .web_ingest
            .get_run(&fixture.state, run.run_id)
            .await
            .context("failed to refresh recursive run after discovery")?;
        assert_eq!(refreshed_run.run_state, "processing");
        assert!(refreshed_run.counts.queued > 0);

        Ok(())
    })
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn concurrent_inline_attempt_claim_is_one_atomic_job_lease() -> Result<()> {
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
                    mutation_item_id: None,
                    connector_id: None,
                    async_operation_id: None,
                    knowledge_document_id: Some(fixture.document_id),
                    knowledge_revision_id: Some(fixture.revision_id),
                    job_kind: "content_mutation".to_string(),
                    priority: 50,
                    dedupe_key: Some(format!("inline-lease-race-{}", Uuid::now_v7())),
                    available_at: None,
                },
            )
            .await?;
        let first = ingest.lease_attempt(
            &fixture.state,
            LeaseAttemptCommand {
                job_id: job.id,
                worker_principal_id: None,
                lease_token: Some("inline-race-a".to_string()),
                expected_queue_lease_token: None,
                knowledge_generation_id: Some(fixture.generation_id),
                current_stage: Some(INGEST_STAGE_EXTRACT_CONTENT.to_string()),
            },
        );
        let second = ingest.lease_attempt(
            &fixture.state,
            LeaseAttemptCommand {
                job_id: job.id,
                worker_principal_id: None,
                lease_token: Some("inline-race-b".to_string()),
                expected_queue_lease_token: None,
                knowledge_generation_id: Some(fixture.generation_id),
                current_stage: Some(INGEST_STAGE_EXTRACT_CONTENT.to_string()),
            },
        );
        let (first, second) = tokio::join!(first, second);
        let results = [first, second];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

        let attempt_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from ingest_attempt
             where job_id = $1",
        )
        .bind(job.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        let lease_shape = sqlx::query_as::<_, (String, bool, bool, bool)>(
            "select queue_state::text,
                    queue_leased_at is not null,
                    queue_lease_token is not null,
                    queue_lease_owner is not null
             from ingest_job
             where id = $1",
        )
        .bind(job.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(attempt_count, 1);
        assert_eq!(lease_shape, ("leased".to_string(), true, true, true));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
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
                    mutation_item_id: None,
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
                    expected_queue_lease_token: None,
                    knowledge_generation_id: Some(fixture.generation_id),
                    current_stage: Some(INGEST_STAGE_EXTRACT_CONTENT.to_string()),
                },
            )
            .await
            .context("failed to lease stage-events attempt")?;

        for (stage_name, stage_state) in
            [(INGEST_STAGE_EXTRACT_CONTENT, "started"), (INGEST_STAGE_CHUNK_CONTENT, "started")]
        {
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
                .with_context(|| {
                    format!("failed to record stage event {stage_name}/{stage_state}")
                })?;
        }

        let events = ingest
            .list_stage_events(&fixture.state, attempt.id)
            .await
            .context("failed to list emitted stage events")?;
        assert!(events.len() >= 2);
        assert!(events.iter().any(|event| event.stage_name == INGEST_STAGE_EXTRACT_CONTENT));
        assert!(events.iter().any(|event| event.stage_name == INGEST_STAGE_CHUNK_CONTENT));
        assert!(events.iter().all(|event| event.attempt_id == attempt.id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
