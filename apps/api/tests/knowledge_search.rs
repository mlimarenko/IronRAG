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
use reqwest::{Client, StatusCode as ReqwestStatusCode};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::{Instant, sleep};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::arangodb::{
        bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
        client::ArangoClient,
        collections::{
            KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
            KNOWLEDGE_ENTITY_VECTOR_COLLECTION, KNOWLEDGE_SEARCH_VIEW,
        },
        document_store::{
            ArangoDocumentStore, KnowledgeChunkRow, KnowledgeDocumentRow,
            KnowledgeLibraryGenerationRow, KnowledgeRevisionRow, KnowledgeTechnicalFactRow,
        },
        graph_store::{ArangoGraphStore, NewKnowledgeEntity, NewKnowledgeEvidence},
        search_store::{ArangoSearchStore, KnowledgeChunkVectorRow, KnowledgeEntityVectorRow},
    },
    infra::repositories::{self, ai_repository, iam_repository},
    integrations::llm::{EmbeddingRequest, EmbeddingResponse, LlmGateway},
    interfaces::http::{auth::hash_token, authorization::PERMISSION_LIBRARY_READ, router},
    services::query::search::SearchService,
};

const SEARCH_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const SEARCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

struct TempArangoDatabase {
    base_url: String,
    username: String,
    password: String,
    name: String,
    http: Client,
}

impl TempArangoDatabase {
    async fn create(settings: &Settings) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("knowledge_search_{}", Uuid::now_v7().simple());
        let http = Client::builder()
            .timeout(Duration::from_secs(settings.arangodb_request_timeout_seconds.max(1)))
            .build()
            .context("failed to build ArangoDB admin http client")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&serde_json::json!({ "name": name }))
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
        if response.status() != ReqwestStatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow!(
                "failed to drop temp ArangoDB database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }

    async fn drop_collection(&self, collection_name: &str) -> Result<()> {
        let response = self
            .http
            .delete(format!(
                "{}/_db/{}/_api/collection/{}",
                self.base_url, self.name, collection_name
            ))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .with_context(|| format!("failed to drop collection {collection_name}"))?;
        if response.status() != ReqwestStatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow!(
                "failed to drop collection {}: status {}",
                collection_name,
                response.status()
            ));
        }
        Ok(())
    }
}

struct KnowledgeSearchFixture {
    temp_database: TempArangoDatabase,
    settings: Settings,
    client: Arc<ArangoClient>,
    document_store: ArangoDocumentStore,
    graph_store: ArangoGraphStore,
    search_store: ArangoSearchStore,
}

impl KnowledgeSearchFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for knowledge search tests")?;
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
                views: true,
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
            document_store: ArangoDocumentStore::new(Arc::clone(&client)),
            graph_store: ArangoGraphStore::new(Arc::clone(&client)),
            search_store: ArangoSearchStore::new(Arc::clone(&client)),
            temp_database,
            settings,
            client,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.temp_database.drop().await
    }

    async fn fetch_view_properties(&self, view_name: &str) -> Result<Value> {
        let response = Client::new()
            .get(self.client.database_api_url(&format!("_api/view/{view_name}/properties")))
            .basic_auth(&self.settings.arangodb_username, Some(&self.settings.arangodb_password))
            .send()
            .await
            .with_context(|| {
                format!("failed to fetch ArangoSearch view properties for {view_name}")
            })?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "failed to fetch ArangoSearch view properties for {}: status {}",
                view_name,
                response.status()
            ));
        }
        response
            .json::<Value>()
            .await
            .with_context(|| format!("failed to decode view properties for {view_name}"))
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
                .search_chunks(library_id, query, expected_chunk_ids.len().max(8))
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
        sqlx::query(&format!("drop database if exists \"{name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {name}"))?;
        sqlx::query(&format!("create database \"{name}\""))
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
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
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
    temp_arango: TempArangoDatabase,
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
        let temp_arango = TempArangoDatabase::create(&settings).await?;
        settings.arangodb_database = temp_arango.name.clone();

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
        bootstrap_knowledge_plane(
            state.arango_client.as_ref(),
            &ArangoBootstrapOptions {
                collections: true,
                views: true,
                graph: true,
                vector_indexes: false,
                vector_dimensions: 3072,
                vector_index_n_lists: 100,
                vector_index_default_n_probe: 8,
                vector_index_training_iterations: 25,
            },
        )
        .await
        .context("failed to bootstrap knowledge search arango plane")?;

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
        let credential = ai_repository::create_provider_credential(
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
        .context("failed to create knowledge search provider credential")?;
        let preset = ai_repository::create_model_preset(
            &state.persistence.postgres,
            "workspace",
            Some(workspace.id),
            None,
            model_catalog.id,
            "knowledge-search-model-preset",
            None,
            None,
            None,
            None,
            json!({}),
            None,
        )
        .await
        .context("failed to create knowledge search model preset")?;
        ai_repository::create_binding_assignment(
            &state.persistence.postgres,
            "library",
            Some(workspace.id),
            Some(library.id),
            "embed_chunk",
            credential.id,
            preset.id,
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
        let generation_id = Uuid::now_v7();
        let now = Utc::now();

        state
            .arango_document_store
            .upsert_document(&KnowledgeDocumentRow {
                key: document_id.to_string(),
                arango_id: None,
                arango_rev: None,
                document_id,
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: "search-document".to_string(),
                file_name: None,
                title: Some("Search Document".to_string()),
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .context("failed to insert knowledge search document")?;
        state
            .arango_document_store
            .upsert_revision(&KnowledgeRevisionRow {
                key: revision_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id,
                workspace_id: workspace.id,
                library_id: library.id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://knowledge-search".to_string()),
                source_uri: Some("memory://knowledge-search/source".to_string()),
                mime_type: "text/plain".to_string(),
                checksum: "knowledge-search-checksum".to_string(),
                title: Some("Knowledge Search".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("knowledge-search-text-checksum".to_string()),
                text_state: "text_readable".to_string(),
                vector_state: "vector_ready".to_string(),
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
            .arango_document_store
            .upsert_chunk(&KnowledgeChunkRow {
                key: chunk_id.to_string(),
                arango_id: None,
                arango_rev: None,
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
            })
            .await
            .context("failed to insert knowledge search chunk")?;
        state
            .arango_document_store
            .replace_technical_facts(
                revision_id,
                &[KnowledgeTechnicalFactRow {
                    key: fact_id.to_string(),
                    arango_id: None,
                    arango_rev: None,
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
            .arango_document_store
            .upsert_library_generation(&KnowledgeLibraryGenerationRow {
                key: format!("{}:{}", library.id, generation_id),
                arango_id: None,
                arango_rev: None,
                generation_id,
                workspace_id: workspace.id,
                library_id: library.id,
                active_text_generation: 1,
                active_vector_generation: 1,
                active_graph_generation: 1,
                degraded_state: "ready".to_string(),
                updated_at: now,
            })
            .await
            .context("failed to insert knowledge search generation")?;

        let entity_doc = json!({
            "_key": entity_id.to_string(),
            "entity_id": entity_id,
            "workspace_id": workspace.id,
            "library_id": library.id,
            "canonical_label": "Orion Signal",
            "aliases": ["Signal Orion"],
            "entity_type": "concept",
            "summary": "Orion entity summary",
            "confidence": 0.95,
            "support_count": 3,
            "freshness_generation": 1,
            "entity_state": "active",
            "created_at": now,
            "updated_at": now,
        });
        let _: Value = state
            .arango_graph_store
            .client()
            .query_json(
                "UPSERT { _key: @key }
                 INSERT @doc
                 UPDATE @doc
                 IN @@collection
                 RETURN NEW",
                json!({
                    "@collection": "knowledge_entity",
                    "key": entity_id.to_string(),
                    "doc": entity_doc,
                }),
            )
            .await
            .context("failed to insert knowledge search entity")?;

        let relation_doc = json!({
            "_key": relation_id.to_string(),
            "relation_id": relation_id,
            "workspace_id": workspace.id,
            "library_id": library.id,
            "predicate": "Orion relation",
            "canonical_label": "Orion relation",
            "summary": "Orion relation summary",
            "normalized_assertion": "orion relation",
            "confidence": 0.9,
            "support_count": 2,
            "contradiction_state": "none",
            "freshness_generation": 1,
            "relation_state": "active",
            "created_at": now,
            "updated_at": now,
        });
        let _: Value = state
            .arango_graph_store
            .client()
            .query_json(
                "UPSERT { _key: @key }
                 INSERT @doc
                 UPDATE @doc
                 IN @@collection
                 RETURN NEW",
                json!({
                    "@collection": "knowledge_relation",
                    "key": relation_id.to_string(),
                    "doc": relation_doc,
                }),
            )
            .await
            .context("failed to insert knowledge search relation")?;
        state
            .arango_graph_store
            .upsert_relation_subject_edge(relation_id, entity_id, library.id)
            .await
            .context("failed to link knowledge search relation subject")?;
        state
            .arango_graph_store
            .upsert_relation_object_edge(relation_id, entity_id, library.id)
            .await
            .context("failed to link knowledge search relation object")?;
        state
            .arango_search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                key: format!("{}:{}:1", chunk_id, model_catalog.id),
                arango_id: None,
                arango_rev: None,
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
            })
            .await
            .context("failed to insert knowledge search chunk vector")?;
        state
            .arango_search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                key: format!("{}:{}:1", entity_id, model_catalog.id),
                arango_id: None,
                arango_rev: None,
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
            .arango_graph_store
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
            temp_arango,
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
        self.temp_postgres.drop().await?;
        self.temp_arango.drop().await
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
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
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
#[ignore = "requires local ArangoDB service with database create/drop access"]
async fn lexical_chunk_search_view_bootstraps_and_stays_library_scoped() -> Result<()> {
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

        let view_properties = fixture.fetch_view_properties(KNOWLEDGE_SEARCH_VIEW).await?;
        let links = view_properties
            .get("links")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("knowledge search view is missing links object"))?;
        assert!(links.contains_key(KNOWLEDGE_CHUNK_COLLECTION));

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                key: target_document_id.to_string(),
                arango_id: None,
                arango_rev: None,
                document_id: target_document_id,
                workspace_id,
                library_id: target_library_id,
                external_key: "lexical-target".to_string(),
                file_name: None,
                title: Some("Target".to_string()),
                document_state: "active".to_string(),
                active_revision_id: Some(target_revision_id),
                readable_revision_id: Some(target_revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .context("failed to insert target document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                key: target_revision_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id: target_revision_id,
                workspace_id,
                library_id: target_library_id,
                document_id: target_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://target".to_string()),
                source_uri: Some("memory://target/source".to_string()),
                mime_type: "text/plain".to_string(),
                checksum: "target-checksum".to_string(),
                title: Some("Target".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("target-text-checksum".to_string()),
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
                key: target_chunk_id.to_string(),
                arango_id: None,
                arango_rev: None,
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
            })
            .await
            .context("failed to insert target chunk")?;

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                key: distractor_document_id.to_string(),
                arango_id: None,
                arango_rev: None,
                document_id: distractor_document_id,
                workspace_id,
                library_id: distractor_library_id,
                external_key: "lexical-distractor".to_string(),
                file_name: None,
                title: Some("Distractor".to_string()),
                document_state: "active".to_string(),
                active_revision_id: Some(distractor_revision_id),
                readable_revision_id: Some(distractor_revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .context("failed to insert distractor document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                key: distractor_revision_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id: distractor_revision_id,
                workspace_id,
                library_id: distractor_library_id,
                document_id: distractor_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://distractor".to_string()),
                source_uri: Some("memory://distractor/source".to_string()),
                mime_type: "text/plain".to_string(),
                checksum: "distractor-checksum".to_string(),
                title: Some("Distractor".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("distractor-text-checksum".to_string()),
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
                key: distractor_chunk_id.to_string(),
                arango_id: None,
                arango_rev: None,
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
#[ignore = "requires local ArangoDB service with database create/drop access"]
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
                key: document_id.to_string(),
                arango_id: None,
                arango_rev: None,
                document_id,
                workspace_id,
                library_id,
                external_key: "vector-doc".to_string(),
                file_name: None,
                title: Some("Vector Doc".to_string()),
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .context("failed to insert vector test document")?;
        fixture
            .document_store
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
                storage_ref: Some("memory://vector-doc".to_string()),
                source_uri: Some("memory://vector-doc/source".to_string()),
                mime_type: "text/plain".to_string(),
                checksum: "vector-checksum".to_string(),
                title: Some("Vector Doc".to_string()),
                byte_size: 32,
                normalized_text: Some("vector generation anchor".to_string()),
                text_checksum: Some("vector-text-checksum".to_string()),
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
                key: chunk_id.to_string(),
                arango_id: None,
                arango_rev: None,
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
                key: format!("{chunk_id}:{model_catalog_id}:1"),
                arango_id: None,
                arango_rev: None,
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
            })
            .await
            .context("failed to insert generation 1 chunk vector")?;
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                key: format!("{chunk_id}:{model_catalog_id}:2"),
                arango_id: None,
                arango_rev: None,
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
            })
            .await
            .context("failed to insert generation 2 chunk vector")?;

        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                key: format!("{entity_id}:{model_catalog_id}:1"),
                arango_id: None,
                arango_rev: None,
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
                key: format!("{entity_id}:{model_catalog_id}:2"),
                arango_id: None,
                arango_rev: None,
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
#[ignore = "requires local ArangoDB service with database create/drop access"]
async fn revision_replacement_updates_readiness_generation_and_chunk_search_surface() -> Result<()>
{
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_one_id = Uuid::now_v7();
        let revision_two_id = Uuid::now_v7();
        let chunk_one_id = Uuid::now_v7();
        let chunk_two_id = Uuid::now_v7();
        let generation_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                key: document_id.to_string(),
                arango_id: None,
                arango_rev: None,
                document_id,
                workspace_id,
                library_id,
                external_key: "replacement-doc".to_string(),
                file_name: None,
                title: Some("Replacement Doc".to_string()),
                document_state: "active".to_string(),
                active_revision_id: Some(revision_one_id),
                readable_revision_id: Some(revision_one_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .context("failed to insert replacement document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                key: revision_one_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id: revision_one_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision-one".to_string()),
                source_uri: Some("memory://revision-one/source".to_string()),
                mime_type: "text/plain".to_string(),
                checksum: "revision-one".to_string(),
                title: Some("Revision One".to_string()),
                byte_size: 32,
                normalized_text: Some("obsolete nebula anchor".to_string()),
                text_checksum: Some("replacement-text-checksum-1".to_string()),
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
                key: chunk_one_id.to_string(),
                arango_id: None,
                arango_rev: None,
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
            })
            .await
            .context("failed to insert revision one chunk")?;
        fixture
            .document_store
            .upsert_library_generation(&KnowledgeLibraryGenerationRow {
                key: format!("{library_id}:{generation_id}"),
                arango_id: None,
                arango_rev: None,
                generation_id,
                workspace_id,
                library_id,
                active_text_generation: 1,
                active_vector_generation: 0,
                active_graph_generation: 0,
                degraded_state: "text_only".to_string(),
                updated_at: now,
            })
            .await
            .context("failed to insert generation row for revision one")?;

        let old_hits = fixture
            .wait_for_chunk_hits(library_id, "obsolete nebula anchor", &[chunk_one_id])
            .await?;
        assert_eq!(old_hits, vec![chunk_one_id]);

        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                key: revision_two_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id: revision_two_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 2,
                revision_state: "active".to_string(),
                revision_kind: "replace".to_string(),
                storage_ref: Some("memory://revision-two".to_string()),
                source_uri: Some("memory://revision-two/source".to_string()),
                mime_type: "text/plain".to_string(),
                checksum: "revision-two".to_string(),
                title: Some("Revision Two".to_string()),
                byte_size: 32,
                normalized_text: Some("fresh pulsar anchor".to_string()),
                text_checksum: Some("replacement-text-checksum-2".to_string()),
                text_state: "graph_ready".to_string(),
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
                key: chunk_two_id.to_string(),
                arango_id: None,
                arango_rev: None,
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
        fixture
            .document_store
            .upsert_library_generation(&KnowledgeLibraryGenerationRow {
                key: format!("{library_id}:{generation_id}"),
                arango_id: None,
                arango_rev: None,
                generation_id,
                workspace_id,
                library_id,
                active_text_generation: 2,
                active_vector_generation: 2,
                active_graph_generation: 2,
                degraded_state: "ready".to_string(),
                updated_at: now + chrono::TimeDelta::seconds(1),
            })
            .await
            .context("failed to update generation row after replacement")?;

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

        let generations = fixture
            .document_store
            .list_library_generations(library_id)
            .await
            .context("failed to reload library generations")?;
        assert_eq!(generations.len(), 1);
        assert_eq!(generations[0].active_text_generation, 2);
        assert_eq!(generations[0].active_vector_generation, 2);
        assert_eq!(generations[0].active_graph_generation, 2);
        assert_eq!(generations[0].degraded_state, "ready");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
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
#[ignore = "requires local postgres, redis, and arango services"]
async fn search_documents_endpoint_falls_back_to_lexical_when_vector_collections_fail() -> Result<()>
{
    let fixture = KnowledgeSearchHttpFixture::create().await?;

    let result = async {
        fixture
            .temp_arango
            .drop_collection(KNOWLEDGE_CHUNK_VECTOR_COLLECTION)
            .await
            .context("failed to remove chunk vector collection for fallback test")?;
        fixture
            .temp_arango
            .drop_collection(KNOWLEDGE_ENTITY_VECTOR_COLLECTION)
            .await
            .context("failed to remove entity vector collection for fallback test")?;

        let body = fixture.search_document_hit("orion").await?;
        let document_hits =
            body["documentHits"].as_array().context("documentHits must be an array")?;
        assert_eq!(document_hits.len(), 1);
        assert_eq!(document_hits[0]["document"]["documentId"], json!(fixture.document_id));
        assert_eq!(document_hits[0]["chunkHits"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hits[0]["technicalFactSummary"]["typedFactCount"], json!(1));
        assert_eq!(document_hits[0]["technicalFactSamples"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hits[0]["vectorChunkHits"].as_array().map_or(0, Vec::len), 0);
        assert_eq!(body["vectorChunkHits"].as_array().map_or(0, Vec::len), 0);
        assert_eq!(body["vectorEntityHits"].as_array().map_or(0, Vec::len), 0);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn lexical_chunk_search_ignores_non_chunk_documents_in_shared_search_view() -> Result<()> {
    let fixture = KnowledgeSearchHttpFixture::create().await?;

    let result = async {
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        loop {
            let hits = fixture
                .state
                .arango_search_store
                .search_chunks(fixture.library_id, "/orion/status", 8)
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
#[ignore = "requires local postgres, redis, and arango services"]
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
            .arango_document_store
            .replace_technical_facts(
                fixture.revision_id,
                &[
                    KnowledgeTechnicalFactRow {
                        key: fixture.fact_id.to_string(),
                        arango_id: None,
                        arango_rev: None,
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
                        key: url_fact_id.to_string(),
                        arango_id: None,
                        arango_rev: None,
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
                        key: method_fact_id.to_string(),
                        arango_id: None,
                        arango_rev: None,
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
                        key: parameter_fact_id.to_string(),
                        arango_id: None,
                        arango_rev: None,
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
                        key: distractor_parameter_fact_id.to_string(),
                        arango_id: None,
                        arango_rev: None,
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
