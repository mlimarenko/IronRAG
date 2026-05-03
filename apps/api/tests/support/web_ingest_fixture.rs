use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::{content::ContentDocumentSummary, ingest::WebIngestRun},
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
    },
    shared::web::ingest::WebIngestUrlFilter,
};

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str, prefix: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("{prefix}_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .with_context(|| format!("failed to connect admin postgres for {prefix}"))?;

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
            .context("failed to reconnect admin postgres for cleanup")?;
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
    async fn create(settings: &Settings, prefix: &str) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("{prefix}_{}", Uuid::now_v7().simple());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(settings.arangodb_request_timeout_seconds.max(1)))
            .build()
            .with_context(|| format!("failed to build ArangoDB client for {prefix}"))?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .with_context(|| format!("failed to create temp Arango database for {prefix}"))?;
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
            .context("failed to drop temp Arango database")?;
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

pub struct WebIngestFixture {
    pub state: AppState,
    temp_database: TempDatabase,
    temp_arango: TempArangoDatabase,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
}

impl WebIngestFixture {
    pub async fn create(prefix: &str) -> Result<Self> {
        let mut settings = Settings::from_env().context("failed to load settings for fixture")?;
        let temp_database = TempDatabase::create(&settings.database_url, prefix).await?;
        let temp_arango = TempArangoDatabase::create(&settings, prefix).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.arangodb_database = temp_arango.name.clone();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect postgres for fixture")?;
        sqlx::raw_sql(include_str!("../../migrations/0001_init.sql"))
            .execute(&postgres)
            .await
            .context("failed to apply 0001_init.sql")?;

        let arango_client = Arc::new(
            ArangoClient::from_settings(&settings).context("failed to build Arango client")?,
        );
        arango_client.ping().await.context("failed to ping ArangoDB")?;
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
        .context("failed to bootstrap Arango knowledge plane")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to build redis client")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = AppState::from_dependencies(settings, persistence, arango_client)?;
        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("{prefix}-workspace-{}", Uuid::now_v7().simple())),
                    display_name: format!("{prefix} workspace"),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create workspace")?;
        let library = state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("{prefix}-library-{}", Uuid::now_v7().simple())),
                    display_name: format!("{prefix} library"),
                    description: Some(format!("{prefix} integration test fixture")),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create library")?;

        Ok(Self {
            state,
            temp_database,
            temp_arango,
            workspace_id: workspace.id,
            library_id: library.id,
        })
    }

    #[allow(dead_code)]
    pub async fn submit_single_page_run(&self, seed_url: String) -> Result<WebIngestRun> {
        self.submit_run(seed_url, "single_page", None, None, None).await
    }

    pub async fn submit_recursive_run(
        &self,
        seed_url: String,
        boundary_policy: &str,
        max_depth: Option<i32>,
        max_pages: Option<i32>,
    ) -> Result<WebIngestRun> {
        self.submit_run(
            seed_url,
            "recursive_crawl",
            Some(boundary_policy.to_string()),
            max_depth,
            max_pages,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn submit_recursive_run_with_url_filter(
        &self,
        seed_url: String,
        boundary_policy: &str,
        max_depth: Option<i32>,
        max_pages: Option<i32>,
        url_filter: WebIngestUrlFilter,
    ) -> Result<WebIngestRun> {
        self.state
            .canonical_services
            .web_ingest
            .create_run(
                &self.state,
                CreateWebIngestRunCommand {
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    seed_url,
                    mode: "recursive_crawl".to_string(),
                    boundary_policy: Some(boundary_policy.to_string()),
                    max_depth,
                    max_pages,
                    url_filter,
                    requested_by_principal_id: None,
                    request_surface: "test".to_string(),
                    idempotency_key: None,
                },
            )
            .await
            .context("failed to submit filtered recursive web ingest run")
    }

    async fn submit_run(
        &self,
        seed_url: String,
        mode: &str,
        boundary_policy: Option<String>,
        max_depth: Option<i32>,
        max_pages: Option<i32>,
    ) -> Result<WebIngestRun> {
        self.state
            .canonical_services
            .web_ingest
            .create_run(
                &self.state,
                CreateWebIngestRunCommand {
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    seed_url,
                    mode: mode.to_string(),
                    boundary_policy,
                    max_depth,
                    max_pages,
                    url_filter: ironrag_backend::shared::web::ingest::default_web_ingest_policy()
                        .url_filter,
                    requested_by_principal_id: None,
                    request_surface: "test".to_string(),
                    idempotency_key: None,
                },
            )
            .await
            .context("failed to submit web ingest run")
    }

    #[allow(dead_code)]
    pub async fn wait_for_document_ready(
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

    #[allow(dead_code)]
    pub async fn wait_for_run_terminal(
        &self,
        run_id: Uuid,
        timeout: Duration,
    ) -> Result<WebIngestRun> {
        let deadline = time::Instant::now() + timeout;
        loop {
            let run = self
                .state
                .canonical_services
                .web_ingest
                .get_run(&self.state, run_id)
                .await
                .context("failed to poll web ingest run")?;
            if matches!(
                run.run_state.as_str(),
                "completed" | "completed_partial" | "failed" | "canceled"
            ) {
                return Ok(run);
            }
            if time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for web ingest run {run_id} to finish");
            }
            time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_arango.drop().await?;
        self.temp_database.drop().await
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
