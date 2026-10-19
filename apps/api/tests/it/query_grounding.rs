use crate::temp_postgres_database;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::{
        agent_runtime::{
            RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeStageKind, RuntimeTaskKind,
        },
        query::{QueryExecution, QueryVerificationState},
    },
    domains::{
        audit::AuditEventSubject,
        ops::{OpsAsyncOperation, OpsAsyncOperationStatus},
    },
    infra::repositories::{
        self, iam_repository, ops_repository, query_repository, query_result_cache_repository,
        runtime_repository,
    },
    infra::{
        knowledge_plane::{BundleStateUpdate, ContextStore, DocumentStore, GraphStore},
        knowledge_rows::{
            KnowledgeBundleChunkEdgeRow, KnowledgeBundleChunkReferenceRow,
            KnowledgeBundleEntityEdgeRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleEvidenceReferenceRow,
            KnowledgeBundleRelationEdgeRow, KnowledgeBundleRelationReferenceRow, KnowledgeChunkRow,
            KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow, KnowledgeDocumentRow,
            KnowledgeRetrievalTraceRow, KnowledgeRevisionRow, KnowledgeStructuredBlockRow,
            KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow, NewKnowledgeEntity,
        },
        postgres::{
            pg_context_store::PgContextStore, pg_document_store::PgDocumentStore,
            pg_graph_store::PgGraphStore,
        },
    },
    services::query::service::QueryService,
    shared::extraction::technical_facts::TechnicalFactKind,
};

struct QueryGroundingFixture {
    temp_database: TempPostgresDatabase,
    postgres: PgPool,
    document_store: PgDocumentStore,
    context_store: PgContextStore,
    graph_store: PgGraphStore,
}

struct SeedChunk<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    content_text: &'a str,
}

struct AuthoritativeEvidenceSeed<'a> {
    query_text: &'a str,
    verification_state: &'a str,
    verification_warnings: serde_json::Value,
    chunk_ids: Vec<Uuid>,
    entity_ids: Vec<Uuid>,
    relation_ids: Vec<Uuid>,
    structured_blocks: Vec<KnowledgeStructuredBlockRow>,
    technical_facts: Vec<KnowledgeTechnicalFactRow>,
}

struct AuthoritativeBundleReferences {
    chunk_ids: Vec<Uuid>,
    entity_ids: Vec<Uuid>,
    relation_ids: Vec<Uuid>,
    technical_facts: Vec<KnowledgeTechnicalFactRow>,
}

impl QueryGroundingFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for query grounding tests")?;
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "query_grounding_http").await?;
        settings.database_url = temp_database.database_url.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect query grounding postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply query grounding migrations")?;

        Ok(Self {
            temp_database,
            postgres: postgres.clone(),
            document_store: PgDocumentStore { pool: postgres.clone() },
            context_store: PgContextStore { pool: postgres.clone() },
            graph_store: PgGraphStore { pool: postgres.clone() },
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.postgres.close().await;
        self.temp_database.drop().await
    }

    async fn seed_chunk(&self, seed: SeedChunk<'_>) -> Result<()> {
        let SeedChunk {
            workspace_id,
            library_id,
            document_id,
            revision_id,
            chunk_id,
            content_text,
        } = seed;
        let now = Utc::now();

        self.document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: format!("grounding-{document_id}"),
                file_name: None,
                title: Some("Grounding Document".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert grounding document")?;

        self.document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some(format!("memory://grounding/{revision_id}")),
                source_uri: Some(format!("memory://grounding/source/{revision_id}")),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: format!("sha256:{0}{0}", revision_id.simple()),
                title: Some("Grounding Revision".to_string()),
                byte_size: i64::try_from(content_text.len()).unwrap_or(i64::MAX),
                normalized_text: Some(content_text.to_string()),
                text_checksum: Some(format!("text-checksum-{revision_id}")),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "accepted".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert grounding revision")?;

        self.document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_index: 0,
                chunk_kind: Some("paragraph".to_string()),
                content_text: content_text.to_string(),
                normalized_text: content_text.to_string(),
                span_start: Some(0),
                span_end: Some(i32::try_from(content_text.len()).unwrap_or(i32::MAX)),
                token_count: Some(3),
                support_block_ids: Vec::new(),
                section_path: vec!["grounding".to_string()],
                heading_trail: vec!["Grounding".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: None,
                quality_score: None,

                window_text: None,

                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert grounding chunk")?;

        Ok(())
    }
}

async fn seed_completed_mcp_conversation(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    age_seconds: i32,
) -> Result<(Uuid, Uuid)> {
    let conversation = query_repository::create_conversation(
        postgres,
        &query_repository::NewQueryConversation {
            workspace_id,
            library_id,
            created_by_principal_id: None,
            title: Some("Transient MCP retention fixture"),
            conversation_state: "active",
            request_surface: "mcp",
        },
        64,
    )
    .await?;
    sqlx::query(
        "update query_conversation
         set created_at = now() - ($2 * interval '1 second'),
             updated_at = now() - ($2 * interval '1 second')
         where id = $1",
    )
    .bind(conversation.id)
    .bind(age_seconds)
    .execute(postgres)
    .await?;

    let execution_id = Uuid::now_v7();
    let runtime_execution_id = Uuid::now_v7();
    runtime_repository::create_runtime_execution(
        postgres,
        &runtime_repository::NewRuntimeExecution {
            id: runtime_execution_id,
            owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
            owner_id: execution_id,
            task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
            surface_kind: "mcp",
            contract_name: "query_answer",
            contract_version: "retention-test",
            lifecycle_state: RuntimeLifecycleState::Completed.as_str(),
            active_stage: None,
            turn_budget: 1,
            turn_count: 1,
            parallel_action_limit: 1,
            failure_code: None,
            failure_summary_redacted: None,
            parent_execution_id: None,
        },
    )
    .await?;
    query_repository::create_execution(
        postgres,
        &query_repository::NewQueryExecution {
            execution_id,
            context_bundle_id: Uuid::now_v7(),
            workspace_id,
            library_id,
            conversation_id: conversation.id,
            request_turn_id: None,
            response_turn_id: None,
            binding_id: None,
            runtime_execution_id,
            query_text: "Synthetic retention probe",
            failure_code: None,
        },
    )
    .await?;
    query_repository::update_execution(
        postgres,
        execution_id,
        &query_repository::UpdateQueryExecution { request_turn_id: None, response_turn_id: None },
    )
    .await?
    .context("completed MCP execution disappeared")?;
    // `age_seconds` means the whole exchange happened that long ago, runtime
    // completion included; `create_runtime_execution` stamps `now()`.
    sqlx::query(
        "update runtime_execution
         set accepted_at = now() - ($2 * interval '1 second'),
             completed_at = now() - ($2 * interval '1 second')
         where id = $1",
    )
    .bind(runtime_execution_id)
    .bind(age_seconds)
    .execute(postgres)
    .await?;

    Ok((conversation.id, execution_id))
}

struct QueryGroundingAppFixture {
    temp_postgres: TempPostgresDatabase,
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    conversation_id: Uuid,
}

impl QueryGroundingAppFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for query grounding app test")?;
        let temp_postgres =
            TempPostgresDatabase::create(&settings.database_url, "query_grounding_http").await?;
        settings.database_url = temp_postgres.database_url.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect to query grounding postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply query grounding migrations")?;
        postgres.close().await;

        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let state = AppState::new(settings.clone()).await?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("query-grounding-workspace-{suffix}"),
            "Query Grounding Workspace",
            None,
        )
        .await
        .context("failed to create query grounding workspace")?;
        let library = repositories::catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("query-grounding-library-{suffix}"),
            "Query Grounding Library",
            Some("query grounding regression fixture"),
            None,
        )
        .await
        .context("failed to create query grounding library")?;
        let conversation = query_repository::create_conversation(
            &state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: workspace.id,
                library_id: library.id,
                created_by_principal_id: None,
                title: Some("Grounding Regression"),
                conversation_state: "active",
                request_surface: "ui",
            },
            8,
        )
        .await
        .context("failed to create query grounding conversation")?;

        Ok(Self {
            temp_postgres,
            state,
            workspace_id: workspace.id,
            library_id: library.id,
            conversation_id: conversation.id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_postgres.drop().await
    }

    async fn create_execution_detail(
        &self,
        query_text: &str,
        verification_state: &str,
        verification_warnings: serde_json::Value,
    ) -> Result<ironrag_backend::domains::query::QueryExecutionDetail> {
        let request_turn = query_repository::create_turn(
            &self.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: self.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: query_text,
                execution_id: None,
            },
        )
        .await
        .context("failed to create grounding request turn")?;
        let execution_id = Uuid::now_v7();
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
                lifecycle_state: RuntimeLifecycleState::Completed.as_str(),
                active_stage: None,
                turn_budget: 4,
                turn_count: 4,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                parent_execution_id: None,
            },
        )
        .await
        .context("failed to create grounding runtime execution")?;
        let execution = query_repository::create_execution(
            &self.state.persistence.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: deterministic_context_bundle_id(execution_id),
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                conversation_id: self.conversation_id,
                request_turn_id: Some(request_turn.id),
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text,
                failure_code: None,
            },
        )
        .await
        .context("failed to create grounding execution")?;

        let mut bundle = sample_context_bundle(
            self.workspace_id,
            self.library_id,
            &map_execution_row(&execution),
        );
        bundle.bundle_state = "ready".to_string();
        bundle.verification_state = verification_state.to_string();
        bundle.verification_warnings = verification_warnings;
        bundle.assembly_diagnostics = json!({
            "question": query_text,
            "status": "ready"
        });
        self.state
            .context_store
            .upsert_bundle(&bundle)
            .await
            .context("failed to persist grounding verification bundle")?;

        QueryService::new()
            .get_execution(&self.state, execution.id)
            .await
            .map_err(|error| anyhow!("failed to load execution detail: {error}"))
    }

    async fn create_execution_detail_with_authoritative_evidence(
        &self,
        seed: AuthoritativeEvidenceSeed<'_>,
    ) -> Result<ironrag_backend::domains::query::QueryExecutionDetail> {
        let query_text = seed.query_text;
        let request_turn = query_repository::create_turn(
            &self.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: self.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: query_text,
                execution_id: None,
            },
        )
        .await
        .context("failed to create grounding request turn with authoritative evidence")?;
        let execution_id = Uuid::now_v7();
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
                lifecycle_state: RuntimeLifecycleState::Completed.as_str(),
                active_stage: None,
                turn_budget: 4,
                turn_count: 4,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                parent_execution_id: None,
            },
        )
        .await
        .context("failed to create grounded authoritative runtime execution")?;
        let execution = query_repository::create_execution(
            &self.state.persistence.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: deterministic_context_bundle_id(execution_id),
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                conversation_id: self.conversation_id,
                request_turn_id: Some(request_turn.id),
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text,
                failure_code: None,
            },
        )
        .await
        .context("failed to create grounded execution with authoritative evidence")?;

        let now = Utc::now();
        let revision_document_ids = self.seed_authoritative_document_revisions(&seed, now).await?;
        self.seed_authoritative_chunks(&seed, &revision_document_ids).await?;
        self.seed_authoritative_runtime_graph(&seed.entity_ids, &seed.relation_ids).await?;
        let AuthoritativeEvidenceSeed {
            verification_state,
            verification_warnings,
            chunk_ids,
            entity_ids,
            relation_ids,
            technical_facts,
            ..
        } = seed;
        self.persist_authoritative_bundle(
            &execution,
            query_text,
            verification_state,
            verification_warnings,
            AuthoritativeBundleReferences { chunk_ids, entity_ids, relation_ids, technical_facts },
        )
        .await?;
        QueryService::new().get_execution(&self.state, execution.id).await.map_err(|error| {
            anyhow!("failed to load execution detail with authoritative evidence: {error}")
        })
    }

    async fn seed_authoritative_document_revisions(
        &self,
        seed: &AuthoritativeEvidenceSeed<'_>,
        now: chrono::DateTime<Utc>,
    ) -> Result<std::collections::BTreeMap<Uuid, Uuid>> {
        let AuthoritativeEvidenceSeed {
            query_text,
            structured_blocks,
            technical_facts,
            chunk_ids,
            ..
        } = seed;
        let mut revision_document_ids = std::collections::BTreeMap::<Uuid, Uuid>::new();
        for block in structured_blocks {
            revision_document_ids.insert(block.revision_id, block.document_id);
        }
        for fact in technical_facts {
            revision_document_ids.insert(fact.revision_id, fact.document_id);
        }

        let mut document_revision_ids = std::collections::BTreeMap::<Uuid, Uuid>::new();
        for (revision_id, document_id) in &revision_document_ids {
            document_revision_ids.entry(*document_id).or_insert(*revision_id);
        }

        for (document_id, active_revision_id) in document_revision_ids {
            self.state
                .document_store
                .upsert_document(&KnowledgeDocumentRow {
                    document_id,
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    external_key: format!("grounding-detail-{document_id}"),
                    file_name: None,
                    title: Some("Grounding Detail Document".to_string()),
                    source_uri: None,
                    document_hint: None,
                    document_state: "active".to_string(),
                    active_revision_id: Some(active_revision_id),
                    readable_revision_id: Some(active_revision_id),
                    latest_revision_no: Some(1),
                    created_at: now,
                    updated_at: now,
                    deleted_at: None,
                    parent_document_id: None,
                    document_role: "primary".to_string(),
                })
                .await
                .context("failed to seed grounding detail document")?;
        }

        for (revision_id, document_id) in &revision_document_ids {
            let revision_blocks = structured_blocks
                .iter()
                .filter(|block| block.revision_id == *revision_id)
                .cloned()
                .collect::<Vec<_>>();
            let revision_facts = technical_facts
                .iter()
                .filter(|fact| fact.revision_id == *revision_id)
                .cloned()
                .collect::<Vec<_>>();

            self.state
                .document_store
                .upsert_revision(&KnowledgeRevisionRow {
                    revision_id: *revision_id,
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    document_id: *document_id,
                    revision_number: 1,
                    revision_state: "active".to_string(),
                    revision_kind: "upload".to_string(),
                    storage_ref: Some(format!("memory://query-grounding/{revision_id}")),
                    source_uri: Some(format!("memory://query-grounding/source/{revision_id}")),
                    document_hint: None,
                    mime_type: "text/plain".to_string(),
                    checksum: format!("sha256:{0}{0}", revision_id.simple()),
                    title: Some("Grounding Detail Revision".to_string()),
                    byte_size: 128,
                    normalized_text: Some(query_text.to_string()),
                    text_checksum: Some(format!("text-checksum-{revision_id}")),
                    image_checksum: None,
                    text_state: "text_readable".to_string(),
                    vector_state: "ready".to_string(),
                    graph_state: "ready".to_string(),
                    text_readable_at: Some(now),
                    vector_ready_at: Some(now),
                    graph_ready_at: Some(now),
                    superseded_by_revision_id: None,
                    created_at: now,
                })
                .await
                .context("failed to seed grounding detail revision")?;
            self.publish_authoritative_revision(*revision_id).await?;
            self.state
                .document_store
                .replace_structured_revision_artifact(
                    &KnowledgeStructuredRevisionRow {
                        revision_id: *revision_id,
                        workspace_id: self.workspace_id,
                        library_id: self.library_id,
                        document_id: *document_id,
                        preparation_state: "prepared".to_string(),
                        normalization_profile: "authoritative".to_string(),
                        source_format: "pdf".to_string(),
                        language_code: Some("ru".to_string()),
                        block_count: i32::try_from(revision_blocks.len()).unwrap_or(i32::MAX),
                        chunk_count: i32::try_from(chunk_ids.len()).unwrap_or(i32::MAX),
                        typed_fact_count: i32::try_from(revision_facts.len()).unwrap_or(i32::MAX),
                        outline_json: json!({
                            "headings": ["Grounding Detail"]
                        }),
                        prepared_at: now,
                        updated_at: now,
                    },
                    &revision_blocks,
                )
                .await
                .context("failed to seed structured revision artifact for grounding detail")?;
            self.state
                .document_store
                .replace_technical_facts(*revision_id, &revision_facts)
                .await
                .context("failed to seed technical facts for grounding detail")?;
        }
        Ok(revision_document_ids)
    }

    async fn publish_authoritative_revision(&self, revision_id: Uuid) -> Result<()> {
        let pg = &self.state.persistence.postgres;
        sqlx::query(
            "insert into content_document (id,workspace_id,library_id,external_key,revision_sequence)
             select document_id,workspace_id,library_id,external_key,1 from knowledge_document
             where document_id=(select document_id from knowledge_revision where revision_id=$1)
             on conflict (id) do nothing",
        ).bind(revision_id).execute(pg).await?;
        let admission = crate::content_repository::imported_knowledge_revision_admission(
            pg,
            revision_id,
            "grounding-source.txt",
        )
        .await?;
        sqlx::query(
            "insert into content_revision (id,document_id,workspace_id,library_id,revision_number,
                content_source_kind,checksum,mime_type,byte_size,title,source_uri,document_hint,admission_identity)
             select revision_id,document_id,workspace_id,library_id,revision_number::integer,
                'upload',checksum,mime_type,byte_size,title,source_uri,document_hint,$2
             from knowledge_revision where revision_id=$1",
        )
        .bind(revision_id)
        .bind(admission)
        .execute(pg)
        .await?;
        sqlx::query(
            "insert into content_document_head (document_id,active_revision_id,readable_revision_id)
             select document_id,active_revision_id,readable_revision_id from knowledge_document
             where readable_revision_id=$1
             on conflict (document_id) do update set active_revision_id=excluded.active_revision_id,
                 readable_revision_id=excluded.readable_revision_id",
        ).bind(revision_id).execute(pg).await?;
        Ok(())
    }

    async fn seed_authoritative_chunks(
        &self,
        seed: &AuthoritativeEvidenceSeed<'_>,
        revision_document_ids: &std::collections::BTreeMap<Uuid, Uuid>,
    ) -> Result<()> {
        let AuthoritativeEvidenceSeed { query_text, technical_facts, chunk_ids, .. } = seed;

        // Chunk references hydrate from `knowledge_chunk` rows keyed by the
        // bundle's chunk edges (see `load_execution_prepared_reference_context`),
        // so every referenced chunk needs a backing row. Attach each chunk to
        // the revision that cites it through a technical fact, falling back to
        // any seeded revision.
        let fallback_chunk_revision =
            revision_document_ids.iter().next().map(|(revision, document)| (*revision, *document));
        for (chunk_index, chunk_id) in chunk_ids.iter().enumerate() {
            let Some((chunk_revision_id, chunk_document_id)) = technical_facts
                .iter()
                .find(|fact| fact.support_chunk_ids.contains(chunk_id))
                .map(|fact| (fact.revision_id, fact.document_id))
                .or(fallback_chunk_revision)
            else {
                continue;
            };
            self.state
                .document_store
                .upsert_chunk(&KnowledgeChunkRow {
                    chunk_id: *chunk_id,
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    document_id: chunk_document_id,
                    revision_id: chunk_revision_id,
                    chunk_index: i32::try_from(chunk_index).unwrap_or(i32::MAX),
                    content_text: query_text.to_string(),
                    normalized_text: query_text.to_string(),
                    span_start: Some(0),
                    span_end: Some(i32::try_from(query_text.len()).unwrap_or(i32::MAX)),
                    token_count: Some(1),
                    chunk_kind: Some("paragraph".to_string()),
                    support_block_ids: Vec::new(),
                    section_path: Vec::new(),
                    heading_trail: Vec::new(),
                    literal_digest: None,
                    chunk_state: "ready".to_string(),
                    text_generation: Some(1),
                    vector_generation: Some(1),
                    quality_score: None,
                    window_text: None,
                    occurred_at: None,
                    occurred_until: None,
                })
                .await
                .context("failed to seed grounding detail chunk")?;
        }
        Ok(())
    }

    async fn seed_authoritative_runtime_graph(
        &self,
        entity_ids: &[Uuid],
        relation_ids: &[Uuid],
    ) -> Result<()> {
        // Graph node/edge references hydrate from the bundle's entity/relation
        // edges, which only persist for ids present in the runtime graph
        // (`replace_bundle_entity_edges` joins `runtime_graph_node`,
        // `replace_bundle_relation_edges` joins `runtime_graph_edge`). Seed the
        // runtime projection so those edges — and the detail references — resolve.
        let runtime_projection_version = 1_i64;
        for entity_id in entity_ids {
            seed_runtime_graph_entity_node(
                &self.state.persistence.postgres,
                *entity_id,
                self.library_id,
                runtime_projection_version,
            )
            .await?;
        }
        if !entity_ids.is_empty() {
            for (relation_index, relation_id) in relation_ids.iter().enumerate() {
                let from_node_id = entity_ids[relation_index % entity_ids.len()];
                let to_node_id = entity_ids[(relation_index + 1) % entity_ids.len()];
                seed_runtime_graph_relation_edge(
                    &self.state.persistence.postgres,
                    *relation_id,
                    self.library_id,
                    from_node_id,
                    to_node_id,
                    runtime_projection_version,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn persist_authoritative_bundle(
        &self,
        execution: &query_repository::QueryExecutionRow,
        query_text: &str,
        verification_state: &str,
        verification_warnings: serde_json::Value,
        references: AuthoritativeBundleReferences,
    ) -> Result<()> {
        let AuthoritativeBundleReferences { chunk_ids, entity_ids, relation_ids, technical_facts } =
            references;
        let bundle_id = deterministic_context_bundle_id(execution.id);

        let mut bundle = sample_context_bundle(
            self.workspace_id,
            self.library_id,
            &map_execution_row(execution),
        );
        bundle.bundle_state = "ready".to_string();
        bundle.verification_state = verification_state.to_string();
        bundle.verification_warnings = verification_warnings;
        bundle.selected_fact_ids = technical_facts.iter().map(|fact| fact.fact_id).collect();
        bundle.candidate_summary = json!({
            "chunks": chunk_ids.len(),
            "entities": entity_ids.len(),
            "relations": relation_ids.len(),
            "facts": technical_facts.len()
        });
        bundle.assembly_diagnostics = json!({
            "question": query_text,
            "status": "ready",
            "grounding_kind": "hybrid"
        });
        self.state
            .context_store
            .upsert_bundle(&bundle)
            .await
            .context("failed to persist grounding authoritative evidence bundle")?;

        if !chunk_ids.is_empty() {
            let chunk_edges = chunk_ids
                .into_iter()
                .map(|chunk_id| sample_chunk_edge(bundle_id, chunk_id))
                .collect::<Vec<_>>();
            self.state
                .context_store
                .replace_bundle_chunk_edges(bundle_id, self.library_id, &chunk_edges)
                .await
                .context("failed to persist grounding chunk edges")?;
        }
        if !entity_ids.is_empty() {
            let entity_edges = entity_ids
                .into_iter()
                .map(|entity_id| sample_entity_edge(bundle_id, entity_id))
                .collect::<Vec<_>>();
            self.state
                .context_store
                .replace_bundle_entity_edges(bundle_id, self.library_id, &entity_edges)
                .await
                .context("failed to persist grounding entity edges")?;
        }
        if !relation_ids.is_empty() {
            let relation_edges = relation_ids
                .into_iter()
                .map(|relation_id| sample_relation_edge(bundle_id, relation_id))
                .collect::<Vec<_>>();
            self.state
                .context_store
                .replace_bundle_relation_edges(bundle_id, self.library_id, &relation_edges)
                .await
                .context("failed to persist grounding relation edges")?;
        }

        Ok(())
    }

    async fn create_replayable_execution_detail(
        &self,
        query_text: &str,
        answer_text: &str,
    ) -> Result<ironrag_backend::domains::query::QueryExecutionDetail> {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let fact = sample_technical_fact_row(TechnicalFactRowSeed {
            workspace_id: self.workspace_id,
            library_id: self.library_id,
            document_id,
            revision_id,
            fact_kind: TechnicalFactKind::ConfigurationKey,
            normalized_value: "enabled",
            display_value: "enabled",
            support_block_ids: Vec::new(),
            support_chunk_ids: Vec::new(),
        });
        let detail = self
            .create_execution_detail_with_authoritative_evidence(AuthoritativeEvidenceSeed {
                query_text,
                verification_state: "verified",
                verification_warnings: json!([]),
                chunk_ids: Vec::new(),
                entity_ids: Vec::new(),
                relation_ids: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: vec![fact],
            })
            .await?;
        let request_turn = detail.request_turn.context("replay source request turn missing")?;
        let response_turn = query_repository::create_turn(
            &self.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: self.conversation_id,
                turn_kind: "assistant",
                author_principal_id: None,
                content_text: answer_text,
                execution_id: Some(detail.execution.id),
            },
        )
        .await
        .context("failed to create replay source response turn")?;
        query_repository::update_execution(
            &self.state.persistence.postgres,
            detail.execution.id,
            &query_repository::UpdateQueryExecution {
                request_turn_id: Some(request_turn.id),
                response_turn_id: Some(response_turn.id),
            },
        )
        .await
        .context("failed to link replay source response turn")?
        .context("replay source execution disappeared")?;

        let query_ir: ironrag_backend::domains::query_ir::QueryIR =
            serde_json::from_value(json!({
                "act": "retrieve_value", "scope": "single_document", "language": "en",
                "retrieval_query": query_text
            }))?;
        sqlx::query(
            "update knowledge_context_bundle set
                assembly_diagnostics=assembly_diagnostics || jsonb_build_object('queryIr',$2::jsonb),
                candidate_summary=candidate_summary || '{\"answerDisposition\":\"factual_ready\"}'::jsonb
             where query_execution_id=$1",
        ).bind(detail.execution.id).bind(serde_json::to_value(query_ir)?).
            execute(&self.state.persistence.postgres).await?;

        anyhow::ensure!(
            query_result_cache_repository::execution_sources_are_current(
                &self.state.persistence.postgres,
                detail.execution.id,
                self.workspace_id,
                self.library_id,
            )
            .await?,
            "replay fixture must retain only canonically current sources"
        );
        anyhow::ensure!(
            !detail.technical_fact_references.is_empty(),
            "replay fixture must hydrate its selected technical fact"
        );

        QueryService::new()
            .get_execution(&self.state, detail.execution.id)
            .await
            .map_err(|error| anyhow!("failed to reload replayable execution detail: {error}"))
    }
}

fn deterministic_context_bundle_id(execution_id: Uuid) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, execution_id.as_bytes())
}

/// Seeds a runtime-plane graph node with an explicit id so bundle entity edges
/// (`replace_bundle_entity_edges` joins `runtime_graph_node`) and graph
/// traversals (`list_entity_neighborhood`) can resolve the entity. Graph queries
/// read the runtime projection, not the knowledge-plane `knowledge_entity` rows.
async fn seed_runtime_graph_entity_node(
    pool: &PgPool,
    node_id: Uuid,
    library_id: Uuid,
    projection_version: i64,
) -> Result<()> {
    sqlx::query(
        "insert into runtime_graph_node (
            id, library_id, \u{63}anonical_key, label, node_type, summary,
            support_count, projection_version
         ) values ($1, $2, $3, $4, 'entity', $5, 1, $6)
         on conflict (id) do nothing",
    )
    .bind(node_id)
    .bind(library_id)
    .bind(format!("entity:{node_id}"))
    .bind(format!("Entity {node_id}"))
    .bind("seeded runtime graph entity")
    .bind(projection_version)
    .execute(pool)
    .await
    .context("failed to seed runtime graph entity node")?;
    Ok(())
}

/// Seeds a runtime-plane graph edge with an explicit id so bundle relation edges
/// (`replace_bundle_relation_edges` joins `runtime_graph_edge`) resolve the
/// relation. `from`/`to` must reference seeded runtime nodes.
async fn seed_runtime_graph_relation_edge(
    pool: &PgPool,
    edge_id: Uuid,
    library_id: Uuid,
    from_node_id: Uuid,
    to_node_id: Uuid,
    projection_version: i64,
) -> Result<()> {
    sqlx::query(
        "insert into runtime_graph_edge (
            id, library_id, from_node_id, to_node_id, relation_type, \u{63}anonical_key,
            summary, weight, support_count, projection_version
         ) values ($1, $2, $3, $4, 'related_to', $5, $6, 1.0, 1, $7)
         on conflict (id) do nothing",
    )
    .bind(edge_id)
    .bind(library_id)
    .bind(from_node_id)
    .bind(to_node_id)
    .bind(format!("edge:{edge_id}"))
    .bind("seeded runtime graph relation")
    .bind(projection_version)
    .execute(pool)
    .await
    .context("failed to seed runtime graph relation edge")?;
    Ok(())
}

/// Seeds a runtime-plane evidence row with an explicit id so bundle evidence
/// edges (`replace_bundle_evidence_edges` joins `runtime_graph_evidence`)
/// resolve the evidence reference.
async fn seed_runtime_graph_evidence(
    pool: &PgPool,
    evidence_id: Uuid,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "insert into runtime_graph_evidence (
            id, library_id, evidence_identity_key, target_kind, target_id, evidence_text,
            rank, inclusion_reason
         ) values ($1, $2, $3, $4, $5, 'seeded runtime graph evidence', 1, 'evidence_grounding')
         on conflict (library_id, evidence_identity_key) do nothing",
    )
    .bind(evidence_id)
    .bind(library_id)
    .bind(format!("evidence:{evidence_id}"))
    .bind(target_kind)
    .bind(target_id)
    .execute(pool)
    .await
    .context("failed to seed runtime graph evidence")?;
    Ok(())
}

fn map_execution_row(row: &query_repository::QueryExecutionRow) -> QueryExecution {
    QueryExecution {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        conversation_id: row.conversation_id,
        context_bundle_id: row.context_bundle_id,
        request_turn_id: row.request_turn_id,
        response_turn_id: row.response_turn_id,
        binding_id: row.binding_id,
        runtime_execution_id: row.runtime_execution_id,
        lifecycle_state: row.runtime_lifecycle_state,
        active_stage: row.runtime_active_stage,
        query_text: row.query_text.clone(),
        failure_code: row.failure_code.clone(),
        started_at: row.started_at,
    }
}

fn sample_query_execution(
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    query_text: &str,
) -> QueryExecution {
    QueryExecution {
        id: execution_id,
        workspace_id,
        library_id,
        conversation_id: Uuid::now_v7(),
        context_bundle_id: deterministic_context_bundle_id(execution_id),
        request_turn_id: None,
        response_turn_id: None,
        binding_id: None,
        runtime_execution_id: Uuid::now_v7(),
        lifecycle_state: RuntimeLifecycleState::Running,
        active_stage: Some(RuntimeStageKind::Retrieve),
        query_text: query_text.to_string(),
        failure_code: None,
        started_at: Utc::now(),
    }
}

fn sample_context_bundle(
    workspace_id: Uuid,
    library_id: Uuid,
    execution: &QueryExecution,
) -> KnowledgeContextBundleRow {
    let now = Utc::now();
    KnowledgeContextBundleRow {
        bundle_id: deterministic_context_bundle_id(execution.id),
        workspace_id,
        library_id,
        query_execution_id: Some(execution.id),
        bundle_state: "assembling".to_string(),
        bundle_strategy: "grounded_answer".to_string(),
        requested_mode: "grounded_answer".to_string(),
        resolved_mode: "grounded_answer".to_string(),
        selected_fact_ids: Vec::new(),
        verification_state: "not_run".to_string(),
        verification_warnings: json!([]),
        freshness_snapshot: json!({
            "active_text_generation": 7,
            "active_vector_generation": 7,
            "active_graph_generation": 7
        }),
        candidate_summary: json!({
            "chunks": 0,
            "entities": 0,
            "relations": 0,
            "evidence": 0
        }),
        assembly_diagnostics: json!({
            "question": execution.query_text,
            "status": "assembling"
        }),
        created_at: now,
        updated_at: now,
    }
}

struct LinkedQueryExecutionSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    conversation_id: Uuid,
    query_text: &'a str,
    lifecycle_state: RuntimeLifecycleState,
    active_stage: Option<RuntimeStageKind>,
    failure_code: Option<&'a str>,
    request_turn_id: Option<Uuid>,
    response_turn_id: Option<Uuid>,
    binding_id: Option<Uuid>,
}

fn sample_linked_query_execution(seed: LinkedQueryExecutionSeed<'_>) -> QueryExecution {
    let LinkedQueryExecutionSeed {
        workspace_id,
        library_id,
        execution_id,
        conversation_id,
        query_text,
        lifecycle_state,
        active_stage,
        failure_code,
        request_turn_id,
        response_turn_id,
        binding_id,
    } = seed;
    QueryExecution {
        conversation_id,
        request_turn_id,
        response_turn_id,
        binding_id,
        lifecycle_state,
        active_stage,
        failure_code: failure_code.map(ToString::to_string),
        ..sample_query_execution(workspace_id, library_id, execution_id, query_text)
    }
}

fn sample_trace(
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    bundle_id: Uuid,
) -> KnowledgeRetrievalTraceRow {
    let now = Utc::now();
    KnowledgeRetrievalTraceRow {
        trace_id: Uuid::now_v7(),
        workspace_id,
        library_id,
        query_execution_id: Some(execution_id),
        bundle_id,
        trace_state: "ready".to_string(),
        retrieval_strategy: "chunk_lexical_first".to_string(),
        candidate_counts: json!({
            "chunk_candidates": 1,
            "entity_candidates": 1,
            "relation_candidates": 1,
            "evidence_candidates": 1
        }),
        dropped_reasons: json!([
            {
                "kind": "debug_scaffold",
                "note": "no ground-truth drops were generated for this fixture"
            }
        ]),
        timing_breakdown: json!({
            "lexical_ms": 1,
            "entity_ms": 1,
            "relation_ms": 1,
            "evidence_ms": 1,
            "bundle_ms": 1
        }),
        diagnostics_json: json!({
            "top_k": 1,
            "answerable": true,
            "grounding_kind": "hybrid"
        }),
        created_at: now,
        updated_at: now,
    }
}

fn sample_async_operation(
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    status: OpsAsyncOperationStatus,
    failure_code: Option<&str>,
) -> OpsAsyncOperation {
    OpsAsyncOperation {
        id: Uuid::now_v7(),
        workspace_id,
        library_id: Some(library_id),
        operation_kind: "query_execution".to_string(),
        status,
        surface_kind: Some("rest".to_string()),
        subject_kind: Some("query_execution".to_string()),
        subject_id: Some(execution_id),
        parent_async_operation_id: None,
        failure_code: failure_code.map(ToString::to_string),
        // A query execution never runs the ingest failure finalisation, so the
        // server holds no terminal disposition for it.
        failure_disposition: None,
        created_at: Utc::now(),
        completed_at: matches!(
            status,
            OpsAsyncOperationStatus::Ready
                | OpsAsyncOperationStatus::Failed
                | OpsAsyncOperationStatus::Canceled
        )
        .then(Utc::now),
        // A query execution publishes its answer through its own surface, not
        // through the operation envelope.
        result_json: None,
    }
}

fn sample_audit_subject(
    subject_kind: &str,
    subject_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Option<Uuid>,
) -> AuditEventSubject {
    AuditEventSubject {
        audit_event_id: Uuid::now_v7(),
        subject_kind: subject_kind.to_string(),
        subject_id,
        workspace_id: Some(workspace_id),
        library_id: Some(library_id),
        document_id,
        query_session_id: (subject_kind == "query_session").then_some(subject_id),
        query_execution_id: (subject_kind == "query_execution").then_some(subject_id),
        runtime_execution_id: (subject_kind == "runtime_execution").then_some(subject_id),
        context_bundle_id: (subject_kind == "knowledge_bundle").then_some(subject_id),
        async_operation_id: (subject_kind == "async_operation").then_some(subject_id),
    }
}

fn sample_chunk_edge(bundle_id: Uuid, chunk_id: Uuid) -> KnowledgeBundleChunkEdgeRow {
    KnowledgeBundleChunkEdgeRow {
        bundle_id,
        chunk_id,
        rank: 1,
        score: 0.91,
        inclusion_reason: Some("lexical_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_entity_edge(bundle_id: Uuid, entity_id: Uuid) -> KnowledgeBundleEntityEdgeRow {
    KnowledgeBundleEntityEdgeRow {
        bundle_id,
        entity_id,
        rank: 1,
        score: 0.87,
        inclusion_reason: Some("entity_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_relation_edge(bundle_id: Uuid, relation_id: Uuid) -> KnowledgeBundleRelationEdgeRow {
    KnowledgeBundleRelationEdgeRow {
        bundle_id,
        relation_id,
        rank: 1,
        score: 0.84,
        inclusion_reason: Some("relation_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_evidence_edge(bundle_id: Uuid, evidence_id: Uuid) -> KnowledgeBundleEvidenceEdgeRow {
    KnowledgeBundleEvidenceEdgeRow {
        bundle_id,
        evidence_id,
        rank: 1,
        score: 0.83,
        inclusion_reason: Some("evidence_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_chunk_reference(bundle_id: Uuid, chunk_id: Uuid) -> KnowledgeBundleChunkReferenceRow {
    KnowledgeBundleChunkReferenceRow {
        bundle_id,
        chunk_id,
        rank: 1,
        score: 0.91,
        inclusion_reason: Some("lexical_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_entity_reference(bundle_id: Uuid, entity_id: Uuid) -> KnowledgeBundleEntityReferenceRow {
    KnowledgeBundleEntityReferenceRow {
        bundle_id,
        entity_id,
        rank: 1,
        score: 0.87,
        inclusion_reason: Some("entity_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_relation_reference(
    bundle_id: Uuid,
    relation_id: Uuid,
) -> KnowledgeBundleRelationReferenceRow {
    KnowledgeBundleRelationReferenceRow {
        bundle_id,
        relation_id,
        rank: 1,
        score: 0.84,
        inclusion_reason: Some("relation_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_evidence_reference(
    bundle_id: Uuid,
    evidence_id: Uuid,
) -> KnowledgeBundleEvidenceReferenceRow {
    KnowledgeBundleEvidenceReferenceRow {
        bundle_id,
        evidence_id,
        rank: 1,
        score: 0.83,
        inclusion_reason: Some("evidence_grounding".to_string()),
        created_at: Utc::now(),
    }
}

struct StructuredBlockRowSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    ordinal: i32,
    block_kind: &'a str,
    text: &'a str,
    heading_trail: Vec<String>,
    section_path: Vec<String>,
}

fn sample_structured_block_row(seed: StructuredBlockRowSeed<'_>) -> KnowledgeStructuredBlockRow {
    let StructuredBlockRowSeed {
        workspace_id,
        library_id,
        document_id,
        revision_id,
        ordinal,
        block_kind,
        text,
        heading_trail,
        section_path,
    } = seed;
    let now = Utc::now();
    let block_id = Uuid::now_v7();
    KnowledgeStructuredBlockRow {
        block_id,
        workspace_id,
        library_id,
        document_id,
        revision_id,
        ordinal,
        block_kind: block_kind.to_string(),
        text: text.to_string(),
        normalized_text: text.to_string(),
        heading_trail,
        section_path,
        page_number: Some(1),
        span_start: Some(0),
        span_end: Some(i32::try_from(text.len()).unwrap_or(i32::MAX)),
        parent_block_id: None,
        table_coordinates_json: None,
        code_language: None,
        created_at: now,
        updated_at: now,
    }
}

struct TechnicalFactRowSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    fact_kind: TechnicalFactKind,
    normalized_value: &'a str,
    display_value: &'a str,
    support_block_ids: Vec<Uuid>,
    support_chunk_ids: Vec<Uuid>,
}

fn sample_technical_fact_row(seed: TechnicalFactRowSeed<'_>) -> KnowledgeTechnicalFactRow {
    let TechnicalFactRowSeed {
        workspace_id,
        library_id,
        document_id,
        revision_id,
        fact_kind,
        normalized_value,
        display_value,
        support_block_ids,
        support_chunk_ids,
    } = seed;
    let now = Utc::now();
    let fact_id = Uuid::now_v7();
    KnowledgeTechnicalFactRow {
        fact_id,
        workspace_id,
        library_id,
        document_id,
        revision_id,
        fact_kind: fact_kind.as_str().to_owned(),
        normalized_value_text: normalized_value.to_string(),
        normalized_value_exact: normalized_value.to_string(),
        normalized_value_json: json!(normalized_value),
        display_value: display_value.to_string(),
        qualifiers_json: json!({}),
        support_block_ids,
        support_chunk_ids,
        confidence: Some(0.95),
        extraction_kind: "parser_first".to_string(),
        conflict_group_id: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn authoritative_query_execution_scaffold_uses_execution_keyed_bundle_ids() {
    let _service = QueryService::new();
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let execution = sample_query_execution(
        workspace_id,
        library_id,
        execution_id,
        "What supports the authoritative answer?",
    );
    let bundle = sample_context_bundle(workspace_id, library_id, &execution);

    assert_eq!(execution.context_bundle_id, deterministic_context_bundle_id(execution.id));
    assert_eq!(bundle.bundle_id, deterministic_context_bundle_id(execution.id));
    assert_eq!(bundle.query_execution_id, Some(execution.id));
    assert_eq!(bundle.bundle_strategy, "grounded_answer");
}

#[test]
fn typed_bundle_reference_rows_cover_all_grounding_kinds() {
    let bundle_id = Uuid::now_v7();
    let query_execution_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let entity_id = Uuid::now_v7();
    let relation_id = Uuid::now_v7();
    let evidence_id = Uuid::now_v7();

    let chunk_edge = sample_chunk_edge(bundle_id, chunk_id);
    let entity_edge = sample_entity_edge(bundle_id, entity_id);
    let relation_edge = sample_relation_edge(bundle_id, relation_id);
    let evidence_edge = sample_evidence_edge(bundle_id, evidence_id);
    let chunk_reference = sample_chunk_reference(bundle_id, chunk_id);
    let entity_reference = sample_entity_reference(bundle_id, entity_id);
    let relation_reference = sample_relation_reference(bundle_id, relation_id);
    let evidence_reference = sample_evidence_reference(bundle_id, evidence_id);

    assert_eq!(chunk_edge.bundle_id, bundle_id);
    assert_eq!(chunk_edge.chunk_id, chunk_id);
    assert_eq!(entity_edge.bundle_id, bundle_id);
    assert_eq!(entity_edge.entity_id, entity_id);
    assert_eq!(relation_edge.bundle_id, bundle_id);
    assert_eq!(relation_edge.relation_id, relation_id);
    assert_eq!(evidence_edge.bundle_id, bundle_id);
    assert_eq!(evidence_edge.evidence_id, evidence_id);

    let reference_set = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: Some(query_execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: "grounded_answer".to_string(),
            requested_mode: "grounded_answer".to_string(),
            resolved_mode: "grounded_answer".to_string(),
            selected_fact_ids: Vec::new(),
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: json!({
                "active_text_generation": 7,
                "active_vector_generation": 7,
                "active_graph_generation": 7
            }),
            candidate_summary: json!({
                "chunks": 1,
                "entities": 1,
                "relations": 1,
                "evidence": 1
            }),
            assembly_diagnostics: json!({
                "answerable": true,
                "grounding_kind": "hybrid"
            }),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: vec![chunk_reference],
        entity_references: vec![entity_reference],
        relation_references: vec![relation_reference],
        evidence_references: vec![evidence_reference],
    };

    assert_bundle_reference_metadata(&reference_set, bundle_id, query_execution_id);
    assert_bundle_reference_targets(
        &reference_set,
        bundle_id,
        chunk_id,
        entity_id,
        relation_id,
        evidence_id,
    );
    assert_eq!(reference_set.chunk_references.len(), 1);
    assert_eq!(reference_set.entity_references.len(), 1);
    assert_eq!(reference_set.relation_references.len(), 1);
    assert_eq!(reference_set.evidence_references.len(), 1);
}

fn assert_bundle_reference_metadata(
    reference_set: &KnowledgeContextBundleReferenceSetRow,
    bundle_id: Uuid,
    query_execution_id: Uuid,
) {
    assert_eq!(reference_set.bundle.bundle_id, bundle_id);
    assert_eq!(reference_set.bundle.query_execution_id, Some(query_execution_id));
    assert_eq!(reference_set.bundle.candidate_summary["chunks"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["entities"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["relations"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["evidence"], json!(1));
    assert_eq!(reference_set.bundle.assembly_diagnostics["grounding_kind"], json!("hybrid"));
}

fn assert_bundle_reference_targets(
    reference_set: &KnowledgeContextBundleReferenceSetRow,
    bundle_id: Uuid,
    chunk_id: Uuid,
    entity_id: Uuid,
    relation_id: Uuid,
    evidence_id: Uuid,
) {
    assert_eq!(reference_set.chunk_references[0].chunk_id, chunk_id);
    assert_eq!(reference_set.chunk_references[0].bundle_id, bundle_id);
    assert_eq!(
        reference_set.chunk_references[0].inclusion_reason.as_deref(),
        Some("lexical_grounding")
    );
    assert_eq!(reference_set.entity_references[0].entity_id, entity_id);
    assert_eq!(reference_set.entity_references[0].bundle_id, bundle_id);
    assert_eq!(
        reference_set.entity_references[0].inclusion_reason.as_deref(),
        Some("entity_grounding")
    );
    assert_eq!(reference_set.relation_references[0].relation_id, relation_id);
    assert_eq!(reference_set.relation_references[0].bundle_id, bundle_id);
    assert_eq!(
        reference_set.relation_references[0].inclusion_reason.as_deref(),
        Some("relation_grounding")
    );
    assert_eq!(reference_set.evidence_references[0].evidence_id, evidence_id);
    assert_eq!(reference_set.evidence_references[0].bundle_id, bundle_id);
    assert_eq!(
        reference_set.evidence_references[0].inclusion_reason.as_deref(),
        Some("evidence_grounding")
    );
}

#[test]
fn failure_cancellation_and_retry_scaffold_preserve_execution_bundle_linkage() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let conversation_id = Uuid::now_v7();
    let request_turn_id = Uuid::now_v7();
    let response_turn_id = Uuid::now_v7();
    let binding_id = Uuid::now_v7();
    let query_text = "Which anchors survive failure, cancellation, and retry?";

    let failed_execution_id = Uuid::now_v7();
    let canceled_execution_id = Uuid::now_v7();
    let retry_execution_id = Uuid::now_v7();

    let failed = sample_linked_query_execution(LinkedQueryExecutionSeed {
        workspace_id,
        library_id,
        execution_id: failed_execution_id,
        conversation_id,
        query_text,
        lifecycle_state: RuntimeLifecycleState::Failed,
        active_stage: None,
        failure_code: Some("provider_timeout"),
        request_turn_id: Some(request_turn_id),
        response_turn_id: None,
        binding_id: Some(binding_id),
    });
    let canceled = sample_linked_query_execution(LinkedQueryExecutionSeed {
        workspace_id,
        library_id,
        execution_id: canceled_execution_id,
        conversation_id,
        query_text,
        lifecycle_state: RuntimeLifecycleState::Canceled,
        active_stage: None,
        failure_code: Some("canceled_by_user"),
        request_turn_id: Some(request_turn_id),
        response_turn_id: None,
        binding_id: Some(binding_id),
    });
    let retried = sample_linked_query_execution(LinkedQueryExecutionSeed {
        workspace_id,
        library_id,
        execution_id: retry_execution_id,
        conversation_id,
        query_text,
        lifecycle_state: RuntimeLifecycleState::Running,
        active_stage: Some(RuntimeStageKind::Retrieve),
        failure_code: None,
        request_turn_id: Some(request_turn_id),
        response_turn_id: Some(response_turn_id),
        binding_id: Some(binding_id),
    });

    let failed_bundle = sample_context_bundle(workspace_id, library_id, &failed);
    let canceled_bundle = sample_context_bundle(workspace_id, library_id, &canceled);
    let retried_bundle = sample_context_bundle(workspace_id, library_id, &retried);

    for (execution, bundle) in
        [(&failed, &failed_bundle), (&canceled, &canceled_bundle), (&retried, &retried_bundle)]
    {
        assert_execution_bundle_linkage(execution, bundle, request_turn_id, binding_id, query_text);
    }
    assert_failure_and_retry_states(&failed, &canceled, &retried, response_turn_id);

    let failed_operation = sample_async_operation(
        workspace_id,
        library_id,
        failed.id,
        OpsAsyncOperationStatus::Failed,
        failed.failure_code.as_deref(),
    );
    let canceled_operation = sample_async_operation(
        workspace_id,
        library_id,
        canceled.id,
        OpsAsyncOperationStatus::Failed,
        canceled.failure_code.as_deref(),
    );
    let retried_operation = sample_async_operation(
        workspace_id,
        library_id,
        retried.id,
        OpsAsyncOperationStatus::Processing,
        None,
    );

    let failed_execution_subject =
        sample_audit_subject("query_execution", failed.id, workspace_id, library_id, None);
    let failed_bundle_subject = sample_audit_subject(
        "knowledge_bundle",
        failed_bundle.bundle_id,
        workspace_id,
        library_id,
        None,
    );
    let failed_operation_subject = sample_audit_subject(
        "async_operation",
        failed_operation.id,
        workspace_id,
        library_id,
        None,
    );

    for (operation, execution_id, failure_code) in [
        (&failed_operation, failed.id, Some("provider_timeout")),
        (&canceled_operation, canceled.id, Some("canceled_by_user")),
        (&retried_operation, retried.id, None),
    ] {
        assert_eq!(operation.subject_kind.as_deref(), Some("query_execution"));
        assert_eq!(operation.subject_id, Some(execution_id));
        assert_eq!(operation.failure_code.as_deref(), failure_code);
    }
    assert_eq!(failed_execution_subject.query_execution_id, Some(failed.id));
    assert_eq!(failed_bundle_subject.context_bundle_id, Some(failed_bundle.bundle_id));
    assert_eq!(failed_operation_subject.async_operation_id, Some(failed_operation.id));
    assert_eq!(failed_execution_subject.library_id, Some(library_id));
    assert_eq!(failed_bundle_subject.workspace_id, Some(workspace_id));
    assert_eq!(failed_operation_subject.subject_kind, "async_operation");
}

fn assert_execution_bundle_linkage(
    execution: &QueryExecution,
    bundle: &KnowledgeContextBundleRow,
    request_turn_id: Uuid,
    binding_id: Uuid,
    query_text: &str,
) {
    assert_eq!(execution.context_bundle_id, deterministic_context_bundle_id(execution.id));
    assert_eq!(execution.request_turn_id, Some(request_turn_id));
    assert_eq!(execution.binding_id, Some(binding_id));
    assert_eq!(execution.query_text, query_text);
    assert_eq!(bundle.assembly_diagnostics["question"], json!(query_text));
    assert_eq!(bundle.candidate_summary["chunks"], json!(0));
    assert_eq!(bundle.query_execution_id, Some(execution.id));
    assert_eq!(bundle.bundle_id, execution.context_bundle_id);
    assert_eq!(bundle.bundle_strategy, "grounded_answer");
}

fn assert_failure_and_retry_states(
    failed: &QueryExecution,
    canceled: &QueryExecution,
    retried: &QueryExecution,
    response_turn_id: Uuid,
) {
    assert_eq!(failed.response_turn_id, None);
    assert_eq!(canceled.response_turn_id, None);
    assert_eq!(retried.response_turn_id, Some(response_turn_id));
    assert_eq!(failed.lifecycle_state, RuntimeLifecycleState::Failed);
    assert_eq!(canceled.lifecycle_state, RuntimeLifecycleState::Canceled);
    assert_eq!(retried.lifecycle_state, RuntimeLifecycleState::Running);
    assert_eq!(retried.active_stage, Some(RuntimeStageKind::Retrieve));
    assert_eq!(failed.failure_code.as_deref(), Some("provider_timeout"));
    assert_eq!(canceled.failure_code.as_deref(), Some("canceled_by_user"));
    assert_eq!(retried.failure_code, None);
}

/// Creates the workspace and library a database-backed grounding fixture writes into.
async fn create_grounding_library(fixture: &QueryGroundingFixture) -> Result<(Uuid, Uuid)> {
    let suffix = Uuid::now_v7().simple().to_string();
    let workspace = ironrag_backend::infra::repositories::catalog_repository::create_workspace(
        &fixture.postgres,
        &format!("grounding-workspace-{suffix}"),
        "Grounding Workspace",
        None,
    )
    .await?;
    let library = ironrag_backend::infra::repositories::catalog_repository::create_library(
        &fixture.postgres,
        workspace.id,
        &format!("grounding-library-{suffix}"),
        "Grounding Library",
        None,
        None,
    )
    .await?;
    Ok((workspace.id, library.id))
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn setup_structured_block_repository_bounds_and_deduplicates_lanes() -> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        // The knowledge plane is owned by `catalog_library`, so the fixture
        // seeds a real workspace and library instead of inventing ids the
        // product could never write.
        let (workspace_id, library_id) = create_grounding_library(&fixture).await?;
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        fixture
            .seed_chunk(SeedChunk {
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_id: Uuid::now_v7(),
                content_text: "neutral grounding chunk",
            })
            .await?;

        let blocks = vec![
            sample_structured_block_row(StructuredBlockRowSeed {
                workspace_id,
                library_id,
                document_id,
                revision_id,
                ordinal: 0,
                block_kind: "table_row",
                text: "early structural row",
                heading_trail: Vec::new(),
                section_path: Vec::new(),
            }),
            sample_structured_block_row(StructuredBlockRowSeed {
                workspace_id,
                library_id,
                document_id,
                revision_id,
                ordinal: 1,
                block_kind: "paragraph",
                text: "early plain row",
                heading_trail: Vec::new(),
                section_path: Vec::new(),
            }),
            sample_structured_block_row(StructuredBlockRowSeed {
                workspace_id,
                library_id,
                document_id,
                revision_id,
                ordinal: 10,
                block_kind: "code_block",
                text: "late code row",
                heading_trail: Vec::new(),
                section_path: Vec::new(),
            }),
            sample_structured_block_row(StructuredBlockRowSeed {
                workspace_id,
                library_id,
                document_id,
                revision_id,
                ordinal: 11,
                block_kind: "source_unit",
                text: "late source row",
                heading_trail: Vec::new(),
                section_path: Vec::new(),
            }),
        ];
        let now = Utc::now();
        fixture
            .document_store
            .replace_structured_revision_artifact(
                &KnowledgeStructuredRevisionRow {
                    revision_id,
                    workspace_id,
                    library_id,
                    document_id,
                    preparation_state: "prepared".to_string(),
                    normalization_profile: "query-grounding-test-v1".to_string(),
                    source_format: "text".to_string(),
                    language_code: None,
                    block_count: i32::try_from(blocks.len()).unwrap_or(i32::MAX),
                    chunk_count: 1,
                    typed_fact_count: 0,
                    outline_json: json!([]),
                    prepared_at: now,
                    updated_at: now,
                },
                &blocks,
            )
            .await
            .context("failed to seed setup structured revision artifact")?;

        let selected = fixture
            .document_store
            .list_setup_structured_blocks_by_revision(revision_id, 2, 2)
            .await
            .context("failed to list bounded setup structured blocks")?;
        let selected_again = fixture
            .document_store
            .list_setup_structured_blocks_by_revision(revision_id, 2, 2)
            .await
            .context("failed to repeat bounded setup structured block read")?;
        let selected_ids = selected.iter().map(|block| block.block_id).collect::<Vec<_>>();
        let selected_id_set =
            selected_ids.iter().copied().collect::<std::collections::HashSet<_>>();

        assert_eq!(selected.len(), 4);
        assert_eq!(selected_id_set.len(), selected.len());
        assert_eq!(
            selected.iter().map(|block| block.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 10, 11]
        );
        assert_eq!(
            selected_again.iter().map(|block| block.block_id).collect::<Vec<_>>(),
            selected_ids
        );
        assert!(selected.iter().any(|block| block.block_kind == "code_block"));
        assert!(selected.iter().any(|block| block.block_kind == "source_unit"));
        assert!(
            fixture
                .document_store
                .list_setup_structured_blocks_by_revision(revision_id, 0, 0)
                .await?
                .is_empty()
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn context_bundle_roundtrip_by_query_execution_persists_trace_and_chunk_references()
-> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        // A real catalog library is required: the runtime-plane rows the bundle
        // edges reference (`runtime_graph_node`, `_edge`, `_evidence`) and the
        // persisted `query_execution` all have foreign keys to `catalog_library`.
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("context-bundle-roundtrip-workspace-{suffix}"),
            "Context Bundle Roundtrip Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("context-bundle-roundtrip-library-{suffix}"),
            "Context Bundle Roundtrip Library",
            None,
            None,
        )
        .await?;
        let workspace_id = workspace.id;
        let library_id = library.id;
        let execution_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let relation_id = Uuid::now_v7();
        let evidence_id = Uuid::now_v7();
        let execution = sample_query_execution(
            workspace_id,
            library_id,
            execution_id,
            "Which chunk grounds this answer?",
        );
        let bundle = sample_context_bundle(workspace_id, library_id, &execution);

        // `get_bundle_by_query_execution` joins `query_execution`, so the
        // execution the bundle is keyed on must be persisted.
        let conversation = query_repository::create_conversation(
            &fixture.postgres,
            &query_repository::NewQueryConversation {
                workspace_id,
                library_id,
                created_by_principal_id: None,
                title: Some("Context bundle roundtrip"),
                conversation_state: "active",
                request_surface: "mcp",
            },
            64,
        )
        .await?;
        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &fixture.postgres,
            &runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: execution.id,
                task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
                surface_kind: "mcp",
                contract_name: "query_answer",
                contract_version: "1",
                lifecycle_state: RuntimeLifecycleState::Completed.as_str(),
                active_stage: None,
                turn_budget: 1,
                turn_count: 1,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                parent_execution_id: None,
            },
        )
        .await?;
        query_repository::create_execution(
            &fixture.postgres,
            &query_repository::NewQueryExecution {
                execution_id: execution.id,
                context_bundle_id: bundle.bundle_id,
                workspace_id,
                library_id,
                conversation_id: conversation.id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text: "Which chunk grounds this answer?",
                failure_code: None,
            },
        )
        .await?;

        // The bundle's entity/relation/evidence edges join the runtime graph, so
        // seed the runtime projection rows their edges reference.
        seed_runtime_graph_entity_node(&fixture.postgres, entity_id, library_id, 1).await?;
        seed_runtime_graph_relation_edge(
            &fixture.postgres,
            relation_id,
            library_id,
            entity_id,
            entity_id,
            1,
        )
        .await?;
        seed_runtime_graph_evidence(&fixture.postgres, evidence_id, library_id, "node", entity_id)
            .await?;

        fixture
            .seed_chunk(SeedChunk {
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_id,
                content_text: "grounding anchor chunk",
            })
            .await?;

        fixture
            .context_store
            .upsert_bundle(&bundle)
            .await
            .context("failed to persist grounding context bundle")?;
        fixture
            .context_store
            .upsert_trace(&sample_trace(workspace_id, library_id, execution.id, bundle.bundle_id))
            .await
            .context("failed to persist grounding retrieval trace")?;
        fixture
            .context_store
            .replace_bundle_chunk_edges(
                bundle.bundle_id,
                library_id,
                &[sample_chunk_edge(bundle.bundle_id, chunk_id)],
            )
            .await
            .context("failed to persist grounding chunk references")?;
        fixture
            .context_store
            .replace_bundle_entity_edges(
                bundle.bundle_id,
                library_id,
                &[sample_entity_edge(bundle.bundle_id, entity_id)],
            )
            .await
            .context("failed to persist grounding entity references")?;
        fixture
            .context_store
            .replace_bundle_relation_edges(
                bundle.bundle_id,
                library_id,
                &[sample_relation_edge(bundle.bundle_id, relation_id)],
            )
            .await
            .context("failed to persist grounding relation references")?;
        fixture
            .context_store
            .replace_bundle_evidence_edges(
                bundle.bundle_id,
                library_id,
                &[sample_evidence_edge(bundle.bundle_id, evidence_id)],
            )
            .await
            .context("failed to persist grounding evidence references")?;
        fixture
            .context_store
            .update_bundle_state(BundleStateUpdate {
                bundle_id: bundle.bundle_id,
                bundle_state: "ready",
                selected_fact_ids: &[],
                verification_state: "not_run",
                verification_warnings: json!([]),
                freshness_snapshot: json!({
                    "active_text_generation": 7,
                    "active_vector_generation": 7,
                    "active_graph_generation": 7
                }),
                candidate_summary: json!({
                    "chunks": 1,
                    "entities": 1,
                    "relations": 1,
                    "evidence": 1
                }),
                assembly_diagnostics: json!({
                    "answerable": true,
                    "grounding_kind": "hybrid"
                }),
            })
            .await
            .context("failed to update grounding bundle state")?
            .ok_or_else(|| anyhow!("grounding context bundle disappeared during update"))?;

        let persisted_bundle = fixture
            .context_store
            .get_bundle_by_query_execution(execution.id)
            .await
            .context("failed to load context bundle by query execution")?
            .ok_or_else(|| anyhow!("context bundle not found for query execution"))?;
        assert_eq!(persisted_bundle.bundle_id, bundle.bundle_id);
        assert_eq!(persisted_bundle.bundle_state, "ready");

        let reference_set = fixture
            .context_store
            .get_bundle_reference_set_by_query_execution(execution.id)
            .await
            .context("failed to load materialized context bundle by query execution")?
            .ok_or_else(|| anyhow!("materialized context bundle not found for query execution"))?;
        assert_eq!(reference_set.bundle.query_execution_id, Some(execution.id));
        assert_eq!(reference_set.chunk_references.len(), 1);
        assert_eq!(reference_set.chunk_references[0].chunk_id, chunk_id);
        assert_eq!(reference_set.chunk_references[0].rank, 1);
        assert_eq!(
            reference_set.chunk_references[0].inclusion_reason.as_deref(),
            Some("lexical_grounding")
        );
        assert_eq!(reference_set.entity_references.len(), 1);
        assert_eq!(reference_set.entity_references[0].entity_id, entity_id);
        assert_eq!(reference_set.entity_references[0].rank, 1);
        assert_eq!(
            reference_set.entity_references[0].inclusion_reason.as_deref(),
            Some("entity_grounding")
        );
        assert_eq!(reference_set.relation_references.len(), 1);
        assert_eq!(reference_set.relation_references[0].relation_id, relation_id);
        assert_eq!(reference_set.relation_references[0].rank, 1);
        assert_eq!(
            reference_set.relation_references[0].inclusion_reason.as_deref(),
            Some("relation_grounding")
        );
        assert_eq!(reference_set.evidence_references.len(), 1);
        assert_eq!(reference_set.evidence_references[0].evidence_id, evidence_id);
        assert_eq!(reference_set.evidence_references[0].rank, 1);
        assert_eq!(
            reference_set.evidence_references[0].inclusion_reason.as_deref(),
            Some("evidence_grounding")
        );

        let traces = fixture
            .context_store
            .list_traces_by_query_execution(execution.id)
            .await
            .context("failed to list retrieval traces by query execution")?;
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].bundle_id, bundle.bundle_id);
        assert_eq!(traces[0].query_execution_id, Some(execution.id));
        assert_eq!(traces[0].trace_state, "ready");
        assert_eq!(traces[0].retrieval_strategy, "chunk_lexical_first");
        assert_eq!(traces[0].candidate_counts["chunk_candidates"], json!(1));
        assert_eq!(traces[0].candidate_counts["entity_candidates"], json!(1));
        assert_eq!(traces[0].candidate_counts["relation_candidates"], json!(1));
        assert_eq!(traces[0].candidate_counts["evidence_candidates"], json!(1));
        assert_eq!(traces[0].dropped_reasons[0]["kind"], json!("debug_scaffold"));
        assert_eq!(traces[0].timing_breakdown["bundle_ms"], json!(1));
        assert_eq!(traces[0].diagnostics_json["grounding_kind"], json!("hybrid"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn persistent_query_result_cache_expires_and_replaces_stale_winner() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let first =
            fixture.create_execution_detail("First cache source", "verified", json!([])).await?;
        let second = fixture
            .create_execution_detail("Replacement cache source", "verified", json!([]))
            .await?;
        let source_truth_version = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let cache_key = "query_result:v3:persistent-expiry-test";
        let initial = query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: first.execution.id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:v1",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:v1",
                ttl_seconds: 300,
            },
        )
        .await?
        .context("current-generation cache winner was not persisted")?;
        assert_eq!(initial.source_execution_id, first.execution.id);

        sqlx::query(
            "update query_result_cache
             set updated_at = now() - interval '301 seconds'
             where cache_key = $1",
        )
        .bind(cache_key)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert!(
            query_result_cache_repository::get_query_result_cache(
                &fixture.state.persistence.postgres,
                cache_key,
                300,
            )
            .await?
            .is_none()
        );

        let replacement = query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: second.execution.id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:v2",
                graph_projection_version: 2,
                graph_topology_generation: 2,
                binding_fingerprint: "bindings:v2",
                ttl_seconds: 300,
            },
        )
        .await?
        .context("current-generation replacement cache winner was not persisted")?;
        assert_eq!(replacement.source_execution_id, second.execution.id);
        assert_eq!(replacement.hit_count, 0);

        let losing_conflict = query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: first.execution.id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:v1",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:v1",
                ttl_seconds: 300,
            },
        )
        .await?
        .context("current-generation conflicting cache winner was not returned")?;
        assert_eq!(losing_conflict.source_execution_id, second.execution.id);
        assert_eq!(losing_conflict.updated_at, replacement.updated_at);
        assert_eq!(losing_conflict.hit_count, 1);
        assert_eq!(
            query_result_cache_repository::delete_query_result_cache(
                &fixture.state.persistence.postgres,
                cache_key,
                first.execution.id,
            )
            .await?,
            0,
            "an old reader must not evict a concurrently replaced winner",
        );
        let winner_after_stale_evict = query_result_cache_repository::get_query_result_cache(
            &fixture.state.persistence.postgres,
            cache_key,
            300,
        )
        .await?
        .context("replacement winner was removed by stale eviction")?;
        assert_eq!(winner_after_stale_evict.source_execution_id, second.execution.id);

        repositories::touch_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let stale_winner = query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key: "query_result:v3:stale-generation-winner",
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: first.execution.id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:stale",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:v1",
                ttl_seconds: 300,
            },
        )
        .await?;
        assert!(stale_winner.is_none());
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn query_result_cache_gc_uses_db_ttl_and_keeps_each_batch_bounded() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let detail = fixture
            .create_replayable_execution_detail(
                "Cache retention fixture",
                "Retained replay audit fixture",
            )
            .await?;
        let request_turn = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: fixture.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Replay the retained cache answer.",
                execution_id: None,
            },
        )
        .await?;
        let source_truth_version = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let cache_key = "query_result:v3:gc-replay-audit";
        query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: detail.execution.id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:gc-replay-audit",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:gc-replay-audit",
                ttl_seconds: 300,
            },
        )
        .await?
        .context("failed to persist replay-audit cache winner")?;
        let (_, replay) = query_result_cache_repository::create_query_execution_replay(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::CreateQueryExecutionReplayInput {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                conversation_id: fixture.conversation_id,
                request_turn_id: request_turn.id,
                source_execution_id: detail.execution.id,
                expected_source_truth_version: source_truth_version,
                cache_key,
                ttl_seconds: 300,
            },
        )
        .await?
        .context("current-generation replay should be materialized")?;

        let gc_index_exists: bool = sqlx::query_scalar(
            "select to_regclass('public.idx_query_result_cache_gc_updated') is not null",
        )
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert!(gc_index_exists, "the global TTL sweep index must be installed");

        let expired_rows: i64 =
            query_result_cache_repository::MAX_QUERY_RESULT_CACHE_GC_BATCH_LIMIT + 1;
        sqlx::query(
            "insert into query_result_cache (
                cache_key,
                workspace_id,
                library_id,
                source_execution_id,
                readable_content_fingerprint,
                graph_projection_version,
                graph_topology_generation,
                binding_fingerprint,
                updated_at
             )
             select
                'query_result:v3:gc-expired:' || ordinal::text,
                $1,
                $2,
                $3,
                'content:gc-expired',
                1,
                1,
                'bindings:gc-expired',
                now() - interval '301 seconds'
             from generate_series(1::bigint, $4) ordinal",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(detail.execution.id)
        .bind(expired_rows)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        sqlx::query(
            "insert into query_result_cache (
                cache_key,
                workspace_id,
                library_id,
                source_execution_id,
                readable_content_fingerprint,
                graph_projection_version,
                graph_topology_generation,
                binding_fingerprint,
                updated_at
             ) values ($1, $2, $3, $4, 'content:gc-fresh', 1, 1, 'bindings:gc-fresh', now())",
        )
        .bind("query_result:v3:gc-fresh")
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(detail.execution.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let initial_backlog =
            query_result_cache_repository::probe_expired_query_result_cache_backlog(
                &fixture.state.persistence.postgres,
                300,
                i64::MAX,
            )
            .await?;
        assert_eq!(
            initial_backlog.sample_limit,
            u64::try_from(
                query_result_cache_repository::MAX_QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT,
            )
            .unwrap_or(u64::MAX),
        );
        assert_eq!(initial_backlog.sampled_expired_rows, 501);
        assert!(initial_backlog.sample_at_capacity());
        assert!(
            initial_backlog.oldest_expired_age_seconds.is_some_and(|age| age >= 1.0),
            "the bounded probe must report seconds past TTL using PostgreSQL's clock",
        );

        let removed =
            ironrag_backend::services::maintenance::scheduler::gc_expired_query_result_cache_once(
                &fixture.state,
                std::time::Duration::from_secs(300),
                i64::MAX,
            )
            .await?;
        assert_eq!(
            removed,
            u64::try_from(query_result_cache_repository::MAX_QUERY_RESULT_CACHE_GC_BATCH_LIMIT)
                .unwrap_or(u64::MAX),
            "the repository hard cap must bound a scheduler pass",
        );

        let expired_remaining: i64 = sqlx::query_scalar(
            "select count(*)::bigint
             from query_result_cache
             where cache_key like 'query_result:v3:gc-expired:%'",
        )
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(expired_remaining, 1);
        let one_row_backlog =
            query_result_cache_repository::probe_expired_query_result_cache_backlog(
                &fixture.state.persistence.postgres,
                300,
                i64::MAX,
            )
            .await?;
        assert_eq!(one_row_backlog.sampled_expired_rows, 1);
        assert!(!one_row_backlog.sample_at_capacity());
        assert!(one_row_backlog.oldest_expired_age_seconds.is_some());
        let fresh_remaining: bool = sqlx::query_scalar(
            "select exists (
                select 1 from query_result_cache where cache_key = $1
             )",
        )
        .bind("query_result:v3:gc-fresh")
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert!(fresh_remaining, "a DB-clock-fresh cache row must survive GC");

        let replay_remaining: bool = sqlx::query_scalar(
            "select exists (
                select 1 from query_execution_replay where id = $1
             )",
        )
        .bind(replay.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert!(
            replay_remaining,
            "target-conversation replay provenance must outlive short-lived winner cache TTL",
        );

        let no_op_removed =
            ironrag_backend::services::maintenance::scheduler::gc_expired_query_result_cache_once(
                &fixture.state,
                std::time::Duration::from_secs(300),
                0,
            )
            .await?;
        assert_eq!(no_op_removed, 0, "a zero-sized batch must remain a no-op");

        let final_removed =
            ironrag_backend::services::maintenance::scheduler::gc_expired_query_result_cache_once(
                &fixture.state,
                std::time::Duration::from_secs(300),
                1,
            )
            .await?;
        assert_eq!(final_removed, 1);
        let empty_backlog =
            query_result_cache_repository::probe_expired_query_result_cache_backlog(
                &fixture.state.persistence.postgres,
                300,
                i64::MAX,
            )
            .await?;
        assert_eq!(empty_backlog.sampled_expired_rows, 0);
        assert!(!empty_backlog.sample_at_capacity());
        assert!(empty_backlog.oldest_expired_age_seconds.is_none());
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn cached_replay_turn_and_audit_row_commit_atomically() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let detail = fixture
            .create_replayable_execution_detail(
                "What is the supported value?",
                "The supported value is grounded.",
            )
            .await?;
        let request_turn = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: fixture.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Repeat the supported value.",
                execution_id: None,
            },
        )
        .await?;
        let stale_source_truth_version = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let current_source_truth_version = repositories::touch_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let mut input = query_result_cache_repository::CreateQueryExecutionReplayInput {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            conversation_id: fixture.conversation_id,
            request_turn_id: request_turn.id,
            source_execution_id: detail.execution.id,
            expected_source_truth_version: stale_source_truth_version,
            cache_key: "query_result:v3:atomic-replay-test",
            ttl_seconds: 300,
        };

        assert!(
            query_result_cache_repository::create_query_execution_replay(
                &fixture.state.persistence.postgres,
                &input,
            )
            .await?
            .is_none(),
            "a stale source generation must not materialize a replay turn",
        );
        assert_eq!(
            query_repository::list_turns_by_conversation(
                &fixture.state.persistence.postgres,
                fixture.conversation_id,
            )
            .await?
            .len(),
            3,
        );
        input.expected_source_truth_version = current_source_truth_version;
        query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key: input.cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: detail.execution.id,
                expected_source_truth_version: current_source_truth_version,
                readable_content_fingerprint: "content:atomic-replay",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:atomic-replay",
                ttl_seconds: input.ttl_seconds,
            },
        )
        .await?
        .context("failed to persist atomic replay cache winner")?;

        let (response_turn, replay) = query_result_cache_repository::create_query_execution_replay(
            &fixture.state.persistence.postgres,
            &input,
        )
        .await?
        .context("current source generation should allow replay")?;
        assert_eq!(replay.response_turn_id, response_turn.id);
        assert_eq!(replay.request_turn_id, request_turn.id);
        assert_eq!(response_turn.content_text, "The supported value is grounded.");

        let turns_after_success = query_repository::list_turns_by_conversation(
            &fixture.state.persistence.postgres,
            fixture.conversation_id,
        )
        .await?;
        assert_eq!(turns_after_success.len(), 4);

        // Reusing the request turn violates the replay uniqueness constraint
        // after the helper has tentatively inserted another assistant turn.
        // The encompassing transaction must roll that turn back.
        assert!(
            query_result_cache_repository::create_query_execution_replay(
                &fixture.state.persistence.postgres,
                &input,
            )
            .await
            .is_err()
        );
        let turns_after_failure = query_repository::list_turns_by_conversation(
            &fixture.state.persistence.postgres,
            fixture.conversation_id,
        )
        .await?;
        assert_eq!(turns_after_failure.len(), turns_after_success.len());
        let replay_count: i64 = sqlx::query_scalar(
            "select count(*) from query_execution_replay where request_turn_id = $1",
        )
        .bind(request_turn.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(replay_count, 1);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[cfg(feature = "test-support")]
async fn seed_service_replay_request(
    fixture: &QueryGroundingAppFixture,
    execution_id: Uuid,
    cache_key: &'static str,
) -> Result<query_result_cache_repository::CreateQueryExecutionReplayInput<'static>> {
    let pg = &fixture.state.persistence.postgres;
    let request = query_repository::create_turn(
        pg,
        &query_repository::NewQueryTurn {
            conversation_id: fixture.conversation_id,
            turn_kind: "user",
            author_principal_id: None,
            content_text: "Repeat the grounded value",
            execution_id: None,
        },
    )
    .await?;
    let generation = repositories::get_library_source_truth_version(pg, fixture.library_id).await?;
    query_result_cache_repository::upsert_query_result_cache_winner(
        pg,
        &query_result_cache_repository::UpsertQueryResultCacheInput {
            cache_key,
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            source_execution_id: execution_id,
            expected_source_truth_version: generation,
            readable_content_fingerprint: "content:service-replay",
            graph_projection_version: 0,
            graph_topology_generation: 0,
            binding_fingerprint: "bindings:service-replay",
            ttl_seconds: 300,
        },
    )
    .await?
    .context("service replay winner missing")?;
    Ok(query_result_cache_repository::CreateQueryExecutionReplayInput {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        conversation_id: fixture.conversation_id,
        request_turn_id: request.id,
        source_execution_id: execution_id,
        expected_source_truth_version: generation,
        cache_key,
        ttl_seconds: 300,
    })
}

#[cfg(feature = "test-support")]
async fn replay_materialization_counts(fixture: &QueryGroundingAppFixture) -> Result<(i64, i64)> {
    Ok(sqlx::query_as(
        "select (select count(*) from query_turn where conversation_id=$1 and turn_kind='assistant'),
                (select count(*) from query_execution_replay where conversation_id=$1)",
    ).bind(fixture.conversation_id).fetch_one(&fixture.state.persistence.postgres).await?)
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn cached_replay_service_returns_exact_selected_answer_and_audit() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let detail = fixture
            .create_replayable_execution_detail(
                "What value is supported?",
                "Selected source A grounded answer.",
            )
            .await?;
        let input = seed_service_replay_request(
            &fixture,
            detail.execution.id,
            "query_result:v3:service-exact-winner",
        )
        .await?;
        let before = replay_materialization_counts(&fixture).await?;
        let replay = fixture
            .state
            .application_services
            .query
            .replay_cached_answer_for_test(&fixture.state, &input)
            .await?
            .context("unchanged exact winner must replay through the owning service")?;
        assert_eq!(
            replay.response_turn.context("replayed assistant turn")?.content_text,
            "Selected source A grounded answer."
        );
        assert_eq!(replay.execution.id, detail.execution.id);
        assert!(!replay.technical_fact_references.is_empty());
        assert_eq!(replay_materialization_counts(&fixture).await?, (before.0 + 1, before.1 + 1));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn cached_replay_service_denies_retained_a_after_intent_and_bound_c() -> Result<()> {
    use ironrag_backend::infra::repositories::admission_repository::{
        self, ContentAdmissionRequest, ContentAdmissionTarget, RevisionAdmission,
        VisibilityAdmissionRequest,
    };
    use ironrag_contracts::documents::{CreateVisibilityIntentRequest, ReplacementIdentity};
    use sha2::{Digest, Sha256};
    use std::time::Duration;

    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let pg = &fixture.state.persistence.postgres;
        let detail = fixture
            .create_replayable_execution_detail(
                "What value is supported?",
                "Restricted A assistant bytes must not escape.",
            )
            .await?;
        let a_revision =
            detail.technical_fact_references.first().context("selected A fact")?.revision_id;
        let document_id: Uuid =
            sqlx::query_scalar("select document_id from content_revision where id=$1")
                .bind(a_revision)
                .fetch_one(pg)
                .await?;
        let input = seed_service_replay_request(
            &fixture,
            detail.execution.id,
            "query_result:v3:service-intent-race",
        )
        .await?;
        let before = replay_materialization_counts(&fixture).await?;
        let service = &fixture.state.application_services.query;
        let (reached, release) = service.pause_next_cache_replay(fixture.library_id).await;
        let state = fixture.state.clone();
        let replay_task = tokio::spawn(async move {
            state.application_services.query.replay_cached_answer_for_test(&state, &input).await
        });
        tokio::time::timeout(Duration::from_secs(10), reached).await??;
        // The real handler now retains A's hydrated detail and answer bytes,
        // outside the library lock. A clean C successor must not authorize A.
        let replacement = ReplacementIdentity {
            file_name: "public-c.txt".to_owned(),
            checksum: format!("sha256:{}", hex::encode(Sha256::digest([b'c'; 128]))),
            mime_type: "text/plain".to_owned(),
            byte_size: 128,
            title: Some("Public C".to_owned()),
            language_code: None,
            source_uri: None,
            document_hint: None,
        };
        let intent = admission_repository::admit_visibility_intent_with_failpoint(
            pg,
            &VisibilityAdmissionRequest {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id,
                requested_by_principal_id: None,
                request_surface: "rest".to_owned(),
                intent: CreateVisibilityIntentRequest {
                    idempotency_key: "service-replay-intent".to_owned(),
                    source_identity: "service-replay-source".to_owned(),
                    materialization_token: None,
                    replacement: replacement.clone(),
                },
            },
            None,
        )
        .await?
        .into_bundle();
        assert_eq!(
            repositories::content_fingerprint_repository::get_current_revision_id(pg, document_id)
                .await?,
            None
        );
        let c = admission_repository::admit_content_with_failpoint(
            pg,
            &ContentAdmissionRequest {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                operation_kind: "replace".to_owned(),
                requested_by_principal_id: None,
                request_surface: "rest".to_owned(),
                idempotency_key: Some("service-replay-c".to_owned()),
                source_identity: Some("service-replay-source".to_owned()),
                target: ContentAdmissionTarget::Existing { document_id },
                revision: Some(RevisionAdmission {
                    content_source_kind: "upload".to_owned(),
                    replacement,
                    visibility_intent_id: Some(intent.receipt.visibility_intent_id),
                    base_revision_id: None,
                    storage_key: None,
                }),
                parent_async_operation_id: None,
                priority: 100,
            },
            None,
        )
        .await?
        .into_bundle();
        let c_revision = c.revision.context("bound C revision")?.id;
        let c_item = c.item.context("bound C item")?.id;
        sqlx::query("update content_mutation set mutation_state='applied' where id=$1")
            .bind(c.mutation.id)
            .execute(pg)
            .await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1")
            .bind(c_item)
            .execute(pg)
            .await?;
        sqlx::query(
            "update knowledge_revision set text_state='text_readable',text_readable_at=now(),
            vector_state='ready',vector_ready_at=now(),graph_state='ready',graph_ready_at=now(),
            normalized_text='Public C source bytes' where revision_id=$1",
        )
        .bind(c_revision)
        .execute(pg)
        .await?;
        sqlx::query(
            "update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1",
        )
        .bind(document_id)
        .bind(c_revision)
        .execute(pg)
        .await?;
        assert_eq!(
            repositories::content_fingerprint_repository::get_current_revision_id(pg, document_id)
                .await?,
            Some(c_revision)
        );
        release.send(()).map_err(|_| anyhow!("selected replay dropped before release"))?;
        let replay = tokio::time::timeout(Duration::from_secs(10), replay_task).await???;
        assert!(replay.is_none(), "retained A assistant bytes must not be returned using public C");
        assert_eq!(
            replay_materialization_counts(&fixture).await?,
            before,
            "denied replay must add neither an assistant turn nor audit row"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn execution_sources_require_exact_execution_scope_and_canonical_bundle() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let pg = &fixture.state.persistence.postgres;
        let detail = fixture
            .create_replayable_execution_detail(
                "Inspect source provenance",
                "Grounded source answer",
            )
            .await?;
        assert!(
            query_result_cache_repository::execution_sources_are_current(
                pg,
                detail.execution.id,
                fixture.workspace_id,
                fixture.library_id,
            )
            .await?
        );
        for (execution_id, workspace_id, library_id) in [
            (Uuid::now_v7(), fixture.workspace_id, fixture.library_id),
            (detail.execution.id, Uuid::now_v7(), fixture.library_id),
            (detail.execution.id, fixture.workspace_id, Uuid::now_v7()),
        ] {
            assert!(
                !query_result_cache_repository::execution_sources_are_current(
                    pg,
                    execution_id,
                    workspace_id,
                    library_id,
                )
                .await?,
                "absent or wrong-scoped execution must not authorize a source read"
            );
        }
        sqlx::query("update knowledge_context_bundle set query_execution_id=null where query_execution_id=$1")
            .bind(detail.execution.id).execute(pg).await?;
        assert!(!query_result_cache_repository::execution_sources_are_current(
            pg, detail.execution.id, fixture.workspace_id, fixture.library_id,
        ).await?, "a detached bundle must not stand in for the execution's canonical bundle");
        sqlx::query("update knowledge_context_bundle set query_execution_id=$2 where bundle_id=$1")
            .bind(detail.execution.context_bundle_id).bind(detail.execution.id).execute(pg).await?;
        sqlx::query("delete from knowledge_context_bundle where query_execution_id=$1")
            .bind(detail.execution.id)
            .execute(pg)
            .await?;
        assert!(
            !query_result_cache_repository::execution_sources_are_current(
                pg,
                detail.execution.id,
                fixture.workspace_id,
                fixture.library_id,
            )
            .await?,
            "missing canonical bundle must not authorize an existing execution"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[cfg(feature = "test-support")]
async fn withdraw_replay_source(
    fixture: &QueryGroundingAppFixture,
    document_id: Uuid,
) -> Result<()> {
    use ironrag_backend::infra::repositories::admission_repository::{
        self, VisibilityAdmissionRequest,
    };
    use ironrag_contracts::documents::{CreateVisibilityIntentRequest, ReplacementIdentity};
    use sha2::{Digest, Sha256};
    admission_repository::admit_visibility_intent_with_failpoint(
        &fixture.state.persistence.postgres,
        &VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id,
            requested_by_principal_id: None,
            request_surface: "rest".to_owned(),
            intent: CreateVisibilityIntentRequest {
                idempotency_key: "withdraw-replay-source".to_owned(),
                source_identity: "withdraw-replay-source".to_owned(),
                materialization_token: None,
                replacement: ReplacementIdentity {
                    file_name: "replacement.txt".to_owned(),
                    checksum: format!("sha256:{}", hex::encode(Sha256::digest(b"replacement"))),
                    mime_type: "text/plain".to_owned(),
                    byte_size: 11,
                    title: None,
                    language_code: None,
                    source_uri: None,
                    document_hint: None,
                },
            },
        },
        None,
    )
    .await?;
    assert_eq!(
        repositories::content_fingerprint_repository::get_current_revision_id(
            &fixture.state.persistence.postgres,
            document_id,
        )
        .await?,
        None
    );
    Ok(())
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn execution_detail_http_denies_deleted_execution_after_hydration_and_withdrawal()
-> Result<()> {
    use axum::{
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use std::time::Duration;
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let pg = &fixture.state.persistence.postgres;
        let detail = fixture
            .create_replayable_execution_detail(
                "Inspect selected A",
                "Restricted A completed assistant answer",
            )
            .await?;
        let source_revision =
            detail.technical_fact_references.first().context("A fact")?.revision_id;
        let document_id: Uuid =
            sqlx::query_scalar("select document_id from content_revision where id=$1")
                .bind(source_revision)
                .fetch_one(pg)
                .await?;
        let actor =
            iam_repository::create_principal(pg, "user", "Execution read owner", None).await?;
        sqlx::query("update query_conversation set created_by_principal_id=$2 where id=$1")
            .bind(fixture.conversation_id)
            .bind(actor.id)
            .execute(pg)
            .await?;
        let auth = crate::iam_token_support::scoped_workspace_auth(
            actor.id,
            fixture.workspace_id,
            &["query_read", "library_read"],
        );
        let (reached, release) = fixture
            .state
            .application_services
            .query
            .pause_next_execution_read(fixture.library_id)
            .await;
        let state = fixture.state.clone();
        let execution_id = detail.execution.id;
        let read = tokio::spawn(async move {
            Box::pin(ironrag_backend::interfaces::http::query::get_execution(
                auth,
                State(state),
                Path(execution_id),
            ))
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), reached).await??;
        assert_eq!(
            query_repository::delete_ui_conversation(pg, fixture.conversation_id, actor.id, false,)
                .await?,
            query_repository::DeleteQueryConversationOutcome::Deleted
        );
        assert!(query_repository::get_execution_by_id(pg, execution_id).await?.is_none());
        withdraw_replay_source(&fixture, document_id).await?;
        release.send(()).map_err(|_| anyhow!("hydrated read ended before release"))?;
        let observed = tokio::time::timeout(Duration::from_secs(10), read).await??;
        let status =
            observed.map(|_| StatusCode::OK).unwrap_or_else(|error| error.into_response().status());
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "deleted execution must not return the retained restricted assistant answer"
        );
        assert_eq!(replay_materialization_counts(&fixture).await?, (0, 0));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn execution_detail_denies_withdrawn_expanded_fact_support_blocks() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let pg = &fixture.state.persistence.postgres;
        let source = fixture.create_replayable_execution_detail(
            "Restricted supporting source", "Restricted supporting answer",
        ).await?;
        let restricted_revision = source.technical_fact_references.first().context("supporting fact")?.revision_id;
        let document_id: Uuid = sqlx::query_scalar("select document_id from content_revision where id=$1")
            .bind(restricted_revision).fetch_one(pg).await?;
        let current = fixture.create_replayable_execution_detail(
            "Current selected owner", "Current owner answer",
        ).await?;
        let fact_id = current.technical_fact_references.first().context("current fact")?.fact_id;
        let block_id = Uuid::now_v7();
        sqlx::query("insert into knowledge_structured_block (block_id,workspace_id,library_id,
            document_id,revision_id,ordinal,block_kind,text,normalized_text,heading_trail,created_at,updated_at)
            values ($1,$2,$3,$4,$5,0,'paragraph','Restricted expanded source text',
            'Restricted expanded source text',array['Restricted expanded source heading'],now(),now())")
            .bind(block_id).bind(fixture.workspace_id).bind(fixture.library_id)
            .bind(document_id).bind(restricted_revision).execute(pg).await?;
        // Array support identifiers have no same-owner FK. Corrupt one retained
        // fact while its own canonical revision remains current.
        sqlx::query("update knowledge_technical_fact set support_block_ids=array[$2::uuid] where fact_id=$1")
            .bind(fact_id).bind(block_id).execute(pg).await?;
        let before = fixture.state.application_services.query.get_execution(
            &fixture.state, current.execution.id,
        ).await?;
        assert!(before.prepared_segment_references.iter().any(|reference| {
            reference.segment_id == block_id && reference.heading_trail.iter().any(
                |heading| heading == "Restricted expanded source heading")
        }), "real hydration must expand the corrupted support-block reference");
        withdraw_replay_source(&fixture, document_id).await?;
        assert!(!query_result_cache_repository::execution_sources_are_current(
            pg, current.execution.id, fixture.workspace_id, fixture.library_id,
        ).await?, "current fact owner cannot authorize a withdrawn expanded support block");
        assert!(fixture.state.application_services.query.get_execution(
            &fixture.state, current.execution.id,
        ).await.is_err(), "expanded stale source must not reach execution detail");
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn cached_replay_rechecks_exact_db_winner_and_ttl_before_materializing() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let first = fixture
            .create_replayable_execution_detail("First cache source?", "First grounded answer.")
            .await?;
        let second = fixture
            .create_replayable_execution_detail("Second cache source?", "Second grounded answer.")
            .await?;
        let source_truth_version = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let cache_key = "query_result:v3:replay-winner-race";
        let winner_input =
            |source_execution_id| query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:replay-winner-race",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:replay-winner-race",
                ttl_seconds: 300,
            };
        query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &winner_input(first.execution.id),
        )
        .await?
        .context("failed to persist first race winner")?;
        let observed = query_result_cache_repository::get_query_result_cache(
            &fixture.state.persistence.postgres,
            cache_key,
            300,
        )
        .await?
        .context("failed to observe first race winner")?;
        assert_eq!(observed.source_execution_id, first.execution.id);

        sqlx::query(
            "update query_result_cache
             set updated_at = clock_timestamp() - interval '301 seconds'
             where cache_key = $1",
        )
        .bind(cache_key)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &winner_input(second.execution.id),
        )
        .await?
        .context("failed to replace race winner")?;

        let stale_request = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: fixture.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Do not replay the stale winner.",
                execution_id: None,
            },
        )
        .await?;
        let turns_before_stale_replay = query_repository::list_turns_by_conversation(
            &fixture.state.persistence.postgres,
            fixture.conversation_id,
        )
        .await?
        .len();
        let stale_replay = query_result_cache_repository::create_query_execution_replay(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::CreateQueryExecutionReplayInput {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                conversation_id: fixture.conversation_id,
                request_turn_id: stale_request.id,
                source_execution_id: first.execution.id,
                expected_source_truth_version: source_truth_version,
                cache_key,
                ttl_seconds: 300,
            },
        )
        .await?;
        assert!(stale_replay.is_none(), "a replaced winner must not be replayed");
        assert_eq!(
            query_repository::list_turns_by_conversation(
                &fixture.state.persistence.postgres,
                fixture.conversation_id,
            )
            .await?
            .len(),
            turns_before_stale_replay,
            "a rejected stale winner must not leave an assistant turn",
        );

        sqlx::query(
            "update query_result_cache
             set updated_at = clock_timestamp() - interval '301 seconds'
             where cache_key = $1",
        )
        .bind(cache_key)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        let expired_request = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: fixture.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Do not replay the expired winner.",
                execution_id: None,
            },
        )
        .await?;
        let expired_replay = query_result_cache_repository::create_query_execution_replay(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::CreateQueryExecutionReplayInput {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                conversation_id: fixture.conversation_id,
                request_turn_id: expired_request.id,
                source_execution_id: second.execution.id,
                expected_source_truth_version: source_truth_version,
                cache_key,
                ttl_seconds: 300,
            },
        )
        .await?;
        assert!(expired_replay.is_none(), "an expired winner must not be replayed");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn parallel_cached_replays_in_one_conversation_serialize_without_deadlock() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = Box::pin(async {
        let source = fixture
            .create_replayable_execution_detail(
                "Which setting is enabled?",
                "The grounded setting is enabled.",
            )
            .await?;
        let source_truth_version = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let cache_key = "query_result:v3:parallel-replay-lock-order";
        query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: source.execution.id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:parallel-replay",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:parallel-replay",
                ttl_seconds: 300,
            },
        )
        .await?
        .context("failed to persist parallel replay winner")?;
        let first_request = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: fixture.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "First parallel replay request.",
                execution_id: None,
            },
        )
        .await?;
        let second_request = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: fixture.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Second parallel replay request.",
                execution_id: None,
            },
        )
        .await?;

        let first_input = query_result_cache_repository::CreateQueryExecutionReplayInput {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            conversation_id: fixture.conversation_id,
            request_turn_id: first_request.id,
            source_execution_id: source.execution.id,
            expected_source_truth_version: source_truth_version,
            cache_key,
            ttl_seconds: 300,
        };
        let second_input = query_result_cache_repository::CreateQueryExecutionReplayInput {
            request_turn_id: second_request.id,
            ..first_input.clone()
        };
        let first_replay = query_result_cache_repository::create_query_execution_replay(
            &fixture.state.persistence.postgres,
            &first_input,
        );
        let second_replay = query_result_cache_repository::create_query_execution_replay(
            &fixture.state.persistence.postgres,
            &second_input,
        );
        let (first, second) =
            Box::pin(tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(first_replay, second_replay)
            }))
            .await
            .context(
                "parallel replay transactions timed out, likely due to a lock-order deadlock",
            )?;
        let (first_turn, _) = first?.context("first parallel replay was rejected")?;
        let (second_turn, _) = second?.context("second parallel replay was rejected")?;
        assert_ne!(first_turn.id, second_turn.id);
        assert_ne!(first_turn.turn_index, second_turn.turn_index);
        assert_eq!(first_turn.content_text, "The grounded setting is enabled.");
        assert_eq!(second_turn.content_text, "The grounded setting is enabled.");

        Ok(())
    })
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn cached_replay_rechecks_terminal_verification_and_grounding_in_transaction() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let source = fixture
            .create_replayable_execution_detail(
                "Which neutral option is supported?",
                "The supported option is enabled.",
            )
            .await?;
        let source_truth_version = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let cache_key = "query_result:v3:terminal-replay-fence";
        query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: source.execution.id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:terminal-replay-fence",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:terminal-replay-fence",
                ttl_seconds: 300,
            },
        )
        .await?
        .context("failed to persist terminal replay winner")?;

        sqlx::query(
            "update knowledge_context_bundle
             set verification_state = 'failed', updated_at = clock_timestamp()
             where bundle_id = $1",
        )
        .bind(source.execution.context_bundle_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        let downgraded_request = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: fixture.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Do not replay downgraded verification.",
                execution_id: None,
            },
        )
        .await?;
        let downgraded_input = query_result_cache_repository::CreateQueryExecutionReplayInput {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            conversation_id: fixture.conversation_id,
            request_turn_id: downgraded_request.id,
            source_execution_id: source.execution.id,
            expected_source_truth_version: source_truth_version,
            cache_key,
            ttl_seconds: 300,
        };
        let downgraded = query_result_cache_repository::create_query_execution_replay(
            &fixture.state.persistence.postgres,
            &downgraded_input,
        )
        .await?;
        assert!(downgraded.is_none());

        sqlx::query(
            "update knowledge_context_bundle
             set verification_state = 'verified', updated_at = clock_timestamp()
             where bundle_id = $1",
        )
        .bind(source.execution.context_bundle_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        sqlx::query(
            "delete from knowledge_technical_fact as fact
             using knowledge_context_bundle as bundle
             where bundle.bundle_id = $1
               and fact.fact_id = any(bundle.selected_fact_ids)",
        )
        .bind(source.execution.context_bundle_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        let ungrounded_request = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: fixture.conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Do not replay deleted grounding.",
                execution_id: None,
            },
        )
        .await?;
        let ungrounded_input = query_result_cache_repository::CreateQueryExecutionReplayInput {
            request_turn_id: ungrounded_request.id,
            ..downgraded_input
        };
        let ungrounded = query_result_cache_repository::create_query_execution_replay(
            &fixture.state.persistence.postgres,
            &ungrounded_input,
        )
        .await?;
        assert!(ungrounded.is_none());

        let replay_count: i64 = sqlx::query_scalar(
            "select count(*)::bigint
             from query_execution_replay
             where cache_key = $1",
        )
        .bind(cache_key)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(replay_count, 0);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn freshly_completed_mcp_conversation_survives_a_sibling_prune() -> Result<()> {
    // The race this pins down: under concurrent load, turn A's child
    // grounded-answer conversation completes, and turn B's tool call (which
    // protects only ITS own conversation) prunes the MCP overflow before
    // turn A's parent has attached the child's grounding. The cascade takes
    // the child's `query_execution` row with it, and turn A answers with an
    // empty reference list. Measured on stage: 43 of 47 child executions
    // orphaned in one N=100 window, 9 answers without citations.
    //
    // A completed conversation must therefore survive pruning for a grace
    // period after its last runtime completion — the same shape of guard the
    // empty-conversation branch already has for the creation window.
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("mcp-grace-workspace-{suffix}"),
            "MCP Grace Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("mcp-grace-library-{suffix}"),
            "MCP Grace Library",
            None,
            None,
        )
        .await?;

        // Three siblings past the grace period, two that completed seconds
        // ago, and the pruning caller's own protected conversation.
        let mut old = Vec::new();
        for age_seconds in [1400, 1300, 1200] {
            old.push(
                seed_completed_mcp_conversation(
                    &fixture.postgres,
                    workspace.id,
                    library.id,
                    age_seconds,
                )
                .await?,
            );
        }
        let mut fresh = Vec::new();
        for age_seconds in [3, 2] {
            fresh.push(
                seed_completed_mcp_conversation(
                    &fixture.postgres,
                    workspace.id,
                    library.id,
                    age_seconds,
                )
                .await?,
            );
        }
        let (protected_conversation_id, _) =
            seed_completed_mcp_conversation(&fixture.postgres, workspace.id, library.id, 1).await?;

        // Cap far below the row count: without the grace guard the two fresh
        // siblings are inside the oldest-first overflow and get deleted.
        let deleted = query_repository::prune_mcp_conversation_overflow(
            &fixture.postgres,
            library.id,
            2,
            protected_conversation_id,
        )
        .await?;
        assert_eq!(deleted, 3, "only conversations past the completion grace period are prunable");

        for (conversation_id, execution_id) in &fresh {
            assert!(
                query_repository::get_conversation_by_id(&fixture.postgres, *conversation_id)
                    .await?
                    .is_some(),
                "a freshly completed sibling conversation must survive the prune"
            );
            assert!(
                query_repository::get_execution_scope_by_id(&fixture.postgres, *execution_id)
                    .await?
                    .is_some(),
                "the fresh child execution row a parent still attaches grounding from must survive"
            );
        }
        for (conversation_id, _) in &old {
            assert!(
                query_repository::get_conversation_by_id(&fixture.postgres, *conversation_id)
                    .await?
                    .is_none(),
                "conversations past the grace period must still be pruned"
            );
        }

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn aged_completed_mcp_overflow_is_pruned_without_deleting_durable_audit() -> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("mcp-retention-workspace-{suffix}"),
            "MCP Retention Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("mcp-retention-library-{suffix}"),
            "MCP Retention Library",
            None,
            None,
        )
        .await?;

        let mut conversations = Vec::new();
        // All four completed beyond the 10-minute grace period, so the cap
        // is the only thing deciding which of them survive.
        for age_seconds in [1500, 1400, 1300, 1200] {
            conversations.push(
                seed_completed_mcp_conversation(
                    &fixture.postgres,
                    workspace.id,
                    library.id,
                    age_seconds,
                )
                .await?,
            );
        }
        let (oldest_conversation_id, oldest_execution_id) = conversations[0];
        let protected_conversation_id = conversations[3].0;
        let audit_event_id = Uuid::now_v7();
        sqlx::query(
            "insert into audit_event (
                id, surface_kind, action_kind, result_kind, redacted_message, internal_message
             ) values (
                $1, 'mcp'::surface_kind, 'query.execution.run', 'succeeded'::audit_result_kind,
                'retention proof', 'retention proof'
             )",
        )
        .bind(audit_event_id)
        .execute(&fixture.postgres)
        .await?;
        sqlx::query(
            "insert into audit_event_subject (
                audit_event_id, subject_kind, subject_id, workspace_id, library_id
             ) values ($1, 'query_execution', $2, $3, $4)",
        )
        .bind(audit_event_id)
        .bind(oldest_execution_id)
        .bind(workspace.id)
        .bind(library.id)
        .execute(&fixture.postgres)
        .await?;

        let deleted = query_repository::prune_mcp_conversation_overflow(
            &fixture.postgres,
            library.id,
            2,
            protected_conversation_id,
        )
        .await?;

        assert_eq!(deleted, 2, "completed rows past the grace period are bounded by the cap");
        let retained_count: i64 = sqlx::query_scalar(
            "select count(*)::bigint
             from query_conversation
             where library_id = $1 and request_surface = 'mcp'",
        )
        .bind(library.id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(retained_count, 2);
        assert!(
            query_repository::get_conversation_by_id(&fixture.postgres, oldest_conversation_id,)
                .await?
                .is_none()
        );
        assert!(
            query_repository::get_conversation_by_id(&fixture.postgres, protected_conversation_id,)
                .await?
                .is_some()
        );
        let audit_survived: bool = sqlx::query_scalar(
            "select exists(
                 select 1
                 from audit_event_subject
                 where audit_event_id = $1
                   and subject_kind = 'query_execution'
                   and subject_id = $2
             )",
        )
        .bind(audit_event_id)
        .bind(oldest_execution_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert!(audit_survived, "query storage retention must not cascade into durable audit");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn ui_conversation_rename_and_delete_are_owner_guarded_and_durable() -> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("session-mutation-workspace-{suffix}"),
            "Session Mutation Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("session-mutation-library-{suffix}"),
            "Session Mutation Library",
            None,
            None,
        )
        .await?;
        let owner = iam_repository::create_principal(
            &fixture.postgres,
            "user",
            "Session Mutation Owner",
            None,
        )
        .await?;
        let conversation = query_repository::create_conversation(
            &fixture.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: workspace.id,
                library_id: library.id,
                created_by_principal_id: Some(owner.id),
                title: Some("Initial title"),
                conversation_state: "active",
                request_surface: "ui",
            },
            8,
        )
        .await?;

        let foreign_principal_id = Uuid::now_v7();
        assert!(
            query_repository::rename_ui_conversation(
                &fixture.postgres,
                conversation.id,
                foreign_principal_id,
                false,
                "Foreign title",
            )
            .await?
            .is_none(),
        );
        let renamed = query_repository::rename_ui_conversation(
            &fixture.postgres,
            conversation.id,
            owner.id,
            false,
            "Durable title",
        )
        .await?
        .context("owned conversation was not renamed")?;
        assert_eq!(renamed.title.as_deref(), Some("Durable title"));
        let after_auto_title = query_repository::initialize_conversation_title(
            &fixture.postgres,
            conversation.id,
            "Automatic title",
        )
        .await?;
        assert_eq!(after_auto_title.title.as_deref(), Some("Durable title"));

        assert_eq!(
            query_repository::delete_ui_conversation(
                &fixture.postgres,
                conversation.id,
                foreign_principal_id,
                false,
            )
            .await?,
            query_repository::DeleteQueryConversationOutcome::NotFoundOrForbidden,
        );
        assert_eq!(
            query_repository::delete_ui_conversation(
                &fixture.postgres,
                conversation.id,
                owner.id,
                false,
            )
            .await?,
            query_repository::DeleteQueryConversationOutcome::Deleted,
        );
        assert!(
            query_repository::get_conversation_by_id(&fixture.postgres, conversation.id)
                .await?
                .is_none(),
        );

        let mcp_conversation = query_repository::create_conversation(
            &fixture.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: workspace.id,
                library_id: library.id,
                created_by_principal_id: Some(owner.id),
                title: Some("Tool execution"),
                conversation_state: "active",
                request_surface: "mcp",
            },
            8,
        )
        .await?;
        assert!(
            query_repository::rename_ui_conversation(
                &fixture.postgres,
                mcp_conversation.id,
                owner.id,
                true,
                "UI title",
            )
            .await?
            .is_none(),
        );
        assert_eq!(
            query_repository::delete_ui_conversation(
                &fixture.postgres,
                mcp_conversation.id,
                owner.id,
                true,
            )
            .await?,
            query_repository::DeleteQueryConversationOutcome::NotFoundOrForbidden,
        );
        assert!(
            query_repository::get_conversation_by_id(&fixture.postgres, mcp_conversation.id)
                .await?
                .is_some(),
        );

        let active_conversation = query_repository::create_conversation(
            &fixture.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: workspace.id,
                library_id: library.id,
                created_by_principal_id: Some(owner.id),
                title: Some("Active execution"),
                conversation_state: "active",
                request_surface: "ui",
            },
            8,
        )
        .await?;
        let mismatched_execution_id = Uuid::now_v7();
        let mismatched_runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &fixture.postgres,
            &runtime_repository::NewRuntimeExecution {
                id: mismatched_runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: Uuid::now_v7(),
                task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
                surface_kind: "ui",
                contract_name: "query_answer",
                contract_version: "owner-integrity-test",
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
        .await?;
        let mismatched_owner_result = query_repository::create_execution(
            &fixture.postgres,
            &query_repository::NewQueryExecution {
                execution_id: mismatched_execution_id,
                context_bundle_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                conversation_id: active_conversation.id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id: mismatched_runtime_execution_id,
                query_text: "Owner integrity fixture",
                failure_code: None,
            },
        )
        .await;
        assert!(matches!(mismatched_owner_result, Err(sqlx::Error::RowNotFound)));
        let mismatched_query_exists: bool =
            sqlx::query_scalar("select exists(select 1 from query_execution where id = $1)")
                .bind(mismatched_execution_id)
                .fetch_one(&fixture.postgres)
                .await?;
        assert!(!mismatched_query_exists);

        let execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &fixture.postgres,
            &runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: execution_id,
                task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
                surface_kind: "ui",
                contract_name: "query_answer",
                contract_version: "session-mutation-test",
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
        .await?;
        query_repository::create_execution(
            &fixture.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                conversation_id: active_conversation.id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text: "Neutral lifecycle fixture",
                failure_code: None,
            },
        )
        .await?;
        assert_eq!(
            query_repository::delete_ui_conversation(
                &fixture.postgres,
                active_conversation.id,
                owner.id,
                false,
            )
            .await?,
            query_repository::DeleteQueryConversationOutcome::ActiveExecution,
        );
        runtime_repository::update_runtime_execution(
            &fixture.postgres,
            runtime_execution_id,
            &runtime_repository::UpdateRuntimeExecution {
                lifecycle_state: RuntimeLifecycleState::Completed.as_str(),
                active_stage: None,
                turn_count: 0,
                failure_code: None,
                failure_summary_redacted: None,
                completed_at: Some(Utc::now()),
            },
        )
        .await?;
        assert_eq!(
            query_repository::delete_ui_conversation(
                &fixture.postgres,
                active_conversation.id,
                owner.id,
                false,
            )
            .await?,
            query_repository::DeleteQueryConversationOutcome::Deleted,
        );
        let query_execution_survived: bool =
            sqlx::query_scalar("select exists(select 1 from query_execution where id = $1)")
                .bind(execution_id)
                .fetch_one(&fixture.postgres)
                .await?;
        assert!(!query_execution_survived, "query-owned execution must cascade");
        let runtime_execution_survived: bool =
            sqlx::query_scalar("select exists(select 1 from runtime_execution where id = $1)")
                .bind(runtime_execution_id)
                .fetch_one(&fixture.postgres)
                .await?;
        assert!(runtime_execution_survived, "independent runtime retention must survive");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn concurrent_mcp_retention_enforcement_is_serialized_and_bounded() -> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("mcp-retention-race-workspace-{suffix}"),
            "MCP Retention Race Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("mcp-retention-race-library-{suffix}"),
            "MCP Retention Race Library",
            None,
            None,
        )
        .await?;

        let mut conversations = Vec::new();
        // Aged past the completion grace period: this test is about the
        // serialization and boundedness of concurrent pruning, so every row
        // must actually be prunable.
        for age_seconds in (1201..=1208).rev() {
            conversations.push(
                seed_completed_mcp_conversation(
                    &fixture.postgres,
                    workspace.id,
                    library.id,
                    age_seconds,
                )
                .await?,
            );
        }
        let protected_conversation_id = conversations
            .last()
            .map(|row| row.0)
            .context("MCP retention fixture did not create a conversation")?;

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let postgres = fixture.postgres.clone();
            tasks.spawn(async move {
                query_repository::prune_mcp_conversation_overflow(
                    &postgres,
                    library.id,
                    3,
                    protected_conversation_id,
                )
                .await
            });
        }
        let mut deleted = 0_u64;
        while let Some(result) = tasks.join_next().await {
            deleted = deleted.saturating_add(result.context("retention task panicked")??);
        }

        assert_eq!(deleted, 5);
        let retained_count: i64 = sqlx::query_scalar(
            "select count(*)::bigint
             from query_conversation
             where library_id = $1 and request_surface = 'mcp'",
        )
        .bind(library.id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(retained_count, 3);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn replay_provenance_keeps_external_source_conversation_from_eviction() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let source = fixture
            .create_replayable_execution_detail(
                "Which provenance value is authoritative?",
                "The authoritative provenance value is enabled.",
            )
            .await?;
        let target_conversation = query_repository::create_conversation(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                created_by_principal_id: None,
                title: Some("Replay provenance target"),
                conversation_state: "active",
                request_surface: "ui",
            },
            64,
        )
        .await?;
        let target_request = query_repository::create_turn(
            &fixture.state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: target_conversation.id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Replay with durable provenance.",
                execution_id: None,
            },
        )
        .await?;
        let source_truth_version = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let cache_key = "query_result:v3:durable-replay-provenance";
        query_result_cache_repository::upsert_query_result_cache_winner(
            &fixture.state.persistence.postgres,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: source.execution.id,
                expected_source_truth_version: source_truth_version,
                readable_content_fingerprint: "content:durable-replay-provenance",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "bindings:durable-replay-provenance",
                ttl_seconds: 300,
            },
        )
        .await?
        .context("failed to persist provenance cache winner")?;
        let (response_turn, replay) =
            query_result_cache_repository::create_query_execution_replay(
                &fixture.state.persistence.postgres,
                &query_result_cache_repository::CreateQueryExecutionReplayInput {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    conversation_id: target_conversation.id,
                    request_turn_id: target_request.id,
                    source_execution_id: source.execution.id,
                    expected_source_truth_version: source_truth_version,
                    cache_key,
                    ttl_seconds: 300,
                },
            )
            .await?
            .context("external replay was rejected")?;

        let delete_source = query_repository::delete_ui_conversation(
            &fixture.state.persistence.postgres,
            fixture.conversation_id,
            Uuid::now_v7(),
            true,
        )
        .await?;
        assert_eq!(
            delete_source,
            query_repository::DeleteQueryConversationOutcome::RetainedByExternalReplay,
            "source deletion must be restricted while another conversation retains replay provenance",
        );
        assert!(
            query_repository::get_conversation_by_id(
                &fixture.state.persistence.postgres,
                fixture.conversation_id,
            )
            .await?
            .is_some()
        );
        let target_turn = query_repository::get_turn_by_id(
            &fixture.state.persistence.postgres,
            response_turn.id,
        )
        .await?
        .context("target replay response disappeared")?;
        assert_eq!(target_turn.content_text, "The authoritative provenance value is enabled.");
        let replay_still_present: bool = sqlx::query_scalar(
            "select exists(select 1 from query_execution_replay where id = $1)",
        )
        .bind(replay.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert!(replay_still_present);
        assert!(
            fixture
                .state
                .context_store
                .get_bundle_reference_set_by_query_execution(source.execution.id)
                .await?
                .is_some(),
            "source grounding evidence must remain hydratable",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn interrupted_query_execution_cleanup_is_atomic_and_idempotent() -> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("interrupted-query-workspace-{suffix}"),
            "Interrupted Query Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("interrupted-query-library-{suffix}"),
            "Interrupted Query Library",
            None,
            None,
        )
        .await?;
        let conversation = query_repository::create_conversation(
            &fixture.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: workspace.id,
                library_id: library.id,
                created_by_principal_id: None,
                title: Some("Interrupted Query"),
                conversation_state: "active",
                request_surface: "mcp",
            },
            4,
        )
        .await?;
        let request_turn = query_repository::create_turn(
            &fixture.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: conversation.id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "Summarize the available evidence.",
                execution_id: None,
            },
        )
        .await?;
        let execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &fixture.postgres,
            &runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: execution_id,
                task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
                surface_kind: "mcp",
                contract_name: "query_answer",
                contract_version: "1",
                lifecycle_state: RuntimeLifecycleState::Running.as_str(),
                active_stage: Some(RuntimeStageKind::Retrieve.as_str()),
                turn_budget: 4,
                turn_count: 1,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                parent_execution_id: None,
            },
        )
        .await?;
        query_repository::create_execution(
            &fixture.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: deterministic_context_bundle_id(execution_id),
                workspace_id: workspace.id,
                library_id: library.id,
                conversation_id: conversation.id,
                request_turn_id: Some(request_turn.id),
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text: "Summarize the available evidence.",
                failure_code: None,
            },
        )
        .await?;
        let operation = ops_repository::create_async_operation(
            &fixture.postgres,
            &ops_repository::NewOpsAsyncOperation {
                workspace_id: workspace.id,
                library_id: Some(library.id),
                operation_kind: "query_execution",
                surface_kind: "mcp",
                requested_by_principal_id: None,
                status: "processing",
                subject_kind: "query_execution",
                subject_id: Some(execution_id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await?;

        sqlx::query("update runtime_execution set accepted_at = $2 where id = $1")
            .bind(runtime_execution_id)
            .bind(Utc::now() - chrono::Duration::minutes(10))
            .execute(&fixture.postgres)
            .await?;

        assert_eq!(
            query_repository::reap_stale_query_executions(
                &fixture.postgres,
                Utc::now() - chrono::Duration::minutes(5),
                10,
            )
            .await?,
            1,
        );
        assert_eq!(
            query_repository::reap_stale_query_executions(
                &fixture.postgres,
                Utc::now() - chrono::Duration::minutes(5),
                10,
            )
            .await?,
            0,
        );
        assert!(
            !query_repository::cancel_interrupted_execution(
                &fixture.postgres,
                execution_id,
                runtime_execution_id,
                operation.id,
            )
            .await?
        );

        let execution = query_repository::get_execution_by_id(&fixture.postgres, execution_id)
            .await?
            .context("interrupted query execution missing")?;
        let runtime = runtime_repository::get_runtime_execution_by_id(
            &fixture.postgres,
            runtime_execution_id,
        )
        .await?
        .context("interrupted runtime execution missing")?;
        let operation = ops_repository::get_async_operation_by_id(&fixture.postgres, operation.id)
            .await?
            .context("interrupted async operation missing")?;
        assert_eq!(execution.failure_code.as_deref(), Some("query_execution_interrupted"));
        let persisted_query_failure_code: Option<String> =
            sqlx::query_scalar("select failure_code from query_execution where id = $1")
                .bind(execution_id)
                .fetch_one(&fixture.postgres)
                .await?;
        assert_eq!(persisted_query_failure_code, None);
        let query_completion_column_exists: bool = sqlx::query_scalar(
            "select exists(
                select 1
                from information_schema.columns
                where table_schema = 'public'
                  and table_name = 'query_execution'
                  and column_name = 'completed_at'
             )",
        )
        .fetch_one(&fixture.postgres)
        .await?;
        assert!(!query_completion_column_exists);
        assert_eq!(runtime.lifecycle_state, RuntimeLifecycleState::Canceled);
        assert_eq!(runtime.active_stage, None);
        assert_eq!(runtime.failure_code.as_deref(), Some("query_execution_interrupted"));
        assert!(runtime.completed_at.is_some());
        assert_eq!(operation.status, "canceled");
        assert_eq!(operation.failure_code.as_deref(), Some("query_execution_interrupted"));
        assert!(operation.completed_at.is_some());
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn entity_neighborhood_filters_out_context_bundle_vertices() -> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        // A real catalog library is required: `runtime_graph_node.library_id`
        // has a foreign key to `catalog_library`.
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("entity-neighborhood-workspace-{suffix}"),
            "Entity Neighborhood Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("entity-neighborhood-library-{suffix}"),
            "Entity Neighborhood Library",
            None,
            None,
        )
        .await?;
        let workspace_id = workspace.id;
        let library_id = library.id;
        let execution_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let execution = sample_query_execution(
            workspace_id,
            library_id,
            execution_id,
            "Which neighbors should stay inside the domain graph?",
        );
        let bundle = sample_context_bundle(workspace_id, library_id, &execution);

        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id,
                workspace_id,
                library_id,
                primary_label: "Paging Parameter".to_string(),
                aliases: vec!["pageSize".to_string()],
                entity_type: "parameter".to_string(),
                entity_sub_type: None,
                summary: Some("Pagination parameter surfaced for regression coverage.".to_string()),
                confidence: Some(0.99),
                support_count: 1,
                freshness_generation: 1,
                entity_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .await
            .context("failed to seed entity for traversal regression")?;

        // `list_entity_neighborhood` traverses the runtime graph projection, so
        // the domain entity must exist as a `runtime_graph_node` for the
        // traversal to surface it (and for the bundle entity edge below to
        // persist). The knowledge-plane entity above is not read by traversal.
        seed_runtime_graph_entity_node(&fixture.postgres, entity_id, library_id, 1)
            .await
            .context("failed to seed runtime graph node for traversal regression")?;

        fixture
            .context_store
            .upsert_bundle(&bundle)
            .await
            .context("failed to persist traversal regression bundle")?;
        fixture
            .context_store
            .replace_bundle_entity_edges(
                bundle.bundle_id,
                library_id,
                &[sample_entity_edge(bundle.bundle_id, entity_id)],
            )
            .await
            .context("failed to persist traversal regression bundle edge")?;

        let rows = fixture
            .graph_store
            .list_entity_neighborhood(entity_id, library_id, 2, 16)
            .await
            .context("failed to list entity neighborhood after bundle edge insert")?;

        assert_eq!(rows.len(), 1, "service should keep only domain vertices in traversal rows");
        assert_eq!(rows[0].vertex_kind, "knowledge_entity");
        assert_eq!(rows[0].vertex_id, entity_id);
        assert_eq!(rows[0].path_length, 0);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn execution_detail_maps_authoritative_verification_states_for_grounding_regressions()
-> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let cases = [
            (
                "What is the exact endpoint path for the status call?",
                "insufficient_evidence",
                json!([{
                    "code": "unsupported_literal",
                    "message": "Literal `/api/status` is not grounded in selected evidence.",
                    "relatedSegmentId": null,
                    "relatedFactId": null
                }]),
                QueryVerificationState::InsufficientEvidence,
                Some("unsupported_literal"),
            ),
            (
                "Is there a GraphQL API in this library?",
                "verified",
                json!([]),
                QueryVerificationState::Verified,
                None,
            ),
            (
                "Which port is authoritative for the service?",
                "conflicting_evidence",
                json!([{
                    "code": "conflicting_evidence",
                    "message": "Selected evidence contains 2 conflicting technical fact group(s).",
                    "relatedSegmentId": null,
                    "relatedFactId": null
                }]),
                QueryVerificationState::Conflicting,
                Some("conflicting_evidence"),
            ),
            (
                "Compare the REST and SOAP endpoints across both documents.",
                "partially_supported",
                json!([{
                    "code": "multi_document_skew",
                    "message": "Only one of two referenced documents supplied authoritative endpoint facts.",
                    "relatedSegmentId": null,
                    "relatedFactId": null
                }]),
                QueryVerificationState::PartiallySupported,
                Some("multi_document_skew"),
            ),
        ];

        for (
            query_text,
            verification_state,
            verification_warnings,
            expected_state,
            expected_warning_code,
        ) in cases
        {
            let detail = fixture
                .create_execution_detail(query_text, verification_state, verification_warnings)
                .await?;
            assert_eq!(detail.execution.query_text, query_text);
            assert_eq!(detail.execution.context_bundle_id, deterministic_context_bundle_id(detail.execution.id));
            assert_eq!(detail.verification_state, expected_state);
            match expected_warning_code {
                Some(code) => assert_eq!(
                    detail.verification_warnings.first().map(|warning| warning.code.as_str()),
                    Some(code)
                ),
                None => assert!(detail.verification_warnings.is_empty()),
            }
        }

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn execution_detail_surfaces_noisy_layout_segments_and_technical_facts() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let relation_id = Uuid::now_v7();

        let parameter_block = sample_structured_block_row(StructuredBlockRowSeed {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id,
            revision_id,
            ordinal: 0,
            block_kind: "table_row",
            text: "pageNu mber | pageS ize | withCar ds | number_start ing",
            heading_trail: vec!["Accounts".to_string(), "Pagination".to_string()],
            section_path: vec!["accounts".to_string(), "pagination".to_string()],
        });
        let technical_facts = vec![
            sample_technical_fact_row(TechnicalFactRowSeed {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id,
                revision_id,
                fact_kind: TechnicalFactKind::ParameterName,
                normalized_value: "pageNumber",
                display_value: "pageNumber",
                support_block_ids: vec![parameter_block.block_id],
                support_chunk_ids: vec![chunk_id],
            }),
            sample_technical_fact_row(TechnicalFactRowSeed {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id,
                revision_id,
                fact_kind: TechnicalFactKind::ParameterName,
                normalized_value: "pageSize",
                display_value: "pageSize",
                support_block_ids: vec![parameter_block.block_id],
                support_chunk_ids: vec![chunk_id],
            }),
            sample_technical_fact_row(TechnicalFactRowSeed {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id,
                revision_id,
                fact_kind: TechnicalFactKind::ParameterName,
                normalized_value: "withCards",
                display_value: "withCards",
                support_block_ids: vec![parameter_block.block_id],
                support_chunk_ids: vec![chunk_id],
            }),
        ];

        let detail = fixture
            .create_execution_detail_with_authoritative_evidence(AuthoritativeEvidenceSeed {
                query_text: "List the pageNumber, pageSize, and withCards parameters.",
                verification_state: "verified",
                verification_warnings: json!([]),
                chunk_ids: vec![chunk_id],
                entity_ids: vec![entity_id],
                relation_ids: vec![relation_id],
                structured_blocks: vec![parameter_block.clone()],
                technical_facts: technical_facts.clone(),
            })
            .await?;

        assert_eq!(detail.verification_state, QueryVerificationState::Verified);
        assert_eq!(detail.prepared_segment_references.len(), 1);
        assert_eq!(detail.prepared_segment_references[0].segment_id, parameter_block.block_id);
        assert_eq!(detail.technical_fact_references.len(), 3);
        assert_eq!(
            detail
                .technical_fact_references
                .iter()
                .map(|fact| fact.normalized_value.as_str())
                .collect::<Vec<_>>(),
            vec!["pageNumber", "pageSize", "withCards"]
        );
        assert_eq!(detail.chunk_references.len(), 1);
        assert_eq!(detail.graph_node_references.len(), 1);
        assert_eq!(detail.graph_edge_references.len(), 1);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn execution_detail_surfaces_multihop_graph_and_multi_document_fact_support() -> Result<()> {
    let fixture = QueryGroundingAppFixture::create().await?;
    let result = async {
        let inventory_document_id = Uuid::now_v7();
        let inventory_revision_id = Uuid::now_v7();
        let rest_document_id = Uuid::now_v7();
        let rest_revision_id = Uuid::now_v7();
        let inventory_chunk_id = Uuid::now_v7();
        let rest_chunk_id = Uuid::now_v7();
        let entity_ids = vec![Uuid::now_v7(), Uuid::now_v7()];
        let relation_ids = vec![Uuid::now_v7(), Uuid::now_v7()];

        let inventory_block = sample_structured_block_row(StructuredBlockRowSeed {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id: inventory_document_id,
            revision_id: inventory_revision_id,
            ordinal: 0,
            block_kind: "endpoint_block",
            text: "SOAP WSDL http://demo.local:8080/inventory-api/ws/inventory.wsdl",
            heading_trail: vec!["Inventory API".to_string()],
            section_path: vec!["inventory".to_string(), "wsdl".to_string()],
        });
        let rest_block = sample_structured_block_row(StructuredBlockRowSeed {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id: rest_document_id,
            revision_id: rest_revision_id,
            ordinal: 0,
            block_kind: "endpoint_block",
            text: "GET /v1/accounts",
            heading_trail: vec!["REST API".to_string(), "Accounts".to_string()],
            section_path: vec!["rest".to_string(), "accounts".to_string()],
        });
        let technical_facts = vec![
            sample_technical_fact_row(TechnicalFactRowSeed {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: inventory_document_id,
                revision_id: inventory_revision_id,
                fact_kind: TechnicalFactKind::Url,
                normalized_value: "http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                display_value: "http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                support_block_ids: vec![inventory_block.block_id],
                support_chunk_ids: vec![inventory_chunk_id],
            }),
            sample_technical_fact_row(TechnicalFactRowSeed {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: rest_document_id,
                revision_id: rest_revision_id,
                fact_kind: TechnicalFactKind::EndpointPath,
                normalized_value: "/v1/accounts",
                display_value: "/v1/accounts",
                support_block_ids: vec![rest_block.block_id],
                support_chunk_ids: vec![rest_chunk_id],
            }),
        ];

        let detail = fixture
            .create_execution_detail_with_authoritative_evidence(AuthoritativeEvidenceSeed {
                query_text: "If the agent needs the WSDL inventory service and the REST accounts list, what URLs does it require?",
                verification_state: "verified",
                verification_warnings: json!([]),
                chunk_ids: vec![inventory_chunk_id, rest_chunk_id],
                entity_ids: entity_ids.clone(),
                relation_ids: relation_ids.clone(),
                structured_blocks: vec![inventory_block.clone(), rest_block.clone()],
                technical_facts: technical_facts.clone(),
            })
            .await?;

        assert_eq!(detail.verification_state, QueryVerificationState::Verified);
        assert_eq!(detail.prepared_segment_references.len(), 2);
        assert_eq!(detail.technical_fact_references.len(), 2);
        assert_eq!(
            detail
                .technical_fact_references
                .iter()
                .map(|fact| fact.normalized_value.as_str())
                .collect::<Vec<_>>(),
            vec!["http://demo.local:8080/inventory-api/ws/inventory.wsdl", "/v1/accounts"]
        );
        assert_eq!(detail.chunk_references.len(), 2);
        assert_eq!(detail.graph_node_references.len(), entity_ids.len());
        assert_eq!(detail.graph_edge_references.len(), relation_ids.len());
        assert_eq!(
            detail
                .prepared_segment_references
                .iter()
                .map(|segment| segment.revision_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            2
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
