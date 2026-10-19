//! PostgreSQL integration coverage for identity-safe document graph metrics.
//!
//! Run with:
//! `cargo test -p ironrag-backend --features test-support --test it -- --ignored content_document_graph_coverage::`

use crate::temp_postgres_database;

use anyhow::{Context, Result};
use chrono::Utc;
use ironrag_backend::infra::repositories::{
    self, catalog_repository, content_document_list_repository, ingest_repository,
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use temp_postgres_database::TempPostgresDatabase;
use uuid::Uuid;

const PROJECTION_VERSION: i64 = 7;

#[derive(Clone, Copy)]
struct RenderableDocument {
    document_id: Uuid,
    revision_id: Uuid,
}

struct GraphCoverageFixture {
    database: TempPostgresDatabase,
    pool: PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl GraphCoverageFixture {
    async fn create() -> Result<Self> {
        let database_url = std::env::var("IRONRAG_DATABASE_URL")
            .context("IRONRAG_DATABASE_URL must be configured")?;
        let database = TempPostgresDatabase::create(&database_url, "graph_coverage").await?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database.database_url)
            .await
            .context("failed to connect graph coverage postgres")?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply graph coverage migrations")?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("graph-coverage-workspace-{suffix}"),
            "Graph Coverage Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("graph-coverage-library-{suffix}"),
            "Graph Coverage Library",
            None,
            None,
        )
        .await?;

        Ok(Self { database, pool, workspace_id: workspace.id, library_id: library.id })
    }

    async fn create_renderable_document(&self, label: &str, state: &str) -> Result<Uuid> {
        Ok(self.create_renderable_revision(label, state).await?.document_id)
    }

    async fn create_renderable_revision(
        &self,
        label: &str,
        state: &str,
    ) -> Result<RenderableDocument> {
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
                id, workspace_id, library_id, external_key, document_state, deleted_at
             ) values (
                $1, $2, $3, $4, $5::content_document_state,
                case when $5 = 'deleted' then now() else null end
             )",
        )
        .bind(document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(format!("graph-coverage-{label}-{document_id}"))
        .bind(state)
        .execute(&self.pool)
        .await
        .context("failed to create graph coverage document")?;
        sqlx::query(
            "insert into content_revision (
                id, document_id, workspace_id, library_id, revision_number,
                content_source_kind, checksum, mime_type, byte_size, title, admission_identity
             ) values (
                $1, $2, $3, $4, 1, 'upload', $5, 'text/plain', 128, $6, $7
             )",
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
        .context("failed to create graph coverage revision")?;
        if state == "active" {
            crate::content_lifecycle_support::seed_readable_revision_head(
                &self.pool,
                document_id,
                revision_id,
                &format!("{label}.txt"),
            )
            .await?;
        } else {
            ironrag_backend::infra::repositories::content_document_head_repository::upsert_document_head(
                &self.pool,
                &ironrag_backend::infra::repositories::content_document_head_repository::NewContentDocumentHead {
                    document_id, active_revision_id: Some(revision_id),
                    readable_revision_id: None, publishing_mutation_item_id: None,
                    latest_mutation_id: None, latest_successful_attempt_id: None,
                },
            ).await?;
        }
        sqlx::query(
            "insert into knowledge_document (
                document_id, workspace_id, library_id, external_key, file_name,
                title, document_state, active_revision_id, readable_revision_id,
                latest_revision_no, created_at, updated_at, deleted_at
             ) values (
                $1, $2, $3, $4, $5, $6, $7, $8,
                case when $7 = 'active' then $8 else null end, 1, now(), now(),
                case when $7 = 'deleted' then now() else null end
             )",
        )
        .bind(document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(format!("graph-coverage-{label}-{document_id}"))
        .bind(format!("{label}.txt"))
        .bind(format!("{label} fixture"))
        .bind(state)
        .bind(revision_id)
        .execute(&self.pool)
        .await
        .context("failed to create renderable graph coverage projection")?;
        sqlx::query(
            "insert into knowledge_revision (
                revision_id, workspace_id, library_id, document_id, revision_number,
                revision_state, revision_kind, mime_type, checksum, title, byte_size,
                normalized_text, text_checksum, text_state, vector_state, graph_state,
                text_readable_at, vector_ready_at, created_at
             ) values (
                $1, $2, $3, $4, 1,
                'ready', 'upload', 'text/plain', $5, $6, 128,
                '', $7, 'text_readable', 'ready', 'graph_degraded',
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
        .context("failed to create graph coverage knowledge revision")?;
        if state == "active" {
            crate::content_lifecycle_support::assert_current_revision(
                &self.pool,
                document_id,
                revision_id,
            )
            .await?;
        }
        sqlx::query(
            "insert into knowledge_structured_revision (
                revision_id, workspace_id, library_id, document_id,
                preparation_state, normalization_profile, source_format, language_code,
                block_count, chunk_count, typed_fact_count, outline_json,
                prepared_at, updated_at
             ) values (
                $1, $2, $3, $4,
                'prepared', 'graph-coverage-v1', 'text', null,
                0, 0, 0, '[]'::jsonb,
                now(), now()
             )",
        )
        .bind(revision_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(document_id)
        .execute(&self.pool)
        .await
        .context("failed to create prepared graph coverage artifact")?;
        Ok(RenderableDocument { document_id, revision_id })
    }

    async fn create_job(
        &self,
        document: RenderableDocument,
        queue_state: &str,
        label: &str,
    ) -> Result<ingest_repository::IngestJobRow> {
        let now = Utc::now();
        ingest_repository::create_ingest_job(
            &self.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(document.document_id),
                knowledge_revision_id: Some(document.revision_id),
                job_kind: "graph_refresh".to_string(),
                queue_state: queue_state.to_string(),
                priority: 20,
                dedupe_key: Some(format!("graph-coverage:{label}:{}", document.revision_id)),
                queued_at: Some(now),
                available_at: Some(now),
                completed_at: (queue_state != "leased").then_some(now),
            },
        )
        .await
        .map_err(Into::into)
    }

    async fn create_leased_attempt(
        &self,
        job_id: Uuid,
        label: &str,
        progress_percent: i32,
    ) -> Result<ingest_repository::IngestAttemptRow> {
        let now = Utc::now();
        ingest_repository::create_ingest_attempt(
            &self.pool,
            &ingest_repository::NewIngestAttempt {
                job_id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some(format!("graph-coverage-{label}")),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(now),
                heartbeat_at: Some(now),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent,
                retryable: false,
            },
        )
        .await
        .map_err(Into::into)
    }

    async fn create_document_node(&self, document_id: Uuid, label: &str) -> Result<Uuid> {
        let node = repositories::upsert_runtime_graph_document_node(
            &self.pool,
            repositories::RuntimeGraphDocumentNodeInput {
                library_id: self.library_id,
                document_id,
                identity_key: &format!("document:{document_id}"),
                label,
                aliases_json: serde_json::json!([]),
                summary: None,
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version: PROJECTION_VERSION,
            },
        )
        .await
        .context("failed to create graph coverage document node")?;
        let revision_id = sqlx::query_scalar::<_, Uuid>(
            // Deleted fixtures deliberately retain stale graph evidence, not a
            // readable head. Its source is their operational active revision.
            "select active_revision_id
             from content_document_head
             where document_id = $1",
        )
        .bind(document_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to resolve graph coverage readable revision")?;
        self.create_document_node_evidence(node.id, document_id, revision_id, label).await?;
        Ok(node.id)
    }

    async fn create_document_node_evidence(
        &self,
        node_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        label: &str,
    ) -> Result<()> {
        repositories::create_runtime_graph_evidence(
            &self.pool,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "node",
                target_id: node_id,
                document_id: Some(document_id),
                revision_id: Some(revision_id),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("graph-coverage.txt"),
                page_ref: None,
                evidence_text: "Graph coverage publication evidence",
                confidence_score: None,
                evidence_context_key: label,
            },
        )
        .await
        .context("failed to create exact-revision graph coverage evidence")?;
        Ok(())
    }

    async fn replace_readable_revision(
        &self,
        document: RenderableDocument,
        label: &str,
    ) -> Result<RenderableDocument> {
        let revision_id = Uuid::now_v7();
        let checksum =
            crate::content_lifecycle_support::fixture_checksum(&format!("{label}:{revision_id}"));
        let admission = crate::content_repository::synthetic_import_admission(
            ironrag_contracts::documents::ReplacementIdentity {
                checksum: checksum.clone(),
                byte_size: 128,
                mime_type: "text/plain".to_owned(),
                file_name: format!("{label}.txt"),
                title: Some(format!("{label} replacement fixture")),
                language_code: None,
                source_uri: None,
                document_hint: None,
            },
        )?;
        sqlx::query(
            "insert into content_revision (
                id, document_id, workspace_id, library_id, revision_number,
                parent_revision_id, content_source_kind, checksum, mime_type,
                byte_size, title, admission_identity
             ) values (
                $1, $2, $3, $4, 2, $5, 'upload', $6, 'text/plain', 128, $7, $8
             )",
        )
        .bind(revision_id)
        .bind(document.document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(document.revision_id)
        .bind(checksum)
        .bind(format!("{label} replacement fixture"))
        .bind(serde_json::to_value(admission)?)
        .execute(&self.pool)
        .await
        .context("failed to create replacement graph coverage revision")?;
        crate::content_lifecycle_support::seed_readable_revision_projection(
            &self.pool,
            document.document_id,
            revision_id,
            &format!("{label}.txt"),
        )
        .await?;
        Ok(RenderableDocument { document_id: document.document_id, revision_id })
    }

    async fn publish_snapshot(&self, node_count: i32) -> Result<()> {
        let build_epoch = Uuid::now_v7();
        let claimed = repositories::upsert_runtime_graph_snapshot(
            &self.pool,
            repositories::RuntimeGraphSnapshotInput {
                library_id: self.library_id,
                graph_status: "building",
                projection_version: PROJECTION_VERSION,
                build_epoch,
                node_count,
                edge_count: 0,
                provenance_coverage_percent: None,
                last_error_message: None,
            },
            false,
        )
        .await?;
        anyhow::ensure!(claimed.is_some(), "graph coverage snapshot claim was rejected");
        let published = repositories::upsert_runtime_graph_snapshot(
            &self.pool,
            repositories::RuntimeGraphSnapshotInput {
                library_id: self.library_id,
                graph_status: "ready",
                projection_version: PROJECTION_VERSION,
                build_epoch,
                node_count,
                edge_count: 0,
                provenance_coverage_percent: None,
                last_error_message: None,
            },
            false,
        )
        .await?;
        anyhow::ensure!(published.is_some(), "graph coverage snapshot publish was rejected");
        Ok(())
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        self.database.drop().await
    }
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn dashboard_metrics_keep_one_snapshot_across_concurrent_graph_publication() -> Result<()> {
    let fixture = GraphCoverageFixture::create().await?;

    let result = async {
        let stable = fixture.create_renderable_revision("snapshot-stable", "active").await?;
        let transitioning = fixture
            .create_renderable_revision("snapshot-transitioning", "active")
            .await?;
        let transitioning_job = fixture
            .create_job(transitioning, "leased", "snapshot-transitioning")
            .await?;
        let _transitioning_attempt = fixture
            .create_leased_attempt(transitioning_job.id, "snapshot-transitioning", 50)
            .await?;

        let before = content_document_list_repository::aggregate_library_document_metrics(
            &fixture.pool,
            fixture.library_id,
        )
        .await?;
        assert_eq!(before.total, 2);
        assert_eq!(before.ready, 1);
        assert_eq!(before.processing, 1);
        assert_eq!(before.graph_ready, 0);
        assert_eq!(before.graph_sparse, 1);

        let (status_read_tx, status_read_rx) = tokio::sync::oneshot::channel();
        let (release_reader_tx, release_reader_rx) = tokio::sync::oneshot::channel();
        let reader_pool = fixture.pool.clone();
        let library_id = fixture.library_id;
        let reader = tokio::spawn(async move {
            content_document_list_repository::load_library_dashboard_snapshot_after_status_read_for_test(
                &reader_pool,
                library_id,
                async move {
                    let _ = status_read_tx.send(());
                    let _ = release_reader_rx.await;
                },
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), status_read_rx)
            .await
            .context("dashboard metrics did not pause after its first snapshot read")?
            .context("dashboard metrics dropped its snapshot pause signal")?;

        // Commit the state that the historical multi-statement READ COMMITTED
        // implementation could mix with `before`: the in-flight document
        // becomes ready and a two-document graph snapshot becomes visible.
        fixture
            .create_document_node(stable.document_id, "Snapshot stable document")
            .await?;
        fixture
            .create_document_node(transitioning.document_id, "Snapshot transitioned document")
            .await?;
        fixture.publish_snapshot(2).await?;
        let typed_fact_update = sqlx::query(
            "update knowledge_structured_revision
             set typed_fact_count = 1, updated_at = now()
             where revision_id = $1",
        )
        .bind(transitioning.revision_id)
        .execute(&fixture.pool)
        .await?;
        assert_eq!(
            typed_fact_update.rows_affected(),
            1,
            "fixture must publish typed facts for exactly one revision",
        );
        let completed = sqlx::query(
            "update ingest_job
             set queue_state = 'completed', completed_at = now()
             where id = $1",
        )
        .bind(transitioning_job.id)
        .execute(&fixture.pool)
        .await?;
        assert_eq!(completed.rows_affected(), 1, "fixture must complete exactly one job");

        let _ = release_reader_tx.send(());
        let during_publication =
            tokio::time::timeout(std::time::Duration::from_secs(5), reader)
                .await
                .context("dashboard metrics remained blocked after releasing the snapshot hook")?
                .context("dashboard metrics snapshot task panicked")??;
        assert_eq!(during_publication.metrics.total, 2);
        assert_eq!(during_publication.metrics.ready, 1);
        assert_eq!(during_publication.metrics.processing, 1);
        assert_eq!(
            during_publication.metrics.graph_ready, 0,
            "the old status denominator must not observe the newly published graph numerator"
        );
        assert_eq!(during_publication.metrics.graph_sparse, 1);
        assert!(
            during_publication.graph_snapshot.is_none(),
            "node/edge counts must come from the same pre-publication snapshot as metrics"
        );
        assert_eq!(during_publication.typed_fact_document_count, 0);

        let after = content_document_list_repository::load_library_dashboard_snapshot(
            &fixture.pool,
            fixture.library_id,
        )
        .await?;
        assert_eq!(after.metrics.total, 2);
        assert_eq!(after.metrics.ready, 2);
        assert_eq!(after.metrics.processing, 0);
        assert_eq!(after.metrics.graph_ready, 2);
        assert_eq!(after.metrics.graph_sparse, 0);
        assert_eq!(after.graph_snapshot.as_ref().map(|row| row.node_count), Some(2));
        assert_eq!(after.typed_fact_document_count, 1);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn dashboard_metrics_do_not_reacquire_the_pool_inside_their_snapshot() -> Result<()> {
    let fixture = GraphCoverageFixture::create().await?;

    let result = async {
        let document = fixture.create_renderable_revision("single-connection", "active").await?;
        fixture.create_document_node(document.document_id, "Single connection document").await?;
        fixture.publish_snapshot(1).await?;

        let single_connection_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&fixture.database.database_url)
            .await
            .context("failed to connect single-connection graph coverage pool")?;
        let metrics = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            content_document_list_repository::aggregate_library_document_metrics(
                &single_connection_pool,
                fixture.library_id,
            ),
        )
        .await
        .context("dashboard metrics self-deadlocked on a single-connection pool")??;
        assert_eq!(metrics.total, 1);
        assert_eq!(metrics.ready, 1);
        assert_eq!(metrics.graph_ready, 1);
        assert_eq!(metrics.graph_sparse, 0);
        single_connection_pool.close().await;
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn stale_document_node_cannot_compensate_for_missing_ready_document() -> Result<()> {
    let fixture = GraphCoverageFixture::create().await?;

    let result = async {
        let projected_ready = fixture.create_renderable_document("projected", "active").await?;
        let _missing_ready = fixture.create_renderable_document("missing", "active").await?;
        let stale_deleted = fixture.create_renderable_document("stale", "deleted").await?;

        fixture.create_document_node(projected_ready, "Projected document").await?;
        fixture.create_document_node(stale_deleted, "Stale deleted document").await?;
        fixture.publish_snapshot(2).await?;

        let raw_document_node_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from runtime_graph_node
             where library_id = $1
               and projection_version = $2
               and node_type = 'document'",
        )
        .bind(fixture.library_id)
        .bind(PROJECTION_VERSION)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(
            raw_document_node_count, 2,
            "fixture must reproduce the aggregate-count collision"
        );

        let graph_ready =
            content_document_list_repository::count_library_ready_documents_in_graph_projection(
                &fixture.pool,
                fixture.library_id,
                PROJECTION_VERSION,
            )
            .await?;
        assert_eq!(graph_ready, 1, "only the projected ready document belongs in the numerator");

        let metrics = content_document_list_repository::aggregate_library_document_metrics(
            &fixture.pool,
            fixture.library_id,
        )
        .await?;
        assert_eq!(metrics.total, 2);
        assert_eq!(metrics.ready, 2);
        assert_eq!(metrics.graph_ready, 1);
        assert_eq!(metrics.graph_sparse, 1);
        assert_eq!(metrics.graph_ready + metrics.graph_sparse, metrics.ready);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_coverage_requires_evidence_from_the_exact_readable_revision() -> Result<()> {
    let fixture = GraphCoverageFixture::create().await?;

    let result = async {
        let revision_a = fixture.create_renderable_revision("revision-a", "active").await?;
        let document_node = fixture
            .create_document_node(revision_a.document_id, "Revision A document")
            .await?;
        fixture.publish_snapshot(1).await?;

        let revision_b = fixture.replace_readable_revision(revision_a, "revision-b").await?;

        let stale_count =
            content_document_list_repository::count_library_ready_documents_in_graph_projection(
                &fixture.pool,
                fixture.library_id,
                PROJECTION_VERSION,
            )
            .await?;
        assert_eq!(
            stale_count, 0,
            "a document node evidenced only by the prior revision must remain graph-sparse",
        );
        let stale_metrics =
            content_document_list_repository::aggregate_library_document_metrics(
                &fixture.pool,
                fixture.library_id,
            )
            .await?;
        assert_eq!(stale_metrics.ready, 1);
        assert_eq!(stale_metrics.graph_ready, 0);
        assert_eq!(stale_metrics.graph_sparse, 1);

        let mut publication = fixture.pool.begin().await?;
        let stale_publication_coverage = content_document_list_repository::count_library_graph_coverage_for_publication_with_executor(
            &mut *publication,
            fixture.library_id,
            PROJECTION_VERSION,
            revision_b.document_id,
            revision_b.revision_id,
        )
        .await?;
        publication.rollback().await?;
        assert_eq!(
            stale_publication_coverage,
            (1, 0),
            "publication coverage must not adopt a prior-revision document node",
        );

        fixture
            .create_document_node_evidence(
                document_node,
                revision_b.document_id,
                revision_b.revision_id,
                "revision-b-document-node",
            )
            .await?;

        let current_count =
            content_document_list_repository::count_library_ready_documents_in_graph_projection(
                &fixture.pool,
                fixture.library_id,
                PROJECTION_VERSION,
            )
            .await?;
        assert_eq!(current_count, 1);
        let current_metrics =
            content_document_list_repository::aggregate_library_document_metrics(
                &fixture.pool,
                fixture.library_id,
            )
            .await?;
        assert_eq!(current_metrics.graph_ready, 1);
        assert_eq!(current_metrics.graph_sparse, 0);

        let mut publication = fixture.pool.begin().await?;
        let current_publication_coverage = content_document_list_repository::count_library_graph_coverage_for_publication_with_executor(
            &mut *publication,
            fixture.library_id,
            PROJECTION_VERSION,
            revision_b.document_id,
            revision_b.revision_id,
        )
        .await?;
        publication.rollback().await?;
        assert_eq!(current_publication_coverage, (1, 1));
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn provenance_coverage_counts_share_one_repeatable_read_snapshot() -> Result<()> {
    let fixture = GraphCoverageFixture::create().await?;

    let result = async {
        let document = fixture.create_renderable_revision("snapshot-race", "active").await?;
        let (ready_read_tx, ready_read_rx) = tokio::sync::oneshot::channel();
        let (continue_tx, continue_rx) = tokio::sync::oneshot::channel();
        let pool = fixture.pool.clone();
        let library_id = fixture.library_id;
        let snapshot_read = tokio::spawn(async move {
            content_document_list_repository::count_library_ready_and_projected_documents_after_ready_read_for_test(
                &pool,
                library_id,
                PROJECTION_VERSION,
                async move {
                    let _ = ready_read_tx.send(());
                    let _ = continue_rx.await;
                },
            )
            .await
        });

        ready_read_rx.await.context("coverage read did not establish its snapshot")?;
        fixture
            .create_document_node(document.document_id, "Concurrent projection")
            .await?;
        continue_tx.send(()).map_err(|_| anyhow::anyhow!("coverage read ended early"))?;

        let snapshot_counts = snapshot_read.await.context("coverage read task panicked")??;
        assert_eq!(
            snapshot_counts,
            (1, 0),
            "the numerator must not observe a projection committed after the denominator read",
        );
        let current_counts = content_document_list_repository::count_library_ready_and_projected_documents_in_snapshot(
            &fixture.pool,
            fixture.library_id,
            PROJECTION_VERSION,
        )
        .await?;
        assert_eq!(current_counts, (1, 1));
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_publication_computes_exact_ten_percent_and_excludes_active_reprocessing()
-> Result<()> {
    let fixture = GraphCoverageFixture::create().await?;

    let result = async {
        let mut documents = Vec::new();
        for index in 0..10 {
            documents.push(
                fixture.create_renderable_revision(&format!("coverage-{index}"), "active").await?,
            );
        }
        let projected = documents[0];
        fixture.create_document_node(projected.document_id, "Projected document").await?;
        fixture.publish_snapshot(1).await?;

        let publishing_job = fixture.create_job(projected, "leased", "publisher").await?;
        let publishing_attempt =
            fixture.create_leased_attempt(publishing_job.id, "publisher", 90).await?;
        let now = Utc::now();
        let publication = ingest_repository::publish_graph_refresh_success(
            &fixture.pool,
            &ingest_repository::PublishGraphRefreshSuccess {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: projected.document_id,
                revision_id: projected.revision_id,
                attempt_id: publishing_attempt.id,
                graph_state: "ready".to_string(),
                graph_ready_at: Some(now),
                completed_at: now,
            },
        )
        .await?;
        assert!(matches!(
            publication,
            ingest_repository::PublishGraphRefreshSuccessOutcome::Applied { .. }
        ));
        let snapshot = repositories::get_runtime_graph_snapshot(&fixture.pool, fixture.library_id)
            .await?
            .context("graph coverage snapshot disappeared after publication")?;
        assert_eq!(
            snapshot.provenance_coverage_percent,
            Some(10.0),
            "one projected ready identity out of ten must publish exactly 10%, not its complement"
        );

        fixture.create_document_node(documents[1].document_id, "Failed reprocess document").await?;
        fixture.create_document_node(documents[2].document_id, "Leased reprocess document").await?;
        fixture.publish_snapshot(3).await?;
        let _failed_job = fixture.create_job(documents[1], "failed", "failed-reprocess").await?;
        let processing_job = fixture.create_job(documents[2], "leased", "leased-reprocess").await?;
        let _processing_attempt =
            fixture.create_leased_attempt(processing_job.id, "processing", 50).await?;

        let statuses = content_document_list_repository::aggregate_document_list_status_counts(
            &fixture.pool,
            content_document_list_repository::DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &content_document_list_repository::DocumentListFilters::default(),
                search_ranges: &content_document_list_repository::DocumentListSearchRanges::default(
                ),
                status_filter: &[],
                id_filter: None,
                snapshot_at: chrono::Utc::now(),
            },
        )
        .await?;
        assert_eq!(statuses.total, Some(10));
        assert_eq!(statuses.ready, Some(9));
        assert_eq!(statuses.processing, Some(1));
        assert_eq!(statuses.failed, Some(0));
        assert_eq!(statuses.queued, Some(0));
        assert_eq!(statuses.canceled, Some(0));

        let second_publishing_job =
            fixture.create_job(documents[3], "leased", "second-publisher").await?;
        let second_publishing_attempt =
            fixture.create_leased_attempt(second_publishing_job.id, "second-publisher", 90).await?;
        let second_completed_at = Utc::now();
        let second_publication = ingest_repository::publish_graph_refresh_success(
            &fixture.pool,
            &ingest_repository::PublishGraphRefreshSuccess {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: documents[3].document_id,
                revision_id: documents[3].revision_id,
                attempt_id: second_publishing_attempt.id,
                graph_state: "graph_degraded".to_string(),
                graph_ready_at: None,
                completed_at: second_completed_at,
            },
        )
        .await?;
        assert!(matches!(
            second_publication,
            ingest_repository::PublishGraphRefreshSuccessOutcome::Applied { .. }
        ));
        let snapshot_after_reprocessing =
            repositories::get_runtime_graph_snapshot(&fixture.pool, fixture.library_id)
                .await?
                .context("graph coverage snapshot disappeared after reprocessing publication")?;
        let coverage_percent = snapshot_after_reprocessing
            .provenance_coverage_percent
            .context("reprocessing publication did not record coverage")?;
        assert!(
            (coverage_percent - (2.0 / 9.0 * 100.0)).abs() < f64::EPSILON,
            "publication coverage must keep readable failed refreshes and exclude active retries"
        );

        let graph_ready =
            content_document_list_repository::count_library_ready_documents_in_graph_projection(
                &fixture.pool,
                fixture.library_id,
                PROJECTION_VERSION,
            )
            .await?;
        assert_eq!(
            graph_ready, 2,
            "readable failed refreshes remain represented while active retries are excluded"
        );
        let metrics = content_document_list_repository::aggregate_library_document_metrics(
            &fixture.pool,
            fixture.library_id,
        )
        .await?;
        assert_eq!(metrics.total, 10);
        assert_eq!(metrics.ready, 9);
        assert_eq!(metrics.processing, 1);
        assert_eq!(metrics.failed, 0);
        assert_eq!(metrics.graph_ready, 2);
        assert_eq!(metrics.graph_sparse, 7);
        assert_eq!(metrics.graph_ready + metrics.graph_sparse, metrics.ready);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// The documents-list `indexing=graph_ready|graph_gap` filters must resolve the
/// exact sets the dashboard's `graphReady` / `graphSparse` counters count, over
/// the same active-head population — a retired document cannot leak into
/// either set, and a plain graphless ready document is the whole gap.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_coverage_filters_match_the_dashboard_counters() -> Result<()> {
    let fixture = GraphCoverageFixture::create().await?;

    let result = async {
        let covered = fixture.create_renderable_revision("covered", "active").await?;
        fixture.create_document_node(covered.document_id, "covered").await?;
        let graphless = fixture.create_renderable_revision("graphless", "active").await?;
        let retired = fixture.create_renderable_revision("retired", "deleted").await?;
        fixture.publish_snapshot(1).await?;

        let graph_ready = content_document_list_repository::list_library_documents_graph_ready(
            &fixture.pool,
            fixture.library_id,
        )
        .await?;
        let graph_gap = content_document_list_repository::list_library_documents_with_graph_gap(
            &fixture.pool,
            fixture.library_id,
        )
        .await?;
        let metrics = content_document_list_repository::aggregate_library_document_metrics(
            &fixture.pool,
            fixture.library_id,
        )
        .await?;

        assert_eq!(graph_ready, vec![covered.document_id]);
        assert_eq!(graph_gap, vec![graphless.document_id]);
        assert_eq!(i64::try_from(graph_ready.len()).unwrap(), metrics.graph_ready);
        assert_eq!(i64::try_from(graph_gap.len()).unwrap(), metrics.graph_sparse);
        assert!(
            !graph_ready.contains(&retired.document_id)
                && !graph_gap.contains(&retired.document_id),
            "a retired document must stay outside both exact graph-coverage sets"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
