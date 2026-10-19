//! PostgreSQL integration coverage for graph re-extract sweep eligibility.
//!
//! Run with:
//! `cargo test -p ironrag-backend --test it -- --ignored graph_reextract_repository::`

use crate::temp_postgres_database;

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
    infra::{
        persistence::Persistence,
        repositories::{
            AUTOMATIC_GRAPH_REEXTRACT_PRIORITY, EXPLICIT_GRAPH_REEXTRACT_PRIORITY,
            catalog_repository, ingest_repository,
            runtime_graph_repository::{
                self, CreateRuntimeGraphEvidenceInput, CreateRuntimeGraphExtractionRecordInput,
                GraphReextractSelectionMode, RuntimeGraphEvidenceSearchQuery,
            },
            runtime_repository::{self, NewRuntimeExecution},
        },
    },
    services::graph::error::GraphServiceError,
};
use temp_postgres_database::TempPostgresDatabase;

const GRAPH_EXTRACTION_VERSION: &str = "graph_extract";
/// The extraction-contract stamp the fixtures write into completed-stage
/// details and pass back to the repository boundary. Any self-consistent value
/// exercises the contract gap; it mirrors the production literal so a failure
/// reads like real data.
const GRAPH_EXTRACTION_CONTRACT_VERSION: &str = "graph_extract_contract_3_stable_request_context";

struct GraphReextractFixture {
    database: TempPostgresDatabase,
    pool: PgPool,
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    runtime_execution_id: Uuid,
}

/// One graph re-extract job's queue policy on an exact document revision.
struct GraphJobPolicySeed<'a> {
    document_id: Uuid,
    revision_id: Uuid,
    queue_state: &'a str,
    queued_at: DateTime<Utc>,
    priority: i32,
    dedupe_key: String,
}

/// Extraction-version stamp carried by the completed-graph fixtures.
struct GraphCompletionStamp<'a> {
    extraction_version: &'a str,
    extraction_contract_version: Option<&'a str>,
    graph_contribution_count: serde_json::Value,
}

impl GraphReextractFixture {
    async fn create() -> Result<Self> {
        let database_url = std::env::var("IRONRAG_DATABASE_URL")
            .context("IRONRAG_DATABASE_URL must be configured")?;
        let database = TempPostgresDatabase::create(&database_url, "graph_reextract").await?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database.database_url)
            .await
            .context("failed to connect graph re-extract postgres")?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply graph re-extract migrations")?;
        let mut settings = Settings::from_env().context("load graph re-extract settings")?;
        settings.database_url = database.database_url.clone();
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let redis = redis::Client::open(settings.redis_url.clone())
            .context("create graph re-extract redis client")?;
        let state =
            AppState::from_dependencies(settings, Persistence::for_tests(pool.clone(), redis))?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("graph-reextract-workspace-{suffix}"),
            "Graph Re-extract Workspace",
            None,
        )
        .await
        .context("failed to create graph re-extract workspace")?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("graph-reextract-library-{suffix}"),
            "Graph Re-extract Library",
            None,
            None,
        )
        .await
        .context("failed to create graph re-extract library")?;
        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &pool,
            &NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::GraphExtractionAttempt.as_str(),
                owner_id: Uuid::now_v7(),
                task_kind: RuntimeTaskKind::GraphExtract.as_str(),
                surface_kind: "worker",
                contract_name: "graph_extract",
                contract_version: "graph-reextract-test",
                lifecycle_state: RuntimeLifecycleState::Running.as_str(),
                active_stage: None,
                turn_budget: 1,
                turn_count: 0,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                parent_execution_id: None,
            },
        )
        .await
        .context("failed to create graph re-extract runtime execution")?;

        Ok(Self {
            database,
            pool,
            state,
            workspace_id: workspace.id,
            library_id: library.id,
            runtime_execution_id,
        })
    }

    async fn create_readable_revision(&self, label: &str) -> Result<(Uuid, Uuid)> {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let checksum =
            crate::content_lifecycle_support::fixture_checksum(&format!("{label}:{revision_id}"));
        let admission = crate::content_repository::synthetic_import_admission(
            ironrag_contracts::documents::ReplacementIdentity {
                checksum: checksum.clone(),
                byte_size: 128,
                mime_type: "text/plain".to_owned(),
                file_name: format!("{label}.txt"),
                title: Some(format!("{label} fixture")),
                language_code: None,
                source_uri: None,
                document_hint: None,
            },
        )?;
        sqlx::query(
            "insert into content_document (
                id, workspace_id, library_id, external_key, document_state
             ) values ($1, $2, $3, $4, 'active')",
        )
        .bind(document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(format!("graph-reextract-{label}-{document_id}"))
        .execute(&self.pool)
        .await
        .context("failed to create graph re-extract document")?;
        sqlx::query(
            "insert into content_revision (
                id, document_id, workspace_id, library_id, revision_number,
                content_source_kind, checksum, mime_type, byte_size, title, admission_identity
             ) values ($1, $2, $3, $4, 1, 'upload', $5, 'text/plain', 128, $6, $7)",
        )
        .bind(revision_id)
        .bind(document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(checksum)
        .bind(format!("{label} fixture"))
        .bind(serde_json::to_value(admission)?)
        .execute(&self.pool)
        .await
        .context("failed to create graph re-extract revision")?;
        self.publish_revision(document_id, revision_id, label).await?;
        Ok((document_id, revision_id))
    }

    async fn publish_revision(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        label: &str,
    ) -> Result<()> {
        crate::content_lifecycle_support::seed_readable_revision_projection(
            &self.pool,
            document_id,
            revision_id,
            &format!("{label}.txt"),
        )
        .await
    }

    async fn create_extraction(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        status: &str,
    ) -> Result<()> {
        runtime_graph_repository::create_runtime_graph_extraction_record(
            &self.pool,
            &CreateRuntimeGraphExtractionRecordInput {
                id: Uuid::now_v7(),
                runtime_execution_id: self.runtime_execution_id,
                library_id: self.library_id,
                document_id,
                chunk_id: Uuid::now_v7(),
                provider_kind: "synthetic".to_string(),
                model_name: "fixture".to_string(),
                extraction_version: "test".to_string(),
                prompt_hash: "sha256:fixture".to_string(),
                status: status.to_string(),
                raw_output_json: serde_json::json!({
                    "lifecycle": { "revision_id": revision_id }
                }),
                normalized_output_json: serde_json::json!({
                    "entities": [],
                    "relations": []
                }),
                glean_pass_count: 0,
                error_message: (status != "ready")
                    .then(|| "synthetic extraction failure".to_string()),
            },
        )
        .await
        .context("failed to create graph re-extract extraction record")?;
        Ok(())
    }

    async fn create_revision_chunk(&self, revision_id: Uuid, label: &str) -> Result<Uuid> {
        let chunk_id = Uuid::now_v7();
        let normalized_text = format!(
            "A neutral {label} record connects reusable entities through explicit structural evidence. The fixture remains domain independent and exercises graph reconstruction deterministically."
        );
        let end_offset = i32::try_from(normalized_text.len())
            .context("graph re-extract fixture text exceeds content chunk offset capacity")?;
        sqlx::query(
            "insert into content_chunk (
                id, revision_id, chunk_index, start_offset, end_offset,
                token_count, normalized_text, text_checksum
             ) values ($1, $2, 0, 0, $3, 24, $4, $5)",
        )
        .bind(chunk_id)
        .bind(revision_id)
        .bind(end_offset)
        .bind(&normalized_text)
        .bind(format!("sha256:graph-reextract-chunk:{label}:{chunk_id}"))
        .execute(&self.pool)
        .await
        .context("failed to create graph re-extract content chunk")?;
        Ok(chunk_id)
    }

    async fn create_current_ready_extraction(
        &self,
        document_id: Uuid,
        chunk_id: Uuid,
        lifecycle_revision_id: Option<Uuid>,
        lifecycle_attempt_id: Option<Uuid>,
        entity_label: &str,
    ) -> Result<Uuid> {
        let extraction_id = Uuid::now_v7();
        let raw_output_json = lifecycle_revision_id.map_or_else(
            || serde_json::json!({}),
            |revision_id| {
                serde_json::json!({
                    "lifecycle": {
                        "revision_id": revision_id,
                        "activated_by_attempt_id": lifecycle_attempt_id,
                    }
                })
            },
        );
        runtime_graph_repository::create_runtime_graph_extraction_record(
            &self.pool,
            &CreateRuntimeGraphExtractionRecordInput {
                id: extraction_id,
                runtime_execution_id: self.runtime_execution_id,
                library_id: self.library_id,
                document_id,
                chunk_id,
                provider_kind: "synthetic".to_string(),
                model_name: "fixture".to_string(),
                extraction_version: GRAPH_EXTRACTION_VERSION.to_string(),
                prompt_hash: format!("sha256:graph-reextract:{extraction_id}"),
                status: "ready".to_string(),
                raw_output_json,
                normalized_output_json: serde_json::json!({
                    "entities": [{
                        "label": entity_label,
                        "node_type": "concept",
                        "sub_type": null,
                        "aliases": [],
                        "summary": "Neutral fixture entity"
                    }],
                    "relations": []
                }),
                glean_pass_count: 0,
                error_message: None,
            },
        )
        .await
        .context("failed to create current graph re-extract extraction record")?;
        Ok(extraction_id)
    }

    async fn publish_non_empty_ready_projection(
        &self,
        revision_label: &str,
        entity_label: &str,
    ) -> Result<(Uuid, Uuid)> {
        let (document_id, revision_id) = self.create_readable_revision(revision_label).await?;
        let chunk_id = self.create_revision_chunk(revision_id, revision_label).await?;
        let attempt_id = self
            .create_completed_graph_stage_with_count(
                document_id,
                revision_id,
                GRAPH_EXTRACTION_VERSION,
                serde_json::json!(1),
            )
            .await?;
        self.create_current_ready_extraction(
            document_id,
            chunk_id,
            Some(revision_id),
            Some(attempt_id),
            entity_label,
        )
        .await?;

        let outcome = self
            .state
            .application_services
            .graph
            .rebuild_library_graph(&self.state, self.library_id)
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to publish ready graph projection: {error}")
            })?;
        anyhow::ensure!(
            outcome.projection_version == 1
                && outcome.node_count > 0
                && outcome.graph_status == "ready",
            "expected non-empty ready projection v1, got {outcome:?}"
        );
        Ok((document_id, revision_id))
    }

    async fn projection_row_count(&self, projection_version: i64) -> Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "select
                (select count(*) from runtime_graph_node
                 where library_id = $1 and projection_version = $2)
              + (select count(*) from runtime_graph_edge
                 where library_id = $1 and projection_version = $2)",
        )
        .bind(self.library_id)
        .bind(projection_version)
        .fetch_one(&self.pool)
        .await
        .context("failed to count graph re-extract projection rows")
    }

    async fn evidence_count(&self) -> Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_evidence where library_id = $1",
        )
        .bind(self.library_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to count graph re-extract evidence rows")
    }

    async fn graph_state_digest(&self) -> Result<String> {
        sqlx::query_scalar::<_, String>(
            "select jsonb_build_object(
                'snapshot', coalesce(
                    (select to_jsonb(snapshot)
                     from runtime_graph_snapshot as snapshot
                     where snapshot.library_id = $1),
                    'null'::jsonb
                ),
                'nodes', coalesce(
                    (select jsonb_agg(
                        to_jsonb(node) order by node.projection_version, node.id
                     )
                     from runtime_graph_node as node
                     where node.library_id = $1),
                    '[]'::jsonb
                ),
                'edges', coalesce(
                    (select jsonb_agg(
                        to_jsonb(edge) order by edge.projection_version, edge.id
                     )
                     from runtime_graph_edge as edge
                     where edge.library_id = $1),
                    '[]'::jsonb
                ),
                'evidence', coalesce(
                    (select jsonb_agg(to_jsonb(evidence) order by evidence.id)
                     from runtime_graph_evidence as evidence
                     where evidence.library_id = $1),
                    '[]'::jsonb
                )
             )::text",
        )
        .bind(self.library_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to capture graph re-extract state digest")
    }

    async fn replace_readable_revision(&self, document_id: Uuid, label: &str) -> Result<Uuid> {
        let revision_id = Uuid::now_v7();
        let checksum =
            crate::content_lifecycle_support::fixture_checksum(&format!("{label}:{revision_id}"));
        let title = format!("{label} replacement fixture");
        let admission = crate::content_repository::synthetic_import_admission(
            ironrag_contracts::documents::ReplacementIdentity {
                checksum: checksum.clone(),
                byte_size: 128,
                mime_type: "text/plain".to_owned(),
                file_name: format!("{label}.txt"),
                title: Some(title.clone()),
                language_code: None,
                source_uri: None,
                document_hint: None,
            },
        )?;
        sqlx::query(
            "insert into content_revision (
                id, document_id, workspace_id, library_id, revision_number,
                content_source_kind, checksum, mime_type, byte_size, title, admission_identity
             ) values ($1, $2, $3, $4, 2, 'upload', $5, 'text/plain', 128, $6, $7)",
        )
        .bind(revision_id)
        .bind(document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(checksum)
        .bind(title)
        .bind(serde_json::to_value(admission)?)
        .execute(&self.pool)
        .await
        .context("failed to create replacement graph re-extract revision")?;
        self.publish_revision(document_id, revision_id, label).await?;
        Ok(revision_id)
    }

    async fn create_graph_job(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        queue_state: &str,
        queued_at: DateTime<Utc>,
    ) -> Result<ingest_repository::IngestJobRow> {
        self.create_graph_job_with_policy(GraphJobPolicySeed {
            document_id,
            revision_id,
            queue_state,
            queued_at,
            priority: 200,
            dedupe_key: format!(
                "graph_reextract:{revision_id}:{queue_state}:{}",
                queued_at.timestamp_micros()
            ),
        })
        .await
    }

    async fn create_graph_job_with_policy(
        &self,
        seed: GraphJobPolicySeed<'_>,
    ) -> Result<ingest_repository::IngestJobRow> {
        ingest_repository::create_ingest_job(
            &self.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(seed.document_id),
                knowledge_revision_id: Some(seed.revision_id),
                job_kind: "graph_refresh".to_string(),
                queue_state: seed.queue_state.to_string(),
                priority: seed.priority,
                dedupe_key: Some(seed.dedupe_key),
                queued_at: Some(seed.queued_at),
                available_at: Some(seed.queued_at),
                completed_at: matches!(seed.queue_state, "completed" | "failed" | "canceled")
                    .then_some(seed.queued_at),
            },
        )
        .await
        .context("failed to create graph re-extract job")
    }

    async fn create_terminal_automatic_graph_job(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        terminal_ordinal: usize,
        completed_at: DateTime<Utc>,
        queue_rank: Option<i64>,
    ) -> Result<Uuid> {
        let job = self
            .create_graph_job_with_policy(GraphJobPolicySeed {
                document_id,
                revision_id,
                queue_state: "failed",
                queued_at: completed_at,
                priority: AUTOMATIC_GRAPH_REEXTRACT_PRIORITY,
                dedupe_key: format!(
                    "graph_reextract:test:{revision_id}:terminal:{terminal_ordinal}"
                ),
            })
            .await?;
        if let Some(queue_rank) = queue_rank {
            sqlx::query("update ingest_job set queue_rank = $2 where id = $1")
                .bind(job.id)
                .bind(queue_rank)
                .execute(&self.pool)
                .await
                .context("failed to set durable automatic graph job rank")?;
        }
        ingest_repository::create_ingest_attempt(
            &self.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 5,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "failed".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(completed_at),
                heartbeat_at: Some(completed_at),
                finished_at: Some(completed_at),
                failure_class: Some("graph_refresh".to_string()),
                failure_code: Some("deterministic_fixture_failure".to_string()),
                failure_message: Some("synthetic terminal graph failure".to_string()),
                progress_percent: 10,
                retryable: false,
            },
        )
        .await
        .context("failed to create terminal automatic graph attempt")?;
        Ok(job.id)
    }

    /// Plants a terminal operator (priority-200) version-recovery job on the
    /// exact document/revision/extractor tuple. The durable terminal-failure
    /// history counts the calling path's own repair priority, so version
    /// recovery dead-letters the tuple at the shared ceiling exactly as
    /// automatic projection repair does for its priority-250 jobs.
    async fn create_terminal_explicit_graph_job(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        terminal_ordinal: usize,
        completed_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        let job = self
            .create_graph_job_with_policy(GraphJobPolicySeed {
                document_id,
                revision_id,
                queue_state: "failed",
                queued_at: completed_at,
                priority: EXPLICIT_GRAPH_REEXTRACT_PRIORITY,
                dedupe_key: format!(
                    "graph_reextract:test:{revision_id}:terminal:{terminal_ordinal}"
                ),
            })
            .await?;
        Ok(job.id)
    }

    async fn version_recovery_candidate_documents(
        &self,
        extraction_version: &str,
    ) -> Result<HashSet<Uuid>> {
        Ok(runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &self.pool,
            self.library_id,
            extraction_version,
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: true },
            64,
        )
        .await?
        .into_iter()
        .map(|row| row.1)
        .collect())
    }

    async fn create_completed_graph_stage(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        extraction_version: &str,
    ) -> Result<Uuid> {
        self.create_completed_graph_stage_at(
            document_id,
            revision_id,
            extraction_version,
            serde_json::json!(0),
            Utc::now(),
        )
        .await
    }

    async fn create_completed_graph_stage_with_count(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        extraction_version: &str,
        graph_contribution_count: serde_json::Value,
    ) -> Result<Uuid> {
        self.create_completed_graph_stage_at(
            document_id,
            revision_id,
            extraction_version,
            graph_contribution_count,
            Utc::now(),
        )
        .await
    }

    async fn create_interrupted_graph_stage(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        stage_state: &str,
    ) -> Result<()> {
        self.create_interrupted_graph_stage_at(document_id, revision_id, stage_state, Utc::now())
            .await
    }

    async fn create_interrupted_graph_stage_at(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        stage_state: &str,
        recorded_at: DateTime<Utc>,
    ) -> Result<()> {
        let job = self.create_graph_job(document_id, revision_id, "failed", recorded_at).await?;
        let attempt = ingest_repository::create_ingest_attempt(
            &self.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "failed".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(recorded_at),
                heartbeat_at: Some(recorded_at),
                finished_at: Some(recorded_at),
                failure_class: Some("synthetic".to_string()),
                failure_code: Some("graph_interrupted".to_string()),
                failure_message: Some("synthetic graph interruption".to_string()),
                progress_percent: 50,
                retryable: true,
            },
        )
        .await
        .context("failed to create interrupted graph re-extract attempt")?;
        ingest_repository::create_ingest_stage_event(
            &self.pool,
            &ingest_repository::NewIngestStageEvent {
                attempt_id: attempt.id,
                stage_name: "extract_graph".to_string(),
                stage_state: stage_state.to_string(),
                ordinal: 1,
                message: None,
                details_json: serde_json::json!({ "revisionId": revision_id }),
                recorded_at: Some(recorded_at),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: Some(1),
            },
        )
        .await
        .context("failed to create interrupted extract_graph stage")?;
        Ok(())
    }

    async fn create_document_node(
        &self,
        document_id: Uuid,
        projection_version: i64,
    ) -> Result<Uuid> {
        let node = runtime_graph_repository::upsert_runtime_graph_document_node(
            &self.pool,
            runtime_graph_repository::RuntimeGraphDocumentNodeInput {
                library_id: self.library_id,
                document_id,
                identity_key: &format!("document:{document_id}"),
                label: "Synthetic document",
                aliases_json: serde_json::json!([]),
                summary: None,
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version,
            },
        )
        .await
        .context("failed to create graph re-extract document node")?;
        Ok(node.id)
    }

    async fn create_published_document_node(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        projection_version: i64,
    ) -> Result<()> {
        let node_id = self.create_document_node(document_id, projection_version).await?;
        runtime_graph_repository::create_runtime_graph_evidence(
            &self.pool,
            CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "node",
                target_id: node_id,
                document_id: Some(document_id),
                revision_id: Some(revision_id),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("synthetic.txt"),
                page_ref: None,
                evidence_text: "Synthetic publication evidence",
                confidence_score: None,
                evidence_context_key: "document_node",
            },
        )
        .await
        .context("failed to create exact-revision graph publication evidence")?;
        Ok(())
    }

    async fn create_graph_completion_event(
        &self,
        attempt_id: Uuid,
        stamp: GraphCompletionStamp<'_>,
        ordinal: i32,
        recorded_at: DateTime<Utc>,
    ) -> Result<()> {
        let mut details_json = serde_json::json!({
            "extractionVersion": stamp.extraction_version,
            "graphContributionCount": stamp.graph_contribution_count,
        });
        if let Some(extraction_contract_version) = stamp.extraction_contract_version {
            details_json["extractionContractVersion"] =
                serde_json::Value::String(extraction_contract_version.to_string());
        }
        ingest_repository::create_ingest_stage_event(
            &self.pool,
            &ingest_repository::NewIngestStageEvent {
                attempt_id,
                stage_name: "extract_graph".to_string(),
                stage_state: "completed".to_string(),
                ordinal,
                message: None,
                details_json,
                recorded_at: Some(recorded_at),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: Some(1),
            },
        )
        .await
        .context("failed to create completed graph extraction stage")?;
        Ok(())
    }

    async fn create_completed_graph_attempt_on_job(
        &self,
        job_id: Uuid,
        attempt_number: i32,
        stamp: GraphCompletionStamp<'_>,
        completed_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        let attempt = ingest_repository::create_ingest_attempt(
            &self.pool,
            &ingest_repository::NewIngestAttempt {
                job_id,
                attempt_number,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "succeeded".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(completed_at),
                heartbeat_at: Some(completed_at),
                finished_at: Some(completed_at),
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 100,
                retryable: false,
            },
        )
        .await
        .context("failed to create completed graph re-extract attempt")?;
        self.create_graph_completion_event(attempt.id, stamp, 1, completed_at).await?;
        Ok(attempt.id)
    }

    async fn create_completed_graph_stage_at_with_contract(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        stamp: GraphCompletionStamp<'_>,
        completed_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        let job =
            self.create_graph_job(document_id, revision_id, "completed", completed_at).await?;
        self.create_completed_graph_attempt_on_job(job.id, 1, stamp, completed_at).await
    }

    async fn create_completed_graph_stage_at(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        extraction_version: &str,
        graph_contribution_count: serde_json::Value,
        completed_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        self.create_completed_graph_stage_at_with_contract(
            document_id,
            revision_id,
            GraphCompletionStamp {
                extraction_version,
                extraction_contract_version: Some(GRAPH_EXTRACTION_CONTRACT_VERSION),
                graph_contribution_count,
            },
            completed_at,
        )
        .await
    }

    /// Plants a pre-IRG-197 completion: a completed stage whose details carry
    /// no extraction-contract stamp, exactly like a revision extracted before
    /// the contract key existed.
    async fn create_legacy_completed_graph_stage(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        extraction_version: &str,
    ) -> Result<Uuid> {
        self.create_completed_graph_stage_at_with_contract(
            document_id,
            revision_id,
            GraphCompletionStamp {
                extraction_version,
                extraction_contract_version: None,
                graph_contribution_count: serde_json::json!(0),
            },
            Utc::now(),
        )
        .await
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        self.database.drop().await
    }
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_reextract_sweep_requires_active_library_state() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let (document_id, revision_id) = fixture.create_readable_revision("archived").await?;
    fixture.create_extraction(document_id, revision_id, "failed").await?;

    let active_ids = catalog_repository::list_active_library_ids(&fixture.pool).await?;
    assert!(active_ids.contains(&fixture.library_id));
    assert_eq!(
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: false },
            64,
        )
        .await?
        .len(),
        1
    );

    sqlx::query(
        "update catalog_library
         set lifecycle_state = 'archived'::catalog_library_lifecycle_state
         where id = $1",
    )
    .bind(fixture.library_id)
    .execute(&fixture.pool)
    .await
    .context("failed to archive graph re-extract fixture library")?;

    let active_ids = catalog_repository::list_active_library_ids(&fixture.pool).await?;
    assert!(!active_ids.contains(&fixture.library_id));
    assert!(
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: false },
            64,
        )
        .await?
        .is_empty()
    );
    let enqueue = ingest_repository::enqueue_active_library_ingest_job_with_live_dedupe(
        &fixture.pool,
        &ingest_repository::NewIngestJob {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            mutation_id: None,
            mutation_item_id: None,
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: Some(document_id),
            knowledge_revision_id: Some(revision_id),
            job_kind: "graph_refresh".to_string(),
            queue_state: "queued".to_string(),
            priority: 200,
            dedupe_key: Some(format!("graph_reextract:test:{revision_id}")),
            queued_at: Some(Utc::now()),
            available_at: Some(Utc::now()),
            completed_at: None,
        },
    )
    .await?;
    assert!(enqueue.is_none());

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_reextract_candidates_require_a_current_completed_stage() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let (absent_document_id, absent_revision_id) =
        fixture.create_readable_revision("absent").await?;
    fixture.create_extraction(absent_document_id, Uuid::now_v7(), "ready").await?;
    fixture.create_graph_job(absent_document_id, Uuid::now_v7(), "queued", Utc::now()).await?;

    let (partial_document_id, partial_revision_id) =
        fixture.create_readable_revision("partial").await?;
    fixture.create_extraction(partial_document_id, partial_revision_id, "ready").await?;
    fixture.create_extraction(partial_document_id, partial_revision_id, "failed").await?;

    let (complete_document_id, complete_revision_id) =
        fixture.create_readable_revision("complete").await?;
    fixture.create_extraction(complete_document_id, complete_revision_id, "ready").await?;
    fixture
        .create_completed_graph_stage(complete_document_id, complete_revision_id, "test")
        .await?;

    let (old_version_document_id, old_version_revision_id) =
        fixture.create_readable_revision("old-version").await?;
    fixture.create_extraction(old_version_document_id, old_version_revision_id, "ready").await?;
    fixture
        .create_completed_graph_stage(
            old_version_document_id,
            old_version_revision_id,
            "previous-version",
        )
        .await?;

    let (never_document_id, never_revision_id) = fixture.create_readable_revision("never").await?;

    let (live_document_id, live_revision_id) = fixture.create_readable_revision("live").await?;
    fixture.create_extraction(live_document_id, live_revision_id, "failed").await?;
    fixture.create_graph_job(live_document_id, live_revision_id, "queued", Utc::now()).await?;

    let candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: true },
            64,
        )
        .await?;
    let candidate_documents =
        candidates.iter().map(|(_, document_id, _)| *document_id).collect::<HashSet<_>>();
    assert_eq!(
        candidate_documents,
        HashSet::from([
            absent_document_id,
            partial_document_id,
            old_version_document_id,
            never_document_id,
        ])
    );
    assert!(candidates.contains(&(fixture.workspace_id, absent_document_id, absent_revision_id)));
    assert!(candidates.contains(&(fixture.workspace_id, partial_document_id, partial_revision_id)));
    assert!(candidates.contains(&(
        fixture.workspace_id,
        old_version_document_id,
        old_version_revision_id,
    )));
    assert!(candidates.contains(&(fixture.workspace_id, never_document_id, never_revision_id)));
    assert!(!candidate_documents.contains(&complete_document_id));
    assert!(!candidate_documents.contains(&live_document_id));

    let history_only =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: false },
            64,
        )
        .await?;
    assert!(!history_only.iter().any(|row| row.1 == never_document_id));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_reextract_selection_modes_separate_automatic_repair_from_version_migration()
-> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    sqlx::query(
        "insert into runtime_graph_snapshot (library_id, graph_status, projection_version)
         values ($1, 'ready', 2)",
    )
    .bind(fixture.library_id)
    .execute(&fixture.pool)
    .await
    .context("failed to create active graph projection fixture")?;

    let (missing_id, missing_revision_id) = fixture.create_readable_revision("missing").await?;
    fixture.create_extraction(missing_id, missing_revision_id, "failed").await?;

    let (never_missing_id, _) = fixture.create_readable_revision("never-missing").await?;

    let (old_history_only_id, old_history_only_revision_id) =
        fixture.create_readable_revision("old-history-only").await?;
    fixture.create_extraction(old_history_only_id, old_history_only_revision_id, "failed").await?;
    fixture
        .create_published_document_node(old_history_only_id, old_history_only_revision_id, 2)
        .await?;
    fixture.replace_readable_revision(old_history_only_id, "old-history-only-r2").await?;

    let (stale_publication_id, stale_publication_revision_id) =
        fixture.create_readable_revision("stale-publication-r1").await?;
    fixture
        .create_published_document_node(stale_publication_id, stale_publication_revision_id, 2)
        .await?;
    let stale_publication_current_revision_id =
        fixture.replace_readable_revision(stale_publication_id, "stale-publication-r2").await?;
    fixture
        .create_extraction(stale_publication_id, stale_publication_current_revision_id, "failed")
        .await?;

    let (started_interruption_id, started_interruption_revision_id) =
        fixture.create_readable_revision("started-interruption").await?;
    fixture
        .create_interrupted_graph_stage(
            started_interruption_id,
            started_interruption_revision_id,
            "started",
        )
        .await?;

    let (failed_interruption_id, failed_interruption_revision_id) =
        fixture.create_readable_revision("failed-interruption").await?;
    fixture
        .create_interrupted_graph_stage(
            failed_interruption_id,
            failed_interruption_revision_id,
            "failed",
        )
        .await?;

    let (live_interruption_id, live_interruption_revision_id) =
        fixture.create_readable_revision("live-interruption").await?;
    fixture
        .create_interrupted_graph_stage(
            live_interruption_id,
            live_interruption_revision_id,
            "failed",
        )
        .await?;
    fixture
        .create_graph_job(live_interruption_id, live_interruption_revision_id, "queued", Utc::now())
        .await?;

    let (completed_after_interruption_id, completed_after_interruption_revision_id) =
        fixture.create_readable_revision("completed-after-interruption").await?;
    // A later-created job carries an older terminal timestamp. Terminal/event
    // causality must keep the more recent failure authoritative.
    fixture
        .create_interrupted_graph_stage_at(
            completed_after_interruption_id,
            completed_after_interruption_revision_id,
            "failed",
            Utc::now() + Duration::minutes(1),
        )
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    fixture
        .create_completed_graph_stage_at(
            completed_after_interruption_id,
            completed_after_interruption_revision_id,
            "test",
            serde_json::json!("malformed"),
            Utc::now() - Duration::minutes(1),
        )
        .await?;

    let (active_node_missing_completion_id, active_node_missing_completion_revision_id) =
        fixture.create_readable_revision("active-node-missing-completion").await?;
    fixture
        .create_completed_graph_stage_with_count(
            active_node_missing_completion_id,
            active_node_missing_completion_revision_id,
            "legacy-version",
            serde_json::json!(1),
        )
        .await?;

    let (positive_without_publication_id, positive_without_publication_revision_id) =
        fixture.create_readable_revision("positive-without-publication").await?;
    fixture
        .create_extraction(
            positive_without_publication_id,
            positive_without_publication_revision_id,
            "ready",
        )
        .await?;
    fixture
        .create_completed_graph_stage_with_count(
            positive_without_publication_id,
            positive_without_publication_revision_id,
            "test",
            serde_json::json!(1),
        )
        .await?;
    fixture.create_document_node(positive_without_publication_id, 2).await?;
    fixture
        .create_published_document_node(
            active_node_missing_completion_id,
            active_node_missing_completion_revision_id,
            2,
        )
        .await?;

    let (active_node_history_missing_completion_id, active_node_history_revision_id) =
        fixture.create_readable_revision("active-node-history-missing-completion").await?;
    fixture
        .create_extraction(
            active_node_history_missing_completion_id,
            active_node_history_revision_id,
            "failed",
        )
        .await?;
    fixture
        .create_published_document_node(
            active_node_history_missing_completion_id,
            active_node_history_revision_id,
            2,
        )
        .await?;

    let (malformed_id, malformed_revision_id) =
        fixture.create_readable_revision("malformed").await?;
    fixture.create_extraction(malformed_id, malformed_revision_id, "ready").await?;
    fixture
        .create_completed_graph_stage_with_count(
            malformed_id,
            malformed_revision_id,
            "test",
            serde_json::json!("malformed"),
        )
        .await?;

    let (positive_missing_id, positive_missing_revision_id) =
        fixture.create_readable_revision("positive-missing").await?;
    fixture.create_extraction(positive_missing_id, positive_missing_revision_id, "ready").await?;
    fixture
        .create_completed_graph_stage_with_count(
            positive_missing_id,
            positive_missing_revision_id,
            "test",
            serde_json::json!(1),
        )
        .await?;

    let (zero_id, zero_revision_id) = fixture.create_readable_revision("zero").await?;
    fixture.create_extraction(zero_id, zero_revision_id, "ready").await?;
    fixture.create_completed_graph_stage(zero_id, zero_revision_id, "test").await?;

    let (old_projection_id, old_projection_revision_id) =
        fixture.create_readable_revision("old-projection").await?;
    fixture.create_extraction(old_projection_id, old_projection_revision_id, "ready").await?;
    fixture
        .create_completed_graph_stage_with_count(
            old_projection_id,
            old_projection_revision_id,
            "test",
            serde_json::json!(1),
        )
        .await?;
    fixture
        .create_published_document_node(old_projection_id, old_projection_revision_id, 1)
        .await?;

    let (active_projection_id, active_projection_revision_id) =
        fixture.create_readable_revision("active-projection").await?;
    fixture.create_extraction(active_projection_id, active_projection_revision_id, "ready").await?;
    fixture
        .create_completed_graph_stage_with_count(
            active_projection_id,
            active_projection_revision_id,
            "test",
            serde_json::json!(1),
        )
        .await?;
    fixture
        .create_published_document_node(active_projection_id, active_projection_revision_id, 2)
        .await?;

    let (deleted_id, deleted_revision_id) = fixture.create_readable_revision("deleted").await?;
    fixture.create_extraction(deleted_id, deleted_revision_id, "failed").await?;
    sqlx::query(
        "update content_document
         set document_state = 'deleted'::content_document_state,
             deleted_at = now()
         where id = $1",
    )
    .bind(deleted_id)
    .execute(&fixture.pool)
    .await
    .context("failed to mark graph re-extract document deleted")?;

    let (unreadable_id, unreadable_revision_id) =
        fixture.create_readable_revision("unreadable").await?;
    fixture.create_extraction(unreadable_id, unreadable_revision_id, "failed").await?;
    sqlx::query(
        "update content_document_head
         set readable_revision_id = null
         where document_id = $1",
    )
    .bind(unreadable_id)
    .execute(&fixture.pool)
    .await
    .context("failed to mark graph re-extract head unreadable")?;

    let candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::ProjectionRepair,
            64,
        )
        .await?;
    let candidate_documents = candidates.iter().map(|row| row.1).collect::<HashSet<_>>();
    assert_eq!(
        candidate_documents,
        HashSet::from([
            missing_id,
            stale_publication_id,
            started_interruption_id,
            failed_interruption_id,
            completed_after_interruption_id,
            malformed_id,
            positive_missing_id,
            positive_without_publication_id,
            old_projection_id,
        ])
    );
    assert!(!candidate_documents.contains(&never_missing_id));
    assert!(!candidate_documents.contains(&old_history_only_id));
    assert!(!candidate_documents.contains(&live_interruption_id));
    assert!(!candidate_documents.contains(&zero_id));
    assert!(!candidate_documents.contains(&active_projection_id));
    assert!(!candidate_documents.contains(&active_node_missing_completion_id));
    assert!(!candidate_documents.contains(&active_node_history_missing_completion_id));
    assert!(!candidate_documents.contains(&deleted_id));
    assert!(!candidate_documents.contains(&unreadable_id));

    let operator_candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: true },
            64,
        )
        .await?;
    let operator_candidate_documents =
        operator_candidates.iter().map(|row| row.1).collect::<HashSet<_>>();
    assert_eq!(
        operator_candidate_documents,
        HashSet::from([
            missing_id,
            never_missing_id,
            old_history_only_id,
            stale_publication_id,
            started_interruption_id,
            failed_interruption_id,
            completed_after_interruption_id,
            active_node_missing_completion_id,
            active_node_history_missing_completion_id,
            malformed_id,
            positive_missing_id,
            positive_without_publication_id,
            old_projection_id,
        ])
    );
    assert!(!operator_candidate_documents.contains(&live_interruption_id));

    let history_only_operator_candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: false },
            64,
        )
        .await?;
    let history_only_operator_documents =
        history_only_operator_candidates.iter().map(|row| row.1).collect::<HashSet<_>>();
    assert!(!history_only_operator_documents.contains(&never_missing_id));
    assert!(history_only_operator_documents.contains(&old_history_only_id));
    assert!(history_only_operator_documents.contains(&stale_publication_id));
    assert!(!history_only_operator_documents.contains(&started_interruption_id));
    assert!(!history_only_operator_documents.contains(&failed_interruption_id));
    assert!(!history_only_operator_documents.contains(&completed_after_interruption_id));
    assert!(!history_only_operator_documents.contains(&active_node_missing_completion_id));
    assert!(history_only_operator_documents.contains(&active_node_history_missing_completion_id));
    assert!(history_only_operator_documents.contains(&positive_without_publication_id));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_reextract_contract_gap_is_operator_only() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;

    // A completion under the running contract with zero contributions is
    // terminal for both boundaries.
    let (current_document_id, current_revision_id) =
        fixture.create_readable_revision("contract-current").await?;
    fixture.create_extraction(current_document_id, current_revision_id, "ready").await?;
    fixture.create_completed_graph_stage(current_document_id, current_revision_id, "test").await?;

    // A pre-IRG-197 completion: matching extraction version, no contract stamp.
    let (legacy_document_id, legacy_revision_id) =
        fixture.create_readable_revision("contract-legacy").await?;
    fixture.create_extraction(legacy_document_id, legacy_revision_id, "ready").await?;
    fixture
        .create_legacy_completed_graph_stage(legacy_document_id, legacy_revision_id, "test")
        .await?;

    let operator_candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: false },
            64,
        )
        .await?;
    let operator_documents = operator_candidates.iter().map(|row| row.1).collect::<HashSet<_>>();
    assert!(
        operator_documents.contains(&legacy_document_id),
        "explicit version recovery must re-enqueue a completion stamped by another extraction contract"
    );
    assert!(
        !operator_documents.contains(&current_document_id),
        "a zero-contribution completion under the running contract stays terminal for the operator"
    );

    let automatic_candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::ProjectionRepair,
            64,
        )
        .await?;
    assert!(
        automatic_candidates.is_empty(),
        "automatic projection repair must not cross an extraction-contract gap"
    );

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_reextract_candidates_prioritize_never_attempted_documents() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let (retried_document_id, retried_revision_id) =
        fixture.create_readable_revision("retried").await?;
    fixture.create_extraction(retried_document_id, retried_revision_id, "failed").await?;
    fixture
        .create_graph_job(
            retried_document_id,
            retried_revision_id,
            "failed",
            Utc::now() - Duration::minutes(5),
        )
        .await?;

    let (fresh_one_id, _) = fixture.create_readable_revision("fresh-one").await?;
    let (fresh_two_id, _) = fixture.create_readable_revision("fresh-two").await?;

    let candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: true },
            2,
        )
        .await?;
    let candidate_documents = candidates.iter().map(|row| row.1).collect::<HashSet<_>>();
    assert_eq!(candidate_documents, HashSet::from([fresh_one_id, fresh_two_id]));
    assert!(!candidate_documents.contains(&retried_document_id));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn automatic_graph_repair_respects_cancellation_backoff_and_terminal_job_ceiling()
-> Result<()> {
    let fixture = GraphReextractFixture::create().await?;

    let (canceled_document_id, canceled_revision_id) =
        fixture.create_readable_revision("automatic-canceled").await?;
    fixture.create_extraction(canceled_document_id, canceled_revision_id, "failed").await?;
    fixture
        .create_graph_job(canceled_document_id, canceled_revision_id, "canceled", Utc::now())
        .await?;

    let (backoff_document_id, backoff_revision_id) =
        fixture.create_readable_revision("automatic-backoff").await?;
    fixture.create_extraction(backoff_document_id, backoff_revision_id, "failed").await?;
    fixture
        .create_terminal_automatic_graph_job(
            backoff_document_id,
            backoff_revision_id,
            1,
            Utc::now(),
            Some(1),
        )
        .await?;

    let (bounded_document_id, bounded_revision_id) =
        fixture.create_readable_revision("automatic-bounded").await?;
    fixture.create_extraction(bounded_document_id, bounded_revision_id, "failed").await?;
    for terminal_ordinal in 1..=2 {
        fixture
            .create_terminal_automatic_graph_job(
                bounded_document_id,
                bounded_revision_id,
                terminal_ordinal,
                Utc::now() - Duration::hours(2),
                Some(
                    Utc::now().timestamp_micros()
                        + i64::try_from(terminal_ordinal).unwrap_or(i64::MAX),
                ),
            )
            .await?;
    }

    let retry_candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::ProjectionRepair,
            64,
        )
        .await?;
    assert_eq!(
        retry_candidates.iter().map(|row| row.1).collect::<HashSet<_>>(),
        HashSet::from([bounded_document_id]),
        "cancellation is a hard stop, recent completion stays in cooldown despite an old queue rank, and old failures leave cooldown despite recent ranks"
    );

    fixture
        .create_terminal_automatic_graph_job(
            bounded_document_id,
            bounded_revision_id,
            3,
            Utc::now() - Duration::hours(2),
            Some(Utc::now().timestamp_micros()),
        )
        .await?;
    let dead_lettered =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::ProjectionRepair,
            64,
        )
        .await?;
    assert!(
        dead_lettered.is_empty(),
        "three terminal automatic jobs must dead-letter the exact revision/version tuple"
    );

    let explicit =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: false },
            64,
        )
        .await?;
    assert_eq!(
        explicit.iter().map(|row| row.1).collect::<HashSet<_>>(),
        HashSet::from([canceled_document_id, backoff_document_id, bounded_document_id]),
        "an explicit version-recovery request must intentionally override automatic stop policy"
    );

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn version_recovery_respects_canonical_terminal_ceiling_and_cooldown() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;

    let (under_ceiling_id, under_ceiling_revision_id) =
        fixture.create_readable_revision("version-under-ceiling").await?;
    fixture.create_extraction(under_ceiling_id, under_ceiling_revision_id, "failed").await?;
    for terminal_ordinal in 1..=2 {
        fixture
            .create_terminal_explicit_graph_job(
                under_ceiling_id,
                under_ceiling_revision_id,
                terminal_ordinal,
                Utc::now() - Duration::hours(2),
            )
            .await?;
    }

    let (dead_lettered_id, dead_lettered_revision_id) =
        fixture.create_readable_revision("version-dead-lettered").await?;
    fixture.create_extraction(dead_lettered_id, dead_lettered_revision_id, "failed").await?;
    for terminal_ordinal in 1..=3 {
        fixture
            .create_terminal_explicit_graph_job(
                dead_lettered_id,
                dead_lettered_revision_id,
                terminal_ordinal,
                Utc::now() - Duration::hours(2),
            )
            .await?;
    }

    let (cooling_down_id, cooling_down_revision_id) =
        fixture.create_readable_revision("version-cooldown").await?;
    fixture.create_extraction(cooling_down_id, cooling_down_revision_id, "failed").await?;
    fixture
        .create_terminal_explicit_graph_job(
            cooling_down_id,
            cooling_down_revision_id,
            1,
            Utc::now(),
        )
        .await?;

    let admitted = fixture.version_recovery_candidate_documents("test").await?;
    assert!(
        admitted.contains(&under_ceiling_id),
        "two old terminal version-recovery jobs stay inside the canonical ceiling"
    );
    assert!(
        !admitted.contains(&dead_lettered_id),
        "three terminal operator jobs must dead-letter the exact revision/extractor tuple"
    );
    assert!(
        !admitted.contains(&cooling_down_id),
        "a terminal operator job completed now stays in the canonical cooldown"
    );

    // Ageing the terminal completion past the canonical window re-admits the
    // tuple without changing the ceiling: the cooldown was the only blocker.
    sqlx::query(
        "update ingest_job
         set completed_at = now() - interval '2 hours'
         where knowledge_document_id = $1",
    )
    .bind(cooling_down_id)
    .execute(&fixture.pool)
    .await
    .context("failed to age the version-recovery cooldown fixture")?;

    let after_cooldown = fixture.version_recovery_candidate_documents("test").await?;
    assert!(
        after_cooldown.contains(&cooling_down_id),
        "the tuple re-admits once the terminal completion leaves the cooldown window"
    );
    assert!(!after_cooldown.contains(&dead_lettered_id));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_reextract_candidate_fairness_uses_database_queue_rank_before_timestamps()
-> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let (future_timestamp_document_id, future_timestamp_revision_id) =
        fixture.create_readable_revision("future-application-timestamp").await?;
    fixture
        .create_extraction(future_timestamp_document_id, future_timestamp_revision_id, "failed")
        .await?;
    let future_timestamp_job = fixture
        .create_graph_job(
            future_timestamp_document_id,
            future_timestamp_revision_id,
            "failed",
            Utc::now() + Duration::days(30),
        )
        .await?;

    let (past_timestamp_document_id, past_timestamp_revision_id) =
        fixture.create_readable_revision("past-application-timestamp").await?;
    fixture
        .create_extraction(past_timestamp_document_id, past_timestamp_revision_id, "failed")
        .await?;
    let past_timestamp_job = fixture
        .create_graph_job(
            past_timestamp_document_id,
            past_timestamp_revision_id,
            "failed",
            Utc::now() - Duration::days(30),
        )
        .await?;

    sqlx::query(
        "update ingest_job
         set queue_rank = case when id = $1 then 10 else 20 end
         where id in ($1, $2)",
    )
    .bind(future_timestamp_job.id)
    .bind(past_timestamp_job.id)
    .execute(&fixture.pool)
    .await
    .context("failed to plant conflicting durable ranks and application timestamps")?;

    let candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::ProjectionRepair,
            1,
        )
        .await?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].1, future_timestamp_document_id);

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_reextract_progress_uses_completed_current_version_stages() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let (complete_document_id, complete_revision_id) =
        fixture.create_readable_revision("progress-complete").await?;
    fixture.create_extraction(complete_document_id, complete_revision_id, "ready").await?;
    fixture
        .create_completed_graph_stage(complete_document_id, complete_revision_id, "test")
        .await?;

    let (incomplete_document_id, incomplete_revision_id) =
        fixture.create_readable_revision("progress-incomplete").await?;
    fixture.create_extraction(incomplete_document_id, incomplete_revision_id, "ready").await?;
    fixture.create_readable_revision("progress-never").await?;

    let progress = runtime_graph_repository::get_library_graph_reextract_progress(
        &fixture.pool,
        fixture.library_id,
        "test",
    )
    .await?;
    assert_eq!(progress.ready_document_count, 3);
    assert_eq!(progress.known_eligible_document_count, 2);
    assert_eq!(progress.current_extraction_document_count, 1);
    assert_eq!(progress.missing_current_extraction_document_count, 2);
    assert_eq!(progress.outdated_or_incomplete_document_count, 1);
    assert_eq!(progress.never_extracted_document_count, 1);

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_completion_consumers_use_terminal_causality_without_fallback() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let older = Utc::now() - Duration::minutes(2);
    let newer = Utc::now() - Duration::minutes(1);

    let (malformed_document_id, malformed_revision_id) =
        fixture.create_readable_revision("latest-malformed").await?;
    // The later-created job carries an older terminal timestamp. The newer
    // terminal completion must remain authoritative regardless of queue rank.
    fixture
        .create_completed_graph_stage_at(
            malformed_document_id,
            malformed_revision_id,
            "test",
            serde_json::json!(1),
            newer,
        )
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    fixture
        .create_completed_graph_stage_at(
            malformed_document_id,
            malformed_revision_id,
            "test",
            serde_json::json!("malformed"),
            older,
        )
        .await?;

    let (oversized_document_id, oversized_revision_id) =
        fixture.create_readable_revision("latest-oversized").await?;
    fixture
        .create_completed_graph_stage_at(
            oversized_document_id,
            oversized_revision_id,
            "test",
            serde_json::json!(1),
            newer,
        )
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let oversized_attempt_id = fixture
        .create_completed_graph_stage_at(
            oversized_document_id,
            oversized_revision_id,
            "test",
            serde_json::json!(0),
            older,
        )
        .await?;
    sqlx::query(
        "update ingest_stage_event
         set details_json = jsonb_set(
             details_json,
             '{graphContributionCount}',
             to_jsonb($2::numeric)
         )
         where attempt_id = $1",
    )
    .bind(oversized_attempt_id)
    .bind(format!("{}0", usize::MAX))
    .execute(&fixture.pool)
    .await
    .context("failed to plant an oversized latest graph completion count")?;

    let (ordinal_document_id, ordinal_revision_id) =
        fixture.create_readable_revision("latest-ordinal").await?;
    let ordinal_attempt_id = fixture
        .create_completed_graph_stage_at(
            ordinal_document_id,
            ordinal_revision_id,
            "test",
            serde_json::json!(1),
            newer,
        )
        .await?;
    // Within one attempt, repository-owned ordinal 2 must beat ordinal 1 even
    // when its caller-provided timestamp is older.
    fixture
        .create_graph_completion_event(
            ordinal_attempt_id,
            GraphCompletionStamp {
                extraction_version: "test",
                extraction_contract_version: Some(GRAPH_EXTRACTION_CONTRACT_VERSION),
                graph_contribution_count: serde_json::json!("malformed"),
            },
            2,
            older,
        )
        .await?;

    let (attempt_document_id, attempt_revision_id) =
        fixture.create_readable_revision("latest-attempt").await?;
    let attempt_job = fixture
        .create_graph_job(attempt_document_id, attempt_revision_id, "completed", newer)
        .await?;
    fixture
        .create_completed_graph_attempt_on_job(
            attempt_job.id,
            1,
            GraphCompletionStamp {
                extraction_version: "test",
                extraction_contract_version: Some(GRAPH_EXTRACTION_CONTRACT_VERSION),
                graph_contribution_count: serde_json::json!(1),
            },
            newer,
        )
        .await?;
    // Within one job, attempt 2 must beat attempt 1 even when every
    // caller-provided timestamp on attempt 2 is older.
    fixture
        .create_completed_graph_attempt_on_job(
            attempt_job.id,
            2,
            GraphCompletionStamp {
                extraction_version: "test",
                extraction_contract_version: Some(GRAPH_EXTRACTION_CONTRACT_VERSION),
                graph_contribution_count: serde_json::json!("malformed"),
            },
            older,
        )
        .await?;

    // Version recovery also repairs a valid positive completion whose exact
    // revision has not reached the active projection. Seed publication for
    // the two documents that exercise cross-job terminal causality so the
    // candidate assertion isolates completion selection instead of a genuine
    // projection gap.
    fixture.create_published_document_node(malformed_document_id, malformed_revision_id, 1).await?;
    fixture.create_published_document_node(oversized_document_id, oversized_revision_id, 1).await?;

    let candidates =
        runtime_graph_repository::list_library_documents_needing_graph_reextract_for_version(
            &fixture.pool,
            fixture.library_id,
            "test",
            GRAPH_EXTRACTION_CONTRACT_VERSION,
            GraphReextractSelectionMode::VersionRecovery { include_never_extracted: true },
            64,
        )
        .await?;
    let candidate_documents = candidates.iter().map(|row| row.1).collect::<HashSet<_>>();
    assert_eq!(candidate_documents, HashSet::from([ordinal_document_id, attempt_document_id]));

    let progress = runtime_graph_repository::get_library_graph_reextract_progress(
        &fixture.pool,
        fixture.library_id,
        "test",
    )
    .await?;
    assert_eq!(progress.ready_document_count, 4);
    assert_eq!(progress.current_extraction_document_count, 2);
    assert_eq!(progress.missing_current_extraction_document_count, 2);

    let completions = runtime_graph_repository::list_graph_rebuild_document_completions(
        &fixture.pool,
        fixture.library_id,
        "test",
    )
    .await?;
    assert_eq!(completions.len(), 4);
    assert_eq!(
        completions
            .iter()
            .map(|row| (row.document_id, row.graph_contribution_count))
            .collect::<std::collections::HashMap<_, _>>(),
        std::collections::HashMap::from([
            (malformed_document_id, Some(1)),
            (oversized_document_id, Some(1)),
            (ordinal_document_id, None),
            (attempt_document_id, None),
        ])
    );

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn refused_partial_rebuild_cannot_seed_a_later_projection() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let result = async {
        let (first_document_id, first_revision_id) =
            fixture.create_readable_revision("refused-partial-first-r1").await?;
        let first_chunk_id =
            fixture.create_revision_chunk(first_revision_id, "refused-partial-first-r1").await?;
        let first_attempt_id = fixture
            .create_completed_graph_stage_with_count(
                first_document_id,
                first_revision_id,
                GRAPH_EXTRACTION_VERSION,
                serde_json::json!(1),
            )
            .await?;
        fixture
            .create_current_ready_extraction(
                first_document_id,
                first_chunk_id,
                Some(first_revision_id),
                Some(first_attempt_id),
                "First Neutral Entity",
            )
            .await?;

        let (second_document_id, second_revision_id) =
            fixture.create_readable_revision("refused-partial-second-r1").await?;
        let second_chunk_id =
            fixture.create_revision_chunk(second_revision_id, "refused-partial-second-r1").await?;
        let second_attempt_id = fixture
            .create_completed_graph_stage_with_count(
                second_document_id,
                second_revision_id,
                GRAPH_EXTRACTION_VERSION,
                serde_json::json!(1),
            )
            .await?;

        let first_rebuild = fixture
            .state
            .application_services
            .graph
            .rebuild_library_graph(&fixture.state, fixture.library_id)
            .await;
        let first_rebuild_is_coverage_conflict =
            matches!(&first_rebuild, Err(GraphServiceError::StateConflict { .. }));
        let first_rebuild_debug = format!("{first_rebuild:?}");
        let refused_projection_rows = fixture.projection_row_count(1).await?;
        let refused_evidence_rows = fixture.evidence_count().await?;
        let snapshot_rows_after_refusal = sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_snapshot where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count snapshots after refused graph rebuild")?;

        let replacement_revision_id = fixture
            .replace_readable_revision(first_document_id, "refused-partial-first-r2")
            .await?;
        fixture
            .create_completed_graph_stage_with_count(
                first_document_id,
                replacement_revision_id,
                GRAPH_EXTRACTION_VERSION,
                serde_json::json!(1),
            )
            .await?;
        fixture
            .create_current_ready_extraction(
                second_document_id,
                second_chunk_id,
                Some(second_revision_id),
                Some(second_attempt_id),
                "Second Neutral Entity",
            )
            .await?;

        let second_rebuild = fixture
            .state
            .application_services
            .graph
            .rebuild_library_graph(&fixture.state, fixture.library_id)
            .await;
        let second_rebuild_is_coverage_conflict =
            matches!(&second_rebuild, Err(GraphServiceError::StateConflict { .. }));
        let second_rebuild_debug = format!("{second_rebuild:?}");
        let snapshot_rows_after_retry = sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_snapshot where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count snapshots after graph rebuild retry")?;
        let remaining_projection_rows = fixture.projection_row_count(1).await?;
        let stale_evidence_rows = sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_evidence
             where library_id = $1 and document_id = $2 and revision_id = $3",
        )
        .bind(fixture.library_id)
        .bind(first_document_id)
        .bind(first_revision_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count evidence left by refused graph rebuild")?;

        Ok::<_, anyhow::Error>((
            first_rebuild_is_coverage_conflict,
            first_rebuild_debug,
            refused_projection_rows,
            refused_evidence_rows,
            snapshot_rows_after_refusal,
            second_rebuild_is_coverage_conflict,
            second_rebuild_debug,
            snapshot_rows_after_retry,
            remaining_projection_rows,
            stale_evidence_rows,
        ))
    }
    .await;
    let cleanup = fixture.cleanup().await;
    let (
        first_rebuild_is_coverage_conflict,
        first_rebuild_debug,
        refused_projection_rows,
        refused_evidence_rows,
        snapshot_rows_after_refusal,
        second_rebuild_is_coverage_conflict,
        second_rebuild_debug,
        snapshot_rows_after_retry,
        remaining_projection_rows,
        stale_evidence_rows,
    ) = result?;
    cleanup?;

    assert!(
        first_rebuild_is_coverage_conflict,
        "the deliberately partial rebuild must fail its coverage gate: {first_rebuild_debug}"
    );
    assert_eq!(
        refused_projection_rows, 0,
        "a coverage-refused rebuild must roll back every node and edge from its staging projection"
    );
    assert_eq!(
        refused_evidence_rows, 0,
        "a coverage-refused rebuild must roll back evidence targeting its staging projection"
    );
    assert_eq!(
        snapshot_rows_after_refusal, 0,
        "a coverage-refused rebuild must leave the active snapshot unchanged"
    );
    assert!(
        second_rebuild_is_coverage_conflict,
        "a retry missing the replacement revision must not reuse the refused projection's document row: {second_rebuild_debug}"
    );
    assert_eq!(
        snapshot_rows_after_retry, 0,
        "staging residue must never let a later incomplete rebuild publish a snapshot"
    );
    assert_eq!(
        remaining_projection_rows, 0,
        "every refused rebuild attempt must leave its projection generation empty"
    );
    assert_eq!(
        stale_evidence_rows, 0,
        "evidence from the superseded revision must not survive a refused rebuild"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn coverage_refusal_preserves_the_existing_ready_projection_atomically() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let result = async {
        fixture
            .publish_non_empty_ready_projection("atomic-published-r1", "Published Neutral Entity")
            .await?;
        let graph_digest_before_refusal = fixture.graph_state_digest().await?;
        let snapshot_before_refusal = sqlx::query_as::<_, (String, i64, i32, i32)>(
            "select graph_status, projection_version, node_count, edge_count
             from runtime_graph_snapshot where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to load ready graph snapshot before atomic rebuild refusal")?;
        let evidence_before_refusal = fixture.evidence_count().await?;

        let (missing_document_id, missing_revision_id) =
            fixture.create_readable_revision("atomic-missing-r1").await?;
        fixture
            .create_completed_graph_stage_with_count(
                missing_document_id,
                missing_revision_id,
                GRAPH_EXTRACTION_VERSION,
                serde_json::json!(1),
            )
            .await?;

        let refused_rebuild = fixture
            .state
            .application_services
            .graph
            .rebuild_library_graph(&fixture.state, fixture.library_id)
            .await;
        let refused_rebuild_is_coverage_conflict =
            matches!(&refused_rebuild, Err(GraphServiceError::StateConflict { .. }));
        let refused_rebuild_debug = format!("{refused_rebuild:?}");
        let graph_digest_after_refusal = fixture.graph_state_digest().await?;
        let snapshot_after_refusal = sqlx::query_as::<_, (String, i64, i32, i32)>(
            "select graph_status, projection_version, node_count, edge_count
             from runtime_graph_snapshot where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to load graph snapshot after atomic rebuild refusal")?;
        let v2_projection_rows = fixture.projection_row_count(2).await?;
        let evidence_after_refusal = fixture.evidence_count().await?;
        let v2_target_evidence_rows = sqlx::query_scalar::<_, i64>(
            "select count(*)
             from runtime_graph_evidence as evidence
             where evidence.library_id = $1
               and (
                   (
                       evidence.target_kind = 'node'
                       and exists (
                           select 1 from runtime_graph_node as node
                           where node.id = evidence.target_id
                             and node.library_id = $1
                             and node.projection_version = 2
                       )
                   )
                   or (
                       evidence.target_kind = 'edge'
                       and exists (
                           select 1 from runtime_graph_edge as edge
                           where edge.id = evidence.target_id
                             and edge.library_id = $1
                             and edge.projection_version = 2
                       )
                   )
               )",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count evidence targeting refused projection v2")?;
        let recovery_job_rows = sqlx::query_scalar::<_, i64>(
            "select count(*) from ingest_job
             where library_id = $1
               and knowledge_document_id = $2
               and knowledge_revision_id = $3
               and job_kind = 'graph_refresh'
               and queue_state = 'queued'
               and dedupe_key = $4",
        )
        .bind(fixture.library_id)
        .bind(missing_document_id)
        .bind(missing_revision_id)
        .bind(format!("graph_reextract:{GRAPH_EXTRACTION_VERSION}:{missing_revision_id}"))
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count post-rollback graph recovery jobs")?;

        Ok::<_, anyhow::Error>((
            refused_rebuild_is_coverage_conflict,
            refused_rebuild_debug,
            graph_digest_before_refusal,
            graph_digest_after_refusal,
            snapshot_before_refusal,
            snapshot_after_refusal,
            evidence_before_refusal,
            evidence_after_refusal,
            v2_projection_rows,
            v2_target_evidence_rows,
            recovery_job_rows,
        ))
    }
    .await;
    let cleanup = fixture.cleanup().await;
    let (
        refused_rebuild_is_coverage_conflict,
        refused_rebuild_debug,
        graph_digest_before_refusal,
        graph_digest_after_refusal,
        snapshot_before_refusal,
        snapshot_after_refusal,
        evidence_before_refusal,
        evidence_after_refusal,
        v2_projection_rows,
        v2_target_evidence_rows,
        recovery_job_rows,
    ) = result?;
    cleanup?;

    assert_eq!(snapshot_before_refusal.0, "ready");
    assert_eq!(snapshot_before_refusal.1, 1);
    assert!(snapshot_before_refusal.2 > 0);
    assert!(
        refused_rebuild_is_coverage_conflict,
        "the deliberately incomplete v2 rebuild must fail coverage: {refused_rebuild_debug}"
    );
    assert_eq!(
        snapshot_after_refusal, snapshot_before_refusal,
        "coverage refusal must preserve the ready v1 snapshot version and counts"
    );
    assert_eq!(
        graph_digest_after_refusal, graph_digest_before_refusal,
        "coverage refusal must leave the complete published v1 graph state byte-for-byte equivalent"
    );
    assert_eq!(v2_projection_rows, 0, "coverage refusal must leave no v2 nodes or edges");
    assert_eq!(
        evidence_after_refusal, evidence_before_refusal,
        "coverage refusal must leave no evidence created by v2"
    );
    assert_eq!(v2_target_evidence_rows, 0, "no evidence may target a refused v2 projection row");
    assert_eq!(
        recovery_job_rows, 1,
        "the missing revision's recovery must be enqueued after the v2 graph transaction rolls back"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn authoritative_zero_completion_replaces_ready_projection_with_empty_snapshot() -> Result<()>
{
    let fixture = GraphReextractFixture::create().await?;
    let result = async {
        let orphan_evidence_marker = format!("authoritativeorphanproof{}", Uuid::now_v7().simple());
        let (document_id, v1_revision_id) = fixture
            .publish_non_empty_ready_projection(
                "authoritative-empty-r1",
                "Authoritative Empty Transition Entity",
            )
            .await?;
        let orphan_edge_target_id = sqlx::query_scalar::<_, Uuid>(
            "select id
             from runtime_graph_edge
             where library_id = $1 and projection_version = 1
             order by id
             limit 1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to select v1 edge for orphan-evidence regression")?;
        let retained_node_target_id = sqlx::query_scalar::<_, Uuid>(
            "select id
             from runtime_graph_node
             where library_id = $1 and projection_version = 1
             order by id
             limit 1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to select retained v1 node for summary regression")?;
        let orphan_evidence = runtime_graph_repository::create_runtime_graph_evidence(
            &fixture.pool,
            CreateRuntimeGraphEvidenceInput {
                library_id: fixture.library_id,
                target_kind: "edge",
                target_id: orphan_edge_target_id,
                document_id: Some(document_id),
                revision_id: Some(v1_revision_id),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("authoritative-empty-corruption.txt"),
                page_ref: None,
                evidence_text: &orphan_evidence_marker,
                confidence_score: Some(1.0),
                evidence_context_key: "historical_orphan_regression",
            },
        )
        .await
        .context("failed to seed uniquely searchable v1 evidence")?;
        let orphan_evidence_query =
            [RuntimeGraphEvidenceSearchQuery::LiteralOrFormal(orphan_evidence_marker.clone())];
        let orphan_search_hits_before_corruption =
            runtime_graph_repository::search_runtime_graph_evidence_by_text(
                &fixture.pool,
                fixture.library_id,
                &orphan_evidence_query,
                &[],
                10,
            )
            .await?
            .len();
        let summary_id = sqlx::query_scalar::<_, Uuid>(
            "insert into runtime_graph_canonical_summary (
                workspace_id,
                library_id,
                target_kind,
                target_id,
                summary_text,
                confidence_status,
                support_count,
                source_truth_version
             ) values ($1, $2, 'node', $3, $4, 'ready', 1, 1)
             returning id",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(retained_node_target_id)
        .bind("Summary attached to extant superseded v1 topology")
        .fetch_one(&fixture.pool)
        .await
        .context("failed to seed v1 canonical summary")?;
        let raw_deleted_target_count = sqlx::query(
            "delete from runtime_graph_edge
             where library_id = $1 and projection_version = 1 and id = $2",
        )
        .bind(fixture.library_id)
        .bind(orphan_edge_target_id)
        .execute(&fixture.pool)
        .await
        .context("failed to plant historical orphan evidence")?
        .rows_affected();
        let orphan_evidence_rows_before_rebuild = sqlx::query_scalar::<_, i64>(
            "select count(*)
             from runtime_graph_evidence as evidence
             where evidence.id = $1
               and not exists (
                 select 1
                 from runtime_graph_edge as edge
                 where edge.library_id = evidence.library_id
                   and edge.id = evidence.target_id
               )",
        )
        .bind(orphan_evidence.id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to verify planted orphan evidence")?;
        let orphan_search_hits_before_rebuild =
            runtime_graph_repository::search_runtime_graph_evidence_by_text(
                &fixture.pool,
                fixture.library_id,
                &orphan_evidence_query,
                &[],
                10,
            )
            .await?
            .len();
        let v1_projection_rows_before_rebuild = fixture.projection_row_count(1).await?;
        let summary_rows_before_rebuild = sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_canonical_summary where id = $1",
        )
        .bind(summary_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to verify seeded v1 canonical summary")?;

        let current_revision_id =
            fixture.replace_readable_revision(document_id, "authoritative-empty-r2").await?;
        fixture
            .create_completed_graph_stage(
                document_id,
                current_revision_id,
                GRAPH_EXTRACTION_VERSION,
            )
            .await?;

        let empty_rebuild = fixture
            .state
            .application_services
            .graph
            .rebuild_library_graph(&fixture.state, fixture.library_id)
            .await;
        let empty_rebuild_is_published = matches!(
            &empty_rebuild,
            Ok(outcome)
                if outcome.projection_version == 2
                    && outcome.node_count == 0
                    && outcome.edge_count == 0
                    && outcome.graph_status == "empty"
        );
        let empty_rebuild_debug = format!("{empty_rebuild:?}");
        let snapshot = sqlx::query_as::<_, (String, i64, i32, i32)>(
            "select graph_status, projection_version, node_count, edge_count
             from runtime_graph_snapshot where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to load authoritatively emptied graph snapshot")?;
        let v1_projection_rows = fixture.projection_row_count(1).await?;
        let v2_projection_rows = fixture.projection_row_count(2).await?;
        let evidence_rows = fixture.evidence_count().await?;
        let orphan_evidence_rows_after_rebuild = sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_evidence where id = $1",
        )
        .bind(orphan_evidence.id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count orphan evidence after authoritative rebuild")?;
        let summary_rows_after_rebuild = sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_canonical_summary where id = $1",
        )
        .bind(summary_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count v1 summary after authoritative rebuild")?;
        let orphan_search_hits_after_rebuild =
            runtime_graph_repository::search_runtime_graph_evidence_by_text(
                &fixture.pool,
                fixture.library_id,
                &orphan_evidence_query,
                &[],
                10,
            )
            .await?
            .len();

        Ok::<_, anyhow::Error>((
            empty_rebuild_is_published,
            empty_rebuild_debug,
            snapshot,
            orphan_search_hits_before_corruption,
            raw_deleted_target_count,
            orphan_evidence_rows_before_rebuild,
            orphan_search_hits_before_rebuild,
            v1_projection_rows_before_rebuild,
            summary_rows_before_rebuild,
            v1_projection_rows,
            v2_projection_rows,
            evidence_rows,
            orphan_evidence_rows_after_rebuild,
            summary_rows_after_rebuild,
            orphan_search_hits_after_rebuild,
        ))
    }
    .await;
    let cleanup = fixture.cleanup().await;
    let (
        empty_rebuild_is_published,
        empty_rebuild_debug,
        snapshot,
        orphan_search_hits_before_corruption,
        raw_deleted_target_count,
        orphan_evidence_rows_before_rebuild,
        orphan_search_hits_before_rebuild,
        v1_projection_rows_before_rebuild,
        summary_rows_before_rebuild,
        v1_projection_rows,
        v2_projection_rows,
        evidence_rows,
        orphan_evidence_rows_after_rebuild,
        summary_rows_after_rebuild,
        orphan_search_hits_after_rebuild,
    ) = result?;
    cleanup?;

    assert!(
        orphan_search_hits_before_corruption > 0,
        "the unique v1 marker must be searchable before planting historical corruption"
    );
    assert_eq!(
        raw_deleted_target_count, 1,
        "the regression plant must delete exactly its v1 edge target"
    );
    assert_eq!(
        orphan_evidence_rows_before_rebuild, 1,
        "the raw topology delete must leave the unique evidence row orphaned"
    );
    assert!(
        orphan_search_hits_before_rebuild > 0,
        "historically orphaned evidence must reproduce the stale text-search hit before rebuild"
    );
    assert!(
        v1_projection_rows_before_rebuild > 0,
        "the corruption plant must leave other v1 topology for superseded-version pruning"
    );
    assert_eq!(
        summary_rows_before_rebuild, 1,
        "the regression must seed a summary attached to extant v1 topology"
    );
    assert!(
        empty_rebuild_is_published,
        "an exact current zero-contribution completion must publish empty v2: {empty_rebuild_debug}"
    );
    assert_eq!(snapshot, ("empty".to_string(), 2, 0, 0));
    assert_eq!(
        v1_projection_rows, 0,
        "publishing v2 must atomically remove all superseded v1 topology"
    );
    assert_eq!(
        v2_projection_rows, 0,
        "the authoritatively empty projection must contain no nodes or edges"
    );
    assert_eq!(
        evidence_rows, 0,
        "publishing empty v2 must remove superseded and historically orphaned evidence"
    );
    assert_eq!(
        orphan_evidence_rows_after_rebuild, 0,
        "the publication-time orphan sweep must remove historical evidence corruption"
    );
    assert_eq!(
        summary_rows_after_rebuild, 0,
        "superseded v1 summary projections must be removed with their extant targets"
    );
    assert_eq!(
        orphan_search_hits_after_rebuild, 0,
        "historically orphaned v1 evidence text must not remain searchable after v2 publication"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn lifecycleless_old_revision_extraction_is_not_merged_as_the_current_head() -> Result<()> {
    let fixture = GraphReextractFixture::create().await?;
    let result = async {
        let (document_id, old_revision_id) =
            fixture.create_readable_revision("lifecycleless-old-r1").await?;
        let old_chunk_id =
            fixture.create_revision_chunk(old_revision_id, "lifecycleless-old-r1").await?;
        fixture
            .create_current_ready_extraction(
                document_id,
                old_chunk_id,
                None,
                None,
                "Historical Neutral Entity",
            )
            .await?;

        let current_revision_id =
            fixture.replace_readable_revision(document_id, "lifecycleless-current-r2").await?;
        fixture
            .create_completed_graph_stage(
                document_id,
                current_revision_id,
                GRAPH_EXTRACTION_VERSION,
            )
            .await?;

        let rebuild = fixture
            .state
            .application_services
            .graph
            .rebuild_library_graph(&fixture.state, fixture.library_id)
            .await;
        let rebuild_is_empty = matches!(
            &rebuild,
            Ok(outcome)
                if outcome.projection_version == 1
                    && outcome.node_count == 0
                    && outcome.edge_count == 0
                    && outcome.graph_status == "empty"
        );
        let rebuild_debug = format!("{rebuild:?}");
        let impossible_evidence_rows = sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_evidence
             where library_id = $1
               and document_id = $2
               and revision_id = $3
               and chunk_id = $4",
        )
        .bind(fixture.library_id)
        .bind(document_id)
        .bind(current_revision_id)
        .bind(old_chunk_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count cross-revision graph evidence")?;
        let historical_entity_rows = sqlx::query_scalar::<_, i64>(
            "select count(*) from runtime_graph_node
             where library_id = $1 and label = 'Historical Neutral Entity'",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await
        .context("failed to count lifecycle-less historical graph nodes")?;
        let projection_rows = fixture.projection_row_count(1).await?;

        Ok::<_, anyhow::Error>((
            rebuild_is_empty,
            rebuild_debug,
            impossible_evidence_rows,
            historical_entity_rows,
            projection_rows,
        ))
    }
    .await;
    let cleanup = fixture.cleanup().await;
    let (
        rebuild_is_empty,
        rebuild_debug,
        impossible_evidence_rows,
        historical_entity_rows,
        projection_rows,
    ) = result?;
    cleanup?;

    assert!(
        rebuild_is_empty,
        "a lifecycle-less extraction from revision A must not contribute to revision B's rebuild: {rebuild_debug}"
    );
    assert_eq!(
        impossible_evidence_rows, 0,
        "graph evidence must not claim revision B while referencing revision A's chunk"
    );
    assert_eq!(
        historical_entity_rows, 0,
        "a lifecycle-less entity extracted from revision A must not be merged under head B"
    );
    assert_eq!(
        projection_rows, 0,
        "the current head's zero-contribution rebuild must remain empty"
    );
    Ok(())
}
