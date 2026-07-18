use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

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
        knowledge_plane::{ContextStore, DocumentStore, GraphStore},
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
};

struct TempPostgresDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempPostgresDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let name = format!("query_grounding_http_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect to postgres admin database")?;

        terminate_database_connections(&admin_pool, &name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale query grounding database {name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create query grounding database {name}"))?;
        admin_pool.close().await;

        Ok(Self { database_url: replace_database_name(base_database_url, &name)?, admin_url, name })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect postgres admin database for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop query grounding database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct QueryGroundingFixture {
    temp_database: TempPostgresDatabase,
    postgres: PgPool,
    document_store: PgDocumentStore,
    context_store: PgContextStore,
    graph_store: PgGraphStore,
}

impl QueryGroundingFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for query grounding tests")?;
        let temp_database = TempPostgresDatabase::create(&settings.database_url).await?;
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

    async fn seed_chunk(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        chunk_id: Uuid,
        content_text: &str,
    ) -> Result<()> {
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
                checksum: format!("checksum-{revision_id}"),
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

                raptor_level: None,
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
        &query_repository::UpdateQueryExecution {
            request_turn_id: None,
            response_turn_id: None,
            failure_code: None,
            completed_at: Some(Utc::now()),
        },
    )
    .await?
    .context("completed MCP execution disappeared")?;

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
        let temp_postgres = TempPostgresDatabase::create(&settings.database_url).await?;
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
                context_bundle_id: canonical_context_bundle_id(execution_id),
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

    async fn create_execution_detail_with_canonical_evidence(
        &self,
        query_text: &str,
        verification_state: &str,
        verification_warnings: serde_json::Value,
        chunk_ids: Vec<Uuid>,
        entity_ids: Vec<Uuid>,
        relation_ids: Vec<Uuid>,
        structured_blocks: Vec<KnowledgeStructuredBlockRow>,
        technical_facts: Vec<KnowledgeTechnicalFactRow>,
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
        .context("failed to create grounding request turn with canonical evidence")?;
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
        .context("failed to create grounded canonical runtime execution")?;
        let execution = query_repository::create_execution(
            &self.state.persistence.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: canonical_context_bundle_id(execution_id),
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
        .context("failed to create grounded execution with canonical evidence")?;

        let now = Utc::now();
        let bundle_id = canonical_context_bundle_id(execution_id);
        let mut revision_document_ids = std::collections::BTreeMap::<Uuid, Uuid>::new();
        for block in &structured_blocks {
            revision_document_ids.insert(block.revision_id, block.document_id);
        }
        for fact in &technical_facts {
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
                    checksum: format!("checksum-{revision_id}"),
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
            self.state
                .document_store
                .upsert_structured_revision(&KnowledgeStructuredRevisionRow {
                    revision_id: *revision_id,
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    document_id: *document_id,
                    preparation_state: "prepared".to_string(),
                    normalization_profile: "canonical".to_string(),
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
                })
                .await
                .context("failed to seed structured revision for grounding detail")?;
            self.state
                .document_store
                .replace_structured_blocks(*revision_id, &revision_blocks)
                .await
                .context("failed to seed structured blocks for grounding detail")?;
            self.state
                .document_store
                .replace_technical_facts(*revision_id, &revision_facts)
                .await
                .context("failed to seed technical facts for grounding detail")?;
        }

        let mut bundle = sample_context_bundle(
            self.workspace_id,
            self.library_id,
            &map_execution_row(&execution),
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
            .context("failed to persist grounding canonical evidence bundle")?;

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

        QueryService::new().get_execution(&self.state, execution.id).await.map_err(|error| {
            anyhow!("failed to load execution detail with canonical evidence: {error}")
        })
    }

    async fn create_replayable_execution_detail(
        &self,
        query_text: &str,
        answer_text: &str,
    ) -> Result<ironrag_backend::domains::query::QueryExecutionDetail> {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let fact = sample_technical_fact_row(
            self.workspace_id,
            self.library_id,
            document_id,
            revision_id,
            "configuration_value",
            "enabled",
            "enabled",
            Vec::new(),
            Vec::new(),
        );
        let detail = self
            .create_execution_detail_with_canonical_evidence(
                query_text,
                "verified",
                json!([]),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                vec![fact],
            )
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
                failure_code: None,
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .context("failed to link replay source response turn")?
        .context("replay source execution disappeared")?;

        QueryService::new()
            .get_execution(&self.state, detail.execution.id)
            .await
            .map_err(|error| anyhow!("failed to reload replayable execution detail: {error}"))
    }
}

fn canonical_context_bundle_id(execution_id: Uuid) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, execution_id.as_bytes())
}

fn replace_database_name(base_database_url: &str, database_name: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(base_database_url)
        .with_context(|| format!("invalid postgres url: {base_database_url}"))?;
    let path = url.path().trim_matches('/');
    if path.is_empty() {
        return Err(anyhow!("postgres url must include a database name"));
    }
    url.set_path(database_name);
    Ok(url.to_string())
}

async fn terminate_database_connections(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(admin_pool)
    .await
    .context("failed to terminate postgres database connections")?;
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
        runtime_execution_id: Some(row.runtime_execution_id),
        lifecycle_state: row.runtime_lifecycle_state,
        active_stage: row.runtime_active_stage,
        query_text: row.query_text.clone(),
        failure_code: row.failure_code.clone(),
        started_at: row.started_at,
        completed_at: row.completed_at,
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
        context_bundle_id: canonical_context_bundle_id(execution_id),
        request_turn_id: None,
        response_turn_id: None,
        binding_id: None,
        runtime_execution_id: None,
        lifecycle_state: RuntimeLifecycleState::Running,
        active_stage: Some(RuntimeStageKind::Retrieve),
        query_text: query_text.to_string(),
        failure_code: None,
        started_at: Utc::now(),
        completed_at: None,
    }
}

fn sample_context_bundle(
    workspace_id: Uuid,
    library_id: Uuid,
    execution: &QueryExecution,
) -> KnowledgeContextBundleRow {
    let now = Utc::now();
    KnowledgeContextBundleRow {
        bundle_id: canonical_context_bundle_id(execution.id),
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

fn sample_linked_query_execution(
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    conversation_id: Uuid,
    query_text: &str,
    lifecycle_state: RuntimeLifecycleState,
    active_stage: Option<RuntimeStageKind>,
    failure_code: Option<&str>,
    request_turn_id: Option<Uuid>,
    response_turn_id: Option<Uuid>,
    binding_id: Option<Uuid>,
    completed_at: Option<chrono::DateTime<Utc>>,
) -> QueryExecution {
    QueryExecution {
        conversation_id,
        request_turn_id,
        response_turn_id,
        binding_id,
        lifecycle_state,
        active_stage,
        failure_code: failure_code.map(ToString::to_string),
        completed_at,
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
        created_at: Utc::now(),
        completed_at: matches!(
            status,
            OpsAsyncOperationStatus::Ready
                | OpsAsyncOperationStatus::Failed
                | OpsAsyncOperationStatus::Canceled
        )
        .then(Utc::now),
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

fn sample_structured_block_row(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    ordinal: i32,
    block_kind: &str,
    text: &str,
    heading_trail: Vec<String>,
    section_path: Vec<String>,
) -> KnowledgeStructuredBlockRow {
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

fn sample_technical_fact_row(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    fact_kind: &str,
    canonical_value: &str,
    display_value: &str,
    support_block_ids: Vec<Uuid>,
    support_chunk_ids: Vec<Uuid>,
) -> KnowledgeTechnicalFactRow {
    let now = Utc::now();
    let fact_id = Uuid::now_v7();
    KnowledgeTechnicalFactRow {
        fact_id,
        workspace_id,
        library_id,
        document_id,
        revision_id,
        fact_kind: fact_kind.to_string(),
        canonical_value_text: canonical_value.to_string(),
        canonical_value_exact: canonical_value.to_string(),
        canonical_value_json: json!(canonical_value),
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
fn canonical_query_execution_scaffold_uses_execution_keyed_bundle_ids() {
    let _service = QueryService::new();
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let execution = sample_query_execution(
        workspace_id,
        library_id,
        execution_id,
        "What supports the canonical answer?",
    );
    let bundle = sample_context_bundle(workspace_id, library_id, &execution);

    assert_eq!(execution.context_bundle_id, canonical_context_bundle_id(execution.id));
    assert_eq!(bundle.bundle_id, canonical_context_bundle_id(execution.id));
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

    assert_eq!(reference_set.bundle.bundle_id, bundle_id);
    assert_eq!(reference_set.bundle.query_execution_id, Some(query_execution_id));
    assert_eq!(reference_set.bundle.candidate_summary["chunks"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["entities"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["relations"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["evidence"], json!(1));
    assert_eq!(reference_set.bundle.assembly_diagnostics["grounding_kind"], json!("hybrid"));
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
    assert_eq!(reference_set.chunk_references.len(), 1);
    assert_eq!(reference_set.entity_references.len(), 1);
    assert_eq!(reference_set.relation_references.len(), 1);
    assert_eq!(reference_set.evidence_references.len(), 1);
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

    let failed = sample_linked_query_execution(
        workspace_id,
        library_id,
        failed_execution_id,
        conversation_id,
        query_text,
        RuntimeLifecycleState::Failed,
        None,
        Some("provider_timeout"),
        Some(request_turn_id),
        None,
        Some(binding_id),
        Some(Utc::now()),
    );
    let canceled = sample_linked_query_execution(
        workspace_id,
        library_id,
        canceled_execution_id,
        conversation_id,
        query_text,
        RuntimeLifecycleState::Canceled,
        None,
        Some("canceled_by_user"),
        Some(request_turn_id),
        None,
        Some(binding_id),
        Some(Utc::now()),
    );
    let retried = sample_linked_query_execution(
        workspace_id,
        library_id,
        retry_execution_id,
        conversation_id,
        query_text,
        RuntimeLifecycleState::Running,
        Some(RuntimeStageKind::Retrieve),
        None,
        Some(request_turn_id),
        Some(response_turn_id),
        Some(binding_id),
        None,
    );

    let failed_bundle = sample_context_bundle(workspace_id, library_id, &failed);
    let canceled_bundle = sample_context_bundle(workspace_id, library_id, &canceled);
    let retried_bundle = sample_context_bundle(workspace_id, library_id, &retried);

    assert_eq!(failed.context_bundle_id, canonical_context_bundle_id(failed.id));
    assert_eq!(canceled.context_bundle_id, canonical_context_bundle_id(canceled.id));
    assert_eq!(retried.context_bundle_id, canonical_context_bundle_id(retried.id));
    assert_eq!(failed.request_turn_id, Some(request_turn_id));
    assert_eq!(canceled.request_turn_id, Some(request_turn_id));
    assert_eq!(retried.request_turn_id, Some(request_turn_id));
    assert_eq!(failed.response_turn_id, None);
    assert_eq!(canceled.response_turn_id, None);
    assert_eq!(retried.response_turn_id, Some(response_turn_id));
    assert_eq!(failed.binding_id, Some(binding_id));
    assert_eq!(canceled.binding_id, Some(binding_id));
    assert_eq!(retried.binding_id, Some(binding_id));
    assert_eq!(failed.lifecycle_state, RuntimeLifecycleState::Failed);
    assert_eq!(canceled.lifecycle_state, RuntimeLifecycleState::Canceled);
    assert_eq!(retried.lifecycle_state, RuntimeLifecycleState::Running);
    assert_eq!(retried.active_stage, Some(RuntimeStageKind::Retrieve));
    assert_eq!(failed.failure_code.as_deref(), Some("provider_timeout"));
    assert_eq!(canceled.failure_code.as_deref(), Some("canceled_by_user"));
    assert_eq!(retried.failure_code, None);
    assert_eq!(failed.query_text, query_text);
    assert_eq!(canceled.query_text, query_text);
    assert_eq!(retried.query_text, query_text);
    assert_eq!(failed_bundle.assembly_diagnostics["question"], json!(query_text));
    assert_eq!(canceled_bundle.assembly_diagnostics["question"], json!(query_text));
    assert_eq!(retried_bundle.assembly_diagnostics["question"], json!(query_text));
    assert_eq!(failed_bundle.candidate_summary["chunks"], json!(0));
    assert_eq!(canceled_bundle.candidate_summary["chunks"], json!(0));
    assert_eq!(retried_bundle.candidate_summary["chunks"], json!(0));
    assert_eq!(failed_bundle.query_execution_id, Some(failed.id));
    assert_eq!(canceled_bundle.query_execution_id, Some(canceled.id));
    assert_eq!(retried_bundle.query_execution_id, Some(retried.id));
    assert_eq!(failed_bundle.bundle_id, failed.context_bundle_id);
    assert_eq!(canceled_bundle.bundle_id, canceled.context_bundle_id);
    assert_eq!(retried_bundle.bundle_id, retried.context_bundle_id);
    assert_eq!(failed_bundle.bundle_strategy, "grounded_answer");
    assert_eq!(canceled_bundle.bundle_strategy, "grounded_answer");
    assert_eq!(retried_bundle.bundle_strategy, "grounded_answer");

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

    assert_eq!(failed_operation.subject_kind.as_deref(), Some("query_execution"));
    assert_eq!(failed_operation.subject_id, Some(failed.id));
    assert_eq!(failed_operation.failure_code.as_deref(), Some("provider_timeout"));
    assert_eq!(canceled_operation.subject_kind.as_deref(), Some("query_execution"));
    assert_eq!(canceled_operation.subject_id, Some(canceled.id));
    assert_eq!(canceled_operation.failure_code.as_deref(), Some("canceled_by_user"));
    assert_eq!(retried_operation.subject_kind.as_deref(), Some("query_execution"));
    assert_eq!(retried_operation.subject_id, Some(retried.id));
    assert_eq!(retried_operation.failure_code, None);
    assert_eq!(failed_execution_subject.query_execution_id, Some(failed.id));
    assert_eq!(failed_bundle_subject.context_bundle_id, Some(failed_bundle.bundle_id));
    assert_eq!(failed_operation_subject.async_operation_id, Some(failed_operation.id));
    assert_eq!(failed_execution_subject.library_id, Some(library_id));
    assert_eq!(failed_bundle_subject.workspace_id, Some(workspace_id));
    assert_eq!(failed_operation_subject.subject_kind, "async_operation");
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn setup_structured_block_repository_bounds_and_deduplicates_lanes() -> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        fixture
            .seed_chunk(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                Uuid::now_v7(),
                "neutral grounding chunk",
            )
            .await?;

        let blocks = vec![
            sample_structured_block_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                0,
                "table_row",
                "early structural row",
                Vec::new(),
                Vec::new(),
            ),
            sample_structured_block_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                1,
                "paragraph",
                "early plain row",
                Vec::new(),
                Vec::new(),
            ),
            sample_structured_block_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                10,
                "code_block",
                "late code row",
                Vec::new(),
                Vec::new(),
            ),
            sample_structured_block_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                11,
                "source_unit",
                "late source row",
                Vec::new(),
                Vec::new(),
            ),
        ];
        fixture
            .document_store
            .replace_structured_blocks(revision_id, &blocks)
            .await
            .context("failed to seed setup structured blocks")?;

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
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
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

        fixture
            .seed_chunk(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_id,
                "grounding anchor chunk",
            )
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
            .update_bundle_state(
                bundle.bundle_id,
                "ready",
                &[],
                "not_run",
                json!([]),
                json!({
                    "active_text_generation": 7,
                    "active_vector_generation": 7,
                    "active_graph_generation": 7
                }),
                json!({
                    "chunks": 1,
                    "entities": 1,
                    "relations": 1,
                    "evidence": 1
                }),
                json!({
                    "answerable": true,
                    "grounding_kind": "hybrid"
                }),
            )
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
async fn fresh_completed_mcp_overflow_is_pruned_without_deleting_durable_audit() -> Result<()> {
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
        for age_seconds in [4, 3, 2, 1] {
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

        assert_eq!(deleted, 2, "fresh completed rows must not receive the UI grace period");
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
        sqlx::query(
            "update runtime_execution
             set lifecycle_state = 'completed', completed_at = now()
             where id = $1",
        )
        .bind(runtime_execution_id)
        .execute(&fixture.postgres)
        .await?;
        sqlx::query("update query_execution set completed_at = now() where id = $1")
            .bind(execution_id)
            .execute(&fixture.postgres)
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
        for age_seconds in (1..=8).rev() {
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
                "Which provenance value is canonical?",
                "The canonical provenance value is enabled.",
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
        assert_eq!(target_turn.content_text, "The canonical provenance value is enabled.");
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
                context_bundle_id: canonical_context_bundle_id(execution_id),
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

        sqlx::query("update query_execution set started_at = $2 where id = $1")
            .bind(execution_id)
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
        assert!(execution.completed_at.is_some());
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
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
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
                canonical_label: "Paging Parameter".to_string(),
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
async fn execution_detail_maps_canonical_verification_states_for_grounding_regressions()
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
                "Which port is canonical for the service?",
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
                    "message": "Only one of two referenced documents supplied canonical endpoint facts.",
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
            assert_eq!(detail.execution.context_bundle_id, canonical_context_bundle_id(detail.execution.id));
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

        let parameter_block = sample_structured_block_row(
            fixture.workspace_id,
            fixture.library_id,
            document_id,
            revision_id,
            0,
            "table_row",
            "pageNu mber | pageS ize | withCar ds | number_start ing",
            vec!["Accounts".to_string(), "Pagination".to_string()],
            vec!["accounts".to_string(), "pagination".to_string()],
        );
        let technical_facts = vec![
            sample_technical_fact_row(
                fixture.workspace_id,
                fixture.library_id,
                document_id,
                revision_id,
                "parameter_name",
                "pageNumber",
                "pageNumber",
                vec![parameter_block.block_id],
                vec![chunk_id],
            ),
            sample_technical_fact_row(
                fixture.workspace_id,
                fixture.library_id,
                document_id,
                revision_id,
                "parameter_name",
                "pageSize",
                "pageSize",
                vec![parameter_block.block_id],
                vec![chunk_id],
            ),
            sample_technical_fact_row(
                fixture.workspace_id,
                fixture.library_id,
                document_id,
                revision_id,
                "parameter_name",
                "withCards",
                "withCards",
                vec![parameter_block.block_id],
                vec![chunk_id],
            ),
        ];

        let detail = fixture
            .create_execution_detail_with_canonical_evidence(
                "List the pageNumber, pageSize, and withCards parameters.",
                "verified",
                json!([]),
                vec![chunk_id],
                vec![entity_id],
                vec![relation_id],
                vec![parameter_block.clone()],
                technical_facts.clone(),
            )
            .await?;

        assert_eq!(detail.verification_state, QueryVerificationState::Verified);
        assert_eq!(detail.prepared_segment_references.len(), 1);
        assert_eq!(detail.prepared_segment_references[0].segment_id, parameter_block.block_id);
        assert_eq!(detail.technical_fact_references.len(), 3);
        assert_eq!(
            detail
                .technical_fact_references
                .iter()
                .map(|fact| fact.canonical_value.as_str())
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

        let inventory_block = sample_structured_block_row(
            fixture.workspace_id,
            fixture.library_id,
            inventory_document_id,
            inventory_revision_id,
            0,
            "endpoint_block",
            "SOAP WSDL http://demo.local:8080/inventory-api/ws/inventory.wsdl",
            vec!["Inventory API".to_string()],
            vec!["inventory".to_string(), "wsdl".to_string()],
        );
        let rest_block = sample_structured_block_row(
            fixture.workspace_id,
            fixture.library_id,
            rest_document_id,
            rest_revision_id,
            0,
            "endpoint_block",
            "GET /v1/accounts",
            vec!["REST API".to_string(), "Accounts".to_string()],
            vec!["rest".to_string(), "accounts".to_string()],
        );
        let technical_facts = vec![
            sample_technical_fact_row(
                fixture.workspace_id,
                fixture.library_id,
                inventory_document_id,
                inventory_revision_id,
                "url",
                "http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                "http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                vec![inventory_block.block_id],
                vec![inventory_chunk_id],
            ),
            sample_technical_fact_row(
                fixture.workspace_id,
                fixture.library_id,
                rest_document_id,
                rest_revision_id,
                "endpoint_path",
                "/v1/accounts",
                "/v1/accounts",
                vec![rest_block.block_id],
                vec![rest_chunk_id],
            ),
        ];

        let detail = fixture
            .create_execution_detail_with_canonical_evidence(
                "If the agent needs the WSDL inventory service and the REST accounts list, what URLs does it require?",
                "verified",
                json!([]),
                vec![inventory_chunk_id, rest_chunk_id],
                entity_ids.clone(),
                relation_ids.clone(),
                vec![inventory_block.clone(), rest_block.clone()],
                technical_facts.clone(),
            )
            .await?;

        assert_eq!(detail.verification_state, QueryVerificationState::Verified);
        assert_eq!(detail.prepared_segment_references.len(), 2);
        assert_eq!(detail.technical_fact_references.len(), 2);
        assert_eq!(
            detail
                .technical_fact_references
                .iter()
                .map(|fact| fact.canonical_value.as_str())
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
