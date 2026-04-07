use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use reqwest::Client;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::{
            bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
            client::ArangoClient,
        },
        persistence::Persistence,
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        knowledge_service::{
            CreateKnowledgeChunkCommand, CreateKnowledgeDocumentCommand,
            CreateKnowledgeRevisionCommand, PromoteKnowledgeDocumentCommand,
        },
    },
};

struct TempPostgresDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempPostgresDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let name = format!("knowledge_lifecycle_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect to postgres admin database for knowledge_lifecycle")?;

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
        let admin_pool =
            PgPoolOptions::new().max_connections(1).connect(&self.admin_url).await.context(
                "failed to reconnect postgres admin database for knowledge_lifecycle cleanup",
            )?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

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
        let name = format!("knowledge_lifecycle_{}", Uuid::now_v7().simple());
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
            .context("failed to create temp ArangoDB database for knowledge_lifecycle")?;
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
            .context("failed to drop temp ArangoDB database for knowledge_lifecycle")?;
        if response.status() != reqwest::StatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow!(
                "failed to drop temp ArangoDB database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }
}

struct KnowledgeLifecycleFixture {
    state: AppState,
    postgres: TempPostgresDatabase,
    arango: TempArangoDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl KnowledgeLifecycleFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for knowledge_lifecycle")?;
        let postgres = TempPostgresDatabase::create(&settings.database_url).await?;
        let arango = TempArangoDatabase::create(&settings).await?;
        settings.database_url = postgres.database_url.clone();
        settings.arangodb_database = arango.name.clone();

        let postgres_pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&postgres.database_url)
            .await
            .context("failed to connect knowledge_lifecycle postgres")?;
        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&postgres_pool)
            .await
            .context("failed to apply canonical 0001_init.sql for knowledge_lifecycle")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for knowledge_lifecycle")?;
        let arango_client = Arc::new(
            ArangoClient::from_settings(&settings).context("failed to build Arango client")?,
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
        .context("failed to bootstrap Arango knowledge plane for knowledge_lifecycle")?;

        let state = AppState::from_dependencies(
            settings,
            Persistence { postgres: postgres_pool, redis },
            arango_client,
        );

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("knowledge-lifecycle-workspace-{suffix}")),
                    display_name: "Knowledge Lifecycle Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create knowledge_lifecycle workspace")?;
        let library = state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("knowledge-lifecycle-library-{suffix}")),
                    display_name: "Knowledge Lifecycle Library".to_string(),
                    description: Some("knowledge lifecycle integration fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create knowledge_lifecycle library")?;

        Ok(Self { state, postgres, arango, workspace_id: workspace.id, library_id: library.id })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.arango.drop().await?;
        self.postgres.drop().await
    }
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

fn knowledge_revision_command(
    fixture: &KnowledgeLifecycleFixture,
    document_id: Uuid,
    revision_id: Uuid,
    revision_number: i64,
    revision_kind: &str,
    title: &str,
    normalized_text: &str,
) -> CreateKnowledgeRevisionCommand {
    CreateKnowledgeRevisionCommand {
        revision_id,
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        document_id,
        revision_number,
        revision_state: "active".to_string(),
        revision_kind: revision_kind.to_string(),
        storage_ref: Some(format!("memory://{revision_id}")),
        source_uri: Some(format!("memory://knowledge-lifecycle/{revision_id}")),
        mime_type: "text/plain".to_string(),
        checksum: format!("sha256:{revision_id}"),
        byte_size: i64::try_from(normalized_text.len()).unwrap_or(i64::MAX),
        title: Some(title.to_string()),
        normalized_text: Some(normalized_text.to_string()),
        text_checksum: Some(format!("sha256:text:{revision_id}")),
        text_state: "readable".to_string(),
        vector_state: "ready".to_string(),
        graph_state: "pending".to_string(),
        text_readable_at: Some(Utc::now()),
        vector_ready_at: Some(Utc::now()),
        graph_ready_at: None,
        superseded_by_revision_id: None,
    }
}

async fn write_chunk(
    fixture: &KnowledgeLifecycleFixture,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    text: &str,
) -> Result<Uuid> {
    let chunk_id = Uuid::now_v7();
    fixture
        .state
        .canonical_services
        .knowledge
        .write_chunk(
            &fixture.state,
            CreateKnowledgeChunkCommand {
                chunk_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id,
                revision_id,
                chunk_index,
                chunk_kind: None,
                content_text: text.to_string(),
                normalized_text: text.to_string(),
                span_start: None,
                span_end: None,
                token_count: Some(
                    i32::try_from(text.split_whitespace().count()).unwrap_or(i32::MAX),
                ),
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,
            },
        )
        .await
        .context("failed to write knowledge chunk")?;
    Ok(chunk_id)
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango"]
async fn canonical_knowledge_lifecycle_persists_document_shell_revisions_pointers_and_chunks()
-> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let external_key = format!("knowledge-doc-{}", document_id.simple());

        fixture
            .state
            .canonical_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: external_key.clone(),
                    title: Some("Knowledge Lifecycle Document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create knowledge document shell")?;

        fixture
            .state
            .canonical_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    document_id,
                    revision_id,
                    1,
                    "upload",
                    "Initial Revision",
                    "Ada Lovelace wrote the analytical note.",
                ),
            )
            .await
            .context("failed to write knowledge revision")?;

        let chunk_a = write_chunk(
            &fixture,
            document_id,
            revision_id,
            0,
            "Ada Lovelace wrote the analytical note.",
        )
        .await?;
        let chunk_b =
            write_chunk(&fixture, document_id, revision_id, 1, "Charles Babbage built the engine.")
                .await?;

        fixture
            .state
            .canonical_services
            .knowledge
            .promote_document(
                &fixture.state,
                PromoteKnowledgeDocumentCommand {
                    document_id,
                    document_state: "active".to_string(),
                    active_revision_id: Some(revision_id),
                    readable_revision_id: Some(revision_id),
                    latest_revision_no: Some(1),
                    deleted_at: None,
                },
            )
            .await
            .context("failed to promote knowledge document")?;

        let document = fixture
            .state
            .arango_document_store
            .get_document(document_id)
            .await
            .context("failed to reload knowledge document")?
            .context("knowledge document missing from arango")?;
        assert_eq!(document.external_key, external_key);
        assert_eq!(document.document_state, "active");
        assert_eq!(document.active_revision_id, Some(revision_id));
        assert_eq!(document.readable_revision_id, Some(revision_id));
        assert_eq!(document.latest_revision_no, Some(1));

        let revisions = fixture
            .state
            .arango_document_store
            .list_revisions_by_document(document_id)
            .await
            .context("failed to list knowledge revisions")?;
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].revision_id, revision_id);
        assert_eq!(revisions[0].text_state, "readable");

        let chunks = fixture
            .state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .context("failed to list knowledge chunks")?;
        assert_eq!(
            chunks.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            vec![chunk_a, chunk_b]
        );
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[1].chunk_index, 1);
        assert_eq!(chunks[0].document_id, document_id);
        assert_eq!(chunks[0].library_id, fixture.library_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango"]
async fn canonical_knowledge_lifecycle_handles_append_replace_and_delete_without_postgres_content_truth()
-> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let revision_one_id = Uuid::now_v7();
        let revision_two_id = Uuid::now_v7();
        let revision_three_id = Uuid::now_v7();

        fixture
            .state
            .canonical_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: format!("knowledge-mutation-doc-{}", document_id.simple()),
                    title: Some("Knowledge Mutation Document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create mutation document shell")?;

        fixture
            .state
            .canonical_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    document_id,
                    revision_one_id,
                    1,
                    "upload",
                    "Base Revision",
                    "Base memory about the engine.",
                ),
            )
            .await
            .context("failed to write revision one")?;
        write_chunk(&fixture, document_id, revision_one_id, 0, "Base memory about the engine.")
            .await?;

        fixture
            .state
            .canonical_services
            .knowledge
            .promote_document(
                &fixture.state,
                PromoteKnowledgeDocumentCommand {
                    document_id,
                    document_state: "active".to_string(),
                    active_revision_id: Some(revision_one_id),
                    readable_revision_id: Some(revision_one_id),
                    latest_revision_no: Some(1),
                    deleted_at: None,
                },
            )
            .await
            .context("failed to promote revision one")?;

        fixture
            .state
            .canonical_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    document_id,
                    revision_two_id,
                    2,
                    "append",
                    "Appended Revision",
                    "Base memory about the engine. Additional note about Ada.",
                ),
            )
            .await
            .context("failed to write revision two")?;
        write_chunk(
            &fixture,
            document_id,
            revision_two_id,
            0,
            "Base memory about the engine. Additional note about Ada.",
        )
        .await?;

        fixture
            .state
            .canonical_services
            .knowledge
            .promote_document(
                &fixture.state,
                PromoteKnowledgeDocumentCommand {
                    document_id,
                    document_state: "active".to_string(),
                    active_revision_id: Some(revision_two_id),
                    readable_revision_id: Some(revision_one_id),
                    latest_revision_no: Some(2),
                    deleted_at: None,
                },
            )
            .await
            .context("failed to promote revision two with split readable pointer")?;

        let split_pointer_document = fixture
            .state
            .arango_document_store
            .get_document(document_id)
            .await
            .context("failed to reload split pointer document")?
            .context("split pointer document missing")?;
        assert_eq!(split_pointer_document.active_revision_id, Some(revision_two_id));
        assert_eq!(split_pointer_document.readable_revision_id, Some(revision_one_id));
        assert_eq!(split_pointer_document.latest_revision_no, Some(2));

        fixture
            .state
            .canonical_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    document_id,
                    revision_three_id,
                    3,
                    "replace",
                    "Replacement Revision",
                    "Replacement memory about the analytical engine only.",
                ),
            )
            .await
            .context("failed to write revision three")?;
        write_chunk(
            &fixture,
            document_id,
            revision_three_id,
            0,
            "Replacement memory about the analytical engine only.",
        )
        .await?;

        let deleted_chunks = fixture
            .state
            .canonical_services
            .knowledge
            .delete_revision_chunks(&fixture.state, revision_one_id)
            .await
            .context("failed to delete superseded revision one chunks")?;
        assert_eq!(deleted_chunks.len(), 1);
        assert!(
            fixture
                .state
                .arango_document_store
                .list_chunks_by_revision(revision_one_id)
                .await
                .context("failed to re-list revision one chunks")?
                .is_empty()
        );

        fixture
            .state
            .canonical_services
            .knowledge
            .promote_document(
                &fixture.state,
                PromoteKnowledgeDocumentCommand {
                    document_id,
                    document_state: "deleted".to_string(),
                    active_revision_id: None,
                    readable_revision_id: Some(revision_three_id),
                    latest_revision_no: Some(3),
                    deleted_at: Some(Utc::now()),
                },
            )
            .await
            .context("failed to tombstone knowledge document")?;

        let tombstoned = fixture
            .state
            .arango_document_store
            .get_document(document_id)
            .await
            .context("failed to reload tombstoned document")?
            .context("tombstoned document missing")?;
        assert_eq!(tombstoned.document_state, "deleted");
        assert_eq!(tombstoned.active_revision_id, None);
        assert_eq!(tombstoned.readable_revision_id, Some(revision_three_id));
        assert_eq!(tombstoned.latest_revision_no, Some(3));
        assert!(tombstoned.deleted_at.is_some());

        let revisions = fixture
            .state
            .arango_document_store
            .list_revisions_by_document(document_id)
            .await
            .context("failed to list mutation revisions")?;
        assert_eq!(
            revisions.iter().map(|row| row.revision_number).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango"]
async fn knowledge_readiness_coherence_keeps_readable_pointer_until_new_revision_is_ready()
-> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let readable_revision_id = Uuid::now_v7();
        let active_revision_id = Uuid::now_v7();

        fixture
            .state
            .canonical_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: format!("readiness-doc-{}", document_id.simple()),
                    title: Some("Readiness Coherence Document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create readiness coherence document shell")?;

        fixture
            .state
            .canonical_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    document_id,
                    readable_revision_id,
                    1,
                    "upload",
                    "Readable Baseline",
                    "Readable baseline memory for coherence checks.",
                ),
            )
            .await
            .context("failed to write readable baseline revision")?;
        write_chunk(
            &fixture,
            document_id,
            readable_revision_id,
            0,
            "Readable baseline memory for coherence checks.",
        )
        .await?;

        let mut active_revision = knowledge_revision_command(
            &fixture,
            document_id,
            active_revision_id,
            2,
            "append",
            "Active But Not Readable",
            "Active revision is newer but still processing downstream readiness.",
        );
        active_revision.vector_state = "pending".to_string();
        active_revision.graph_state = "pending".to_string();
        active_revision.vector_ready_at = None;
        active_revision.graph_ready_at = None;

        fixture
            .state
            .canonical_services
            .knowledge
            .write_revision(&fixture.state, active_revision)
            .await
            .context("failed to write active non-ready revision")?;
        write_chunk(
            &fixture,
            document_id,
            active_revision_id,
            0,
            "Active revision is newer but still processing downstream readiness.",
        )
        .await?;

        fixture
            .state
            .canonical_services
            .knowledge
            .promote_document(
                &fixture.state,
                PromoteKnowledgeDocumentCommand {
                    document_id,
                    document_state: "active".to_string(),
                    active_revision_id: Some(active_revision_id),
                    readable_revision_id: Some(readable_revision_id),
                    latest_revision_no: Some(2),
                    deleted_at: None,
                },
            )
            .await
            .context("failed to promote split readiness pointers")?;

        let document = fixture
            .state
            .arango_document_store
            .get_document(document_id)
            .await
            .context("failed to reload readiness coherence document")?
            .context("readiness coherence document missing")?;
        assert_eq!(document.active_revision_id, Some(active_revision_id));
        assert_eq!(document.readable_revision_id, Some(readable_revision_id));
        assert_ne!(document.active_revision_id, document.readable_revision_id);
        assert_eq!(document.latest_revision_no, Some(2));

        let readable_chunks = fixture
            .state
            .arango_document_store
            .list_chunks_by_revision(readable_revision_id)
            .await
            .context("failed to list readable baseline chunks")?;
        let active_chunks = fixture
            .state
            .arango_document_store
            .list_chunks_by_revision(active_revision_id)
            .await
            .context("failed to list active processing chunks")?;
        assert!(!readable_chunks.is_empty());
        assert!(!active_chunks.is_empty());
        assert!(readable_chunks.iter().all(|chunk| chunk.chunk_state == "ready"));
        assert!(active_chunks.iter().all(|chunk| chunk.chunk_state == "ready"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
