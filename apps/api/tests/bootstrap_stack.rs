use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::persistence::{canonical_ai_catalog_seeded, canonical_baseline_present},
};

const SEEDED_PROVIDER_COUNT: i64 = 3;
const SEEDED_MODEL_COUNT: i64 = 40;
const SEEDED_PRICE_COUNT: i64 = 118;

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("bootstrap_stack_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect bootstrap-stack admin postgres")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {database_name}"))?;
        admin_pool.close().await;

        let database_url = replace_database_name(base_database_url, &database_name)?;

        Ok(Self { name: database_name, admin_url, database_url })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect bootstrap-stack admin postgres for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct BootstrapStackFixture {
    state: AppState,
    temp_database: TempDatabase,
}

impl BootstrapStackFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for bootstrap-stack test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;

        let state = AppState::new(settings).await?;
        Ok(Self { state, temp_database })
    }

    async fn cleanup(self) -> Result<()> {
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

async fn scalar_count(postgres: &PgPool, table_name: &str) -> Result<i64> {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("select count(*) from {table_name}")))
        .fetch_one(postgres)
        .await
        .with_context(|| format!("failed to count rows in {table_name}"))
}

async fn table_exists(postgres: &PgPool, table_name: &str) -> Result<bool> {
    sqlx::query_scalar::<_, Option<String>>("select to_regclass($1)::text")
        .bind(table_name)
        .fetch_one(postgres)
        .await
        .with_context(|| format!("failed to check table {table_name}"))
        .map(|table| table.is_some())
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn fresh_startup_bootstraps_postgres_catalog_and_knowledge_plane() -> Result<()> {
    let fixture = BootstrapStackFixture::create().await?;

    let result = async {
        assert!(canonical_baseline_present(&fixture.state.persistence.postgres).await?);
        assert!(canonical_ai_catalog_seeded(&fixture.state.persistence.postgres).await?);
        assert_eq!(
            scalar_count(&fixture.state.persistence.postgres, "ai_provider_catalog").await?,
            SEEDED_PROVIDER_COUNT
        );
        assert_eq!(
            scalar_count(&fixture.state.persistence.postgres, "ai_model_catalog").await?,
            SEEDED_MODEL_COUNT
        );
        assert_eq!(
            scalar_count(&fixture.state.persistence.postgres, "ai_price_catalog").await?,
            SEEDED_PRICE_COUNT
        );

        for table in [
            "knowledge_document",
            "knowledge_revision",
            "knowledge_chunk",
            "knowledge_chunk_vector",
            "knowledge_entity",
            "knowledge_relation",
            "knowledge_context_bundle",
        ] {
            assert!(
                table_exists(&fixture.state.persistence.postgres, table).await?,
                "missing {table}"
            );
        }

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
