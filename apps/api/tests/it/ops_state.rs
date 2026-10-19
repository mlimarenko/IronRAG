use crate::temp_postgres_database;

use anyhow::{Context, Result};
use chrono::Utc;
use ironrag_contracts::documents::DocumentReadiness;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

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
        knowledge_plane::RevisionReadinessUpdate,
        knowledge_rows::KnowledgeRevisionRow,
        persistence::Persistence,
        repositories::{ingest_repository, query_repository, runtime_repository},
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::{CreateDocumentCommand, CreateRevisionCommand, PromoteHeadCommand},
        knowledge::service::CreateKnowledgeRevisionCommand,
        ops::service::OpsService,
    },
};

struct OpsStateFixture {
    state: AppState,
    temp_postgres: TempPostgresDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    generation_id: Uuid,
}

impl OpsStateFixture {
    async fn create() -> Result<Self> {
        let mut settings = Settings::from_env().context("failed to load settings for ops_state")?;
        let temp_postgres =
            TempPostgresDatabase::create(&settings.database_url, "ops_state").await?;
        settings.database_url = temp_postgres.database_url.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect ops_state postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply authoritative baseline migrations for ops_state")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for ops_state")?;
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let state = AppState::from_dependencies(settings, Persistence::for_tests(postgres, redis))?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = state
            .application_services
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
            .application_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("ops-state-library-{suffix}")),
                    display_name: "Ops State Library".to_string(),
                    description: Some("ops state authoritative fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create ops_state library")?;

        let document = state
            .application_services
            .content
            .create_document(
                &state,
                CreateDocumentCommand {
                    workspace_id: workspace.id,
                    library_id: library.id,
                    external_key: Some(format!("ops-state-doc-{suffix}")),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create ops_state content document")?;
        let document_id = document.id;
        let content_revision = crate::content_lifecycle_support::create_imported_revision(
            &state,
            CreateRevisionCommand {
                document_id,
                content_source_kind: "upload".to_string(),
                checksum: format!("sha256:{0}{0}", document_id.simple()),
                mime_type: "text/plain".to_string(),
                byte_size: 128,
                title: Some("Ops State Fixture".to_string()),
                language_code: None,
                source_uri: Some(format!("memory://ops-state/source/{document_id}")),
                document_hint: None,
                storage_key: Some(format!("memory://ops-state/{document_id}")),
                created_by_principal_id: None,
            },
            "ops-state.txt",
        )
        .await
        .context("failed to create ops_state content revision")?;
        let revision_id = content_revision.id;
        let generation_id = Uuid::now_v7();
        state
            .application_services
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
                    document_hint: None,
                    mime_type: "text/plain".to_string(),
                    checksum: format!("checksum-{revision_id}"),
                    byte_size: 128,
                    title: Some("Ops State Fixture".to_string()),
                    normalized_text: Some(
                        "Ops state fixture text describing a stale vector and relation window."
                            .to_string(),
                    ),
                    text_checksum: Some(format!("text-checksum-{revision_id}")),
                    text_state: "text_readable".to_string(),
                    // `accepted` is the admitted-but-not-yet-processed state.
                    // `pending` is rejected by
                    // `knowledge_revision_vector_state_canonical_check` and its
                    // graph twin, so every test here failed on a real database.
                    vector_state: "accepted".to_string(),
                    graph_state: "accepted".to_string(),
                    text_readable_at: Some(Utc::now()),
                    vector_ready_at: None,
                    graph_ready_at: None,
                    superseded_by_revision_id: None,
                },
            )
            .await
            .context("failed to write ops_state revision")?;
        state
            .application_services
            .content
            .promote_document_head(
                &state,
                PromoteHeadCommand {
                    publishing_mutation_item_id:
                        crate::content_lifecycle_support::publication_item(
                            &state.persistence.postgres,
                            revision_id,
                            "ops-state.txt",
                        )
                        .await?,
                    document_id,
                    active_revision_id: Some(revision_id),
                    readable_revision_id: Some(revision_id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote ops_state document")?;

        let _queued_job = ingest_repository::create_ingest_job(
            &state.persistence.postgres,
            &ingest_repository::NewIngestJob {
                workspace_id: workspace.id,
                library_id: library.id,
                mutation_id: None,
                mutation_item_id: None,
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
                mutation_item_id: None,
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
                failure_message: None,
                progress_percent: 50,
                retryable: false,
            },
        )
        .await
        .context("failed to seed running ops_state ingest attempt")?;

        Ok(Self {
            state,
            temp_postgres,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id,
            revision_id,
            generation_id,
        })
    }

    async fn seed_failed_rebuild(&self) -> Result<Uuid> {
        let failed_job = ingest_repository::create_ingest_job(
            &self.state.persistence.postgres,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
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
                failure_message: Some("ingest rebuild failed".to_string()),
                progress_percent: 60,
                retryable: true,
            },
        )
        .await
        .context("failed to seed failed ops_state ingest attempt")?;
        Ok(failed_job.id)
    }

    async fn recover_failed_rebuild(&self, job_id: Uuid) -> Result<()> {
        let job = ingest_repository::get_ingest_job_by_id(&self.state.persistence.postgres, job_id)
            .await
            .context("failed to load recovered ops_state ingest job")?
            .context("recovered ops_state ingest job disappeared")?;
        ingest_repository::update_ingest_job(
            &self.state.persistence.postgres,
            job.id,
            &ingest_repository::UpdateIngestJob {
                mutation_id: job.mutation_id,
                connector_id: job.connector_id,
                async_operation_id: job.async_operation_id,
                knowledge_document_id: job.knowledge_document_id,
                knowledge_revision_id: job.knowledge_revision_id,
                job_kind: job.job_kind,
                queue_state: "completed".to_string(),
                priority: job.priority,
                dedupe_key: job.dedupe_key,
                available_at: job.available_at,
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .context("failed to complete recovered ops_state ingest job")?;
        ingest_repository::create_ingest_attempt(
            &self.state.persistence.postgres,
            &ingest_repository::NewIngestAttempt {
                job_id,
                attempt_number: 2,
                worker_principal_id: None,
                lease_token: Some(format!("ops-state-recovered-lease-{}", Uuid::now_v7().simple())),
                knowledge_generation_id: Some(self.generation_id),
                attempt_state: "succeeded".to_string(),
                current_stage: Some("completed".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: Some(Utc::now()),
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 100,
                retryable: false,
            },
        )
        .await
        .context("failed to seed successful retry for ops_state ingest job")?;
        Ok(())
    }

    async fn attach_failed_rebuild_to_document(&self, failed_job_id: Uuid) -> Result<()> {
        let failed_job = ingest_repository::get_ingest_job_by_id(
            &self.state.persistence.postgres,
            failed_job_id,
        )
        .await
        .context("failed to load failed ops_state ingest job for supersession")?
        .context("failed ops_state ingest job disappeared before supersession")?;
        ingest_repository::update_ingest_job(
            &self.state.persistence.postgres,
            failed_job.id,
            &ingest_repository::UpdateIngestJob {
                mutation_id: failed_job.mutation_id,
                connector_id: failed_job.connector_id,
                async_operation_id: failed_job.async_operation_id,
                knowledge_document_id: Some(self.document_id),
                knowledge_revision_id: Some(self.revision_id),
                job_kind: failed_job.job_kind,
                queue_state: failed_job.queue_state,
                priority: failed_job.priority,
                dedupe_key: failed_job.dedupe_key,
                available_at: failed_job.available_at,
                completed_at: failed_job.completed_at,
            },
        )
        .await
        .context("failed to attach document to failed ops_state ingest job")?;
        Ok(())
    }

    async fn supersede_failed_rebuild(&self, failed_job_id: Uuid) -> Result<()> {
        self.attach_failed_rebuild_to_document(failed_job_id).await?;

        ingest_repository::create_ingest_job(
            &self.state.persistence.postgres,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(self.document_id),
                knowledge_revision_id: Some(self.revision_id),
                job_kind: "reembed".to_string(),
                queue_state: "completed".to_string(),
                priority: 80,
                dedupe_key: Some(format!("ops-state-superseding-{}", Uuid::now_v7().simple())),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .context("failed to seed superseding ops_state ingest job")?;
        Ok(())
    }

    /// A failed foreground rebuild. `content_mutation` is not a background
    /// `graph_refresh` / `reembed`, so the failure is reported while it stands —
    /// unlike those, it is not excused by a still-readable revision.
    async fn seed_failed_content_mutation(&self) -> Result<Uuid> {
        let failed_job = ingest_repository::create_ingest_job(
            &self.state.persistence.postgres,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(self.document_id),
                knowledge_revision_id: Some(self.revision_id),
                job_kind: "content_mutation".to_string(),
                queue_state: "failed".to_string(),
                priority: 80,
                dedupe_key: Some(format!("ops-state-failed-mutation-{}", Uuid::now_v7().simple())),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .context("failed to seed failed ops_state content mutation job")?;
        ingest_repository::create_ingest_attempt(
            &self.state.persistence.postgres,
            &ingest_repository::NewIngestAttempt {
                job_id: failed_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some(format!(
                    "ops-state-failed-mutation-lease-{}",
                    Uuid::now_v7().simple()
                )),
                knowledge_generation_id: Some(self.generation_id),
                attempt_state: "failed".to_string(),
                current_stage: Some("embedding".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: Some(Utc::now()),
                failure_class: Some("ingest_failed".to_string()),
                failure_code: Some("ingest failed".to_string()),
                failure_message: Some("ingest failed".to_string()),
                progress_percent: 60,
                retryable: true,
            },
        )
        .await
        .context("failed to seed failed ops_state content mutation attempt")?;
        Ok(failed_job.id)
    }

    /// A retry of the failed job's exact kind and document, already scheduled
    /// (`queued`) but not yet run. The document's derived status is no longer
    /// `failed`, so this supersedes the failure.
    async fn retry_failed_build_as_queued(&self, failed_job_id: Uuid) -> Result<()> {
        let job = ingest_repository::get_ingest_job_by_id(
            &self.state.persistence.postgres,
            failed_job_id,
        )
        .await
        .context("failed to load ops_state failed job for queued retry")?
        .context("ops_state failed job disappeared before queued retry")?;
        ingest_repository::create_ingest_job(
            &self.state.persistence.postgres,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: job.knowledge_document_id,
                knowledge_revision_id: job.knowledge_revision_id,
                job_kind: job.job_kind,
                queue_state: "queued".to_string(),
                priority: job.priority,
                dedupe_key: Some(format!("ops-state-queued-retry-{}", Uuid::now_v7().simple())),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        )
        .await
        .context("failed to seed queued retry for ops_state ingest job")?;
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
                request_surface: "ui",
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
                parent_execution_id: None,
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
            .document_store
            .list_revisions_by_document(self.document_id)
            .await
            .context("failed to load ops_state revisions for resolution")?;
        let revision = revisions
            .iter()
            .find(|revision| revision.revision_id == self.revision_id)
            .cloned()
            .context("ops_state revision missing during resolution")?;
        self.state
            .document_store
            .update_revision_readiness(RevisionReadinessUpdate {
                revision_id: revision.revision_id,
                // The text column has its own vocabulary: readable text is
                // `text_readable`, not `ready`.
                text_state: "text_readable",
                vector_state: "ready",
                graph_state: "ready",
                text_readable_at: revision.text_readable_at.or(Some(Utc::now())),
                vector_ready_at: Some(Utc::now()),
                graph_ready_at: Some(Utc::now()),
                superseded_by_revision_id: None,
            })
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
                            failure_message: None,
                            progress_percent: 100,
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
            },
        )
        .await
        .context("failed to resolve ops_state bundle assembly execution")?;
        let runtime_executions = runtime_repository::list_runtime_executions_by_owner(
            &self.state.persistence.postgres,
            RuntimeExecutionOwnerKind::QueryExecution.as_str(),
            execution_id,
        )
        .await
        .context("failed to load ops_state runtime execution for resolution")?;
        for runtime_execution in runtime_executions {
            runtime_repository::update_runtime_execution(
                &self.state.persistence.postgres,
                runtime_execution.id,
                &runtime_repository::UpdateRuntimeExecution {
                    lifecycle_state: RuntimeLifecycleState::Completed.as_str(),
                    active_stage: None,
                    turn_count: runtime_execution.turn_count,
                    failure_code: None,
                    failure_summary_redacted: None,
                    completed_at: Some(Utc::now()),
                },
            )
            .await
            .context("failed to resolve ops_state runtime execution")?;
        }

        Ok(())
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_postgres.drop().await
    }
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn authoritative_ops_library_state_uses_postgres_workload_and_knowledge_generations()
-> Result<()> {
    let fixture = OpsStateFixture::create().await?;

    let result = async {
        let snapshot = fixture
            .state
            .application_services
            .ops
            .get_library_state_snapshot(&fixture.state, fixture.library_id)
            .await
            .context("failed to load ops library state snapshot")?;

        assert_eq!(snapshot.state.library_id, fixture.library_id);
        assert_eq!(snapshot.state.queue_depth, 1);
        assert_eq!(snapshot.state.running_attempts, 1);
        assert_eq!(snapshot.state.readable_document_count, 0);
        assert_eq!(snapshot.state.failed_document_count, 0);
        assert_eq!(snapshot.state.degraded_state, "rebuilding");
        assert_eq!(snapshot.knowledge_generations.len(), 1);
        let generation = &snapshot.knowledge_generations[0];
        assert_eq!(snapshot.state.latest_knowledge_generation_id, Some(generation.id));
        assert_eq!(snapshot.state.knowledge_generation_state.as_deref(), Some("text_readable"));
        assert_eq!(snapshot.knowledge_generations[0].library_id, fixture.library_id);
        assert_eq!(snapshot.knowledge_generations[0].generation_state, "text_readable");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn authoritative_ops_warnings_cover_failed_rebuild_and_bundle_signals() -> Result<()> {
    let fixture = OpsStateFixture::create().await?;

    let result = async {
        fixture.seed_failed_content_mutation().await?;
        let execution_id = fixture.seed_bundle_failure().await?;

        let warnings = fixture
            .state
            .application_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to load ops library warnings")?;

        assert!(warnings.iter().any(
            |warning| warning.warning_kind == "failed_rebuilds" && warning.severity == "error"
        ));
        assert!(warnings.iter().any(|warning| {
            warning.warning_kind == "bundle_assembly_failures" && warning.severity == "error"
        }));

        fixture.resolve_warning_sources(execution_id).await?;

        let resolved_snapshot = fixture
            .state
            .application_services
            .ops
            .get_library_state_snapshot(&fixture.state, fixture.library_id)
            .await
            .context("failed to reload resolved ops library state snapshot")?;
        let resolved_warnings = fixture
            .state
            .application_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to reload resolved ops library warnings")?;

        // The fixture seeds no runtime graph projection, so its one ready
        // document is graph-sparse (`graph_sparse = ready - graph_ready`). That
        // hole does not close on its own, so the library stays degraded after
        // the rebuild and bundle failures are resolved, and the graph gap is the
        // one warning left standing.
        assert_eq!(resolved_snapshot.state.degraded_state, "degraded");
        assert_eq!(
            resolved_warnings
                .iter()
                .map(|warning| (warning.warning_kind.as_str(), warning.severity.as_str()))
                .collect::<Vec<_>>(),
            vec![("graph_coverage_gap", "warning")],
            "resolving the rebuild/bundle failures leaves only the graph coverage gap"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn recovered_ingest_attempt_does_not_require_user_action() -> Result<()> {
    let fixture = OpsStateFixture::create().await?;

    let result = async {
        let failed_job_id = fixture.seed_failed_rebuild().await?;
        fixture.recover_failed_rebuild(failed_job_id).await?;

        let warnings = fixture
            .state
            .application_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to load warnings after successful retry")?;

        assert!(
            warnings.iter().all(|warning| warning.warning_kind != "failed_rebuilds"),
            "a recovered background attempt must not ask the user to intervene"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn superseded_failed_job_does_not_require_user_action() -> Result<()> {
    let fixture = OpsStateFixture::create().await?;

    let result = async {
        let failed_job_id = fixture.seed_failed_rebuild().await?;
        fixture.supersede_failed_rebuild(failed_job_id).await?;

        let warnings = fixture
            .state
            .application_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to load warnings after a superseding successful job")?;

        assert!(
            warnings.iter().all(|warning| warning.warning_kind != "failed_rebuilds"),
            "a superseded failed job must not ask the user to intervene"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// A failed build with a retry already queued is not something the operator can
/// act on: the retry is scheduled and the document's derived status is `queued`,
/// so the `/documents?status=failed` list the notice points at is empty.
/// Reporting `failed_rebuilds` there sent the operator to a blank screen while
/// the retry was already in progress.
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn queued_retry_does_not_leave_a_failed_rebuild_notice() -> Result<()> {
    let fixture = OpsStateFixture::create().await?;

    let result = async {
        let failed_job_id = fixture.seed_failed_content_mutation().await?;

        let before = fixture
            .state
            .application_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to load warnings before the retry")?;
        assert!(
            before.iter().any(|warning| warning.warning_kind == "failed_rebuilds"),
            "a standing failed build must be reported before any retry exists"
        );

        fixture.retry_failed_build_as_queued(failed_job_id).await?;

        let after = fixture
            .state
            .application_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to load warnings after the retry was queued")?;
        assert!(
            after.iter().all(|warning| warning.warning_kind != "failed_rebuilds"),
            "a queued retry supersedes the failure and must not ask the user to intervene"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn failed_background_refresh_keeps_readable_document_available() -> Result<()> {
    let fixture = OpsStateFixture::create().await?;

    let result = async {
        let failed_job_id = fixture.seed_failed_rebuild().await?;
        fixture.attach_failed_rebuild_to_document(failed_job_id).await?;

        let snapshot = fixture
            .state
            .application_services
            .ops
            .get_library_state_snapshot(&fixture.state, fixture.library_id)
            .await
            .context("failed to load state after a background refresh failure")?;
        let warnings = fixture
            .state
            .application_services
            .ops
            .list_library_warnings(&fixture.state, fixture.library_id)
            .await
            .context("failed to load warnings after a background refresh failure")?;

        assert_eq!(snapshot.state.readable_document_count, 1);
        assert_eq!(snapshot.state.failed_document_count, 0);
        assert!(
            warnings.iter().all(|warning| warning.warning_kind != "failed_rebuilds"),
            "a failed background enhancement must not ask the user to repair readable material"
        );

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
        revision_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_number: 1,
        revision_state: "active".to_string(),
        revision_kind: "upload".to_string(),
        storage_ref: Some("memory://ops-state".to_string()),
        source_uri: Some("memory://ops-state/source".to_string()),
        document_hint: None,
        mime_type: "text/plain".to_string(),
        checksum: "checksum".to_string(),
        title: Some("Ops State".to_string()),
        byte_size: 64,
        normalized_text: Some("content".to_string()),
        text_checksum: Some("text-checksum".to_string()),
        image_checksum: None,
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
        request_surface: "internal".to_string(),
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
        normalization_profile: "authoritative".to_string(),
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
    readiness_kind: DocumentReadiness,
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
        file_name: Some("Ops Summary".to_string()),
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
            document_hint: None,
            storage_key: None,
            created_by_principal_id: None,
            created_at: Utc::now(),
        }),
        source_access: None,
        readiness: Some(ContentRevisionReadiness {
            revision_id,
            text_state: "text_readable".to_string(),
            vector_state: "ready".to_string(),
            graph_state: if graph_coverage_kind == "graph_ready" {
                "ready".to_string()
            } else {
                "processing".to_string()
            },
            text_readable_at: Some(Utc::now()),
            vector_ready_at: Some(Utc::now()),
            graph_ready_at: Some(Utc::now()),
        }),
        readiness_summary: Some(DocumentReadinessSummary {
            document_id,
            active_revision_id: Some(revision_id),
            readiness_kind,
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
fn authoritative_document_knowledge_state_classifies_all_five_readiness_kinds() {
    let service = OpsService::new();

    assert_processing_readiness(&service);
    assert_readable_readiness(&service);
    assert_graph_sparse_readiness(&service);
    assert_graph_ready_readiness(&service);
    assert_legacy_ready_without_prepared_revision_readiness(&service);
    assert_failed_readiness(&service);
}

/// An accepted mutation is processing, independent of any revision row.
fn assert_processing_readiness(service: &OpsService) {
    let processing = service.classify_document_knowledge_state(
        None,
        None,
        Some(&sample_mutation("accepted")),
        None,
        false,
    );
    assert_eq!(processing.readiness_kind, DocumentReadiness::Processing);
    assert_eq!(processing.graph_coverage_kind, "processing");
}

/// A readable revision with pending vector/graph states and a typed fact is
/// readable but graph sparse.
fn assert_readable_readiness(service: &OpsService) {
    let readable = service.classify_document_knowledge_state(
        Some(&sample_revision_row("text_readable", "pending", "pending")),
        Some(&sample_prepared_revision("prepared", 10, 1)),
        None,
        Some(&sample_job("queued")),
        false,
    );
    assert_eq!(readable.readiness_kind, DocumentReadiness::Readable);
    assert_eq!(readable.graph_coverage_kind, "graph_sparse");
    assert!(readable.typed_fact_coverage.unwrap_or_default() > 0.0);
}

/// A vector-ready revision without typed facts is graph sparse.
fn assert_graph_sparse_readiness(service: &OpsService) {
    let graph_sparse = service.classify_document_knowledge_state(
        Some(&sample_revision_row("text_readable", "vector_ready", "pending")),
        Some(&sample_prepared_revision("prepared", 10, 0)),
        None,
        None,
        false,
    );
    assert_eq!(graph_sparse.readiness_kind, DocumentReadiness::GraphSparse);
    assert_eq!(graph_sparse.graph_coverage_kind, "graph_sparse");
}

/// A vector-ready revision with typed facts is graph ready.
fn assert_graph_ready_readiness(service: &OpsService) {
    let graph_ready = service.classify_document_knowledge_state(
        Some(&sample_revision_row("text_readable", "vector_ready", "ready")),
        Some(&sample_prepared_revision("prepared", 10, 3)),
        None,
        None,
        false,
    );
    assert_eq!(graph_ready.readiness_kind, DocumentReadiness::GraphReady);
    assert_eq!(graph_ready.graph_coverage_kind, "graph_ready");
}

/// The graph-ready state without a prepared revision degrades to legacy
/// graph-sparse while staying readable.
fn assert_legacy_ready_without_prepared_revision_readiness(service: &OpsService) {
    let legacy_ready_without_prepared_revision = service.classify_document_knowledge_state(
        Some(&sample_revision_row("text_readable", "vector_ready", "ready")),
        None,
        None,
        None,
        false,
    );
    assert_eq!(
        legacy_ready_without_prepared_revision.readiness_kind,
        DocumentReadiness::GraphSparse
    );
    assert_eq!(legacy_ready_without_prepared_revision.graph_coverage_kind, "graph_sparse");
    assert_eq!(legacy_ready_without_prepared_revision.preparation_state, "pending");
    assert!(legacy_ready_without_prepared_revision.readable);
    assert!(!legacy_ready_without_prepared_revision.graph_ready);
}

/// A failed revision is failed.
fn assert_failed_readiness(service: &OpsService) {
    let failed = service.classify_document_knowledge_state(
        Some(&sample_revision_row("failed", "failed", "failed")),
        Some(&sample_prepared_revision("failed", 10, 0)),
        None,
        None,
        false,
    );
    assert_eq!(failed.readiness_kind, DocumentReadiness::Failed);
    assert_eq!(failed.graph_coverage_kind, "failed");
}

#[test]
fn unpublished_operational_failure_proves_failure_without_readability() {
    let service = OpsService::new();
    let failed = service.classify_document_knowledge_state(None, None, None, None, true);
    assert_eq!(failed.readiness_kind, DocumentReadiness::Failed);
    assert!(!failed.readable);
    assert!(!failed.graph_ready);
    let empty = service.classify_document_knowledge_state(None, None, None, None, false);
    assert_eq!(empty.readiness_kind, DocumentReadiness::Empty);
    assert!(!empty.readable);
}

#[test]
fn authoritative_library_knowledge_coverage_aggregates_readiness_and_graph_sparse_counts() {
    let service = OpsService::new();
    let library_id = Uuid::now_v7();
    let coverage = service.derive_library_knowledge_coverage(
        library_id,
        &[
            sample_document_summary(DocumentReadiness::Processing, "processing", None),
            sample_document_summary(DocumentReadiness::Readable, "graph_sparse", Some(0.1)),
            sample_document_summary(DocumentReadiness::GraphSparse, "graph_sparse", None),
            sample_document_summary(DocumentReadiness::GraphReady, "graph_ready", Some(0.2)),
            sample_document_summary(DocumentReadiness::Failed, "failed", None),
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
