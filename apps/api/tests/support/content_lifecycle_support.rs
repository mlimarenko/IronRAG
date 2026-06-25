use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::persistence::Persistence,
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::CreateRevisionCommand,
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
        let database_name = format!("content_lifecycle_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect admin postgres for content lifecycle test")?;

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
            .context("failed to reconnect admin postgres for content lifecycle cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

pub struct ContentLifecycleFixture {
    pub state: AppState,
    temp_database: TempDatabase,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
}

impl ContentLifecycleFixture {
    pub async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for content lifecycle test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect content lifecycle postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply canonical migrations for content lifecycle test")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for content lifecycle test state")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = AppState::from_dependencies(settings, persistence)?;
        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("content-workspace-{}", Uuid::now_v7().simple())),
                    display_name: "Content Lifecycle Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create content lifecycle workspace")?;
        let library = state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("content-library-{}", Uuid::now_v7().simple())),
                    display_name: "Content Lifecycle Library".to_string(),
                    description: Some("canonical content lifecycle test fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create content lifecycle library")?;

        Ok(Self { state, temp_database, workspace_id: workspace.id, library_id: library.id })
    }

    pub async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
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

pub fn revision_command(
    document_id: Uuid,
    source_kind: &str,
    checksum: &str,
    title: &str,
    source_uri: Option<&str>,
) -> CreateRevisionCommand {
    CreateRevisionCommand {
        document_id,
        content_source_kind: source_kind.to_string(),
        checksum: checksum.to_string(),
        mime_type: "text/plain".to_string(),
        byte_size: 128,
        title: Some(title.to_string()),
        language_code: Some("en".to_string()),
        source_uri: source_uri.map(ToString::to_string),
        document_hint: None,
        storage_key: Some(format!("storage/{checksum}")),
        created_by_principal_id: None,
    }
}
