use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::{
        agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
        content::{
            ContentDocument, ContentDocumentPipelineJob, ContentDocumentPipelineState,
            ContentDocumentSummary, ContentMutation, ContentRevision, ContentRevisionReadiness,
            DocumentReadinessSummary, RuntimeDocumentActivityStatus,
        },
        knowledge::StructuredDocumentRevision,
    },
    infra::{
        arangodb::{
            bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
            client::ArangoClient,
            document_store::KnowledgeRevisionRow,
        },
        persistence::Persistence,
        repositories::{ingest_repository, query_repository, runtime_repository},
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        knowledge::service::{
            CreateKnowledgeDocumentCommand, CreateKnowledgeRevisionCommand,
            PromoteKnowledgeDocumentCommand, RefreshKnowledgeLibraryGenerationCommand,
        },
        ops::service::OpsService,
    },
};

struct TempPostgresDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempPostgresDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let name = format!("ops_state_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect admin postgres for ops_state test")?;

        terminate_database_connections(&admin_pool, &name).await?;
        sqlx::query(&format!("drop database if exists \"{name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale ops_state database {name}"))?;
        sqlx::query(&format!("create database \"{name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create ops_state database {name}"))?;
        admin_pool.close().await;

        Ok(Self {
            name: name.clone(),
            admin_url,
            database_url: replace_database_name(base_database_url, &name)?,
        })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect admin postgres for ops_state cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop ops_state database {}", self.name))?;
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
        let name = format!("ops_state_{}", Uuid::now_v7().simple());
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                settings.arangodb_request_timeout_seconds.max(1),
            ))
            .build()
            .context("failed to build ArangoDB admin http client")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .context("failed to create temp ArangoDB database for ops_state")?;
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
            .context("failed to drop temp ArangoDB database for ops_state")?;
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

struct OpsStateFixture {
    state: AppState,
    temp_postgres: TempPostgresDatabase,
    temp_arango: TempArangoDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    generation_id: Uuid,
}

impl OpsStateFixture {
    async fn create() -> Result<Self> {
        let mut settings = Settings::from_env().context("failed to load settings for ops_state")?;
        let temp_postgres = TempPostgresDatabase::create(&settings.database_url).await?;
        let temp_arango = TempArangoDatabase::create(&settings).await?;
        settings.database_url = temp_postgres.database_url.clone();
        settings.arangodb_database = temp_arango.name.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect ops_state postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply canonical baseline migrations for ops_state")?;

        let arango_client = Arc::new(
            ArangoClient::from_settings(&settings).context("failed to build Arango client")?,
        );
        arango_client.ping().await.context("failed to ping temp ArangoDB for ops_state")?;
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
        .context("failed to bootstrap Arango knowledge plane for ops_state")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for ops_state")?;
        let state = AppState::from_dependencies(
            settings,
            Persistence::for_tests(postgres, redis),
            Arc::clone(&arango_client),
        )?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("ops-state-workspace-{suffix}")),
                    display_name: "Ops State Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create ops_state workspace")?;
        let library = state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("ops-state-library-{suffix}")),
                    display_name: "Ops State Library".to_string(),
                    description: Some("ops state canonical fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create ops_state library")?;

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
                    external_key: format!("ops-state-doc-{suffix}"),
                    file_name: None,
                    title: Some("Ops State Fixture".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create ops_state document shell")?;
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
                    storage_ref: Some(format!("memory://ops-state/{revision_id}")),
                    source_uri: Some(format!("memory://ops-state/source/{revision_id}")),
                    mime_type: "text/plain".to_string(),
                    checksum: format!("checksum-{revision_id}"),
                    byte_size: 128,
                    title: Some("Ops State Fixture".to_string()),
                    normalized_text: Some(
                        "Ops state fixture text describing a stale vector and relation window."
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
            .context("failed to write ops_state revision")?;
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
            .context("failed to promote ops_state document")?;

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
                    active_text_generation: 7,
                    active_vector_generation: 4,
                    active_graph_generation: 2,
                    degraded_state: "rebuilding".to_string(),
                },
            )
            .await
            .context("failed to refresh ops_state library generation")?;

        let _queued_job = ingest_repository::create_ingest_job(
            &state.persistence.postgres,
            &ingest_repository::NewIngestJob {
                workspace_id: workspace.id,
                library_id: library.id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(document_id),
                knowledge_revision_id: Some(revision_id),
                job_kind: "reembed".to_string(),
                queue_state: "queued".to_string(),
                priority: 100,
                dedupe_key: Some(format!("ops-state-queued-{suffix}")),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        )
        .await
        .context("failed to seed queued ops_state ingest job")?;
        let running_job = ingest_repository::create_ingest_job(
            &state.persistence.postgres,
            &ingest_repository::NewIngestJob {
                workspace_id: workspace.id,
                library_id: library.id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(document_id),
                knowledge_revision_id: Some(revision_id),
                job_kind: "graph_refresh".to_string(),
                queue_state: "leased".to_string(),
                priority: 90,
                dedupe_key: Some(format!("ops-state-running-{suffix}")),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        )
        .await
        .context("failed to seed running ops_state ingest job")?;
        ingest_repository::create_ingest_attempt(
            &state.persistence.postgres,
            &ingest_repository::NewIngestAttempt {
                job_id: running_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some(format!("ops-state-lease-{suffix}")),
                knowledge_generation_id: Some(generation_id),
                attempt_state: "running".to_string(),
                current_stage: Some("projecting".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                retryable: false,
            },
        )
        .await
        .context("failed to seed running ops_state ingest attempt")?;

        Ok(Self {
            state,
            temp_postgres,
            temp_arango,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id,
            revision_id,
            generation_id,
        })
    }

    async fn seed_failed_rebuild(&self) -> Result<()> {
        let failed_job = ingest_repository::create_ingest_job(
            &self.state.persistence.postgres,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(self.document_id),
                knowledge_revision_id: Some(self.revision_id),
                job_kind: "reembed".to_string(),
                queue_state: "failed".to_string(),
                priority: 80,
                dedupe_key: Some(format!("ops-state-failed-{}", Uuid::now_v7().simple())),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .context("failed to seed failed ops_state ingest job")?;
        ingest_repository::create_ingest_attempt(
            &self.state.persistence.postgres,
            &ingest_repository::NewIngestAttempt {
                job_id: failed_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some(format!("ops-state-failed-lease-{}", Uuid::now_v7().simple())),
                knowledge_generation_id: Some(self.generation_id),
                attempt_state: "failed".to_string(),
                current_stage: Some("embedding".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: Some(Utc::now()),
                failure_class: Some("rebuild_failed".to_string()),
                failure_code: Some("ingest rebuild failed".to_string()),
                retryable: true,
            },
        )
        .await
        .context("failed to seed failed ops_state ingest attempt")?;
        Ok(())
    }

    async fn seed_bundle_failure(&self) -> Result<Uuid> {
        let conversation = query_repository::create_conversation(
            &self.state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                created_by_principal_id: None,
                title: Some("Ops State Failure Conversation"),
                conversation_state: "active",
            },
            5,
        )
        .await
        .context("failed to create ops_state query conversation")?;
        let request_turn = query_repository::create_turn(
            &self.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: conversation.id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Why did bundle assembly fail?",
                execution_id: None,
            },
        )
        .await
        .context("failed to create ops_state request turn")?;
        let execution_id = Uuid::now_v7();
        let bundle_id = execution_id;
        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &self.state.persistence.postgres,
            &runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: execution_id,
                task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
                surface_kind: "rest",
                contract_name: "query_answer",
                contract_version: "1",
                lifecycle_state: RuntimeLifecycleState::Failed.as_str(),
                active_stage: None,
                turn_budget: 4,
                turn_count: 2,
                parallel_action_limit: 1,
                failure_code: Some("failed to assemble knowledge context bundle"),
                failure_summary_redacted: Some("failed to assemble knowledge context bundle"),
            },
        )
        .await
        .context("failed to seed bundle assembly runtime execution")?;
        query_repository::create_execution(
            &self.state.persistence.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: bundle_id,
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                conversation_id: conversation.id,
                request_turn_id: Some(request_turn.id),
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text: "Why did bundle assembly fail?",
                failure_code: Some(
                    "failed to assemble knowledge context bundle: missing grounded references",
                ),
            },
        )
        .await
        .context("failed to seed bundle assembly failure execution")?;
        Ok(execution_id)
    }

    async fn resolve_warning_sources(&self, execution_id: Uuid) -> Result<()> {
        let revisions = self
            .state
            .arango_document_store
            .list_revisions_by_document(self.document_id)
            .await
            .context("failed to load ops_state revisions for resolution")?;
        let revision = revisions
            .iter()
            .find(|revision| revision.revision_id == self.revision_id)
            .cloned()
            .context("ops_state revision missing during resolution")?;
        self.state
            .arango_document_store
            .update_revision_readiness(
                revision.revision_id,
                "ready",
                "ready",
                "ready",
                revision.text_readable_at.or(Some(Utc::now())),
                Some(Utc::now()),
                Some(Utc::now()),
                None,
            )
            .await
            .context("failed to resolve ops_state revision readiness")?;

        let jobs = ingest_repository::list_ingest_jobs(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            Some(self.library_id),
            None,
            None,
        )
        .await
        .context("failed to list ops_state ingest jobs for resolution")?;
        for job in jobs {
            if matches!(job.queue_state.as_str(), "queued" | "leased" | "failed") {
                ingest_repository::update_ingest_job(
                    &self.state.persistence.postgres,
                    job.id,
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
                .await
                .context("failed to resolve ops_state ingest job")?;
                if let Some(attempt) = ingest_repository::get_latest_ingest_attempt_by_job(
                    &self.state.persistence.postgres,
                    job.id,
                )
                .await
                .context("failed to load ops_state ingest attempt for resolution")?
                {
                    ingest_repository::update_ingest_attempt(
                        &self.state.persistence.postgres,
                        attempt.id,
                        &ingest_repository::UpdateIngestAttempt {
                            worker_principal_id: attempt.worker_principal_id,
                            lease_token: attempt.lease_token.clone(),
                            knowledge_generation_id: attempt.knowledge_generation_id,
                            attempt_state: "succeeded".to_string(),
                            current_stage: Some("completed".to_string()),
                            heartbeat_at: attempt.heartbeat_at.or(Some(Utc::now())),
                            finished_at: Some(Utc::now()),
                            failure_class: None,
                            failure_code: None,
                            retryable: false,
                        },
                    )
                    .await
                    .context("failed to resolve ops_state ingest attempt")?;
                }
            }
        }

        query_repository::update_execution(
            &self.state.persistence.postgres,
            execution_id,
            &query_repository::UpdateQueryExecution {
                request_turn_id: None,
                response_turn_id: None,
                failure_code: None,
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .context("failed to resolve ops_state bundle assembly execution")?;

        Ok(())
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_arango.drop().await?;
        self.temp_postgres.drop().await
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
#[ignore = "requires local postgres, ArangoDB, and redis services"]
async fn canonical_ops_library_state_uses_postgres_workload_and_arango_generations() -> Result<()> {
    let fixture = OpsStateFixture::create().await?;

    let result = async {
        let snapshot = fixture
            .state
            .canonical_services
            .ops
            .get_library_state_snapshot(&fixture.state, fixture.library_id)
            .await
            .context("failed to load ops library state snapshot")?;

        assert_eq!(snapshot.state.library_id, fixture.library_id);
        assert_eq!(snapshot.state.queue_depth, 1);
        assert_eq!(snapshot.state.running_attempts, 1);
        assert_eq!(snapshot.state.readable_document_count, 1);
        assert_eq!(snapshot.state.failed_document_count, 0);
        assert_eq!(snapshot.state.degraded_state, "rebuilding");
        assert_eq!(snapshot.state.latest_knowledge_generation_id, Some(fixture.generation_id));
        assert_eq!(snapshot.state.knowledge_generation_state.as_deref(), Some("graph_ready"));
        assert_eq!(snapshot.knowledge_generations.len(), 1);
        assert_eq!(snapshot.knowledge_generations[0].id, fixture.generation_id);
        assert_eq!(snapshot.knowledge_generations[0].library_id, fixture.library_id);
        assert_eq!(snapshot.knowledge_generations[0].generation_state, "graph_ready");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, ArangoDB, and redis services"]
async fn canonical_ops_warnings_cover_stale_and_failed_rebuild_signals() -> Result<()> {
    let fixture = OpsStateFixture::create().await?;

    let result = async {
        fixture.seed_failed_rebuild().await?;
        let execution_id = fixture.seed_bundle_failure().await?;

        let warnings = fixture
            .state
            .canonical_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to load ops library warnings")?;

        assert!(warnings.iter().any(
            |warning| warning.warning_kind == "stale_vectors" && warning.severity == "warning"
        ));
        assert!(
            warnings.iter().any(|warning| warning.warning_kind == "stale_relations"
                && warning.severity == "warning")
        );
        assert!(warnings.iter().any(
            |warning| warning.warning_kind == "failed_rebuilds" && warning.severity == "error"
        ));
        assert!(warnings.iter().any(|warning| {
            warning.warning_kind == "bundle_assembly_failures" && warning.severity == "error"
        }));

        fixture.resolve_warning_sources(execution_id).await?;

        let resolved_snapshot = fixture
            .state
            .canonical_services
            .ops
            .get_library_state_snapshot(&fixture.state, fixture.library_id)
            .await
            .context("failed to reload resolved ops library state snapshot")?;
        let resolved_warnings = fixture
            .state
            .canonical_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to reload resolved ops library warnings")?;

        assert_eq!(resolved_snapshot.state.degraded_state, "graph_ready");
        assert!(resolved_warnings.is_empty());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

fn sample_revision_row(
    text_state: &str,
    vector_state: &str,
    graph_state: &str,
) -> KnowledgeRevisionRow {
    let now = Utc::now();
    KnowledgeRevisionRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        revision_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_number: 1,
        revision_state: "active".to_string(),
        revision_kind: "upload".to_string(),
        storage_ref: Some("memory://ops-state".to_string()),
        source_uri: Some("memory://ops-state/source".to_string()),
        mime_type: "text/plain".to_string(),
        checksum: "checksum".to_string(),
        title: Some("Ops State".to_string()),
        byte_size: 64,
        normalized_text: Some("content".to_string()),
        text_checksum: Some("text-checksum".to_string()),
        text_state: text_state.to_string(),
        vector_state: vector_state.to_string(),
        graph_state: graph_state.to_string(),
        text_readable_at: Some(now),
        vector_ready_at: Some(now),
        graph_ready_at: Some(now),
        superseded_by_revision_id: None,
        created_at: now,
    }
}

fn sample_mutation(state: &str) -> ContentMutation {
    ContentMutation {
        id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        operation_kind: "replace".to_string(),
        mutation_state: state.to_string(),
        requested_at: Utc::now(),
        completed_at: None,
        requested_by_principal_id: None,
        request_surface: "test".to_string(),
        idempotency_key: None,
        source_identity: None,
        failure_code: None,
        conflict_code: None,
    }
}

fn sample_job(state: &str) -> ContentDocumentPipelineJob {
    let now = Utc::now();
    ContentDocumentPipelineJob {
        id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        mutation_id: None,
        async_operation_id: None,
        job_kind: "process_document".to_string(),
        queue_state: state.to_string(),
        queued_at: now,
        available_at: now,
        completed_at: None,
        claimed_at: None,
        last_activity_at: Some(now),
        current_stage: Some("prepare_structure".to_string()),
        failure_code: None,
        retryable: true,
    }
}

fn sample_prepared_revision(
    preparation_state: &str,
    block_count: i32,
    typed_fact_count: i32,
) -> StructuredDocumentRevision {
    StructuredDocumentRevision {
        revision_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        preparation_state: preparation_state.to_string(),
        normalization_profile: "canonical".to_string(),
        source_format: "text/plain".to_string(),
        language_code: Some("ru".to_string()),
        block_count,
        chunk_count: 1,
        typed_fact_count,
        outline: Vec::new(),
        prepared_at: Utc::now(),
    }
}

fn sample_document_summary(
    readiness_kind: &str,
    graph_coverage_kind: &str,
    typed_fact_coverage: Option<f64>,
) -> ContentDocumentSummary {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    ContentDocumentSummary {
        document: ContentDocument {
            id: document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: format!("doc-{document_id}"),
            document_state: "active".to_string(),
            created_at: Utc::now(),
        },
        file_name: "Ops Summary".to_string(),
        head: None,
        active_revision: Some(ContentRevision {
            id: revision_id,
            document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 1,
            parent_revision_id: None,
            content_source_kind: "upload".to_string(),
            checksum: "checksum".to_string(),
            mime_type: "text/plain".to_string(),
            byte_size: 64,
            title: Some("Ops Summary".to_string()),
            language_code: Some("ru".to_string()),
            source_uri: None,
            storage_key: None,
            created_by_principal_id: None,
            created_at: Utc::now(),
        }),
        source_access: None,
        readiness: Some(ContentRevisionReadiness {
            revision_id,
            text_state: "text_readable".to_string(),
            vector_state: "vector_ready".to_string(),
            graph_state: if graph_coverage_kind == "graph_ready" {
                "graph_ready".to_string()
            } else {
                "pending".to_string()
            },
            text_readable_at: Some(Utc::now()),
            vector_ready_at: Some(Utc::now()),
            graph_ready_at: Some(Utc::now()),
        }),
        readiness_summary: Some(DocumentReadinessSummary {
            document_id,
            active_revision_id: Some(revision_id),
            readiness_kind: readiness_kind.to_string(),
            activity_status: RuntimeDocumentActivityStatus::Ready,
            stalled_reason: None,
            preparation_state: "prepared".to_string(),
            graph_coverage_kind: graph_coverage_kind.to_string(),
            typed_fact_coverage,
            last_mutation_id: None,
            last_job_stage: None,
            updated_at: Utc::now(),
        }),
        prepared_revision: Some(sample_prepared_revision(
            "prepared",
            10,
            if typed_fact_coverage.unwrap_or_default() > 0.0 { 2 } else { 0 },
        )),
        web_page_provenance: None,
        pipeline: ContentDocumentPipelineState { latest_mutation: None, latest_job: None },
    }
}

#[test]
fn canonical_document_knowledge_state_classifies_all_five_readiness_kinds() {
    let service = OpsService::new();

    let processing = service.classify_document_knowledge_state(
        None,
        None,
        Some(&sample_mutation("accepted")),
        None,
    );
    assert_eq!(processing.readiness_kind, "processing");
    assert_eq!(processing.graph_coverage_kind, "processing");

    let readable = service.classify_document_knowledge_state(
        Some(&sample_revision_row("text_readable", "pending", "pending")),
        Some(&sample_prepared_revision("prepared", 10, 1)),
        None,
        Some(&sample_job("queued")),
    );
    assert_eq!(readable.readiness_kind, "readable");
    assert_eq!(readable.graph_coverage_kind, "graph_sparse");
    assert!(readable.typed_fact_coverage.unwrap_or_default() > 0.0);

    let graph_sparse = service.classify_document_knowledge_state(
        Some(&sample_revision_row("text_readable", "vector_ready", "pending")),
        Some(&sample_prepared_revision("prepared", 10, 0)),
        None,
        None,
    );
    assert_eq!(graph_sparse.readiness_kind, "graph_sparse");
    assert_eq!(graph_sparse.graph_coverage_kind, "graph_sparse");

    let graph_ready = service.classify_document_knowledge_state(
        Some(&sample_revision_row("text_readable", "vector_ready", "graph_ready")),
        Some(&sample_prepared_revision("prepared", 10, 3)),
        None,
        None,
    );
    assert_eq!(graph_ready.readiness_kind, "graph_ready");
    assert_eq!(graph_ready.graph_coverage_kind, "graph_ready");

    let legacy_ready_without_prepared_revision = service.classify_document_knowledge_state(
        Some(&sample_revision_row("text_readable", "vector_ready", "graph_ready")),
        None,
        None,
        None,
    );
    assert_eq!(legacy_ready_without_prepared_revision.readiness_kind, "graph_sparse");
    assert_eq!(legacy_ready_without_prepared_revision.graph_coverage_kind, "graph_sparse");
    assert_eq!(legacy_ready_without_prepared_revision.preparation_state, "pending");
    assert!(legacy_ready_without_prepared_revision.readable);
    assert!(!legacy_ready_without_prepared_revision.graph_ready);

    let failed = service.classify_document_knowledge_state(
        Some(&sample_revision_row("failed", "failed", "failed")),
        Some(&sample_prepared_revision("failed", 10, 0)),
        None,
        None,
    );
    assert_eq!(failed.readiness_kind, "failed");
    assert_eq!(failed.graph_coverage_kind, "failed");
}

#[test]
fn canonical_library_knowledge_coverage_aggregates_readiness_and_graph_sparse_counts() {
    let service = OpsService::new();
    let library_id = Uuid::now_v7();
    let coverage = service.derive_library_knowledge_coverage(
        library_id,
        &[
            sample_document_summary("processing", "processing", None),
            sample_document_summary("readable", "graph_sparse", Some(0.1)),
            sample_document_summary("graph_sparse", "graph_sparse", None),
            sample_document_summary("graph_ready", "graph_ready", Some(0.2)),
            sample_document_summary("failed", "failed", None),
        ],
        Some(Uuid::now_v7()),
    );

    assert_eq!(coverage.library_id, library_id);
    assert_eq!(coverage.document_counts_by_readiness.get("processing"), Some(&1));
    assert_eq!(coverage.document_counts_by_readiness.get("readable"), Some(&1));
    assert_eq!(coverage.document_counts_by_readiness.get("graph_sparse"), Some(&1));
    assert_eq!(coverage.document_counts_by_readiness.get("graph_ready"), Some(&1));
    assert_eq!(coverage.document_counts_by_readiness.get("failed"), Some(&1));
    assert_eq!(coverage.graph_sparse_document_count, 2);
    assert_eq!(coverage.graph_ready_document_count, 1);
    assert_eq!(coverage.typed_fact_document_count, 2);
    assert!(coverage.last_generation_id.is_some());
}
