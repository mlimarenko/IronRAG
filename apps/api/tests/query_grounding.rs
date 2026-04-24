use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use reqwest::{Client, StatusCode};
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
    domains::{audit::AuditEventSubject, ops::OpsAsyncOperation},
    infra::arangodb::{
        bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
        client::ArangoClient,
        context_store::{
            ArangoContextStore, KnowledgeBundleChunkEdgeRow, KnowledgeBundleChunkReferenceRow,
            KnowledgeBundleEntityEdgeRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleEvidenceReferenceRow,
            KnowledgeBundleRelationEdgeRow, KnowledgeBundleRelationReferenceRow,
            KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow,
            KnowledgeRetrievalTraceRow,
        },
        document_store::{
            ArangoDocumentStore, KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
            KnowledgeStructuredBlockRow, KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow,
        },
        graph_store::{ArangoGraphStore, NewKnowledgeEntity},
    },
    infra::repositories::{self, query_repository, runtime_repository},
    services::query::service::QueryService,
};

struct TempArangoDatabase {
    base_url: String,
    username: String,
    password: String,
    name: String,
    http: Client,
}

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
        sqlx::query(&format!("drop database if exists \"{name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale query grounding database {name}"))?;
        sqlx::query(&format!("create database \"{name}\""))
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
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop query grounding database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

impl TempArangoDatabase {
    async fn create(settings: &Settings) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("query_grounding_{}", Uuid::now_v7().simple());
        let http = Client::builder()
            .timeout(Duration::from_secs(settings.arangodb_request_timeout_seconds.max(1)))
            .build()
            .context("failed to build ArangoDB admin http client")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&json!({ "name": name }))
            .send()
            .await
            .context("failed to create temp ArangoDB database")?;
        if !response.status().is_success() {
            return Err(anyhow!(
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
            .context("failed to drop temp ArangoDB database")?;
        if response.status() != StatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow!(
                "failed to drop temp ArangoDB database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }
}

struct QueryGroundingFixture {
    temp_database: TempArangoDatabase,
    document_store: ArangoDocumentStore,
    context_store: ArangoContextStore,
    graph_store: ArangoGraphStore,
}

impl QueryGroundingFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for query grounding tests")?;
        let temp_database = TempArangoDatabase::create(&settings).await?;
        settings.arangodb_database = temp_database.name.clone();

        let client = Arc::new(
            ArangoClient::from_settings(&settings).context("failed to build Arango client")?,
        );
        client.ping().await.context("failed to ping temp ArangoDB database")?;
        bootstrap_knowledge_plane(
            &client,
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
        .context("failed to bootstrap temp Arango knowledge plane")?;

        Ok(Self {
            temp_database,
            document_store: ArangoDocumentStore::new(Arc::clone(&client)),
            context_store: ArangoContextStore::new(Arc::clone(&client)),
            graph_store: ArangoGraphStore::new(Arc::clone(&client)),
        })
    }

    async fn cleanup(self) -> Result<()> {
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
                key: document_id.to_string(),
                arango_id: None,
                arango_rev: None,
                document_id,
                workspace_id,
                library_id,
                external_key: format!("grounding-{document_id}"),
                file_name: None,
                title: Some("Grounding Document".to_string()),
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .context("failed to insert grounding document")?;

        self.document_store
            .upsert_revision(&KnowledgeRevisionRow {
                key: revision_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some(format!("memory://grounding/{revision_id}")),
                source_uri: Some(format!("memory://grounding/source/{revision_id}")),
                mime_type: "text/plain".to_string(),
                checksum: format!("checksum-{revision_id}"),
                title: Some("Grounding Revision".to_string()),
                byte_size: i64::try_from(content_text.len()).unwrap_or(i64::MAX),
                normalized_text: Some(content_text.to_string()),
                text_checksum: Some(format!("text-checksum-{revision_id}")),
                text_state: "text_readable".to_string(),
                vector_state: "pending".to_string(),
                graph_state: "pending".to_string(),
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
                key: chunk_id.to_string(),
                arango_id: None,
                arango_rev: None,
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
            })
            .await
            .context("failed to insert grounding chunk")?;

        Ok(())
    }
}

struct QueryGroundingAppFixture {
    temp_postgres: TempPostgresDatabase,
    temp_arango: TempArangoDatabase,
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
        let temp_arango = TempArangoDatabase::create(&settings).await?;
        settings.arangodb_database = temp_arango.name.clone();

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
        bootstrap_knowledge_plane(
            state.arango_client.as_ref(),
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
        .context("failed to bootstrap query grounding knowledge plane")?;

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
            temp_arango,
            state,
            workspace_id: workspace.id,
            library_id: library.id,
            conversation_id: conversation.id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_postgres.drop().await?;
        self.temp_arango.drop().await
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
            .arango_context_store
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
                .arango_document_store
                .upsert_document(&KnowledgeDocumentRow {
                    key: document_id.to_string(),
                    arango_id: None,
                    arango_rev: None,
                    document_id,
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    external_key: format!("grounding-detail-{document_id}"),
                    file_name: None,
                    title: Some("Grounding Detail Document".to_string()),
                    document_state: "active".to_string(),
                    active_revision_id: Some(active_revision_id),
                    readable_revision_id: Some(active_revision_id),
                    latest_revision_no: Some(1),
                    created_at: now,
                    updated_at: now,
                    deleted_at: None,
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
                .arango_document_store
                .upsert_revision(&KnowledgeRevisionRow {
                    key: revision_id.to_string(),
                    arango_id: None,
                    arango_rev: None,
                    revision_id: *revision_id,
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    document_id: *document_id,
                    revision_number: 1,
                    revision_state: "active".to_string(),
                    revision_kind: "upload".to_string(),
                    storage_ref: Some(format!("memory://query-grounding/{revision_id}")),
                    source_uri: Some(format!("memory://query-grounding/source/{revision_id}")),
                    mime_type: "text/plain".to_string(),
                    checksum: format!("checksum-{revision_id}"),
                    title: Some("Grounding Detail Revision".to_string()),
                    byte_size: 128,
                    normalized_text: Some(query_text.to_string()),
                    text_checksum: Some(format!("text-checksum-{revision_id}")),
                    text_state: "text_readable".to_string(),
                    vector_state: "ready".to_string(),
                    graph_state: "graph_ready".to_string(),
                    text_readable_at: Some(now),
                    vector_ready_at: Some(now),
                    graph_ready_at: Some(now),
                    superseded_by_revision_id: None,
                    created_at: now,
                })
                .await
                .context("failed to seed grounding detail revision")?;
            self.state
                .arango_document_store
                .upsert_structured_revision(&KnowledgeStructuredRevisionRow {
                    key: revision_id.to_string(),
                    arango_id: None,
                    arango_rev: None,
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
                .arango_document_store
                .replace_structured_blocks(*revision_id, &revision_blocks)
                .await
                .context("failed to seed structured blocks for grounding detail")?;
            self.state
                .arango_document_store
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
            .arango_context_store
            .upsert_bundle(&bundle)
            .await
            .context("failed to persist grounding canonical evidence bundle")?;

        if !chunk_ids.is_empty() {
            let chunk_edges = chunk_ids
                .into_iter()
                .map(|chunk_id| sample_chunk_edge(bundle_id, chunk_id))
                .collect::<Vec<_>>();
            self.state
                .arango_context_store
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
                .arango_context_store
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
                .arango_context_store
                .replace_bundle_relation_edges(bundle_id, self.library_id, &relation_edges)
                .await
                .context("failed to persist grounding relation edges")?;
        }

        QueryService::new().get_execution(&self.state, execution.id).await.map_err(|error| {
            anyhow!("failed to load execution detail with canonical evidence: {error}")
        })
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
        key: canonical_context_bundle_id(execution.id).to_string(),
        arango_id: None,
        arango_rev: None,
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
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
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
    status: &str,
    failure_code: Option<&str>,
) -> OpsAsyncOperation {
    OpsAsyncOperation {
        id: Uuid::now_v7(),
        workspace_id,
        library_id: Some(library_id),
        operation_kind: "query_execution".to_string(),
        status: status.to_string(),
        surface_kind: Some("rest".to_string()),
        subject_kind: Some("query_execution".to_string()),
        subject_id: Some(execution_id),
        parent_async_operation_id: None,
        failure_code: failure_code.map(ToString::to_string),
        created_at: Utc::now(),
        completed_at: matches!(status, "ready" | "failed" | "canceled").then(Utc::now),
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
        key: format!("{bundle_id}:{chunk_id}"),
        arango_id: None,
        arango_rev: None,
        from: String::new(),
        to: String::new(),
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
        key: format!("{bundle_id}:{entity_id}"),
        arango_id: None,
        arango_rev: None,
        from: String::new(),
        to: String::new(),
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
        key: format!("{bundle_id}:{relation_id}"),
        arango_id: None,
        arango_rev: None,
        from: String::new(),
        to: String::new(),
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
        key: format!("{bundle_id}:{evidence_id}"),
        arango_id: None,
        arango_rev: None,
        from: String::new(),
        to: String::new(),
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
        key: format!("{bundle_id}:{chunk_id}"),
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
        key: format!("{bundle_id}:{entity_id}"),
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
        key: format!("{bundle_id}:{relation_id}"),
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
        key: format!("{bundle_id}:{evidence_id}"),
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
        key: block_id.to_string(),
        arango_id: None,
        arango_rev: None,
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
        key: fact_id.to_string(),
        arango_id: None,
        arango_rev: None,
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
    assert_eq!(chunk_edge.key, format!("{bundle_id}:{chunk_id}"));
    assert_eq!(entity_edge.bundle_id, bundle_id);
    assert_eq!(entity_edge.entity_id, entity_id);
    assert_eq!(entity_edge.key, format!("{bundle_id}:{entity_id}"));
    assert_eq!(relation_edge.bundle_id, bundle_id);
    assert_eq!(relation_edge.relation_id, relation_id);
    assert_eq!(relation_edge.key, format!("{bundle_id}:{relation_id}"));
    assert_eq!(evidence_edge.bundle_id, bundle_id);
    assert_eq!(evidence_edge.evidence_id, evidence_id);
    assert_eq!(evidence_edge.key, format!("{bundle_id}:{evidence_id}"));

    let reference_set = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
        "failed",
        failed.failure_code.as_deref(),
    );
    let canceled_operation = sample_async_operation(
        workspace_id,
        library_id,
        canceled.id,
        "failed",
        canceled.failure_code.as_deref(),
    );
    let retried_operation =
        sample_async_operation(workspace_id, library_id, retried.id, "processing", None);

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
#[ignore = "requires local ArangoDB service with database create/drop access"]
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
        assert_eq!(persisted_bundle.bundle_id, execution.id);
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
        assert_eq!(traces[0].bundle_id, execution.id);
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
#[ignore = "requires local ArangoDB service with database create/drop access"]
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
#[ignore = "requires local postgres, redis, and arango services"]
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
#[ignore = "requires local postgres, redis, and arango services"]
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
                "Перечисли параметры pageNumber, pageSize и withCards.",
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
#[ignore = "requires local postgres, redis, and arango services"]
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
                "Если агенту нужен WSDL inventory api и список счетов rewards accounts, какие адреса ему нужны?",
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
