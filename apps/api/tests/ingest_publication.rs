use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    infra::{
        knowledge_plane::SearchStore,
        knowledge_rows::{KNOWLEDGE_CHUNK_VECTOR_KIND, KnowledgeChunkVectorRow},
        postgres::pg_search_store::PgSearchStore,
        repositories::{catalog_repository, content_repository, ingest_repository, ops_repository},
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
        let name = format!("ingest_publication_{}", Uuid::now_v7().simple());
        let admin = PgPoolOptions::new().max_connections(1).connect(&admin_url).await?;
        terminate_database_connections(&admin, &name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
            .execute(&admin)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin)
            .await?;
        admin.close().await;
        Ok(Self { database_url: replace_database_name(base_database_url, &name)?, admin_url, name })
    }

    async fn drop(self) -> Result<()> {
        let admin = PgPoolOptions::new().max_connections(1).connect(&self.admin_url).await?;
        terminate_database_connections(&admin, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin)
            .await?;
        admin.close().await;
        Ok(())
    }
}

fn replace_database_name(database_url: &str, database_name: &str) -> Result<String> {
    let (without_query, query) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash = without_query.rfind('/').context("database URL has no database component")?;
    let mut rebuilt = format!("{}{database_name}", &without_query[..=slash]);
    if let Some(query) = query {
        rebuilt.push('?');
        rebuilt.push_str(query);
    }
    Ok(rebuilt)
}

async fn terminate_database_connections(pool: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(pool)
    .await?;
    Ok(())
}

struct PublicationFixture {
    pool: PgPool,
    database: TempDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    mutation_id: Uuid,
    mutation_item_id: Uuid,
    async_operation_id: Uuid,
    job_id: Uuid,
    attempt_id: Uuid,
    chunk_id: Uuid,
    embedding_profile_key: String,
}

impl PublicationFixture {
    async fn create(text_state: &str, web_owned: bool) -> Result<Self> {
        let settings = Settings::from_env().context("load ingest-publication test settings")?;
        let database = TempDatabase::create(&settings.database_url).await?;
        let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("publication-workspace-{suffix}"),
            "Publication Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("publication-library-{suffix}"),
            "Publication Library",
            None,
            None,
        )
        .await?;
        let document = content_repository::create_document_with_projection(
            &pool,
            &content_repository::NewContentDocument {
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: &format!("publication-document-{suffix}"),
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
            Some("publication.txt"),
        )
        .await?;
        let revision = match content_repository::create_revision_with_projection(
            &pool,
            &content_repository::NewContentRevisionProjection {
                document_id: document.id,
                workspace_id: workspace.id,
                library_id: library.id,
                content_source_kind: "upload",
                checksum: "sha256:publication",
                mime_type: "text/plain",
                byte_size: 20,
                title: Some("Publication"),
                language_code: None,
                source_uri: None,
                document_hint: None,
                storage_key: Some("memory://publication"),
                created_by_principal_id: None,
            },
        )
        .await?
        {
            content_repository::CreateContentRevisionOutcome::Created(revision) => *revision,
            other => return Err(anyhow!("revision fixture was not created: {other:?}")),
        };
        let mutation = content_repository::create_mutation(
            &pool,
            &content_repository::NewContentMutation {
                workspace_id: workspace.id,
                library_id: library.id,
                operation_kind: if web_owned { "web_capture" } else { "upload" },
                requested_by_principal_id: None,
                request_surface: "internal",
                idempotency_key: None,
                source_identity: None,
                mutation_state: "running",
                failure_code: None,
                conflict_code: None,
            },
        )
        .await?;
        let item = content_repository::create_mutation_item(
            &pool,
            &content_repository::NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: Some(document.id),
                base_revision_id: None,
                result_revision_id: Some(revision.id),
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        content_repository::upsert_document_head(
            &pool,
            &content_repository::NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: None,
                readable_revision_id: None,
                latest_mutation_id: Some(mutation.id),
                latest_successful_attempt_id: None,
            },
        )
        .await?;
        let chunks = content_repository::replace_chunks_with_projection(
            &pool,
            revision.id,
            &[content_repository::NewContentChunkProjection {
                workspace_id: workspace.id,
                library_id: library.id,
                document_id: document.id,
                revision_id: revision.id,
                chunk_index: 0,
                start_offset: 0,
                end_offset: 20,
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                content_text: "publication evidence".to_string(),
                normalized_text: "publication evidence".to_string(),
                text_checksum: "sha256:publication-chunk".to_string(),
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(i64::from(revision.revision_number)),
                vector_generation: Some(i64::from(revision.revision_number)),
                quality_score: Some(1.0),
                window_text: None,
                occurred_at: None,
                occurred_until: None,
            }],
        )
        .await?;
        let chunk = chunks.first().context("publication chunk was not created")?;
        sqlx::query(
            "update knowledge_revision
             set text_state = $2,
                 vector_state = 'processing',
                 graph_state = 'processing',
                 text_readable_at = case when $2 = 'text_readable' then now() else null end
             where revision_id = $1",
        )
        .bind(revision.id)
        .bind(text_state)
        .execute(&pool)
        .await?;

        let embedding_profile_key = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        PgSearchStore { pool: pool.clone() }
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: chunk.id,
                revision_id: revision.id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.1, 0.2, 0.3],
                freshness_generation: i64::from(revision.revision_number),
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        let operation = ops_repository::create_async_operation(
            &pool,
            &ops_repository::NewOpsAsyncOperation {
                workspace_id: workspace.id,
                library_id: Some(library.id),
                operation_kind: "content_mutation",
                surface_kind: "internal",
                requested_by_principal_id: None,
                status: "processing",
                subject_kind: "content_mutation",
                subject_id: Some(mutation.id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await?;
        if web_owned {
            sqlx::query(
                "insert into content_web_ingest_run (
                    id, mutation_id, async_operation_id, workspace_id, library_id,
                    mode, seed_url, normalized_seed_url, boundary_policy,
                    max_depth, max_pages, run_state
                 ) values (
                    $1, $2, $3, $4, $5, 'single_page',
                    'https://fixture.invalid/', 'https://fixture.invalid/',
                    'same_host', 0, 1, 'processing'
                 )",
            )
            .bind(Uuid::now_v7())
            .bind(mutation.id)
            .bind(operation.id)
            .bind(workspace.id)
            .bind(library.id)
            .execute(&pool)
            .await?;
        }
        let job = ingest_repository::create_ingest_job(
            &pool,
            &ingest_repository::NewIngestJob {
                workspace_id: workspace.id,
                library_id: library.id,
                mutation_id: Some(mutation.id),
                mutation_item_id: Some(item.id),
                connector_id: None,
                async_operation_id: Some(operation.id),
                knowledge_document_id: Some(document.id),
                knowledge_revision_id: Some(revision.id),
                job_kind: "content_mutation".to_string(),
                queue_state: "leased".to_string(),
                priority: 10,
                dedupe_key: Some(format!("publication:{mutation_id}", mutation_id = mutation.id)),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        )
        .await?;
        let attempt = ingest_repository::create_ingest_attempt(
            &pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some("publication-attempt".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("extract_content".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 60,
                retryable: false,
            },
        )
        .await?;

        Ok(Self {
            pool,
            database,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id: document.id,
            revision_id: revision.id,
            mutation_id: mutation.id,
            mutation_item_id: item.id,
            async_operation_id: operation.id,
            job_id: job.id,
            attempt_id: attempt.id,
            chunk_id: chunk.id,
            embedding_profile_key,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        self.database.drop().await
    }

    async fn source_truth_version(&self) -> Result<i64> {
        Ok(catalog_repository::get_library_source_truth_version(&self.pool, self.library_id)
            .await?)
    }

    fn publication_command(
        &self,
        expected_source_truth_version: i64,
        completed_at: chrono::DateTime<Utc>,
        graph_state: &str,
        graph_ready_at: Option<chrono::DateTime<Utc>>,
    ) -> ingest_repository::PublishContentIngestSuccess {
        ingest_repository::PublishContentIngestSuccess {
            workspace_id: self.workspace_id,
            library_id: self.library_id,
            document_id: self.document_id,
            revision_id: self.revision_id,
            mutation_id: self.mutation_id,
            mutation_item_id: self.mutation_item_id,
            attempt_id: self.attempt_id,
            expected_source_truth_version,
            embedding_profile_key: Some(self.embedding_profile_key.clone()),
            text_state: "text_readable".to_string(),
            graph_state: graph_state.to_string(),
            text_readable_at: Some(completed_at),
            graph_ready_at,
            completed_at,
        }
    }
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn successful_publication_commits_every_visible_surface_once() -> Result<()> {
    let fixture = PublicationFixture::create("text_readable", false).await?;
    let result = async {
        let source_before = fixture.source_truth_version().await?;
        let completed_at = Utc::now();
        let outcome = ingest_repository::publish_content_ingest_success(
            &fixture.pool,
            &fixture.publication_command(source_before, completed_at, "ready", Some(completed_at)),
        )
        .await?;
        let source_after = match outcome {
            ingest_repository::PublishContentIngestSuccessOutcome::Applied {
                source_truth_version,
                mutation_completed,
            } => {
                assert!(mutation_completed);
                source_truth_version
            }
            other => return Err(anyhow!("unexpected publication outcome: {other:?}")),
        };
        assert!(source_after > source_before);
        assert_eq!(fixture.source_truth_version().await?, source_after);

        let head = content_repository::get_document_head(&fixture.pool, fixture.document_id)
            .await?
            .context("published head missing")?;
        assert_eq!(head.active_revision_id, Some(fixture.revision_id));
        assert_eq!(head.readable_revision_id, Some(fixture.revision_id));
        assert_eq!(head.latest_mutation_id, Some(fixture.mutation_id));
        assert_eq!(head.latest_successful_attempt_id, Some(fixture.attempt_id));
        let lifecycle = sqlx::query_as::<_, (String, String, String, String, String)>(
            "select
                revision.vector_state,
                revision.graph_state,
                item.item_state::text,
                mutation.mutation_state::text,
                operation.status::text
             from knowledge_revision as revision
             join content_mutation_item as item on item.id = $2
             join content_mutation as mutation on mutation.id = item.mutation_id
             join ops_async_operation as operation on operation.id = $3
             where revision.revision_id = $1",
        )
        .bind(fixture.revision_id)
        .bind(fixture.mutation_item_id)
        .bind(fixture.async_operation_id)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(
            lifecycle,
            ("ready".into(), "ready".into(), "applied".into(), "applied".into(), "ready".into())
        );
        let attempt =
            ingest_repository::get_ingest_attempt_by_id(&fixture.pool, fixture.attempt_id)
                .await?
                .context("published attempt missing")?;
        let job = ingest_repository::get_ingest_job_by_id(&fixture.pool, fixture.job_id)
            .await?
            .context("published job missing")?;
        assert_eq!(attempt.attempt_state, "succeeded");
        assert_eq!(job.queue_state, "completed");
        let outbox_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from webhook_lifecycle_outbox
             where event_id = $1",
        )
        .bind(ironrag_backend::domains::webhook::revision_ready_event_id(fixture.revision_id))
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(outbox_count, 1);

        let stale = ingest_repository::publish_content_ingest_success(
            &fixture.pool,
            &fixture.publication_command(source_before, completed_at, "ready", Some(completed_at)),
        )
        .await?;
        assert!(matches!(
            stale,
            ingest_repository::PublishContentIngestSuccessOutcome::AuthorityLost { .. }
        ));
        assert_eq!(fixture.source_truth_version().await?, source_after);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn web_owned_success_does_not_complete_shared_mutation_or_operation() -> Result<()> {
    let fixture = PublicationFixture::create("text_readable", true).await?;
    let result = async {
        let source_before = fixture.source_truth_version().await?;
        let completed_at = Utc::now();
        let outcome = ingest_repository::publish_content_ingest_success(
            &fixture.pool,
            &fixture.publication_command(source_before, completed_at, "ready", Some(completed_at)),
        )
        .await?;
        assert!(matches!(
            outcome,
            ingest_repository::PublishContentIngestSuccessOutcome::Applied {
                mutation_completed: false,
                ..
            }
        ));
        let aggregate = sqlx::query_as::<_, (String, String, String)>(
            "select mutation.mutation_state::text, item.item_state::text, operation.status::text
             from content_mutation mutation
             join content_mutation_item item on item.mutation_id = mutation.id
             join ops_async_operation operation on operation.id = $2
             where mutation.id = $1 and item.id = $3",
        )
        .bind(fixture.mutation_id)
        .bind(fixture.async_operation_id)
        .bind(fixture.mutation_item_id)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(aggregate, ("running".into(), "applied".into(), "processing".into()));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn graph_refresh_publishes_only_current_head_and_settles_superseded_work() -> Result<()> {
    let fixture = PublicationFixture::create("text_readable", false).await?;
    let result = async {
        let content_source = fixture.source_truth_version().await?;
        let content_completed_at = Utc::now();
        ingest_repository::publish_content_ingest_success(
            &fixture.pool,
            &fixture.publication_command(
                content_source,
                content_completed_at,
                "graph_degraded",
                None,
            ),
        )
        .await?;
        let head_before = content_repository::get_document_head(&fixture.pool, fixture.document_id)
            .await?
            .context("graph-refresh head missing")?;
        let outbox_before = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from webhook_lifecycle_outbox where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await?;

        let graph_job = ingest_repository::create_ingest_job(
            &fixture.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(fixture.document_id),
                knowledge_revision_id: Some(fixture.revision_id),
                job_kind: "graph_refresh".to_string(),
                queue_state: "leased".to_string(),
                priority: 20,
                dedupe_key: Some(format!("graph-refresh:{}:1", fixture.revision_id)),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        )
        .await?;
        let graph_attempt = ingest_repository::create_ingest_attempt(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: graph_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some("graph-refresh-attempt".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 90,
                retryable: false,
            },
        )
        .await?;
        let source_before_graph = fixture.source_truth_version().await?;
        let graph_completed_at = Utc::now();
        let graph_outcome = ingest_repository::publish_graph_refresh_success(
            &fixture.pool,
            &ingest_repository::PublishGraphRefreshSuccess {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: fixture.document_id,
                revision_id: fixture.revision_id,
                attempt_id: graph_attempt.id,
                graph_state: "ready".to_string(),
                graph_ready_at: Some(graph_completed_at),
                completed_at: graph_completed_at,
            },
        )
        .await?;
        let source_after_graph = match graph_outcome {
            ingest_repository::PublishGraphRefreshSuccessOutcome::Applied {
                source_truth_version,
            } => source_truth_version,
            other => return Err(anyhow!("unexpected graph publication outcome: {other:?}")),
        };
        assert!(source_after_graph > source_before_graph);
        let head_after = content_repository::get_document_head(&fixture.pool, fixture.document_id)
            .await?
            .context("graph-refresh head disappeared")?;
        assert_eq!(head_after.active_revision_id, head_before.active_revision_id);
        assert_eq!(head_after.readable_revision_id, head_before.readable_revision_id);
        assert_eq!(head_after.latest_mutation_id, head_before.latest_mutation_id);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "select graph_state from knowledge_revision where revision_id = $1",
            )
            .bind(fixture.revision_id)
            .fetch_one(&fixture.pool)
            .await?,
            "ready"
        );
        let outbox_after = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from webhook_lifecycle_outbox where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(outbox_after, outbox_before, "graph maintenance must not emit revision.ready");

        let stale_job = ingest_repository::create_ingest_job(
            &fixture.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(fixture.document_id),
                knowledge_revision_id: Some(fixture.revision_id),
                job_kind: "graph_refresh".to_string(),
                queue_state: "leased".to_string(),
                priority: 20,
                dedupe_key: Some(format!("graph-refresh:{}:2", fixture.revision_id)),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        )
        .await?;
        let stale_attempt = ingest_repository::create_ingest_attempt(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: stale_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some("stale-graph-refresh".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("extract_graph".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 90,
                retryable: false,
            },
        )
        .await?;
        sqlx::query(
            "update content_document_head
             set active_revision_id = null
             where document_id = $1",
        )
        .bind(fixture.document_id)
        .execute(&fixture.pool)
        .await?;
        let source_before_superseded = fixture.source_truth_version().await?;
        let superseded = ingest_repository::publish_graph_refresh_success(
            &fixture.pool,
            &ingest_repository::PublishGraphRefreshSuccess {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: fixture.document_id,
                revision_id: fixture.revision_id,
                attempt_id: stale_attempt.id,
                graph_state: "ready".to_string(),
                graph_ready_at: Some(Utc::now()),
                completed_at: Utc::now(),
            },
        )
        .await?;
        assert!(matches!(
            superseded,
            ingest_repository::PublishGraphRefreshSuccessOutcome::Superseded { .. }
        ));
        assert_eq!(fixture.source_truth_version().await?, source_before_superseded);
        let stale_attempt =
            ingest_repository::get_ingest_attempt_by_id(&fixture.pool, stale_attempt.id)
                .await?
                .context("superseded graph attempt missing")?;
        let stale_job = ingest_repository::get_ingest_job_by_id(&fixture.pool, stale_job.id)
            .await?
            .context("superseded graph job missing")?;
        assert_eq!(stale_attempt.attempt_state, "abandoned");
        assert_eq!(stale_job.queue_state, "completed");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn failure_publication_retries_then_terminally_fails_without_inventing_text_readiness()
-> Result<()> {
    let fixture = PublicationFixture::create("accepted", false).await?;
    let result = async {
        let source_before = fixture.source_truth_version().await?;
        let first_failure = ingest_repository::fail_content_ingest_attempt(
            &fixture.pool,
            &ingest_repository::FailContentIngestAttempt {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: fixture.document_id,
                revision_id: fixture.revision_id,
                mutation_id: fixture.mutation_id,
                mutation_item_id: fixture.mutation_item_id,
                attempt_id: fixture.attempt_id,
                current_stage: None,
                failure_class: Some("provider".to_string()),
                failure_code: Some("temporary".to_string()),
                failure_message: Some("temporary provider failure".to_string()),
                retryable: true,
                delete_vectors: true,
                failed_at: Utc::now(),
            },
        )
        .await?;
        let source_after_retry = match first_failure {
            ingest_repository::FailContentIngestAttemptOutcome::Applied {
                deleted,
                source_truth_version,
                retry_scheduled: true,
                mutation_failed: false,
            } => {
                assert_eq!(deleted, 1);
                source_truth_version
            }
            other => return Err(anyhow!("unexpected retry failure outcome: {other:?}")),
        };
        assert!(source_after_retry > source_before);
        let readiness =
            sqlx::query_as::<_, (String, Option<chrono::DateTime<Utc>>, String, String)>(
                "select text_state, text_readable_at, vector_state, graph_state
             from knowledge_revision
             where revision_id = $1",
            )
            .bind(fixture.revision_id)
            .fetch_one(&fixture.pool)
            .await?;
        assert_eq!(readiness, ("accepted".into(), None, "failed".into(), "failed".into()));
        let first_attempt =
            ingest_repository::get_ingest_attempt_by_id(&fixture.pool, fixture.attempt_id)
                .await?
                .context("retry attempt missing")?;
        let retry_job = ingest_repository::get_ingest_job_by_id(&fixture.pool, fixture.job_id)
            .await?
            .context("retry job missing")?;
        assert_eq!(first_attempt.attempt_state, "failed");
        assert_eq!(first_attempt.current_stage.as_deref(), Some("extract_content"));
        assert!(first_attempt.retryable);
        assert_eq!(retry_job.queue_state, "queued");
        let retry_surfaces = sqlx::query_as::<_, (String, String, String)>(
            "select item.item_state::text, mutation.mutation_state::text, operation.status::text
             from content_mutation_item item
             join content_mutation mutation on mutation.id = item.mutation_id
             join ops_async_operation operation on operation.id = $2
             where item.id = $1",
        )
        .bind(fixture.mutation_item_id)
        .bind(fixture.async_operation_id)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(retry_surfaces, ("pending".into(), "running".into(), "accepted".into()));
        let search_store = PgSearchStore { pool: fixture.pool.clone() };
        assert!(search_store.list_chunk_vectors_by_chunk(fixture.chunk_id).await?.is_empty());

        sqlx::query(
            "update ingest_job
             set queue_state = 'leased',
                 queue_leased_at = now(),
                 queue_lease_token = 'terminal-lease',
                 queue_lease_owner = 'test-worker'
             where id = $1",
        )
        .bind(fixture.job_id)
        .execute(&fixture.pool)
        .await?;
        let terminal_attempt = ingest_repository::create_ingest_attempt(
            &fixture.pool,
            &ingest_repository::NewIngestAttempt {
                job_id: fixture.job_id,
                attempt_number: 5,
                worker_principal_id: None,
                lease_token: Some("terminal-attempt".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("prepare_structure".to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 30,
                retryable: false,
            },
        )
        .await?;
        let terminal = ingest_repository::fail_content_ingest_attempt(
            &fixture.pool,
            &ingest_repository::FailContentIngestAttempt {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: fixture.document_id,
                revision_id: fixture.revision_id,
                mutation_id: fixture.mutation_id,
                mutation_item_id: fixture.mutation_item_id,
                attempt_id: terminal_attempt.id,
                current_stage: None,
                failure_class: Some("provider".to_string()),
                failure_code: Some("exhausted".to_string()),
                failure_message: Some("persistent provider failure".to_string()),
                retryable: true,
                delete_vectors: false,
                failed_at: Utc::now(),
            },
        )
        .await?;
        assert!(matches!(
            terminal,
            ingest_repository::FailContentIngestAttemptOutcome::Applied {
                retry_scheduled: false,
                mutation_failed: true,
                ..
            }
        ));
        let terminal_attempt =
            ingest_repository::get_ingest_attempt_by_id(&fixture.pool, terminal_attempt.id)
                .await?
                .context("terminal attempt missing")?;
        assert_eq!(terminal_attempt.current_stage.as_deref(), Some("prepare_structure"));
        assert!(!terminal_attempt.retryable);
        let terminal_surfaces = sqlx::query_as::<_, (String, String, String, String)>(
            "select item.item_state::text, mutation.mutation_state::text,
                    operation.status::text, job.queue_state::text
             from content_mutation_item item
             join content_mutation mutation on mutation.id = item.mutation_id
             join ops_async_operation operation on operation.id = $2
             join ingest_job job on job.id = $3
             where item.id = $1",
        )
        .bind(fixture.mutation_item_id)
        .bind(fixture.async_operation_id)
        .bind(fixture.job_id)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(
            terminal_surfaces,
            ("failed".into(), "failed".into(), "failed".into(), "failed".into())
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
