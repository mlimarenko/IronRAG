use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::{
            bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
            client::ArangoClient,
        },
        repositories::{ai_repository, catalog_repository, content_repository, graph_repository},
    },
    services::{
        extract_service::{
            CheckpointResumeCursorCommand, ExtractService, MaterializeChunkResultCommand,
            NewEdgeCandidate, NewNodeCandidate, PersistExtractContentCommand,
        },
        graph_service::GraphService,
        search_service::{ChunkEmbeddingWrite, GraphNodeEmbeddingWrite, SearchService},
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
        let database_name = format!("extract_graph_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect extract_graph admin postgres")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(&format!("drop database if exists \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(&format!("create database \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {database_name}"))?;
        admin_pool.close().await;

        Ok(Self {
            name: database_name.clone(),
            admin_url,
            database_url: replace_database_name(base_database_url, &database_name)?,
        })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect extract_graph admin postgres for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct ExtractGraphFixture {
    temp_database: TempDatabase,
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    attempt_id: Uuid,
    projection_id: Uuid,
    node_id: Uuid,
    other_node_id: Uuid,
    edge_id: Uuid,
    primary_model_catalog_id: Uuid,
    alternate_model_catalog_id: Uuid,
}

impl ExtractGraphFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for extract_graph test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect extract_graph postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply extract_graph migrations")?;

        let state = build_test_state(settings, postgres).await?;
        let fixture = Self {
            temp_database,
            state,
            workspace_id: Uuid::nil(),
            library_id: Uuid::nil(),
            document_id: Uuid::nil(),
            revision_id: Uuid::nil(),
            chunk_id: Uuid::nil(),
            attempt_id: Uuid::nil(),
            projection_id: Uuid::nil(),
            node_id: Uuid::nil(),
            other_node_id: Uuid::nil(),
            edge_id: Uuid::nil(),
            primary_model_catalog_id: Uuid::nil(),
            alternate_model_catalog_id: Uuid::nil(),
        };
        fixture.seed().await
    }

    async fn seed(mut self) -> Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &self.state.persistence.postgres,
            &format!("extract-graph-workspace-{suffix}"),
            "Extract Graph Workspace",
            None,
        )
        .await
        .context("failed to create workspace fixture")?;
        let library = catalog_repository::create_library(
            &self.state.persistence.postgres,
            workspace.id,
            &format!("extract-graph-library-{suffix}"),
            "Extract Graph Library",
            Some("extract graph test library"),
            None,
        )
        .await
        .context("failed to create library fixture")?;

        let document = content_repository::create_document(
            &self.state.persistence.postgres,
            &content_repository::NewContentDocument {
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: &format!("extract-graph-doc-{suffix}"),
                document_state: "active",
                created_by_principal_id: None,
            },
        )
        .await
        .context("failed to create content document")?;
        let revision = content_repository::create_revision(
            &self.state.persistence.postgres,
            &content_repository::NewContentRevision {
                document_id: document.id,
                workspace_id: workspace.id,
                library_id: library.id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: "sha256:extract-graph",
                mime_type: "text/plain",
                byte_size: 96,
                title: Some("Extract Graph Fixture"),
                language_code: Some("en"),
                source_uri: None,
                storage_key: None,
                created_by_principal_id: None,
            },
        )
        .await
        .context("failed to create content revision")?;
        let chunk = content_repository::create_chunk(
            &self.state.persistence.postgres,
            &content_repository::NewContentChunk {
                revision_id: revision.id,
                chunk_index: 0,
                start_offset: 0,
                end_offset: 96,
                token_count: Some(16),
                normalized_text: "Readable extracted text for the canonical greenfield test.",
                text_checksum: "sha256:chunk",
            },
        )
        .await
        .context("failed to create content chunk")?;
        let attempt = sqlx::query_scalar::<_, Uuid>(
            "insert into ingest_job (
                workspace_id,
                library_id,
                job_kind,
                queue_state,
                priority,
                queued_at,
                available_at
            )
            values ($1, $2, 'content_mutation', 'queued', 100, now(), now())
            returning id",
        )
        .bind(workspace.id)
        .bind(library.id)
        .fetch_one(&self.state.persistence.postgres)
        .await
        .context("failed to insert ingest job")?;
        let attempt = sqlx::query_scalar::<_, Uuid>(
            "insert into ingest_attempt (
                job_id,
                attempt_number,
                attempt_state,
                current_stage,
                started_at
            )
            values ($1, 1, 'running', 'extracting_graph', now())
            returning id",
        )
        .bind(attempt)
        .fetch_one(&self.state.persistence.postgres)
        .await
        .context("failed to insert ingest attempt")?;

        let extract_service = ExtractService::new();
        let _ = extract_service
            .persist_extract_content(
                &self.state,
                PersistExtractContentCommand {
                    revision_id: revision.id,
                    attempt_id: Some(attempt),
                    extract_state: "readable".to_string(),
                    normalized_text: Some(
                        "Readable extracted text for the canonical greenfield test.".to_string(),
                    ),
                    text_checksum: Some("sha256:extract-graph".to_string()),
                    warning_count: 0,
                },
            )
            .await
            .context("failed to persist extract content")?;
        let chunk_result = extract_service
            .materialize_chunk_result(
                &self.state,
                MaterializeChunkResultCommand {
                    chunk_id: chunk.id,
                    attempt_id: attempt,
                    extract_state: "ready".to_string(),
                    provider_call_id: None,
                    finished_at: Some(Utc::now()),
                    failure_code: None,
                    node_candidates: vec![NewNodeCandidate {
                        canonical_key: "entity:greenfield-test".to_string(),
                        node_kind: "entity".to_string(),
                        display_label: "Greenfield Test".to_string(),
                        summary: Some("Typed node candidate".to_string()),
                    }],
                    edge_candidates: vec![NewEdgeCandidate {
                        canonical_key:
                            "entity:greenfield-test--relates_to--entity:greenfield-other"
                                .to_string(),
                        edge_kind: "relates_to".to_string(),
                        from_canonical_key: "entity:greenfield-test".to_string(),
                        to_canonical_key: "entity:greenfield-other".to_string(),
                        summary: Some("Typed edge candidate".to_string()),
                    }],
                },
            )
            .await
            .context("failed to materialize chunk extraction result")?;
        let cursor = extract_service
            .checkpoint_resume_cursor(
                &self.state,
                CheckpointResumeCursorCommand {
                    attempt_id: attempt,
                    last_completed_chunk_index: 0,
                },
            )
            .await
            .context("failed to checkpoint resume cursor")?;

        let provider_catalogs =
            ai_repository::list_provider_catalog(&self.state.persistence.postgres)
                .await
                .context("failed to list provider catalog")?;
        let openai_provider = provider_catalogs
            .iter()
            .find(|row| row.provider_kind == "openai")
            .context("missing openai provider catalog entry")?;
        let qwen_provider = provider_catalogs
            .iter()
            .find(|row| row.provider_kind == "qwen")
            .context("missing qwen provider catalog entry")?;
        let openai_model = ai_repository::list_model_catalog(
            &self.state.persistence.postgres,
            Some(openai_provider.id),
        )
        .await
        .context("failed to list openai model catalog")?
        .into_iter()
        .find(|row| row.capability_kind == "embedding")
        .context("missing openai embedding model catalog entry")?;
        let qwen_model = ai_repository::list_model_catalog(
            &self.state.persistence.postgres,
            Some(qwen_provider.id),
        )
        .await
        .context("failed to list qwen model catalog")?
        .into_iter()
        .find(|row| row.capability_kind == "embedding")
        .context("missing qwen embedding model catalog entry")?;

        let projection = graph_repository::create_graph_projection(
            &self.state.persistence.postgres,
            workspace.id,
            library.id,
            Some(attempt),
            "active",
        )
        .await
        .context("failed to create graph projection")?;
        let node = graph_repository::create_graph_node(
            &self.state.persistence.postgres,
            projection.id,
            workspace.id,
            library.id,
            "entity:greenfield-test",
            "entity",
            "Greenfield Test",
            Some("Typed node candidate"),
            0,
        )
        .await
        .context("failed to create graph node")?;
        let other_node = graph_repository::create_graph_node(
            &self.state.persistence.postgres,
            projection.id,
            workspace.id,
            library.id,
            "entity:greenfield-other",
            "entity",
            "Greenfield Other",
            Some("Typed node target"),
            0,
        )
        .await
        .context("failed to create second graph node")?;
        let edge = graph_repository::create_graph_edge(
            &self.state.persistence.postgres,
            projection.id,
            workspace.id,
            library.id,
            "entity:greenfield-test--relates_to--entity:greenfield-other",
            "relates_to",
            node.id,
            other_node.id,
            Some("Typed edge candidate"),
            0,
        )
        .await
        .context("failed to create graph edge")?;

        let _ = graph_repository::upsert_graph_node_evidence(
            &self.state.persistence.postgres,
            node.id,
            chunk.id,
            revision.id,
            attempt,
            Some(chunk_result.id),
            rust_decimal::Decimal::new(125, 2),
        )
        .await
        .context("failed to create graph node evidence")?;
        let _ = graph_repository::upsert_graph_edge_evidence(
            &self.state.persistence.postgres,
            edge.id,
            chunk.id,
            revision.id,
            attempt,
            None,
            rust_decimal::Decimal::new(150, 2),
        )
        .await
        .context("failed to create graph edge evidence")?;

        self.workspace_id = workspace.id;
        self.library_id = library.id;
        self.document_id = document.id;
        self.revision_id = revision.id;
        self.chunk_id = chunk.id;
        self.attempt_id = cursor.attempt_id;
        self.projection_id = projection.id;
        self.node_id = node.id;
        self.other_node_id = other_node.id;
        self.edge_id = edge.id;
        self.primary_model_catalog_id = openai_model.id;
        self.alternate_model_catalog_id = qwen_model.id;
        Ok(self)
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

async fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let persistence = rustrag_backend::infra::persistence::Persistence {
        postgres,
        redis: redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for extract_graph test state")?,
    };
    let arango_client = Arc::new(
        ArangoClient::from_settings(&settings)
            .context("failed to build Arango client for extract_graph test state")?,
    );
    bootstrap_knowledge_plane(
        &arango_client,
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
    .context("failed to bootstrap Arango knowledge plane for extract_graph test state")?;

    Ok(AppState::from_dependencies(settings, persistence, arango_client))
}

fn replace_database_name(database_url: &str, new_database: &str) -> Result<String> {
    let (without_query, query_suffix) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash_index = without_query
        .rfind('/')
        .with_context(|| format!("database url is missing database name: {database_url}"))?;
    let mut rebuilt = format!("{}{new_database}", &without_query[..=slash_index]);
    if let Some(query) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(query);
    }
    Ok(rebuilt)
}

async fn terminate_database_connections(postgres: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(postgres)
    .await
    .with_context(|| format!("failed to terminate connections for {database_name}"))?;
    Ok(())
}

async fn assert_legacy_truth_tables_absent(pool: &PgPool) -> Result<()> {
    for table in ["entity", "relation", "runtime_vector_target"] {
        let exists = sqlx::query_scalar::<_, Option<String>>("select to_regclass($1)::text")
            .bind(table)
            .fetch_one(pool)
            .await
            .with_context(|| format!("failed to inspect legacy table {table}"))?;
        assert!(exists.is_none(), "legacy table {table} should not exist");
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn extract_flow_preserves_readable_text_chunk_results_and_resume_cursors() -> Result<()> {
    let fixture = ExtractGraphFixture::create().await?;

    let result = async {
        let extract_service = ExtractService::new();

        let content = extract_service
            .get_extract_content(&fixture.state, fixture.revision_id)
            .await
            .context("failed to load extract content")?;
        assert_eq!(content.extract_state, "readable");
        assert_eq!(
            content.normalized_text.as_deref(),
            Some("Readable extracted text for the canonical greenfield test."),
        );

        let chunk_results = extract_service
            .list_chunk_results(&fixture.state, fixture.attempt_id)
            .await
            .context("failed to list chunk results")?;
        assert_eq!(chunk_results.len(), 1);
        assert_eq!(chunk_results[0].chunk_id, fixture.chunk_id);
        assert_eq!(chunk_results[0].extract_state, "ready");

        let node_candidates = extract_service
            .list_node_candidates(&fixture.state, chunk_results[0].id)
            .await
            .context("failed to list node candidates")?;
        assert_eq!(node_candidates.len(), 1);
        assert_eq!(node_candidates[0].canonical_key, "entity:greenfield-test");

        let edge_candidates = extract_service
            .list_edge_candidates(&fixture.state, chunk_results[0].id)
            .await
            .context("failed to list edge candidates")?;
        assert_eq!(edge_candidates.len(), 1);
        assert_eq!(
            edge_candidates[0].canonical_key,
            "entity:greenfield-test--relates_to--entity:greenfield-other",
        );

        let cursor = extract_service
            .get_resume_cursor(&fixture.state, fixture.attempt_id)
            .await
            .context("failed to load resume cursor")?
            .context("missing resume cursor")?;
        assert_eq!(cursor.last_completed_chunk_index, 0);
        assert_eq!(cursor.replay_count, 0);
        assert_eq!(cursor.downgrade_level, 0);

        let replay = extract_service
            .increment_replay_count(&fixture.state, fixture.attempt_id)
            .await
            .context("failed to increment replay count")?;
        assert_eq!(replay.replay_count, 1);
        let downgrade = extract_service
            .increment_downgrade_level(&fixture.state, fixture.attempt_id)
            .await
            .context("failed to increment downgrade level")?;
        assert_eq!(downgrade.downgrade_level, 1);

        assert_legacy_truth_tables_absent(&fixture.state.persistence.postgres).await
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn graph_evidence_refreshes_support_counts_on_canonical_projection() -> Result<()> {
    let fixture = ExtractGraphFixture::create().await?;

    let result = async {
        let graph_service = GraphService::new();
        let refresh = graph_service
            .refresh_support_counts(
                &fixture.state,
                fixture.projection_id,
                &[fixture.node_id],
                &[fixture.edge_id],
            )
            .await
            .context("failed to refresh graph support counts")?;
        assert_eq!(refresh.scanned_nodes, 1);
        assert_eq!(refresh.scanned_edges, 1);
        assert_eq!(refresh.updated_nodes, 1);
        assert_eq!(refresh.updated_edges, 1);

        let node = graph_repository::get_graph_node_by_id(
            &fixture.state.persistence.postgres,
            fixture.node_id,
        )
        .await
        .context("failed to load refreshed graph node")?
        .context("missing graph node")?;
        let edge = graph_repository::get_graph_edge_by_id(
            &fixture.state.persistence.postgres,
            fixture.edge_id,
        )
        .await
        .context("failed to load refreshed graph edge")?
        .context("missing graph edge")?;
        assert_eq!(node.support_count, 1);
        assert_eq!(edge.support_count, 1);

        let node_evidence = graph_repository::list_graph_node_evidence_by_node(
            &fixture.state.persistence.postgres,
            fixture.node_id,
        )
        .await
        .context("failed to list node evidence")?;
        assert_eq!(node_evidence.len(), 1);
        assert_eq!(node_evidence[0].chunk_id, fixture.chunk_id);

        let edge_evidence = graph_repository::list_graph_edge_evidence_by_edge(
            &fixture.state.persistence.postgres,
            fixture.edge_id,
        )
        .await
        .context("failed to list edge evidence")?;
        assert_eq!(edge_evidence.len(), 1);
        assert_eq!(edge_evidence[0].chunk_id, fixture.chunk_id);

        assert_legacy_truth_tables_absent(&fixture.state.persistence.postgres).await
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn chunk_and_graph_embeddings_rebuild_and_select_active_indexes() -> Result<()> {
    let fixture = ExtractGraphFixture::create().await?;

    let result = async {
        let search_service = SearchService::new();
        let chunk_rebuilt = search_service
            .persist_chunk_embeddings(
                &fixture.state,
                &[
                    ChunkEmbeddingWrite {
                        chunk_id: fixture.chunk_id,
                        model_catalog_id: fixture.primary_model_catalog_id,
                        embedding_vector: vec![1.0, 2.0, 3.0],
                        active: false,
                    },
                    ChunkEmbeddingWrite {
                        chunk_id: fixture.chunk_id,
                        model_catalog_id: fixture.alternate_model_catalog_id,
                        embedding_vector: vec![4.0, 5.0, 6.0],
                        active: true,
                    },
                ],
            )
            .await
            .context("failed to persist chunk embeddings")?;
        assert_eq!(chunk_rebuilt, 2);

        let graph_rebuilt = search_service
            .persist_graph_node_embeddings(
                &fixture.state,
                &[
                    GraphNodeEmbeddingWrite {
                        node_id: fixture.node_id,
                        model_catalog_id: fixture.primary_model_catalog_id,
                        embedding_vector: vec![1.0, 1.0, 1.0],
                        active: false,
                    },
                    GraphNodeEmbeddingWrite {
                        node_id: fixture.node_id,
                        model_catalog_id: fixture.alternate_model_catalog_id,
                        embedding_vector: vec![2.0, 2.0, 2.0],
                        active: true,
                    },
                ],
            )
            .await
            .context("failed to persist graph node embeddings")?;
        assert_eq!(graph_rebuilt, 2);

        let chunk_rows = fixture
            .state
            .arango_search_store
            .list_chunk_vectors_by_chunk(fixture.chunk_id)
            .await
            .context("failed to list canonical chunk vectors")?;
        let chunk_model_keys =
            chunk_rows.iter().map(|row| row.embedding_model_key.as_str()).collect::<Vec<_>>();
        assert_eq!(chunk_rows.len(), 2);
        assert!(chunk_model_keys.contains(&fixture.primary_model_catalog_id.to_string().as_str()));
        assert!(
            chunk_model_keys.contains(&fixture.alternate_model_catalog_id.to_string().as_str())
        );

        let node_rows = fixture
            .state
            .arango_search_store
            .list_entity_vectors_by_entity(fixture.node_id)
            .await
            .context("failed to list canonical entity vectors")?;
        let node_model_keys =
            node_rows.iter().map(|row| row.embedding_model_key.as_str()).collect::<Vec<_>>();
        assert_eq!(node_rows.len(), 2);
        assert!(node_model_keys.contains(&fixture.primary_model_catalog_id.to_string().as_str()));
        assert!(node_model_keys.contains(&fixture.alternate_model_catalog_id.to_string().as_str()));

        let current_chunk_vector = SearchService::new()
            .select_current_chunk_vector(&chunk_rows)
            .context("missing current canonical chunk vector")?;
        assert_eq!(current_chunk_vector.chunk_id, fixture.chunk_id);
        assert_eq!(
            current_chunk_vector.embedding_model_key,
            fixture.alternate_model_catalog_id.to_string()
        );

        let current_node_vector = SearchService::new()
            .select_current_entity_vector(&node_rows)
            .context("missing current canonical entity vector")?;
        assert_eq!(current_node_vector.entity_id, fixture.node_id);
        assert_eq!(
            current_node_vector.embedding_model_key,
            fixture.alternate_model_catalog_id.to_string()
        );

        assert_legacy_truth_tables_absent(&fixture.state.persistence.postgres).await
    }
    .await;

    fixture.cleanup().await?;
    result
}
