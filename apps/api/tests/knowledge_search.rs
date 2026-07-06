#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::{Instant, sleep};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{self, ai_repository, iam_repository},
    infra::{
        knowledge_plane::{DocumentStore, GraphStore, SearchStore},
        knowledge_rows::{
            KNOWLEDGE_CHUNK_VECTOR_KIND, KnowledgeChunkRow, KnowledgeChunkVectorRow,
            KnowledgeDocumentRow, KnowledgeEntityVectorRow, KnowledgeRevisionRow,
            KnowledgeTechnicalFactRow, NewKnowledgeEntity, NewKnowledgeEvidence,
        },
        postgres::{
            pg_document_store::PgDocumentStore, pg_graph_store::PgGraphStore,
            pg_search_store::PgSearchStore,
        },
    },
    integrations::llm::{EmbeddingRequest, EmbeddingResponse, LlmGateway},
    interfaces::http::{auth::hash_token, authorization::PERMISSION_LIBRARY_READ, router},
    services::query::search::SearchService,
};

const SEARCH_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const SEARCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

struct KnowledgeSearchFixture {
    temp_database: TempPostgresDatabase,
    postgres: PgPool,
    document_store: PgDocumentStore,
    graph_store: PgGraphStore,
    search_store: PgSearchStore,
}

impl KnowledgeSearchFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for knowledge search tests")?;
        let temp_database = TempPostgresDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect knowledge search postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply knowledge search migrations")?;

        Ok(Self {
            temp_database,
            postgres: postgres.clone(),
            document_store: PgDocumentStore { pool: postgres.clone() },
            graph_store: PgGraphStore { pool: postgres.clone() },
            search_store: PgSearchStore { pool: postgres.clone() },
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.postgres.close().await;
        self.temp_database.drop().await
    }

    async fn wait_for_chunk_hits(
        &self,
        library_id: Uuid,
        query: &str,
        expected_chunk_ids: &[Uuid],
    ) -> Result<Vec<Uuid>> {
        let expected = expected_chunk_ids.iter().copied().collect::<BTreeSet<_>>();
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        loop {
            let hits = self
                .search_store
                .search_chunks(library_id, query, expected_chunk_ids.len().max(8), None, None)
                .await
                .with_context(|| format!("failed to search chunks for query {query}"))?;
            let actual = hits.iter().map(|row| row.chunk_id).collect::<BTreeSet<_>>();
            if actual == expected {
                return Ok(hits.into_iter().map(|row| row.chunk_id).collect());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "timed out waiting for chunk hits {expected:?} for query {query}; last observed {actual:?}"
                ));
            }
            sleep(SEARCH_POLL_INTERVAL).await;
        }
    }
}

struct TempPostgresDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempPostgresDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let name = format!("knowledge_search_http_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect to postgres admin database")?;

        terminate_database_connections(&admin_pool, &name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {name}"))?;
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
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

#[derive(Clone)]
struct FakeEmbeddingGateway {
    embedding: Vec<f32>,
}

#[async_trait]
impl LlmGateway for FakeEmbeddingGateway {
    async fn generate(
        &self,
        request: ironrag_backend::integrations::llm::ChatRequest,
    ) -> anyhow::Result<ironrag_backend::integrations::llm::ChatResponse> {
        Err(anyhow!("generate not used in knowledge search test: {}", request.provider_kind))
    }

    async fn embed(&self, request: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        Ok(EmbeddingResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            dimensions: self.embedding.len(),
            embedding: self.embedding.clone(),
            usage_json: json!({}),
        })
    }

    async fn embed_many(
        &self,
        request: ironrag_backend::integrations::llm::EmbeddingBatchRequest,
    ) -> anyhow::Result<ironrag_backend::integrations::llm::EmbeddingBatchResponse> {
        let embeddings =
            request.inputs.into_iter().map(|_| self.embedding.clone()).collect::<Vec<_>>();
        Ok(ironrag_backend::integrations::llm::EmbeddingBatchResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            dimensions: self.embedding.len(),
            embeddings,
            usage_json: json!({}),
        })
    }

    async fn vision_extract(
        &self,
        request: ironrag_backend::integrations::llm::VisionRequest,
    ) -> anyhow::Result<ironrag_backend::integrations::llm::VisionResponse> {
        Err(anyhow!("vision_extract not used in knowledge search test: {}", request.provider_kind))
    }
}

struct KnowledgeSearchHttpFixture {
    temp_postgres: TempPostgresDatabase,
    state: AppState,
    token: String,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    fact_id: Uuid,
    entity_id: Uuid,
    relation_id: Uuid,
}

impl KnowledgeSearchHttpFixture {
    async fn create() -> Result<Self> {
        let mut settings = Settings::from_env()
            .context("failed to load settings for knowledge search http test")?;
        let temp_postgres = TempPostgresDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_postgres.database_url.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect to knowledge search postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply knowledge search migrations")?;
        postgres.close().await;

        let mut state = AppState::new(settings.clone()).await?;
        state.llm_gateway = Arc::new(FakeEmbeddingGateway { embedding: vec![0.9, 0.8, 0.7] });

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("knowledge-search-workspace-{suffix}"),
            "Knowledge Search Workspace",
            None,
        )
        .await
        .context("failed to create knowledge search workspace")?;
        let library = repositories::catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("knowledge-search-library-{suffix}"),
            "Knowledge Search Library",
            Some("knowledge search proof fixture"),
            None,
        )
        .await
        .context("failed to create knowledge search library")?;

        let provider_catalog = ai_repository::list_provider_catalog(&state.persistence.postgres)
            .await
            .context("failed to list provider catalog for knowledge search test")?
            .into_iter()
            .find(|row| row.provider_kind == "openai")
            .context("expected seeded openai provider catalog row")?;
        let model_catalog = ai_repository::list_model_catalog(
            &state.persistence.postgres,
            Some(provider_catalog.id),
        )
        .await
        .context("failed to list model catalog for knowledge search test")?
        .into_iter()
        .find(|row| row.capability_kind == "embedding")
        .context("expected seeded embedding model catalog row")?;
        let credential = ai_repository::create_account(
            &state.persistence.postgres,
            "workspace",
            Some(workspace.id),
            None,
            provider_catalog.id,
            "knowledge-search-provider-credential",
            Some("secret://knowledge-search/provider"),
            None,
            None,
        )
        .await
        .context("failed to create knowledge search AI account")?;
        ai_repository::create_binding(
            &state.persistence.postgres,
            "library",
            Some(workspace.id),
            Some(library.id),
            "embed_chunk",
            credential.id,
            model_catalog.id,
            None,
            None,
            None,
            None,
            json!({}),
            None,
        )
        .await
        .context("failed to create knowledge search library binding")?;

        let token =
            mint_library_read_token(&state.persistence.postgres, workspace.id, library.id).await?;

        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let fact_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let relation_id = Uuid::now_v7();
        let evidence_id = Uuid::now_v7();
        let now = Utc::now();

        state
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: "search-document".to_string(),
                file_name: None,
                title: Some("Search Document".to_string()),
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
            .context("failed to insert knowledge search document")?;
        state
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id: workspace.id,
                library_id: library.id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://knowledge-search".to_string()),
                source_uri: Some("memory://knowledge-search/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "knowledge-search-checksum".to_string(),
                title: Some("Knowledge Search".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("knowledge-search-text-checksum".to_string()),
                image_checksum: None,
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
            .context("failed to insert knowledge search revision")?;
        state
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id,
                workspace_id: workspace.id,
                library_id: library.id,
                document_id,
                revision_id,
                chunk_index: 0,
                content_text: "orion lexical anchor".to_string(),
                normalized_text: "orion lexical anchor".to_string(),
                span_start: Some(0),
                span_end: Some(20),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["intro".to_string()],
                heading_trail: vec!["Intro".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert knowledge search chunk")?;
        state
            .document_store
            .replace_technical_facts(
                revision_id,
                &[KnowledgeTechnicalFactRow {
                    fact_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    document_id,
                    revision_id,
                    fact_kind: "endpoint_path".to_string(),
                    canonical_value_text: "/orion/status".to_string(),
                    canonical_value_exact: "/orion/status".to_string(),
                    canonical_value_json: json!({
                        "value_type": "text",
                        "value": "/orion/status"
                    }),
                    display_value: "/orion/status".to_string(),
                    qualifiers_json: json!([]),
                    support_block_ids: Vec::new(),
                    support_chunk_ids: vec![chunk_id],
                    confidence: Some(0.98),
                    extraction_kind: "fixture_seed".to_string(),
                    conflict_group_id: None,
                    created_at: now,
                    updated_at: now,
                }],
            )
            .await
            .context("failed to insert knowledge search technical fact")?;
        state
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id,
                workspace_id: workspace.id,
                library_id: library.id,
                canonical_label: "Orion Signal".to_string(),
                aliases: vec!["Signal Orion".to_string()],
                entity_type: "concept".to_string(),
                entity_sub_type: None,
                summary: Some("Orion entity summary".to_string()),
                confidence: Some(0.95),
                support_count: 3,
                freshness_generation: 1,
                entity_state: "active".to_string(),
                created_at: Some(now),
                updated_at: Some(now),
            })
            .await
            .context("failed to insert knowledge search entity")?;
        state
            .graph_store
            .upsert_relation_with_endpoints(
                &ironrag_backend::infra::knowledge_rows::NewKnowledgeRelation {
                    relation_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    predicate: "Orion relation".to_string(),
                    normalized_assertion: "orion relation".to_string(),
                    confidence: Some(0.9),
                    support_count: 2,
                    contradiction_state: "none".to_string(),
                    freshness_generation: 1,
                    relation_state: "active".to_string(),
                    created_at: Some(now),
                    updated_at: Some(now),
                },
                Some(entity_id),
                Some(entity_id),
                library.id,
            )
            .await
            .context("failed to insert knowledge search relation")?;
        state
            .graph_store
            .upsert_relation_subject_edge(relation_id, entity_id, library.id)
            .await
            .context("failed to link knowledge search relation subject")?;
        state
            .graph_store
            .upsert_relation_object_edge(relation_id, entity_id, library.id)
            .await
            .context("failed to link knowledge search relation object")?;
        state
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id,
                revision_id,
                embedding_model_key: model_catalog.id.to_string(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 3,
                vector: vec![0.9, 0.8, 0.7],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert knowledge search chunk vector")?;
        state
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                entity_id,
                embedding_model_key: model_catalog.id.to_string(),
                vector_kind: "entity_embedding".to_string(),
                dimensions: 3,
                vector: vec![0.9, 0.8, 0.7],
                freshness_generation: 1,
                created_at: now,
            })
            .await
            .context("failed to insert knowledge search entity vector")?;
        state
            .graph_store
            .upsert_evidence_with_edges(
                &NewKnowledgeEvidence {
                    evidence_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    document_id,
                    revision_id,
                    chunk_id: Some(chunk_id),
                    block_id: None,
                    fact_id: Some(fact_id),
                    span_start: Some(0),
                    span_end: Some(20),
                    quote_text: "orion lexical anchor".to_string(),
                    literal_spans_json: serde_json::json!([]),
                    evidence_kind: "chunk_quote".to_string(),
                    extraction_method: "seed".to_string(),
                    confidence: Some(0.99),
                    evidence_state: "active".to_string(),
                    freshness_generation: 1,
                    created_at: Some(now),
                    updated_at: Some(now),
                },
                Some(revision_id),
                Some(entity_id),
                Some(relation_id),
                None,
                library.id,
            )
            .await
            .context("failed to insert knowledge search evidence")?;

        Ok(Self {
            temp_postgres,
            state,
            token,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id,
            revision_id,
            chunk_id,
            fact_id,
            entity_id,
            relation_id,
        })
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_postgres.drop().await
    }

    async fn search_document_hit(&self, query: &str) -> Result<Value> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/search/documents?libraryId={}&query={query}&limit=5&chunkHitLimitPerDocument=3&evidenceSampleLimit=2",
                        self.library_id
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .body(Body::empty())
                    .expect("build knowledge search request"),
            )
            .await
            .context("failed to call knowledge search endpoint")?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .context("failed to read knowledge search response body")?
            .to_bytes();
        serde_json::from_slice::<Value>(&body).context("failed to decode knowledge search json")
    }

    async fn wait_for_query_evidence_top_fact(
        &self,
        query: &str,
        expected_fact_id: Uuid,
    ) -> Result<ironrag_backend::services::query::search::QueryEvidenceSearchResult> {
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        let descriptive_ir = ironrag_backend::domains::query_ir::QueryIR {
            act: ironrag_backend::domains::query_ir::QueryAct::Describe,
            scope: ironrag_backend::domains::query_ir::QueryScope::SingleDocument,
            language: ironrag_backend::domains::query_ir::QueryLanguage::Auto,
            retrieval_query: None,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 1.0,
        };
        loop {
            let result = SearchService::new()
                .search_query_evidence(&self.state, self.library_id, query, &descriptive_ir, 5)
                .await
                .with_context(|| {
                    format!("failed to search query evidence for technical fact query {query}")
                })?;
            if result.technical_fact_hits.first().map(|row| row.fact_id) == Some(expected_fact_id) {
                return Ok(result);
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "timed out waiting for top technical fact {} for query {}; last observed {:?}",
                    expected_fact_id,
                    query,
                    result.technical_fact_hits.first().map(|row| row.fact_id)
                ));
            }
            sleep(SEARCH_POLL_INTERVAL).await;
        }
    }
}

async fn mint_library_read_token(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<String> {
    let plaintext = format!("knowledge-search-{}", Uuid::now_v7());
    let token = iam_repository::create_api_token(
        postgres,
        Some(workspace_id),
        "knowledge-search",
        "knowledge-search",
        None,
        None,
    )
    .await
    .context("failed to create knowledge search api token")?;
    iam_repository::create_api_token_secret(postgres, token.principal_id, &hash_token(&plaintext))
        .await
        .context("failed to create knowledge search token secret")?;
    iam_repository::create_grant(
        postgres,
        token.principal_id,
        "library",
        library_id,
        PERMISSION_LIBRARY_READ,
        None,
        None,
    )
    .await
    .context("failed to create knowledge search grant")?;
    Ok(plaintext)
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

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn library_generation_signals_count_canonical_chunk_embedding_vectors() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 2,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision".to_string()),
                source_uri: Some("memory://revision/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "revision".to_string(),
                title: Some("Generation Signal Fixture".to_string()),
                byte_size: 24,
                normalized_text: Some("generation signal fixture".to_string()),
                text_checksum: Some("generation-signal-fixture".to_string()),
                image_checksum: None,
                text_state: "accepted".to_string(),
                vector_state: "vector_ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: None,
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert revision for generation signal fixture")?;

        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id,
                revision_id,
                embedding_model_key: model_catalog_id.to_string(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.2, 0.4, 0.6],
                freshness_generation: 2,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert canonical chunk embedding vector")?;

        let signals = fixture
            .document_store
            .aggregate_library_generation_signals(library_id)
            .await
            .context("failed to aggregate library generation signals")?;
        assert_eq!(signals.active_vector_generation, 2);
        assert!(signals.has_ready_vector);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn vector_ready_revisions_missing_chunk_vectors_are_counted() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let no_chunk_revision_id = Uuid::now_v7();
        let pending_revision_id = Uuid::now_v7();
        let pending_chunk_id = Uuid::now_v7();
        let superseded_revision_id = Uuid::now_v7();
        let superseded_chunk_id = Uuid::now_v7();
        let other_library_id = Uuid::now_v7();
        let other_revision_id = Uuid::now_v7();
        let other_chunk_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision".to_string()),
                source_uri: Some("memory://revision/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "revision".to_string(),
                title: Some("Vector Inventory Fixture".to_string()),
                byte_size: 24,
                normalized_text: Some("vector inventory fixture".to_string()),
                text_checksum: Some("vector-inventory-fixture".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert vector inventory revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_index: 0,
                content_text: "vector inventory fixture".to_string(),
                normalized_text: "vector inventory fixture".to_string(),
                span_start: Some(0),
                span_end: Some(24),
                token_count: Some(3),
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
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert vector inventory chunk")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: no_chunk_revision_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "no-chunk-revision".to_string(),
                title: Some("No Chunk Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("no chunk".to_string()),
                text_checksum: Some("no-chunk".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert no-chunk revision")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: pending_revision_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "pending-revision".to_string(),
                title: Some("Pending Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("pending".to_string()),
                text_checksum: Some("pending".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "pending".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert pending revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: pending_chunk_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_id: pending_revision_id,
                chunk_index: 0,
                content_text: "pending".to_string(),
                normalized_text: "pending".to_string(),
                span_start: Some(0),
                span_end: Some(7),
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
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert pending chunk")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: superseded_revision_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_number: 1,
                revision_state: "superseded".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "superseded-revision".to_string(),
                title: Some("Superseded Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("superseded".to_string()),
                text_checksum: Some("superseded".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: Some(revision_id),
                created_at: now,
            })
            .await
            .context("failed to insert superseded revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: superseded_chunk_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_id: superseded_revision_id,
                chunk_index: 0,
                content_text: "superseded".to_string(),
                normalized_text: "superseded".to_string(),
                span_start: Some(0),
                span_end: Some(10),
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
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert superseded chunk")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: other_revision_id,
                workspace_id,
                library_id: other_library_id,
                document_id: Uuid::now_v7(),
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "other-revision".to_string(),
                title: Some("Other Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("other".to_string()),
                text_checksum: Some("other".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert other-library revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: other_chunk_id,
                workspace_id,
                library_id: other_library_id,
                document_id: Uuid::now_v7(),
                revision_id: other_revision_id,
                chunk_index: 0,
                content_text: "other".to_string(),
                normalized_text: "other".to_string(),
                span_start: Some(0),
                span_end: Some(5),
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
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert other-library chunk")?;

        let stale_count = fixture
            .document_store
            .count_vector_ready_revisions_missing_chunk_vectors(library_id)
            .await
            .context("failed to count vector inventory mismatch")?;
        assert_eq!(stale_count, 1);

        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id,
                revision_id,
                embedding_model_key: model_catalog_id.to_string(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.2, 0.4, 0.6],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert vector inventory row")?;
        let repaired_count = fixture
            .document_store
            .count_vector_ready_revisions_missing_chunk_vectors(library_id)
            .await
            .context("failed to count repaired vector inventory")?;
        assert_eq!(repaired_count, 0);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn lexical_chunk_search_stays_library_scoped() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let target_library_id = Uuid::now_v7();
        let distractor_library_id = Uuid::now_v7();
        let target_document_id = Uuid::now_v7();
        let target_revision_id = Uuid::now_v7();
        let target_chunk_id = Uuid::now_v7();
        let distractor_document_id = Uuid::now_v7();
        let distractor_revision_id = Uuid::now_v7();
        let distractor_chunk_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id: target_document_id,
                workspace_id,
                library_id: target_library_id,
                external_key: "lexical-target".to_string(),
                file_name: None,
                title: Some("Target".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(target_revision_id),
                readable_revision_id: Some(target_revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert target document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: target_revision_id,
                workspace_id,
                library_id: target_library_id,
                document_id: target_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://target".to_string()),
                source_uri: Some("memory://target/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "target-checksum".to_string(),
                title: Some("Target".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("target-text-checksum".to_string()),
                image_checksum: None,
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
            .context("failed to insert target revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: target_chunk_id,
                workspace_id,
                library_id: target_library_id,
                document_id: target_document_id,
                revision_id: target_revision_id,
                chunk_index: 0,
                content_text: "orion lexical anchor".to_string(),
                normalized_text: "orion lexical anchor".to_string(),
                span_start: Some(0),
                span_end: Some(20),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["intro".to_string()],
                heading_trail: vec!["Intro".to_string()],
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
            .context("failed to insert target chunk")?;

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id: distractor_document_id,
                workspace_id,
                library_id: distractor_library_id,
                external_key: "lexical-distractor".to_string(),
                file_name: None,
                title: Some("Distractor".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(distractor_revision_id),
                readable_revision_id: Some(distractor_revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert distractor document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: distractor_revision_id,
                workspace_id,
                library_id: distractor_library_id,
                document_id: distractor_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://distractor".to_string()),
                source_uri: Some("memory://distractor/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "distractor-checksum".to_string(),
                title: Some("Distractor".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("distractor-text-checksum".to_string()),
                image_checksum: None,
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
            .context("failed to insert distractor revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: distractor_chunk_id,
                workspace_id,
                library_id: distractor_library_id,
                document_id: distractor_document_id,
                revision_id: distractor_revision_id,
                chunk_index: 0,
                content_text: "orion lexical anchor".to_string(),
                normalized_text: "orion lexical anchor".to_string(),
                span_start: Some(0),
                span_end: Some(20),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["intro".to_string()],
                heading_trail: vec!["Intro".to_string()],
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
            .context("failed to insert distractor chunk")?;

        let hits = fixture
            .wait_for_chunk_hits(target_library_id, "orion lexical anchor", &[target_chunk_id])
            .await?;
        assert_eq!(hits, vec![target_chunk_id]);
        let structured_chunks = fixture
            .document_store
            .list_chunks_by_revision(target_revision_id)
            .await
            .context("failed to reload structured chunks for ancestry assertion")?;
        let target_chunk = structured_chunks
            .into_iter()
            .find(|chunk| chunk.chunk_id == target_chunk_id)
            .ok_or_else(|| anyhow!("target chunk vanished before ancestry assertion"))?;
        assert_eq!(target_chunk.section_path, vec!["intro".to_string()]);
        assert_eq!(target_chunk.heading_trail, vec!["Intro".to_string()]);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn chunk_and_entity_vectors_roundtrip_with_generation_order() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: "vector-doc".to_string(),
                file_name: None,
                title: Some("Vector Doc".to_string()),
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
            .context("failed to insert vector test document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://vector-doc".to_string()),
                source_uri: Some("memory://vector-doc/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "vector-checksum".to_string(),
                title: Some("Vector Doc".to_string()),
                byte_size: 32,
                normalized_text: Some("vector generation anchor".to_string()),
                text_checksum: Some("vector-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "vector_ready".to_string(),
                graph_state: "pending".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert vector test revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_index: 0,
                content_text: "vector generation anchor".to_string(),
                normalized_text: "vector generation anchor".to_string(),
                span_start: Some(0),
                span_end: Some(20),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["intro".to_string()],
                heading_trail: vec!["Intro".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert vector test chunk")?;
        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id,
                workspace_id,
                library_id,
                canonical_label: "VectorEntity".to_string(),
                aliases: vec!["Entity Alias".to_string()],
                entity_type: "concept".to_string(),
                entity_sub_type: None,
                summary: Some("Entity vector anchor".to_string()),
                confidence: Some(0.9),
                support_count: 2,
                freshness_generation: 2,
                entity_state: "active".to_string(),
                created_at: Some(now),
                updated_at: Some(now),
            })
            .await
            .context("failed to insert vector test entity")?;

        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id,
                revision_id,
                embedding_model_key: model_catalog_id.to_string(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 3,
                vector: vec![0.1, 0.2, 0.3],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert generation 1 chunk vector")?;
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id,
                revision_id,
                embedding_model_key: model_catalog_id.to_string(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 3,
                vector: vec![0.9, 0.8, 0.7],
                freshness_generation: 2,
                created_at: now + chrono::TimeDelta::seconds(1),
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert generation 2 chunk vector")?;

        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                entity_id,
                embedding_model_key: model_catalog_id.to_string(),
                vector_kind: "entity_embedding".to_string(),
                dimensions: 3,
                vector: vec![1.0, 1.0, 1.0],
                freshness_generation: 1,
                created_at: now,
            })
            .await
            .context("failed to insert generation 1 entity vector")?;
        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                entity_id,
                embedding_model_key: model_catalog_id.to_string(),
                vector_kind: "entity_embedding".to_string(),
                dimensions: 3,
                vector: vec![2.0, 2.0, 2.0],
                freshness_generation: 2,
                created_at: now + chrono::TimeDelta::seconds(1),
            })
            .await
            .context("failed to insert generation 2 entity vector")?;

        let chunk_vectors = fixture
            .search_store
            .list_chunk_vectors_by_chunk(chunk_id)
            .await
            .context("failed to list chunk vectors")?;
        assert_eq!(chunk_vectors.len(), 2);
        assert_eq!(chunk_vectors[0].freshness_generation, 2);
        assert_eq!(chunk_vectors[1].freshness_generation, 1);
        assert_eq!(
            SearchService::new()
                .select_current_chunk_vector(&chunk_vectors)
                .expect("current chunk vector")
                .vector,
            vec![0.9, 0.8, 0.7]
        );

        let entity_vectors = fixture
            .search_store
            .list_entity_vectors_by_entity(entity_id)
            .await
            .context("failed to list entity vectors")?;
        assert_eq!(entity_vectors.len(), 2);
        assert_eq!(entity_vectors[0].freshness_generation, 2);
        assert_eq!(entity_vectors[1].freshness_generation, 1);
        assert_eq!(
            SearchService::new()
                .select_current_entity_vector(&entity_vectors)
                .expect("current entity vector")
                .vector,
            vec![2.0, 2.0, 2.0]
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn revision_replacement_updates_readiness_and_chunk_search_surface() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_one_id = Uuid::now_v7();
        let revision_two_id = Uuid::now_v7();
        let chunk_one_id = Uuid::now_v7();
        let chunk_two_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: "replacement-doc".to_string(),
                file_name: None,
                title: Some("Replacement Doc".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(revision_one_id),
                readable_revision_id: Some(revision_one_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert replacement document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: revision_one_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision-one".to_string()),
                source_uri: Some("memory://revision-one/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "revision-one".to_string(),
                title: Some("Revision One".to_string()),
                byte_size: 32,
                normalized_text: Some("obsolete nebula anchor".to_string()),
                text_checksum: Some("replacement-text-checksum-1".to_string()),
                image_checksum: None,
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
            .context("failed to insert revision one")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: chunk_one_id,
                workspace_id,
                library_id,
                document_id,
                revision_id: revision_one_id,
                chunk_index: 0,
                content_text: "obsolete nebula anchor".to_string(),
                normalized_text: "obsolete nebula anchor".to_string(),
                span_start: Some(0),
                span_end: Some(24),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["revision-one".to_string()],
                heading_trail: vec!["Revision One".to_string()],
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
            .context("failed to insert revision one chunk")?;
        let old_hits = fixture
            .wait_for_chunk_hits(library_id, "obsolete nebula anchor", &[chunk_one_id])
            .await?;
        assert_eq!(old_hits, vec![chunk_one_id]);

        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: revision_two_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 2,
                revision_state: "active".to_string(),
                revision_kind: "replace".to_string(),
                storage_ref: Some("memory://revision-two".to_string()),
                source_uri: Some("memory://revision-two/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "revision-two".to_string(),
                title: Some("Revision Two".to_string()),
                byte_size: 32,
                normalized_text: Some("fresh pulsar anchor".to_string()),
                text_checksum: Some("replacement-text-checksum-2".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "vector_ready".to_string(),
                graph_state: "graph_ready".to_string(),
                text_readable_at: Some(now + chrono::TimeDelta::seconds(1)),
                vector_ready_at: Some(now + chrono::TimeDelta::seconds(1)),
                graph_ready_at: Some(now + chrono::TimeDelta::seconds(1)),
                superseded_by_revision_id: None,
                created_at: now + chrono::TimeDelta::seconds(1),
            })
            .await
            .context("failed to insert revision two")?;
        fixture
            .document_store
            .update_revision_readiness(
                revision_one_id,
                "superseded",
                "superseded",
                "superseded",
                Some(now),
                None,
                None,
                Some(revision_two_id),
            )
            .await
            .context("failed to supersede revision one readiness")?
            .ok_or_else(|| anyhow!("revision one disappeared during supersede update"))?;
        fixture
            .document_store
            .delete_chunks_by_revision(revision_one_id)
            .await
            .context("failed to delete revision one chunks")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: chunk_two_id,
                workspace_id,
                library_id,
                document_id,
                revision_id: revision_two_id,
                chunk_index: 0,
                content_text: "fresh pulsar anchor".to_string(),
                normalized_text: "fresh pulsar anchor".to_string(),
                span_start: Some(0),
                span_end: Some(19),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["revision-two".to_string()],
                heading_trail: vec!["Revision Two".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(2),
                vector_generation: Some(2),
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert revision two chunk")?;
        fixture
            .document_store
            .update_document_pointers(
                document_id,
                "active",
                Some(revision_two_id),
                Some(revision_two_id),
                Some(2),
                None,
                None,
            )
            .await
            .context("failed to update document pointers after replacement")?
            .ok_or_else(|| anyhow!("document disappeared during pointer update"))?;
        fixture.wait_for_chunk_hits(library_id, "obsolete nebula anchor", &[]).await?;
        let new_hits =
            fixture.wait_for_chunk_hits(library_id, "fresh pulsar anchor", &[chunk_two_id]).await?;
        assert_eq!(new_hits, vec![chunk_two_id]);

        let document = fixture
            .document_store
            .get_document(document_id)
            .await
            .context("failed to reload document after replacement")?
            .ok_or_else(|| anyhow!("replacement document not found"))?;
        assert_eq!(document.active_revision_id, Some(revision_two_id));
        assert_eq!(document.readable_revision_id, Some(revision_two_id));
        assert_eq!(document.latest_revision_no, Some(2));

        let revision_one = fixture
            .document_store
            .get_revision(revision_one_id)
            .await
            .context("failed to reload revision one")?
            .ok_or_else(|| anyhow!("revision one not found"))?;
        assert_eq!(revision_one.superseded_by_revision_id, Some(revision_two_id));
        assert_eq!(revision_one.text_state, "superseded");

        let revision_two = fixture
            .document_store
            .get_revision(revision_two_id)
            .await
            .context("failed to reload revision two")?
            .ok_or_else(|| anyhow!("revision two not found"))?;
        assert_eq!(revision_two.vector_state, "vector_ready");
        assert_eq!(revision_two.graph_state, "graph_ready");
        assert!(revision_two.vector_ready_at.is_some());
        assert!(revision_two.graph_ready_at.is_some());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn search_documents_endpoint_returns_hybrid_knowledge_payload() -> Result<()> {
    let fixture = KnowledgeSearchHttpFixture::create().await?;

    let result = async {
        let body = fixture.search_document_hit("orion").await?;
        assert_eq!(body["libraryId"], json!(fixture.library_id));
        assert_eq!(body["queryText"], json!("orion"));
        assert_eq!(body["limit"], json!(5));
        assert_eq!(body["freshnessGeneration"], json!(1));
        assert_eq!(body["embeddingProviderKind"], json!("openai"));
        assert!(!body["embeddingModelName"].as_str().unwrap_or_default().is_empty());

        let document_hits =
            body["documentHits"].as_array().context("documentHits must be an array")?;
        assert_eq!(document_hits.len(), 1);
        let document_hit = &document_hits[0];
        assert_eq!(document_hit["document"]["documentId"], json!(fixture.document_id));
        assert_eq!(document_hit["revision"]["revisionId"], json!(fixture.revision_id));
        assert_eq!(document_hit["provenanceSummary"]["supportingEvidenceCount"], json!(1));
        assert_eq!(document_hit["provenanceSummary"]["lexicalChunkCount"], json!(1));
        assert_eq!(document_hit["provenanceSummary"]["vectorChunkCount"], json!(1));
        assert_eq!(document_hit["technicalFactSummary"]["typedFactCount"], json!(1));
        assert_eq!(
            document_hit["technicalFactSummary"]["factKindCounts"]["endpoint_path"],
            json!(1)
        );
        assert_eq!(document_hit["graphEvidenceSummary"]["evidenceCount"], json!(1));
        assert_eq!(document_hit["graphEvidenceSummary"]["factBackedCount"], json!(1));
        assert_eq!(document_hit["chunkHits"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hit["vectorChunkHits"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hit["evidenceSamples"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hit["technicalFactSamples"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hit["technicalFactSamples"][0]["factId"], json!(fixture.fact_id));
        assert_eq!(document_hit["technicalFactSamples"][0]["displayValue"], json!("/orion/status"));

        let entity_hits = body["entityHits"].as_array().context("entityHits must be an array")?;
        assert_eq!(entity_hits.len(), 1);
        assert_eq!(entity_hits[0]["entityId"], json!(fixture.entity_id));
        assert_eq!(entity_hits[0]["canonicalLabel"], json!("Orion Signal"));

        let relation_hits =
            body["relationHits"].as_array().context("relationHits must be an array")?;
        assert_eq!(relation_hits.len(), 1);
        assert_eq!(relation_hits[0]["relationId"], json!(fixture.relation_id));
        assert_eq!(relation_hits[0]["canonicalLabel"], json!("Orion relation"));

        let vector_chunk_hits =
            body["vectorChunkHits"].as_array().context("vectorChunkHits must be an array")?;
        assert_eq!(vector_chunk_hits.len(), 1);
        assert_eq!(vector_chunk_hits[0]["chunkId"], json!(fixture.chunk_id));

        let vector_entity_hits =
            body["vectorEntityHits"].as_array().context("vectorEntityHits must be an array")?;
        assert_eq!(vector_entity_hits.len(), 1);
        assert_eq!(vector_entity_hits[0]["entityId"], json!(fixture.entity_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn lexical_chunk_search_ignores_non_chunk_documents_in_shared_search_view() -> Result<()> {
    let fixture = KnowledgeSearchHttpFixture::create().await?;

    let result = async {
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        loop {
            let hits = fixture
                .state
                .search_store
                .search_chunks(fixture.library_id, "/orion/status", 8, None, None)
                .await
                .context("failed to run lexical chunk search against shared search view")?;
            let chunk_ids = hits.iter().map(|row| row.chunk_id).collect::<BTreeSet<_>>();
            if chunk_ids == BTreeSet::from([fixture.chunk_id]) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "timed out waiting for lexical chunk search to return only the canonical chunk {}; last observed {:?}",
                    fixture.chunk_id,
                    chunk_ids
                ));
            }
            sleep(SEARCH_POLL_INTERVAL).await;
        }
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn search_query_evidence_ranks_typed_facts_for_url_endpoint_method_and_parameter_questions()
-> Result<()> {
    let fixture = KnowledgeSearchHttpFixture::create().await?;

    let result = async {
        let url_fact_id = Uuid::now_v7();
        let method_fact_id = Uuid::now_v7();
        let parameter_fact_id = Uuid::now_v7();
        let distractor_parameter_fact_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .state
            .document_store
            .replace_technical_facts(
                fixture.revision_id,
                &[
                    KnowledgeTechnicalFactRow {
                        fact_id: fixture.fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "endpoint_path".to_string(),
                        canonical_value_text: "/orion/status".to_string(),
                        canonical_value_exact: "/orion/status".to_string(),
                        canonical_value_json: json!({ "value_type": "text", "value": "/orion/status" }),
                        display_value: "/orion/status".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.98),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                    KnowledgeTechnicalFactRow {
                        fact_id: url_fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "url".to_string(),
                        canonical_value_text: "https://api.example.com/orion/status".to_string(),
                        canonical_value_exact: "https://api.example.com/orion/status".to_string(),
                        canonical_value_json: json!({
                            "value_type": "text",
                            "value": "https://api.example.com/orion/status"
                        }),
                        display_value: "https://api.example.com/orion/status".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.97),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                    KnowledgeTechnicalFactRow {
                        fact_id: method_fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "http_method".to_string(),
                        canonical_value_text: "GET".to_string(),
                        canonical_value_exact: "GET".to_string(),
                        canonical_value_json: json!({ "value_type": "text", "value": "GET" }),
                        display_value: "GET".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.96),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                    KnowledgeTechnicalFactRow {
                        fact_id: parameter_fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "parameter_name".to_string(),
                        canonical_value_text: "pageNumber".to_string(),
                        canonical_value_exact: "pageNumber".to_string(),
                        canonical_value_json: json!({ "value_type": "text", "value": "pageNumber" }),
                        display_value: "pageNumber".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.95),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                    KnowledgeTechnicalFactRow {
                        fact_id: distractor_parameter_fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "parameter_name".to_string(),
                        canonical_value_text: "pageSize".to_string(),
                        canonical_value_exact: "pageSize".to_string(),
                        canonical_value_json: json!({ "value_type": "text", "value": "pageSize" }),
                        display_value: "pageSize".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.94),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                ],
            )
            .await
            .context("failed to reseed canonical technical facts for ranking regression")?;

        let endpoint_result = fixture
            .wait_for_query_evidence_top_fact("/orion/status", fixture.fact_id)
            .await?;
        assert!(endpoint_result.exact_literal_bias);
        assert_eq!(endpoint_result.technical_fact_hits[0].fact_id, fixture.fact_id);
        assert_eq!(endpoint_result.technical_fact_hits[0].fact_kind, "endpoint_path");
        assert!(endpoint_result.technical_fact_hits[0].exact_match);

        let url_result = fixture
            .wait_for_query_evidence_top_fact(
                "https://api.example.com/orion/status",
                url_fact_id,
            )
            .await?;
        assert!(url_result.exact_literal_bias);
        assert_eq!(url_result.technical_fact_hits[0].fact_id, url_fact_id);
        assert_eq!(url_result.technical_fact_hits[0].fact_kind, "url");
        assert!(url_result.technical_fact_hits[0].exact_match);

        let method_result = fixture
            .wait_for_query_evidence_top_fact("HTTP method GET", method_fact_id)
            .await?;
        assert!(method_result.exact_literal_bias);
        assert_eq!(method_result.technical_fact_hits[0].fact_id, method_fact_id);
        assert_eq!(method_result.technical_fact_hits[0].fact_kind, "http_method");

        let parameter_result = fixture
            .wait_for_query_evidence_top_fact("query parameter pageNumber", parameter_fact_id)
            .await?;
        assert!(parameter_result.exact_literal_bias);
        assert_eq!(parameter_result.technical_fact_hits[0].fact_id, parameter_fact_id);
        assert_eq!(parameter_result.technical_fact_hits[0].fact_kind, "parameter_name");
        let distractor_position = parameter_result
            .technical_fact_hits
            .iter()
            .position(|row| row.fact_id == distractor_parameter_fact_id);
        if let Some(position) = distractor_position {
            assert!(position > 0);
        }

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// Two libraries on different embed dims must coexist without cross-library
/// or cross-dimension ANN leakage.
///
/// We go through the low-level `upsert_chunk_vector` API instead of
/// mocking the embed-binding pipeline end-to-end, so the test stays
/// small and focuses on the storage-layer isolation invariant.
#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn test_two_libraries_different_dims_isolated() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_a_id = Uuid::now_v7();
        let library_b_id = Uuid::now_v7();
        let chunk_a_id = Uuid::now_v7();
        let chunk_b_id = Uuid::now_v7();
        let revision_a_id = Uuid::now_v7();
        let revision_b_id = Uuid::now_v7();
        let model_a_key = format!("model-a-{}", Uuid::now_v7());
        let model_b_key = format!("model-b-{}", Uuid::now_v7());
        let now = Utc::now();

        // Library A writes a dim-3 vector.
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id: library_a_id,
                chunk_id: chunk_a_id,
                revision_id: revision_a_id,
                embedding_model_key: model_a_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![1.0, 0.0, 0.0],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to upsert library A chunk vector")?;

        // Library B writes a dim-4 vector; it must not collide with library A's
        // dim-3 vector.
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id: library_b_id,
                chunk_id: chunk_b_id,
                revision_id: revision_b_id,
                embedding_model_key: model_b_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 4,
                vector: vec![1.0, 0.0, 0.0, 0.0],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to upsert library B chunk vector")?;

        // ANN against dim-3 must return library A's chunk and nothing
        // else: library B's vector has a different library and dimension.
        let hits_a = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                3,
                library_a_id,
                &model_a_key,
                &[1.0, 0.0, 0.0],
                16,
                None,
                None,
                None,
            )
            .await
            .context("failed ANN search against library A dim-3 shard")?;
        assert_eq!(hits_a.len(), 1, "library A dim-3 shard must return exactly its own chunk");
        assert_eq!(hits_a[0].chunk_id, chunk_a_id);

        // ANN against dim-4 must return library B's chunk and nothing
        // else.
        let hits_b = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                4,
                library_b_id,
                &model_b_key,
                &[1.0, 0.0, 0.0, 0.0],
                16,
                None,
                None,
                None,
            )
            .await
            .context("failed ANN search against library B dim-4 shard")?;
        assert_eq!(hits_b.len(), 1, "library B dim-4 shard must return exactly its own chunk");
        assert_eq!(hits_b[0].chunk_id, chunk_b_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
