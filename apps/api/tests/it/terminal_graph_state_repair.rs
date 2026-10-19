//! PostgreSQL integration coverage for terminal graph-state reconciliation.
//!
//! Pre-IRG-211 code left a successful `extract_graph` with no graph
//! contribution in `graph_state='processing'` forever. This suite proves the
//! operator repair moves exactly that cohort to the canonical terminal state
//! computed from the persisted completion, and leaves every other revision
//! (in flight, already graphed, positively contributing, or already terminal)
//! untouched. No provider is called.
//!
//! Run with:
//! `cargo test -p ironrag-backend --features test-support --test it -- --ignored terminal_graph_state_repair::`

use crate::temp_postgres_database;

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        persistence::Persistence,
        repositories::{catalog_repository, ingest_repository, runtime_graph_repository},
    },
    services::maintenance::repair,
};
use temp_postgres_database::TempPostgresDatabase;

const GRAPH_EXTRACTION_VERSION: &str = "graph_extract";

struct TerminalGraphStateFixture {
    database: TempPostgresDatabase,
    pool: PgPool,
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl TerminalGraphStateFixture {
    async fn create() -> Result<Self> {
        let database_url = std::env::var("IRONRAG_DATABASE_URL")
            .context("IRONRAG_DATABASE_URL must be configured")?;
        let database = TempPostgresDatabase::create(&database_url, "terminal_graph_state").await?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database.database_url)
            .await
            .context("failed to connect terminal graph-state postgres")?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply terminal graph-state migrations")?;
        let mut settings = Settings::from_env().context("load terminal graph-state settings")?;
        settings.database_url = database.database_url.clone();
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let redis = redis::Client::open(settings.redis_url.clone())
            .context("create terminal graph-state redis client")?;
        let state =
            AppState::from_dependencies(settings, Persistence::for_tests(pool.clone(), redis))?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("terminal-graph-state-workspace-{suffix}"),
            "Terminal Graph State Workspace",
            None,
        )
        .await
        .context("failed to create terminal graph-state workspace")?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("terminal-graph-state-library-{suffix}"),
            "Terminal Graph State Library",
            None,
            None,
        )
        .await
        .context("failed to create terminal graph-state library")?;

        Ok(Self { database, pool, state, workspace_id: workspace.id, library_id: library.id })
    }

    /// Plants a readable active head whose knowledge revision is in
    /// `processing`, exactly like the historical backlog.
    async fn create_processing_revision(&self, label: &str) -> Result<(Uuid, Uuid)> {
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
        // One writable-CTE statement keeps this fixture compact and below the
        // duplication gate's clone window; three separate inserts here would
        // clone the graph re-extract fixture.
        sqlx::query(
            "with document as (
                insert into content_document (
                    id, workspace_id, library_id, external_key, document_state
                ) values ($1, $2, $3, $4, 'active'::content_document_state)
                returning id
             ), revision as (
                insert into content_revision (
                    id, document_id, workspace_id, library_id, revision_number,
                    content_source_kind, checksum, mime_type, byte_size, title, admission_identity
                ) select $5, document.id, $2, $3, 1, 'upload', $6, 'text/plain', 128, $7, $8
                    from document
                returning id, document_id
              ) select id from revision",
        )
        .bind(document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(format!("terminal-graph-state-{label}-{document_id}"))
        .bind(revision_id)
        .bind(checksum)
        .bind(format!("{label} fixture"))
        .bind(serde_json::to_value(admission)?)
        .execute(&self.pool)
        .await
        .context("failed to create terminal graph-state readable revision")?;
        crate::content_lifecycle_support::seed_readable_revision_head(
            &self.pool,
            document_id,
            revision_id,
            &format!("{label}.txt"),
        )
        .await?;
        sqlx::query(
            "insert into knowledge_document (
                document_id, workspace_id, library_id, external_key, file_name,
                title, document_state, active_revision_id, readable_revision_id,
                latest_revision_no, created_at, updated_at, deleted_at
             ) values (
                $1, $2, $3, $4, $5, $6, 'active', $7, $7, 1, now(), now(), null
             )",
        )
        .bind(document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(format!("terminal-graph-state-{label}-{document_id}"))
        .bind(format!("{label}.txt"))
        .bind(format!("{label} fixture"))
        .bind(revision_id)
        .execute(&self.pool)
        .await
        .context("failed to create terminal graph-state knowledge document")?;
        sqlx::query(
            "insert into knowledge_revision (
                revision_id, workspace_id, library_id, document_id, revision_number,
                revision_state, revision_kind, mime_type, checksum, title, byte_size,
                normalized_text, text_checksum, text_state, vector_state, graph_state,
                text_readable_at, vector_ready_at, created_at
             ) values (
                $1, $2, $3, $4, 1,
                'ready', 'upload', 'text/plain', $5, $6, 128,
                '', $7, 'text_readable', 'ready', 'processing',
                now(), now(), now()
             )",
        )
        .bind(revision_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(document_id)
        .bind(format!("sha256:{label}:{revision_id}"))
        .bind(format!("{label} fixture"))
        .bind(format!("sha256:text:{label}:{revision_id}"))
        .execute(&self.pool)
        .await
        .context("failed to create terminal graph-state knowledge revision")?;
        crate::content_lifecycle_support::assert_current_revision(
            &self.pool,
            document_id,
            revision_id,
        )
        .await?;
        Ok((document_id, revision_id))
    }

    /// Plants a completed `extract_graph` whose contribution is
    /// `graph_contribution_count` and whose readiness is `graph_ready`, behind a
    /// succeeded attempt of a completed job.
    async fn plant_completed_extraction(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        graph_contribution_count: i64,
        graph_ready: bool,
    ) -> Result<Uuid> {
        let now = Utc::now();
        let job = ingest_repository::create_ingest_job(
            &self.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(document_id),
                knowledge_revision_id: Some(revision_id),
                job_kind: "graph_refresh".to_string(),
                queue_state: "completed".to_string(),
                priority: 200,
                dedupe_key: Some(format!("terminal-graph-state:{revision_id}")),
                queued_at: Some(now),
                available_at: Some(now),
                completed_at: Some(now),
            },
        )
        .await
        .context("failed to create terminal graph-state job")?;
        let attempt = ingest_repository::create_ingest_attempt(
            &self.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "succeeded".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(now),
                heartbeat_at: Some(now),
                finished_at: Some(now),
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 100,
                retryable: false,
            },
        )
        .await
        .context("failed to create terminal graph-state attempt")?;
        ingest_repository::create_ingest_stage_event(
            &self.pool,
            &ingest_repository::NewIngestStageEvent {
                attempt_id: attempt.id,
                stage_name: "extract_graph".to_string(),
                stage_state: "completed".to_string(),
                ordinal: 1,
                message: None,
                details_json: serde_json::json!({
                    "extractionVersion": GRAPH_EXTRACTION_VERSION,
                    "graphContributionCount": graph_contribution_count,
                    "graphReady": graph_ready,
                }),
                recorded_at: Some(now),
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
        .context("failed to create terminal graph-state stage event")?;
        Ok(job.id)
    }

    /// Adds a live `graph_refresh` job so the revision looks in flight.
    async fn plant_queued_job(&self, document_id: Uuid, revision_id: Uuid) -> Result<()> {
        let now = Utc::now();
        let job = ingest_repository::create_ingest_job(
            &self.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(document_id),
                knowledge_revision_id: Some(revision_id),
                job_kind: "graph_refresh".to_string(),
                queue_state: "queued".to_string(),
                priority: 200,
                dedupe_key: Some(format!("terminal-graph-state-queued:{revision_id}")),
                queued_at: Some(now),
                available_at: Some(now),
                completed_at: None,
            },
        )
        .await
        .context("failed to create terminal graph-state queued job")?;
        ingest_repository::create_ingest_attempt(
            &self.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(now),
                heartbeat_at: Some(now),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 10,
                retryable: false,
            },
        )
        .await
        .context("failed to create terminal graph-state queued attempt")?;
        Ok(())
    }

    async fn plant_document_node(&self, document_id: Uuid) -> Result<()> {
        runtime_graph_repository::upsert_runtime_graph_document_node(
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
                projection_version: 1,
            },
        )
        .await
        .context("failed to create terminal graph-state document node")?;
        Ok(())
    }

    async fn graph_state(&self, revision_id: Uuid) -> Result<String> {
        sqlx::query_scalar::<_, String>(
            "select graph_state from knowledge_revision where revision_id = $1",
        )
        .bind(revision_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to read terminal graph-state revision state")
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        self.database.drop().await
    }
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn reconciles_zero_contribution_processing_revision() -> Result<()> {
    let fixture = TerminalGraphStateFixture::create().await?;
    let (document_id, revision_id) = fixture.create_processing_revision("zero").await?;
    fixture.plant_completed_extraction(document_id, revision_id, 0, false).await?;

    let report = repair::reconcile_terminal_graph_states(&fixture.state, None, false).await?;

    assert_eq!(report.updated, 1, "one zero-contribution revision must transition");
    assert_eq!(report.terminal_state, "graph_degraded");
    assert_eq!(report.by_library.len(), 1);
    assert_eq!(report.by_library[0].library_id, fixture.library_id);
    assert_eq!(fixture.graph_state(revision_id).await?, "graph_degraded");
    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn dry_run_reports_without_writing() -> Result<()> {
    let fixture = TerminalGraphStateFixture::create().await?;
    let (document_id, revision_id) = fixture.create_processing_revision("dry").await?;
    fixture.plant_completed_extraction(document_id, revision_id, 0, false).await?;

    let report = repair::reconcile_terminal_graph_states(&fixture.state, None, true).await?;

    assert_eq!(report.candidates, 1, "dry run must report the cohort");
    assert_eq!(report.updated, 0, "dry run must not write");
    assert!(report.dry_run);
    assert_eq!(fixture.graph_state(revision_id).await?, "processing");
    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn leaves_revision_with_live_job_untouched() -> Result<()> {
    let fixture = TerminalGraphStateFixture::create().await?;
    let (document_id, revision_id) = fixture.create_processing_revision("live").await?;
    fixture.plant_completed_extraction(document_id, revision_id, 0, false).await?;
    fixture.plant_queued_job(document_id, revision_id).await?;

    let report = repair::reconcile_terminal_graph_states(&fixture.state, None, false).await?;

    assert_eq!(report.updated, 0, "an in-flight revision must not be transitioned");
    assert_eq!(fixture.graph_state(revision_id).await?, "processing");
    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn leaves_revision_with_document_node_untouched() -> Result<()> {
    let fixture = TerminalGraphStateFixture::create().await?;
    let (document_id, revision_id) = fixture.create_processing_revision("graphed").await?;
    fixture.plant_completed_extraction(document_id, revision_id, 0, false).await?;
    fixture.plant_document_node(document_id).await?;

    let report = repair::reconcile_terminal_graph_states(&fixture.state, None, false).await?;

    assert_eq!(report.updated, 0, "a revision with a document node must not be transitioned");
    assert_eq!(fixture.graph_state(revision_id).await?, "processing");
    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn leaves_positive_contribution_revision_untouched() -> Result<()> {
    let fixture = TerminalGraphStateFixture::create().await?;
    let (document_id, revision_id) = fixture.create_processing_revision("contrib").await?;
    fixture.plant_completed_extraction(document_id, revision_id, 3, true).await?;

    let report = repair::reconcile_terminal_graph_states(&fixture.state, None, false).await?;

    assert_eq!(report.updated, 0, "a contributing completion is not the zero-contribution cohort");
    assert_eq!(fixture.graph_state(revision_id).await?, "processing");
    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn second_run_is_idempotent() -> Result<()> {
    let fixture = TerminalGraphStateFixture::create().await?;
    let (document_id, revision_id) = fixture.create_processing_revision("idempotent").await?;
    fixture.plant_completed_extraction(document_id, revision_id, 0, false).await?;

    let first = repair::reconcile_terminal_graph_states(&fixture.state, None, false).await?;
    let second = repair::reconcile_terminal_graph_states(&fixture.state, None, false).await?;

    assert_eq!(first.updated, 1);
    assert_eq!(second.updated, 0, "the terminal transition must not repeat");
    assert_eq!(fixture.graph_state(revision_id).await?, "graph_degraded");
    fixture.cleanup().await
}
