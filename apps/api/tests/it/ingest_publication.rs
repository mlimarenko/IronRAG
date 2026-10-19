use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::ops::OpsFailureDisposition,
    infra::{
        knowledge_plane::{
            ActiveEmbeddingProfileSource, IngestVectorWriteFence, SearchStore, VectorWriteFence,
        },
        knowledge_rows::{KNOWLEDGE_CHUNK_VECTOR_KIND, KnowledgeChunkVectorRow},
        persistence::Persistence,
        postgres::pg_search_store::PgSearchStore,
        repositories::{
            catalog_repository, content_chunk_repository, content_document_head_repository,
            content_document_repository, content_knowledge_projection_repository,
            content_mutation_repository, content_revision_repository, ingest_repository,
            ops_repository,
        },
    },
    services::ingest::service::IngestService,
};

struct FixedEmbeddingProfileSource(String);

struct PausedPublicationProfile {
    profile: String,
    entered: tokio::sync::Notify,
    released: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl ActiveEmbeddingProfileSource for PausedPublicationProfile {
    async fn active_embedding_profile_key(&self, _library_id: Uuid) -> Result<String> {
        self.entered.notify_one();
        self.released.notified().await;
        Ok(self.profile.clone())
    }
}

async fn seed_publication_mutation(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    operation_kind: &str,
    document_id: Uuid,
    revision_id: Option<Uuid>,
) -> Result<content_mutation_repository::ContentMutationRow> {
    let revision = match revision_id {
        Some(id) => Some(
            content_revision_repository::get_revision_by_id(pool, id)
                .await?
                .context("publication fixture revision")?,
        ),
        None => None,
    };
    let identity = serde_json::json!({
        "workspaceId":workspace_id,"libraryId":library_id,"operationKind":operation_kind,"sourceIdentity":null,"materializationToken":null,
        "target":{"kind":"existing","documentId":document_id},
        "revision":revision.as_ref().map(|r|serde_json::json!({"contentSourceKind":r.content_source_kind,"baseRevisionId":r.parent_revision_id,"visibilityIntentId":null})),
        "replacement":revision.as_ref().map(|r|serde_json::json!({"checksum":r.checksum,"byteSize":r.byte_size,"mimeType":r.mime_type,"fileName":"publication.txt",
            "title":r.title,"languageCode":r.language_code,"sourceUri":r.source_uri,"documentHint":r.document_hint})),
    });
    crate::content_lifecycle_support::seed_pending_content_mutation(
        pool,
        crate::content_lifecycle_support::PendingContentMutationSeed {
            workspace_id,
            library_id,
            operation_kind,
            request_surface: "internal",
            mutation_state: "running",
            request_identity: &identity,
        },
    )
    .await
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_head_snapshots_retain_revision_authority() -> Result<()> {
    use crate::content_lifecycle_support::{ContentLifecycleFixture, revision_command};
    use ironrag_backend::infra::repositories::admission_repository::{
        VisibilityAdmissionRequest, admit_visibility_intent_with_failpoint,
    };
    use ironrag_backend::services::content::service::CreateDocumentCommand;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pg = &fixture.state.persistence.postgres;
        let content = &fixture.state.application_services.content;
        let document = content.create_document(&fixture.state, CreateDocumentCommand {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id,
            external_key: Some("head-lineage-snapshot".to_owned()), file_name: Some("source.txt".to_owned()),
            created_by_principal_id: None, parent_external_key: None,
        }).await?;
        let revision = crate::content_lifecycle_support::create_imported_revision(&fixture.state,
            revision_command(document.id,"upload","sha256:head-snapshot","Snapshot",None), "source.txt").await?;
        sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(document.id).bind(revision.id).execute(pg).await?;
        let initial = content_document_head_repository::get_document_head(pg,document.id).await?.context("initial head")?;
        assert_eq!(initial.readable_revision_id,Some(revision.id));
        assert_eq!(initial.visibility_intent_id,None);
        let intent = admit_visibility_intent_with_failpoint(pg,&VisibilityAdmissionRequest {
            workspace_id:fixture.workspace_id,library_id:fixture.library_id,document_id:document.id,
            requested_by_principal_id:None,request_surface:"rest".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                idempotency_key:"snapshot-denial".to_owned(),source_identity:"source".to_owned(),materialization_token:None,
                replacement:ironrag_contracts::documents::ReplacementIdentity {
                    checksum:format!("sha256:{}","a".repeat(64)),byte_size:1,mime_type:"text/plain".to_owned(),file_name:"next.txt".to_owned(),
                    title:None,language_code:None,source_uri:None,document_hint:None,
                },
            },
        },None).await?.into_bundle();
        let current = content_document_head_repository::get_document_head(pg,document.id).await?.context("current head")?;
        assert_eq!(current.readable_revision_id,None);
        assert_eq!(current.visibility_intent_id,Some(intent.receipt.visibility_intent_id));
        let listed = content_document_head_repository::list_document_heads_by_document_ids(pg,&[document.id]).await?;
        assert_eq!(listed[0].visibility_intent_id,current.visibility_intent_id);
        assert_eq!(listed[0].readable_revision_id,current.readable_revision_id);
        let mut tx = pg.begin().await?;
        let locked = content_document_head_repository::get_document_head_for_update_with_executor(&mut *tx,document.id).await?.context("locked head")?;
        assert_eq!(locked.visibility_intent_id,current.visibility_intent_id);
        assert_eq!(locked.readable_revision_id,current.readable_revision_id);
        tx.rollback().await?;
        let domain = content.get_document_head(&fixture.state,document.id).await?.context("domain head")?;
        assert_eq!(domain.visibility_intent_id,current.visibility_intent_id);
        assert_eq!(domain.readable_revision_id,current.readable_revision_id);
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[async_trait::async_trait]
impl ActiveEmbeddingProfileSource for FixedEmbeddingProfileSource {
    async fn active_embedding_profile_key(&self, _library_id: Uuid) -> Result<String> {
        Ok(self.0.clone())
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
    async fn attempt_wire(&self, attempt_id: Uuid) -> Result<serde_json::Value> {
        let mut settings = Settings::from_env()?;
        settings.database_url = self.database.database_url.clone();
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let redis = redis::Client::open(settings.redis_url.clone())?;
        let state = AppState::from_dependencies(
            settings,
            Persistence::for_tests(self.pool.clone(), redis),
        )?;
        let attempt = IngestService::new().get_attempt(&state, attempt_id).await?;
        Ok(serde_json::to_value(attempt)?)
    }

    async fn create_mutation_item_for_revision(
        &self,
        result_revision_id: Option<Uuid>,
    ) -> Result<(Uuid, Uuid)> {
        let mutation = seed_publication_mutation(
            &self.pool,
            self.workspace_id,
            self.library_id,
            "upload",
            self.document_id,
            result_revision_id,
        )
        .await?;
        let item = content_mutation_repository::create_mutation_item(
            &self.pool,
            &content_mutation_repository::NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: self.document_id,
                base_revision_id: None,
                result_revision_id,
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        Ok((mutation.id, item.id))
    }

    async fn create(text_state: &str, web_owned: bool) -> Result<Self> {
        let settings = Settings::from_env().context("load ingest-publication test settings")?;
        let database = TempDatabase::create(&settings.database_url).await?;
        let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;

        let suffix = Uuid::now_v7().simple().to_string();
        let (workspace_id, library_id) = seed_publication_catalog(&pool, &suffix).await?;
        let content = seed_publication_content(
            &pool,
            workspace_id,
            library_id,
            text_state,
            web_owned,
            &suffix,
        )
        .await?;
        let embedding_profile_key = seed_publication_embedding(
            &pool,
            workspace_id,
            library_id,
            content.chunk_id,
            content.revision_id,
            content.revision_number,
        )
        .await?;
        let (async_operation_id, job_id, attempt_id) = seed_publication_queue(
            &pool,
            workspace_id,
            library_id,
            web_owned,
            PublicationQueueSeed {
                document_id: content.document_id,
                revision_id: content.revision_id,
                mutation_id: content.mutation_id,
                mutation_item_id: content.mutation_item_id,
            },
        )
        .await?;

        Ok(Self {
            pool,
            database,
            workspace_id,
            library_id,
            document_id: content.document_id,
            revision_id: content.revision_id,
            mutation_id: content.mutation_id,
            mutation_item_id: content.mutation_item_id,
            async_operation_id,
            job_id,
            attempt_id,
            chunk_id: content.chunk_id,
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

    fn vector_row(&self) -> KnowledgeChunkVectorRow {
        KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id: self.workspace_id,
            library_id: self.library_id,
            chunk_id: self.chunk_id,
            revision_id: self.revision_id,
            embedding_model_key: self.embedding_profile_key.clone(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: vec![0.3, 0.2, 0.1],
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        }
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

/// Seeds the workspace and library ids for the publication fixture.
async fn seed_publication_catalog(pool: &PgPool, suffix: &str) -> Result<(Uuid, Uuid)> {
    let workspace = catalog_repository::create_workspace(
        pool,
        &format!("publication-workspace-{suffix}"),
        "Publication Workspace",
        None,
    )
    .await?;
    let library = catalog_repository::create_library(
        pool,
        workspace.id,
        &format!("publication-library-{suffix}"),
        "Publication Library",
        None,
        None,
    )
    .await?;
    Ok((workspace.id, library.id))
}

/// Ids seeded by [`seed_publication_content`].
struct PublicationContentIds {
    document_id: Uuid,
    revision_id: Uuid,
    revision_number: i32,
    mutation_id: Uuid,
    mutation_item_id: Uuid,
    chunk_id: Uuid,
}

/// Seeds the document, its single upload revision, the running mutation and
/// item, the document head, one ready chunk and the prepared structured
/// revision.
async fn seed_publication_content(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    text_state: &str,
    web_owned: bool,
    suffix: &str,
) -> Result<PublicationContentIds> {
    let document = content_knowledge_projection_repository::create_document_with_projection(
        pool,
        &content_document_repository::NewContentDocument {
            workspace_id,
            library_id,
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
    let revision = match content_revision_repository::create_revision_with_projection(
        pool,
        &content_revision_repository::NewContentRevisionProjection {
            admission_identity: Some(&crate::content_repository::unbound_mutation_admission(
                ironrag_contracts::documents::ReplacementIdentity {
                    checksum: format!("sha256:{}", "a".repeat(64)),
                    mime_type: "text/plain".to_owned(),
                    byte_size: 20,
                    file_name: "publication.txt".to_owned(),
                    title: Some("Publication".to_owned()),
                    language_code: None,
                    source_uri: None,
                    document_hint: None,
                },
            )?),
            document_id: document.id,
            workspace_id,
            library_id,
            content_source_kind: "upload",
            checksum: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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
        content_revision_repository::CreateContentRevisionOutcome::Created(revision) => *revision,
        other => return Err(anyhow!("revision fixture was not created: {other:?}")),
    };
    let mutation = seed_publication_mutation(
        pool,
        workspace_id,
        library_id,
        if web_owned { "web_capture" } else { "upload" },
        document.id,
        Some(revision.id),
    )
    .await?;
    let item = content_mutation_repository::create_mutation_item(
        pool,
        &content_mutation_repository::NewContentMutationItem {
            mutation_id: mutation.id,
            document_id: document.id,
            base_revision_id: None,
            result_revision_id: Some(revision.id),
            item_state: "pending",
            message: None,
        },
    )
    .await?;
    content_document_head_repository::upsert_document_head(
        pool,
        &content_document_head_repository::NewContentDocumentHead {
            publishing_mutation_item_id: None,
            document_id: document.id,
            active_revision_id: None,
            readable_revision_id: None,
            latest_mutation_id: Some(mutation.id),
            latest_successful_attempt_id: None,
        },
    )
    .await?;
    let chunks = content_chunk_repository::replace_chunks_with_projection(
        pool,
        revision.id,
        &[crate::knowledge_seed::ready_paragraph_projection(
            crate::knowledge_seed::ReadyChunkSeed {
                workspace_id,
                library_id,
                document_id: document.id,
                revision_id: revision.id,
                chunk_index: 0,
                text: "publication evidence",
            },
            20,
            3,
            "sha256:publication-chunk",
            i64::from(revision.revision_number),
        )],
    )
    .await?;
    let chunk = chunks.first().context("publication chunk was not created")?;
    sqlx::query(
        "insert into knowledge_structured_revision (
            revision_id, workspace_id, library_id, document_id,
            preparation_state, normalization_profile, source_format, language_code,
            block_count, chunk_count, typed_fact_count, outline_json,
            prepared_at, updated_at
         ) values (
            $1, $2, $3, $4, 'prepared', 'fixture-v1', 'text', null,
            0, 1, 0, '[]'::jsonb, now(), now()
         )",
    )
    .bind(revision.id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(document.id)
    .execute(pool)
    .await?;
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
    .execute(pool)
    .await?;

    Ok(PublicationContentIds {
        document_id: document.id,
        revision_id: revision.id,
        revision_number: revision.revision_number,
        mutation_id: mutation.id,
        mutation_item_id: item.id,
        chunk_id: chunk.id,
    })
}

/// Seeds the chunk vector and returns the embedding profile key it records.
async fn seed_publication_embedding(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    revision_number: i32,
) -> Result<String> {
    let embedding_profile_key = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
    PgSearchStore { pool: pool.clone() }
        .upsert_chunk_vector(&KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id,
            library_id,
            chunk_id,
            revision_id,
            embedding_model_key: embedding_profile_key.clone(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: vec![0.1, 0.2, 0.3],
            freshness_generation: i64::from(revision_number),
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        })
        .await?;
    Ok(embedding_profile_key)
}

/// The content ids the queue seeding links together.
struct PublicationQueueSeed {
    document_id: Uuid,
    revision_id: Uuid,
    mutation_id: Uuid,
    mutation_item_id: Uuid,
}

/// Seeds the async operation, the optional web-run row, and the leased job
/// with its first attempt, returning their ids.
async fn seed_publication_queue(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    web_owned: bool,
    seed: PublicationQueueSeed,
) -> Result<(Uuid, Uuid, Uuid)> {
    let operation = ops_repository::create_async_operation(
        pool,
        &ops_repository::NewOpsAsyncOperation {
            workspace_id,
            library_id: Some(library_id),
            operation_kind: "content_mutation",
            surface_kind: "internal",
            requested_by_principal_id: None,
            status: "processing",
            subject_kind: "content_mutation",
            subject_id: Some(seed.mutation_id),
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
        .bind(seed.mutation_id)
        .bind(operation.id)
        .bind(workspace_id)
        .bind(library_id)
        .execute(pool)
        .await?;
    }
    let job = ingest_repository::create_ingest_job(
        pool,
        &ingest_repository::NewIngestJob {
            workspace_id,
            library_id,
            mutation_id: Some(seed.mutation_id),
            mutation_item_id: Some(seed.mutation_item_id),
            connector_id: None,
            async_operation_id: Some(operation.id),
            knowledge_document_id: Some(seed.document_id),
            knowledge_revision_id: Some(seed.revision_id),
            job_kind: "content_mutation".to_string(),
            queue_state: "leased".to_string(),
            priority: 10,
            dedupe_key: Some(format!("publication:{mutation_id}", mutation_id = seed.mutation_id)),
            queued_at: Some(Utc::now()),
            available_at: Some(Utc::now()),
            completed_at: None,
        },
    )
    .await?;
    let attempt = ingest_repository::create_ingest_attempt(
        pool,
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
    Ok((operation.id, job.id, attempt.id))
}

async fn assert_content_publication_remains_unapplied(
    fixture: &PublicationFixture,
    expected_preparation_state: &str,
) -> Result<()> {
    assert_unpublished_head_and_lifecycle(fixture, expected_preparation_state).await?;
    assert_unpublished_attempt_job_and_outbox(fixture).await?;
    Ok(())
}

/// The document head and the joined revision/item/mutation/operation/structured
/// lifecycle rows must still reflect an unapplied publication.
async fn assert_unpublished_head_and_lifecycle(
    fixture: &PublicationFixture,
    expected_preparation_state: &str,
) -> Result<()> {
    let head =
        content_document_head_repository::get_document_head(&fixture.pool, fixture.document_id)
            .await?
            .context("unpublished head missing")?;
    assert_eq!(head.active_revision_id, None);
    assert_eq!(head.readable_revision_id, None);
    assert_eq!(head.latest_successful_attempt_id, None);
    assert_eq!(head.latest_mutation_id, Some(fixture.mutation_id));

    let lifecycle = sqlx::query_as::<_, (String, String, String, String, String, String)>(
        "select
            revision.vector_state,
            revision.graph_state,
            item.item_state::text,
            mutation.mutation_state::text,
            operation.status::text,
            structured.preparation_state
         from knowledge_revision as revision
         join content_mutation_item as item on item.id = $2
         join content_mutation as mutation on mutation.id = item.mutation_id
         join ops_async_operation as operation on operation.id = $3
         join knowledge_structured_revision as structured
           on structured.revision_id = revision.revision_id
         where revision.revision_id = $1",
    )
    .bind(fixture.revision_id)
    .bind(fixture.mutation_item_id)
    .bind(fixture.async_operation_id)
    .fetch_one(&fixture.pool)
    .await?;
    assert_eq!(
        lifecycle,
        (
            "processing".into(),
            "processing".into(),
            "pending".into(),
            "running".into(),
            "processing".into(),
            expected_preparation_state.into(),
        )
    );
    Ok(())
}

/// The lease, queue state and revision-ready outbox must still reflect an
/// unapplied publication.
async fn assert_unpublished_attempt_job_and_outbox(fixture: &PublicationFixture) -> Result<()> {
    let attempt = ingest_repository::get_ingest_attempt_by_id(&fixture.pool, fixture.attempt_id)
        .await?
        .context("unpublished attempt missing")?;
    let job = ingest_repository::get_ingest_job_by_id(&fixture.pool, fixture.job_id)
        .await?
        .context("unpublished job missing")?;
    assert_eq!(attempt.attempt_state, "leased");
    assert!(attempt.finished_at.is_none());
    assert_eq!(job.queue_state, "leased");
    assert!(job.completed_at.is_none());
    let outbox_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from webhook_lifecycle_outbox
         where event_id = $1",
    )
    .bind(ironrag_backend::domains::webhook::revision_ready_event_id(fixture.revision_id))
    .fetch_one(&fixture.pool)
    .await?;
    assert_eq!(outbox_count, 0);
    Ok(())
}

/// Drives the canonical server-declared content rejection the terminal
/// disposition tests start from: the fixture's active attempt fails with
/// `content_unsupported`, and the operation must publish exactly that
/// disposition. The caller owns the fixture, so its cleanup still runs when
/// either this setup or a later assertion fails.
async fn fail_active_attempt_with_content_unsupported(fixture: &PublicationFixture) -> Result<()> {
    let outcome = ingest_repository::fail_content_ingest_attempt(
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
            failure_class: Some("content_extract".to_string()),
            failure_code: Some("upload_extraction_failed".to_string()),
            failure_disposition: Some(OpsFailureDisposition::ContentUnsupported),
            failure_message: Some("server rejected the uploaded content".to_string()),
            retryable: false,
            operational_defer: None,
            delete_vectors: false,
            failed_at: Utc::now(),
        },
    )
    .await?;
    assert!(matches!(
        outcome,
        ingest_repository::FailContentIngestAttemptOutcome::Applied { retry_scheduled: false, .. }
    ));
    let operation =
        ops_repository::get_async_operation_by_id(&fixture.pool, fixture.async_operation_id)
            .await?
            .context("content-rejection operation missing")?;
    assert_eq!(operation.status, "failed");
    assert_eq!(
        operation.failure_disposition.as_deref(),
        Some("content_unsupported"),
        "a server-declared content rejection must publish content_unsupported"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn unrelated_library_source_advance_preserves_revision_vector_write_and_publication()
-> Result<()> {
    let fixture = PublicationFixture::create("text_readable", false).await?;
    let result = async {
        let captured_source_version = fixture.source_truth_version().await?;
        let unrelated_source_version = catalog_repository::touch_library_source_truth_version(
            &fixture.pool,
            fixture.library_id,
        )
        .await?;

        let active_profile_source =
            FixedEmbeddingProfileSource(fixture.embedding_profile_key.clone());
        let observed_source_version = PgSearchStore { pool: fixture.pool.clone() }
            .upsert_ingest_chunk_vectors_bulk_fenced(
                &[fixture.vector_row()],
                &VectorWriteFence {
                    expected_source_truth_version: captured_source_version,
                    embedding_profile_key: fixture.embedding_profile_key.clone(),
                    ingest_attempt: Some(IngestVectorWriteFence {
                        attempt_id: fixture.attempt_id,
                        revision_id: fixture.revision_id,
                    }),
                    advance_source_truth_version: false,
                },
                &active_profile_source,
            )
            .await?;
        assert_eq!(observed_source_version, unrelated_source_version);

        let completed_at = Utc::now();
        let publication = ingest_repository::publish_content_ingest_success_revision_fenced(
            &fixture.pool,
            &fixture.publication_command(
                captured_source_version,
                completed_at,
                "ready",
                Some(completed_at),
            ),
            &active_profile_source,
        )
        .await?;
        assert!(matches!(
            publication,
            ingest_repository::PublishContentIngestSuccessOutcome::Applied { .. }
        ));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn advancing_vector_write_keeps_compare_and_advance_source_semantics() -> Result<()> {
    let fixture = PublicationFixture::create("text_readable", false).await?;
    let result = async {
        let source_before = fixture.source_truth_version().await?;
        let store = PgSearchStore { pool: fixture.pool.clone() };
        let fence = VectorWriteFence {
            expected_source_truth_version: source_before,
            embedding_profile_key: fixture.embedding_profile_key.clone(),
            ingest_attempt: None,
            advance_source_truth_version: true,
        };
        let source_after =
            store.upsert_chunk_vectors_bulk_fenced(&[fixture.vector_row()], &fence).await?;

        assert!(source_after > source_before);
        assert_eq!(fixture.source_truth_version().await?, source_after);
        assert!(
            store.upsert_chunk_vectors_bulk_fenced(&[fixture.vector_row()], &fence).await.is_err(),
            "the compare-and-advance flow must reject a stale source fence",
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn successful_publication_commits_every_visible_surface_once() -> Result<()> {
    for graph_state in ["ready", "graph_degraded"] {
        assert_successful_publication(graph_state).await?;
    }
    Ok(())
}

async fn accept_publication_intent(fixture: &PublicationFixture, key: &str) -> Result<Uuid> {
    use ironrag_backend::infra::repositories::admission_repository::{
        VisibilityAdmissionRequest, admit_visibility_intent_with_failpoint,
    };
    let replacement = content_revision_repository::get_revision_replacement_identity(
        &fixture.pool,
        fixture.revision_id,
    )
    .await?
    .context("fixture immutable identity")?;
    let accepted = admit_visibility_intent_with_failpoint(
        &fixture.pool,
        &VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id: fixture.document_id,
            requested_by_principal_id: None,
            request_surface: "rest".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                idempotency_key: key.to_owned(),
                source_identity: "publication-source".to_owned(),
                materialization_token: None,
                replacement,
            },
        },
        None,
    )
    .await?
    .into_bundle();
    Ok(accepted.receipt.visibility_intent_id)
}

async fn admit_bound_publication_successor(
    fixture: &mut PublicationFixture,
    intent: Uuid,
) -> Result<()> {
    use ironrag_backend::infra::repositories::admission_repository::{
        ContentAdmissionRequest, ContentAdmissionTarget, RevisionAdmission,
        admit_content_with_failpoint,
    };
    let replacement = content_revision_repository::get_revision_replacement_identity(
        &fixture.pool,
        fixture.revision_id,
    )
    .await?
    .context("replacement")?;
    let admitted = admit_content_with_failpoint(
        &fixture.pool,
        &ContentAdmissionRequest {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            operation_kind: "replace".to_owned(),
            requested_by_principal_id: None,
            request_surface: "internal".to_owned(),
            idempotency_key: Some("bound-successor".to_owned()),
            source_identity: Some("publication-source".to_owned()),
            target: ContentAdmissionTarget::Existing { document_id: fixture.document_id },
            revision: Some(RevisionAdmission {
                content_source_kind: "replace".to_owned(),
                replacement,
                visibility_intent_id: Some(intent),
                base_revision_id: None,
                storage_key: Some("memory://bound".to_owned()),
            }),
            parent_async_operation_id: None,
            priority: 10,
        },
        None,
    )
    .await?
    .into_bundle();
    let revision = admitted.revision.context("admitted revision")?;
    let item = admitted.item.context("admitted item")?;
    let job = admitted.job.context("admitted job")?;
    sqlx::query("update ingest_job set queue_state='leased' where id=$1")
        .bind(job.id)
        .execute(&fixture.pool)
        .await?;
    let attempt = ingest_repository::create_ingest_attempt(
        &fixture.pool,
        &ingest_repository::NewIngestAttempt {
            job_id: job.id,
            attempt_number: 1,
            worker_principal_id: None,
            lease_token: Some("bound-attempt".to_owned()),
            knowledge_generation_id: None,
            attempt_state: "leased".to_owned(),
            current_stage: Some("extract_content".to_owned()),
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
    let chunks = content_chunk_repository::replace_chunks_with_projection(
        &fixture.pool,
        revision.id,
        &[crate::knowledge_seed::ready_paragraph_projection(
            crate::knowledge_seed::ReadyChunkSeed {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: fixture.document_id,
                revision_id: revision.id,
                chunk_index: 0,
                text: "bound evidence",
            },
            20,
            3,
            "sha256:bound-chunk",
            i64::from(revision.revision_number),
        )],
    )
    .await?;
    let chunk = chunks.first().context("bound chunk")?;
    sqlx::query("insert into knowledge_structured_revision (revision_id,workspace_id,library_id,document_id,preparation_state,normalization_profile,source_format,block_count,chunk_count,typed_fact_count,outline_json,prepared_at,updated_at)
        values ($1,$2,$3,$4,'prepared','fixture-v1','text',0,1,0,'[]',now(),now())")
        .bind(revision.id).bind(fixture.workspace_id).bind(fixture.library_id).bind(fixture.document_id).execute(&fixture.pool).await?;
    sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now(),vector_state='processing',graph_state='processing' where revision_id=$1")
        .bind(revision.id).execute(&fixture.pool).await?;
    fixture.embedding_profile_key = seed_publication_embedding(
        &fixture.pool,
        fixture.workspace_id,
        fixture.library_id,
        chunk.id,
        revision.id,
        revision.revision_number,
    )
    .await?;
    fixture.revision_id = revision.id;
    fixture.mutation_id = admitted.mutation.id;
    fixture.mutation_item_id = item.id;
    fixture.job_id = job.id;
    fixture.attempt_id = attempt.id;
    fixture.async_operation_id = admitted.async_operation.id;
    fixture.chunk_id = chunk.id;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn visibility_contract_bound_ready_retains_intent_and_superseded_ready_is_invisible()
-> Result<()> {
    for scenario in ["bound", "superseded", "mismatch"] {
        let mut fixture = PublicationFixture::create("text_readable", false).await?;
        let result: Result<()> = Box::pin(async {
            let intent = accept_publication_intent(&fixture, "bound-denial").await?;
            let now = Utc::now();
            ingest_repository::publish_content_ingest_success_revision_fenced(
                &fixture.pool,
                &fixture.publication_command(
                    fixture.source_truth_version().await?,
                    now,
                    "ready",
                    Some(now),
                ),
                &FixedEmbeddingProfileSource(fixture.embedding_profile_key.clone()),
            )
            .await?;
            admit_bound_publication_successor(&mut fixture, intent).await?;
            // This label must not be used as either revision's semantic filename.
            sqlx::query(
                "update content_document set source_file_name='unrelated-label.txt' where id=$1",
            )
            .bind(fixture.document_id)
            .execute(&fixture.pool)
            .await?;
            let current_intent = if scenario == "superseded" {
                accept_publication_intent(&fixture, "newer-denial").await?
            } else {
                intent
            };
            if scenario == "mismatch" {
                sqlx::query(
                    "update content_revision set document_hint='changed after admission' where id=$1",
                )
                .bind(fixture.revision_id)
                .execute(&fixture.pool)
                .await?;
            }
            let now = Utc::now();
            let outcome = ingest_repository::publish_content_ingest_success_revision_fenced(
                &fixture.pool,
                &fixture.publication_command(
                    fixture.source_truth_version().await?,
                    now,
                    "ready",
                    Some(now),
                ),
                &FixedEmbeddingProfileSource(fixture.embedding_profile_key.clone()),
            )
            .await?;
            assert!(matches!(
                outcome,
                ingest_repository::PublishContentIngestSuccessOutcome::Applied {
                    mutation_completed: true,
                    ..
                }
            ));
            let head = content_document_head_repository::get_document_head(
                &fixture.pool,
                fixture.document_id,
            )
            .await?
            .context("head")?;
            assert_eq!(head.visibility_intent_id, Some(current_intent));
            let visible: Option<Uuid> = sqlx::query_scalar(
                "select revision_id from content_current_revision where document_id=$1",
            )
            .bind(fixture.document_id)
            .fetch_optional(&fixture.pool)
            .await?;
            if scenario == "bound" {
                assert_eq!(head.readable_revision_id, Some(fixture.revision_id));
                assert_eq!(visible, Some(fixture.revision_id));
            } else {
                assert_eq!(head.readable_revision_id, None);
                assert_eq!(visible, None);
            }
            assert_eq!(
                ingest_repository::get_ingest_job_by_id(&fixture.pool, fixture.job_id)
                    .await?
                    .context("job")?
                    .queue_state,
                "completed"
            );
            assert_eq!(
                ingest_repository::get_ingest_attempt_by_id(&fixture.pool, fixture.attempt_id)
                    .await?
                    .context("attempt")?
                    .attempt_state,
                "succeeded"
            );
            Ok(())
        })
        .await;
        fixture.cleanup().await?;
        result?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn visibility_contract_foreign_ready_completes_without_exposure() -> Result<()> {
    let fixture = PublicationFixture::create("text_readable", false).await?;
    let result=Box::pin(async {
        let expected=fixture.source_truth_version().await?;
        let intent=accept_publication_intent(&fixture,"foreign-denial").await?;
        let denied_head=content_document_head_repository::get_document_head(&fixture.pool,fixture.document_id).await?.context("denied head")?;
        let now=Utc::now();
        let command=fixture.publication_command(expected,now,"ready",Some(now));
        let outcome=ingest_repository::publish_content_ingest_success_revision_fenced_with_effects(
            &fixture.pool,&command,&ingest_repository::ContentIngestPublicationEffects {
                document_summary:Some("restricted B summary must not escape".to_owned()),terminal_stage_events:Vec::new(),
            },&FixedEmbeddingProfileSource(fixture.embedding_profile_key.clone()),
        ).await?;
        assert!(matches!(outcome,ingest_repository::PublishContentIngestSuccessOutcome::Applied {mutation_completed:true,..}));
        let head=content_document_head_repository::get_document_head(&fixture.pool,fixture.document_id).await?.context("head")?;
        assert_eq!(head.visibility_intent_id,Some(intent));
        assert_eq!(head.readable_revision_id,None);
        assert_eq!(head.active_revision_id,None);
        assert_eq!(head.document_summary,denied_head.document_summary);
        let projected:(Option<Uuid>,Option<Uuid>)=sqlx::query_as("select active_revision_id,readable_revision_id from knowledge_document where document_id=$1")
            .bind(fixture.document_id).fetch_one(&fixture.pool).await?;
        assert_eq!(projected,(None,None));
        let visible:Option<Uuid>=sqlx::query_scalar("select revision_id from content_current_revision where document_id=$1")
            .bind(fixture.document_id).fetch_optional(&fixture.pool).await?;
        assert_eq!(visible,None);
        let job=ingest_repository::get_ingest_job_by_id(&fixture.pool,fixture.job_id).await?.context("job")?;
        let attempt=ingest_repository::get_ingest_attempt_by_id(&fixture.pool,fixture.attempt_id).await?.context("attempt")?;
        assert_eq!(job.queue_state,"completed"); assert_eq!(attempt.attempt_state,"succeeded");
        let state:(String,String,String)=sqlx::query_as("select m.mutation_state::text,i.item_state::text,o.status::text from content_mutation m join content_mutation_item i on i.mutation_id=m.id join ops_async_operation o on o.id=$3 where m.id=$1 and i.id=$2")
            .bind(fixture.mutation_id).bind(fixture.mutation_item_id).bind(fixture.async_operation_id).fetch_one(&fixture.pool).await?;
        assert_eq!(state,("applied".to_owned(),"applied".to_owned(),"ready".to_owned()));
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn visibility_contract_intent_after_publication_lock_denies_published_revision() -> Result<()>
{
    let fixture = PublicationFixture::create("text_readable", false).await?;
    let result = Box::pin(async {
        let profile = std::sync::Arc::new(PausedPublicationProfile {
            profile: fixture.embedding_profile_key.clone(),
            entered: tokio::sync::Notify::new(),
            released: tokio::sync::Notify::new(),
        });
        let pool = fixture.pool.clone();
        let worker_profile = profile.clone();
        let now = Utc::now();
        let command = fixture.publication_command(
            fixture.source_truth_version().await?,
            now,
            "ready",
            Some(now),
        );
        let publication = tokio::spawn(async move {
            ingest_repository::publish_content_ingest_success_revision_fenced(
                &pool,
                &command,
                worker_profile.as_ref(),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(20), profile.entered.notified())
            .await?;
        // READY owns the library lock. The independent admission cannot overtake it.
        let denial = accept_publication_intent(&fixture, "after-lock-denial");
        let release = async {
            profile.released.notify_one();
            publication.await?
        };
        let (intent, publication) = tokio::join!(denial, release);
        let intent = intent?;
        assert!(matches!(
            publication?,
            ingest_repository::PublishContentIngestSuccessOutcome::Applied { .. }
        ));
        let head =
            content_document_head_repository::get_document_head(&fixture.pool, fixture.document_id)
                .await?
                .context("head")?;
        assert_eq!(head.visibility_intent_id, Some(intent));
        assert_eq!(head.readable_revision_id, None);
        assert_eq!(
            ingest_repository::get_ingest_job_by_id(&fixture.pool, fixture.job_id)
                .await?
                .context("job")?
                .queue_state,
            "completed"
        );
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn ready_publication_retains_intent_and_refuses_unowned_null_head_repair() -> Result<()> {
    let mut fixture = PublicationFixture::create("text_readable", false).await?;
    let result = Box::pin(async {
        let intent=accept_publication_intent(&fixture,"repair-denial").await?;
        sqlx::query("update content_document_head set active_revision_id = null, readable_revision_id = null where document_id = $1")
            .bind(fixture.document_id).execute(&fixture.pool).await?;
        let repair = ironrag_backend::infra::repositories::content_knowledge_projection_repository::repair_null_document_head_with_projection;
        assert!(!repair(&fixture.pool, fixture.document_id).await?, "pending suppression must refuse repair");
        let now = Utc::now();
        let outcome = ingest_repository::publish_content_ingest_success(
            &fixture.pool,
            &fixture.publication_command(fixture.source_truth_version().await?, now, "ready", Some(now)),
        ).await?;
        assert!(matches!(outcome, ingest_repository::PublishContentIngestSuccessOutcome::Applied { .. }));
        admit_bound_publication_successor(&mut fixture,intent).await?;
        let now=Utc::now();
        ingest_repository::publish_content_ingest_success(&fixture.pool,
            &fixture.publication_command(fixture.source_truth_version().await?,now,"ready",Some(now))).await?;
        let published_generation = fixture.source_truth_version().await?;
        let reconcile = content_knowledge_projection_repository::reconcile_failed_document_head_with_projection;
        // Neither an older failed owner nor a late failure notification for the
        // current owner can replay pre-READY pointers or discard its successful
        // attempt. Reconciliation now has no snapshot-pointer input.
        for failed_owner in [Uuid::now_v7(), fixture.mutation_id] {
            reconcile(&fixture.pool, fixture.document_id, failed_owner).await?;
            let current = content_document_head_repository::get_document_head(&fixture.pool, fixture.document_id)
                .await?.context("published head missing after reconciliation")?;
            assert_eq!(current.active_revision_id, Some(fixture.revision_id));
            assert_eq!(current.readable_revision_id, Some(fixture.revision_id));
            assert_eq!(current.latest_mutation_id, Some(fixture.mutation_id));
            assert_eq!(current.latest_successful_attempt_id, Some(fixture.attempt_id));
            assert_eq!(fixture.source_truth_version().await?, published_generation);
        }
        // A retained obligation cannot be discharged by an unowned repair candidate.
        sqlx::query("update content_document_head set active_revision_id = null, readable_revision_id = null where document_id = $1")
            .bind(fixture.document_id).execute(&fixture.pool).await?;
        assert!(!repair(&fixture.pool, fixture.document_id).await?, "retained intent must refuse unowned repair");
        let head = content_document_head_repository::get_document_head(&fixture.pool, fixture.document_id).await?.context("repaired head missing")?;
        assert_eq!(head.readable_revision_id, None);
        assert_eq!(head.visibility_intent_id,Some(intent));
        assert_eq!(head.latest_mutation_id, Some(fixture.mutation_id));
        assert_eq!(head.latest_successful_attempt_id, Some(fixture.attempt_id));
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

async fn assert_successful_publication(graph_state: &str) -> Result<()> {
    let fixture = PublicationFixture::create("text_readable", false).await?;
    let result = async {
        let source_before = fixture.source_truth_version().await?;
        let completed_at = Utc::now();
        let graph_ready_at = (graph_state == "ready").then_some(completed_at);
        let outcome = ingest_repository::publish_content_ingest_success(
            &fixture.pool,
            &fixture.publication_command(source_before, completed_at, graph_state, graph_ready_at),
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

        let head =
            content_document_head_repository::get_document_head(&fixture.pool, fixture.document_id)
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
            (
                "ready".into(),
                graph_state.into(),
                "applied".into(),
                "applied".into(),
                "ready".into()
            )
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
        let expected_code = (graph_state == "graph_degraded").then_some(graph_state);
        assert_eq!(attempt.failure_code.as_deref(), expected_code);
        assert_eq!(attempt.failure_class, None);
        assert!(!attempt.retryable);
        let wire = fixture.attempt_wire(fixture.attempt_id).await?;
        assert_eq!(wire["failure_code"], serde_json::to_value(expected_code)?);
        assert!(wire.get("details_json").is_none());
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
#[ignore = "requires local postgres with the pgvector extensions"]
async fn content_publication_rejects_incomplete_structured_artifacts_atomically() -> Result<()> {
    let fixture = PublicationFixture::create("text_readable", false).await?;
    let result = async {
        let source_before = fixture.source_truth_version().await?;
        let command =
            fixture.publication_command(source_before, Utc::now(), "ready", Some(Utc::now()));
        sqlx::query(
            "update knowledge_structured_revision
             set preparation_state = 'preparing'
             where revision_id = $1",
        )
        .bind(fixture.revision_id)
        .execute(&fixture.pool)
        .await?;
        assert!(
            ingest_repository::publish_content_ingest_success(&fixture.pool, &command)
                .await
                .is_err(),
            "publication must reject a preparing structured artifact",
        );
        assert_eq!(fixture.source_truth_version().await?, source_before);
        assert_content_publication_remains_unapplied(&fixture, "preparing").await?;

        sqlx::query(
            "update knowledge_structured_revision
             set preparation_state = 'prepared', chunk_count = chunk_count + 1
             where revision_id = $1",
        )
        .bind(fixture.revision_id)
        .execute(&fixture.pool)
        .await?;
        assert!(
            ingest_repository::publish_content_ingest_success(&fixture.pool, &command)
                .await
                .is_err(),
            "publication must reject structured metadata whose family counts do not match",
        );
        assert_eq!(fixture.source_truth_version().await?, source_before);
        assert_content_publication_remains_unapplied(&fixture, "prepared").await?;
        assert_eq!(
            sqlx::query_scalar::<_, i32>(
                "select chunk_count::int4
                 from knowledge_structured_revision
                 where revision_id = $1",
            )
            .bind(fixture.revision_id)
            .fetch_one(&fixture.pool)
            .await?,
            2,
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
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
#[ignore = "requires local postgres with the pgvector extensions"]
async fn graph_refresh_publishes_only_current_head_and_settles_superseded_work() -> Result<()> {
    for graph_state in ["ready", "graph_degraded"] {
        assert_graph_refresh_publication(graph_state).await?;
    }
    Ok(())
}

async fn assert_graph_refresh_publication(graph_state: &str) -> Result<()> {
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
        let head_before =
            content_document_head_repository::get_document_head(&fixture.pool, fixture.document_id)
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
                graph_state: graph_state.to_string(),
                graph_ready_at: (graph_state == "ready").then_some(graph_completed_at),
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
        let head_after =
            content_document_head_repository::get_document_head(&fixture.pool, fixture.document_id)
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
            graph_state
        );
        let wire = fixture.attempt_wire(graph_attempt.id).await?;
        assert_eq!(wire["attempt_state"], "succeeded");
        assert_eq!(wire["retryable"], false);
        assert_eq!(wire["failure_class"], serde_json::Value::Null);
        assert_eq!(
            wire["failure_code"],
            serde_json::to_value((graph_state == "graph_degraded").then_some(graph_state))?
        );
        assert!(wire.get("details_json").is_none());
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
#[ignore = "requires local postgres with the pgvector extensions"]
async fn failure_publication_retries_then_terminally_fails_without_inventing_text_readiness()
-> Result<()> {
    let fixture = PublicationFixture::create("accepted", false).await?;
    let result = async {
        sqlx::query("update knowledge_structured_revision set preparation_state = 'preparing' where revision_id = $1")
            .bind(fixture.revision_id).execute(&fixture.pool).await?;
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
                failure_disposition: Some(OpsFailureDisposition::RetryExhausted),
                failure_message: Some("temporary provider failure".to_string()),
                retryable: true,
                operational_defer: None,
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
        let preparation: String = sqlx::query_scalar(
            "select preparation_state from knowledge_structured_revision where revision_id = $1",
        ).bind(fixture.revision_id).fetch_one(&fixture.pool).await?;
        assert_eq!(preparation, "preparing", "a queued retry is not terminal");
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
        // A scheduled retry is not a terminal proof: the operation must stay
        // without a disposition even though the caller supplied one.
        let retry_operation =
            ops_repository::get_async_operation_by_id(&fixture.pool, fixture.async_operation_id)
                .await?
                .context("retry operation missing")?;
        assert_eq!(retry_operation.failure_disposition, None);
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
                failure_disposition: Some(OpsFailureDisposition::RetryExhausted),
                failure_message: Some("persistent provider failure".to_string()),
                retryable: true,
                operational_defer: None,
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
        // The exhausted retryable failure is the one path that writes
        // `retry_exhausted`, and it now carries it on the operation.
        let terminal_operation =
            ops_repository::get_async_operation_by_id(&fixture.pool, fixture.async_operation_id)
                .await?
                .context("terminal operation missing")?;
        assert_eq!(
            terminal_operation.failure_disposition.as_deref(),
            Some("retry_exhausted"),
            "an exhausted retryable failure must publish retry_exhausted"
        );
        assert_eq!(terminal_operation.failure_code.as_deref(), Some("exhausted"));
        let preparation: String = sqlx::query_scalar(
            "select preparation_state from knowledge_structured_revision where revision_id = $1",
        ).bind(fixture.revision_id).fetch_one(&fixture.pool).await?;
        assert_eq!(preparation, "failed", "terminal failure must close preparation");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn reconcile_terminal_failed_preparations_leaves_live_and_readable_revisions_untouched()
-> Result<()> {
    let fixture = PublicationFixture::create("accepted", false).await?;
    let result = async {
        sqlx::query("update knowledge_structured_revision set preparation_state = 'preparing' where revision_id = $1")
            .bind(fixture.revision_id).execute(&fixture.pool).await?;
        sqlx::query("update ingest_job set queue_state = 'failed', completed_at = now() where id = $1")
            .bind(fixture.job_id).execute(&fixture.pool).await?;
        let changed = ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?;
        assert_eq!(changed, 1);
        let state: String = sqlx::query_scalar("select preparation_state from knowledge_structured_revision where revision_id = $1")
            .bind(fixture.revision_id).fetch_one(&fixture.pool).await?;
        assert_eq!(state, "failed");
        assert_eq!(ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?, 0);

        // A live job cannot terminalize its revision even after an older job failed.
        sqlx::query("update knowledge_structured_revision set preparation_state = 'preparing' where revision_id = $1")
            .bind(fixture.revision_id).execute(&fixture.pool).await?;
        let (queued_mutation_id, queued_item_id) = fixture
            .create_mutation_item_for_revision(Some(fixture.revision_id))
            .await?;
        let queued = ingest_repository::create_ingest_job(
            &fixture.pool,
            &ingest_repository::NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: Some(queued_mutation_id),
                mutation_item_id: Some(queued_item_id),
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(fixture.document_id),
                knowledge_revision_id: Some(fixture.revision_id),
                job_kind: "content_mutation".to_string(),
                queue_state: "queued".to_string(),
                priority: 10,
                dedupe_key: None,
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        ).await?;
        assert_eq!(ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?, 0);
        sqlx::query("update ingest_job set queue_state = 'leased' where id = $1")
            .bind(queued.id).execute(&fixture.pool).await?;
        assert_eq!(ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?, 0);
        sqlx::query("update ingest_job set queue_state = 'failed', completed_at = now() where id = $1")
            .bind(queued.id).execute(&fixture.pool).await?;

        sqlx::query("update knowledge_document set active_revision_id = $2 where document_id = $1")
            .bind(fixture.document_id).bind(fixture.revision_id).execute(&fixture.pool).await?;
        assert_eq!(ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?, 0);
        sqlx::query("update knowledge_document set active_revision_id = null where document_id = $1")
            .bind(fixture.document_id).execute(&fixture.pool).await?;

        // A readable head is authoritative even if historical job state is failed.
        sqlx::query("update knowledge_document set readable_revision_id = $2 where document_id = $1")
            .bind(fixture.document_id).bind(fixture.revision_id).execute(&fixture.pool).await?;
        assert_eq!(ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?, 0);
        let state: String = sqlx::query_scalar("select preparation_state from knowledge_structured_revision where revision_id = $1")
            .bind(fixture.revision_id).fetch_one(&fixture.pool).await?;
        assert_eq!(state, "preparing");
        sqlx::query("update knowledge_document set readable_revision_id = null where document_id = $1")
            .bind(fixture.document_id).execute(&fixture.pool).await?;
        assert_eq!(ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?, 1);
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn reconcile_terminal_failed_preparations_drains_bounded_existing_backlog() -> Result<()> {
    let fixture = PublicationFixture::create("accepted", false).await?;
    let result = async {
        // Reproduce a historical backlog without routing through live publication.
        // These are derived projection rows: the source plane is intentionally untouched.
        let revisions = sqlx::query_scalar::<_, Uuid>(
            "insert into knowledge_revision (
                revision_id, workspace_id, library_id, document_id, revision_number,
                revision_state, revision_kind, storage_ref, source_uri, document_hint,
                mime_type, checksum, title, byte_size, normalized_text, text_checksum,
                image_checksum, text_state, vector_state, graph_state, text_readable_at,
                vector_ready_at, graph_ready_at, superseded_by_revision_id, created_at
             ) select gen_random_uuid(), workspace_id, library_id, document_id,
                      revision_number + n, revision_state, revision_kind, storage_ref,
                      source_uri, document_hint, mime_type, checksum, title, byte_size,
                      normalized_text, text_checksum, image_checksum, text_state,
                      vector_state, graph_state, text_readable_at, vector_ready_at,
                      graph_ready_at, null, now()
               from knowledge_revision cross join generate_series(1, 257) n
              where revision_id = $1
              returning revision_id",
        )
        .bind(fixture.revision_id)
        .fetch_all(&fixture.pool)
        .await?;
        assert_eq!(revisions.len(), 257);
        sqlx::query(
            "insert into knowledge_structured_revision (
                revision_id, workspace_id, library_id, document_id, preparation_state,
                normalization_profile, source_format, language_code, block_count, chunk_count,
                typed_fact_count, outline_json, prepared_at, updated_at
             ) select revision_id, workspace_id, library_id, document_id, 'preparing',
                      'fixture-v1', 'text', null, 0, 0, 0, '[]'::jsonb, now(), now()
               from knowledge_revision where revision_id = any($1)",
        )
        .bind(&revisions)
        .execute(&fixture.pool)
        .await?;
        for revision_id in &revisions {
            // Synthetic knowledge revisions have no source content_revision row.
            let (mutation_id, mutation_item_id) =
                fixture.create_mutation_item_for_revision(None).await?;
            ingest_repository::create_ingest_job(
                &fixture.pool,
                &ingest_repository::NewIngestJob {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    mutation_id: Some(mutation_id),
                    mutation_item_id: Some(mutation_item_id),
                    connector_id: None,
                    async_operation_id: None,
                    knowledge_document_id: Some(fixture.document_id),
                    knowledge_revision_id: Some(*revision_id),
                    job_kind: "content_mutation".to_string(),
                    queue_state: "failed".to_string(),
                    priority: 10,
                    dedupe_key: None,
                    queued_at: Some(Utc::now()),
                    available_at: Some(Utc::now()),
                    completed_at: Some(Utc::now()),
                },
            )
            .await?;
        }
        assert_eq!(
            ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?,
            256
        );
        assert_eq!(
            ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?,
            1
        );
        assert_eq!(
            ingest_repository::reconcile_terminal_failed_preparations(&fixture.pool).await?,
            0
        );
        let failed: i64 = sqlx::query_scalar(
            "select count(*) from knowledge_structured_revision
             where revision_id = any($1) and preparation_state = 'failed'",
        )
        .bind(&revisions)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(failed, 257);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn content_rejection_publishes_content_unsupported_and_out_of_scope_stays_null() -> Result<()>
{
    let fixture = PublicationFixture::create("accepted", false).await?;
    let result = async {
        fail_active_attempt_with_content_unsupported(&fixture).await?;

        // An operation that no ingest finalisation has touched keeps the
        // honest `null`: the server holds no terminal proof for it.
        let untouched = ops_repository::create_async_operation(
            &fixture.pool,
            &ops_repository::NewOpsAsyncOperation {
                workspace_id: fixture.workspace_id,
                library_id: Some(fixture.library_id),
                operation_kind: "delete_library",
                surface_kind: "rest",
                requested_by_principal_id: None,
                status: "failed",
                subject_kind: "library",
                subject_id: Some(fixture.library_id),
                parent_async_operation_id: None,
                completed_at: Some(Utc::now()),
                failure_code: Some("library_deleting"),
            },
        )
        .await?;
        let untouched_operation =
            ops_repository::get_async_operation_by_id(&fixture.pool, untouched.id)
                .await?
                .context("out-of-scope operation missing")?;
        assert_eq!(untouched_operation.failure_disposition, None);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn settling_a_failed_operation_ready_clears_its_failure_disposition() -> Result<()> {
    let fixture = PublicationFixture::create("accepted", false).await?;
    let result = async {
        fail_active_attempt_with_content_unsupported(&fixture).await?;

        // A settle to `ready` must not keep the earlier failure's disposition:
        // the row now carries no terminal proof.
        let ready = ops_repository::update_async_operation(
            &fixture.pool,
            fixture.async_operation_id,
            &ops_repository::UpdateOpsAsyncOperation {
                status: "ready",
                completed_at: Some(Utc::now()),
                failure_code: None,
            },
        )
        .await?
        .context("ready operation missing")?;
        assert_eq!(ready.status, "ready");
        assert_eq!(ready.failure_disposition, None);
        assert_eq!(ready.failure_code, None);

        // The ingest retry reopens a settled row instead of creating a new one;
        // reopening to `processing` clears the disposition as well.
        let reopened = ops_repository::update_async_operation(
            &fixture.pool,
            fixture.async_operation_id,
            &ops_repository::UpdateOpsAsyncOperation {
                status: "processing",
                completed_at: None,
                failure_code: None,
            },
        )
        .await?
        .context("reopened operation missing")?;
        assert_eq!(reopened.status, "processing");
        assert_eq!(reopened.failure_disposition, None);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
