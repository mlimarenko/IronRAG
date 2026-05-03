#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{sync::broadcast, time};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::content::ContentDocumentSummary,
    infra::{
        arangodb::{
            bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
            client::ArangoClient,
        },
        persistence::Persistence,
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        ingest::web::CreateWebIngestRunCommand,
        ingest::worker,
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
        let database_name = format!("web_ingest_single_page_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect admin postgres for web_ingest_single_page")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(&format!("drop database if exists \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale database {database_name}"))?;
        sqlx::query(&format!("create database \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create database {database_name}"))?;
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
            .context("failed to reconnect admin postgres for web_ingest_single_page cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct TempArangoDatabase {
    base_url: String,
    username: String,
    password: String,
    name: String,
    http: reqwest::Client,
}

impl TempArangoDatabase {
    async fn create(settings: &Settings) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("web_ingest_single_page_{}", Uuid::now_v7().simple());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(settings.arangodb_request_timeout_seconds.max(1)))
            .build()
            .context("failed to build ArangoDB client for web_ingest_single_page")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .context("failed to create temp Arango database for web_ingest_single_page")?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to create temp Arango database {}: status {}",
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
            .context("failed to drop temp Arango database for web_ingest_single_page")?;
        if response.status() != reqwest::StatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to drop temp Arango database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }
}

struct WebIngestSinglePageFixture {
    state: AppState,
    temp_database: TempDatabase,
    temp_arango: TempArangoDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl WebIngestSinglePageFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for web_ingest_single_page")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let temp_arango = TempArangoDatabase::create(&settings).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.arangodb_database = temp_arango.name.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect postgres for web_ingest_single_page")?;
        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&postgres)
            .await
            .context("failed to apply 0001_init.sql for web_ingest_single_page")?;

        let arango_client = Arc::new(
            ArangoClient::from_settings(&settings)
                .context("failed to build Arango client for web_ingest_single_page")?,
        );
        arango_client.ping().await.context("failed to ping ArangoDB for web_ingest_single_page")?;
        bootstrap_knowledge_plane(
            &arango_client,
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
        .context("failed to bootstrap Arango knowledge plane for web_ingest_single_page")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to build redis client for web_ingest_single_page")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = AppState::from_dependencies(settings, persistence, arango_client)?;
        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("web-ingest-workspace-{}", Uuid::now_v7().simple())),
                    display_name: "Web Ingest Single Page Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create workspace for web_ingest_single_page")?;
        let library = state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("web-ingest-library-{}", Uuid::now_v7().simple())),
                    display_name: "Web Ingest Single Page Library".to_string(),
                    description: Some(
                        "web ingest single page integration test fixture".to_string(),
                    ),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create library for web_ingest_single_page")?;

        Ok(Self {
            state,
            temp_database,
            temp_arango,
            workspace_id: workspace.id,
            library_id: library.id,
        })
    }

    async fn submit_single_page_run(
        &self,
        seed_url: String,
    ) -> Result<ironrag_backend::domains::ingest::WebIngestRun> {
        self.state
            .canonical_services
            .web_ingest
            .create_run(
                &self.state,
                CreateWebIngestRunCommand {
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    seed_url,
                    mode: "single_page".to_string(),
                    boundary_policy: None,
                    max_depth: None,
                    max_pages: None,
                    url_filter: ironrag_backend::shared::web::ingest::default_web_ingest_policy()
                        .url_filter,
                    requested_by_principal_id: None,
                    request_surface: "test".to_string(),
                    idempotency_key: None,
                },
            )
            .await
            .context("failed to submit single-page web ingest run")
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_arango.drop().await?;
        self.temp_database.drop().await
    }

    async fn wait_for_document_ready(
        &self,
        document_id: Uuid,
        timeout: Duration,
    ) -> Result<ContentDocumentSummary> {
        let deadline = time::Instant::now() + timeout;
        loop {
            let summary = self
                .state
                .canonical_services
                .content
                .get_document(&self.state, document_id)
                .await
                .context("failed to poll document while waiting for canonical worker")?;
            let readiness = summary.readiness.as_ref();
            let mutation = summary.pipeline.latest_mutation.as_ref();
            if readiness.is_some_and(|value| {
                value.text_state == "ready"
                    && value.vector_state == "ready"
                    && value.graph_state == "ready"
            }) && mutation.is_some_and(|value| value.mutation_state == "applied")
            {
                return Ok(summary);
            }
            if time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for document {document_id} to become ready");
            }
            time::sleep(Duration::from_millis(250)).await;
        }
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

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn single_page_default_does_not_follow_links() -> Result<()> {
    let fixture = WebIngestSinglePageFixture::create().await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run(server.url("/seed")).await?;
        let pages = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list run pages")?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents")?;

        assert_eq!(run.run_state, "completed");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].normalized_url, server.url("/seed"));
        assert_eq!(pages[0].candidate_state, "processed");
        assert_eq!(documents.len(), 1);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn single_page_direct_download_passthrough_materializes_one_document() -> Result<()> {
    let fixture = WebIngestSinglePageFixture::create().await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run(server.url("/download.txt")).await?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after download ingest")?;

        assert_eq!(run.run_state, "completed");
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].active_revision.as_ref().map(|revision| revision.mime_type.as_str()),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            documents[0]
                .active_revision
                .as_ref()
                .and_then(|revision| revision.source_uri.as_deref()),
            Some(server.url("/download.txt").as_str())
        );

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn single_page_unsupported_payload_fails_without_materializing_documents() -> Result<()> {
    let fixture = WebIngestSinglePageFixture::create().await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run(server.url("/unsupported.bin")).await?;
        let pages = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list failed run pages")?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after unsupported ingest")?;

        assert_eq!(run.run_state, "failed");
        assert_eq!(run.failure_code.as_deref(), Some("unsupported_content"));
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].candidate_state, "failed");
        assert_eq!(pages[0].classification_reason.as_deref(), Some("unsupported_content"));
        assert_eq!(documents.len(), 0);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn single_page_redirects_canonicalize_to_final_url_identity() -> Result<()> {
    let fixture = WebIngestSinglePageFixture::create().await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run(server.url("/redirect")).await?;
        let pages = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list redirect run pages")?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after redirect ingest")?;

        assert_eq!(run.run_state, "completed");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].normalized_url, server.url("/redirect"));
        assert_eq!(pages[0].final_url.as_deref(), Some(server.url("/canonical").as_str()));
        assert_eq!(pages[0].canonical_url.as_deref(), Some(server.url("/canonical").as_str()));
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].document.external_key, server.url("/canonical"));

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn repeated_single_page_submission_updates_one_logical_document_per_library() -> Result<()> {
    let fixture = WebIngestSinglePageFixture::create().await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let first_run = fixture.submit_single_page_run(server.url("/seed")).await?;
        let first_page = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, first_run.run_id)
            .await
            .context("failed to list first run pages")?;
        let second_run = fixture.submit_single_page_run(server.url("/seed")).await?;
        let second_page = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, second_run.run_id)
            .await
            .context("failed to list second run pages")?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after repeated submit")?;

        assert_eq!(documents.len(), 1);
        assert_eq!(first_page.len(), 1);
        assert_eq!(second_page.len(), 1);
        assert_eq!(first_page[0].document_id, second_page[0].document_id);

        let document_id = documents[0].document.id;
        let revisions = fixture
            .state
            .canonical_services
            .content
            .list_revisions(&fixture.state, document_id)
            .await
            .context("failed to list revisions after repeated submit")?;
        assert_eq!(revisions.len(), 2);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn single_page_worker_runs_web_capture_through_canonical_pipeline() -> Result<()> {
    let fixture = WebIngestSinglePageFixture::create().await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run(server.url("/seed")).await?;
        let pages = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list single-page run pages before worker execution")?;
        let document_id = pages
            .first()
            .and_then(|page| page.document_id)
            .context("missing materialized document id")?;

        let queued_summary = fixture
            .state
            .canonical_services
            .content
            .get_document(&fixture.state, document_id)
            .await
            .context("failed to load queued document summary")?;
        let queued_readiness_summary = serde_json::to_value(
            queued_summary
                .readiness_summary
                .as_ref()
                .context("missing queued readiness summary")?,
        )
        .context("failed to serialize queued readiness summary")?;
        assert_eq!(
            queued_readiness_summary.get("activityStatus").and_then(serde_json::Value::as_str),
            Some("queued")
        );
        assert_eq!(
            queued_summary
                .readiness_summary
                .as_ref()
                .and_then(|summary| summary.last_job_stage.as_deref()),
            None
        );

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);

        let ready_summary =
            fixture.wait_for_document_ready(document_id, Duration::from_secs(15)).await?;
        let ready_readiness_summary = serde_json::to_value(
            ready_summary.readiness_summary.as_ref().context("missing ready readiness summary")?,
        )
        .context("failed to serialize ready readiness summary")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(
            ready_summary.readiness.as_ref().map(|readiness| readiness.text_state.as_str()),
            Some("ready")
        );
        assert_eq!(
            ready_summary.readiness.as_ref().map(|readiness| readiness.graph_state.as_str()),
            Some("ready")
        );
        assert_eq!(
            ready_readiness_summary.get("activityStatus").and_then(serde_json::Value::as_str),
            Some("ready")
        );
        let mutation_id = ready_summary
            .pipeline
            .latest_mutation
            .as_ref()
            .map(|mutation| mutation.id)
            .context("missing latest mutation after canonical worker execution")?;
        let job_handle = fixture
            .state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(&fixture.state, mutation_id)
            .await
            .context("failed to load canonical ingest job handle")?
            .context("missing canonical ingest job handle after worker execution")?;
        let attempt_id = job_handle
            .latest_attempt
            .as_ref()
            .map(|attempt| attempt.id)
            .context("missing canonical ingest attempt after worker execution")?;
        let stage_events = fixture
            .state
            .canonical_services
            .ingest
            .list_stage_events(&fixture.state, attempt_id)
            .await
            .context("failed to list canonical ingest stage events")?;
        let observed_stages = stage_events
            .iter()
            .map(|event| (event.stage_name.as_str(), event.stage_state.as_str()))
            .collect::<Vec<_>>();

        assert!(observed_stages.contains(&("extract_content", "completed")));
        assert!(observed_stages.contains(&("prepare_structure", "started")));
        assert!(observed_stages.contains(&("prepare_structure", "completed")));
        assert!(observed_stages.contains(&("chunk_content", "completed")));
        assert!(observed_stages.contains(&("extract_technical_facts", "completed")));
        assert!(observed_stages.contains(&("embed_chunk", "completed")));
        assert!(observed_stages.contains(&("extract_graph", "started")));
        assert!(observed_stages.contains(&("extract_graph", "completed")));
        assert!(observed_stages.contains(&("finalizing", "completed")));

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}
