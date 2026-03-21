use redis::Client as RedisClient;
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::app::config::Settings;

const SEEDED_PROVIDER_KINDS: [&str; 3] = ["openai", "deepseek", "qwen"];
const CANONICAL_BASELINE_TABLES: [&str; 7] = [
    "catalog_workspace",
    "catalog_library",
    "iam_principal",
    "iam_user",
    "ai_provider_catalog",
    "ai_model_catalog",
    "ai_price_catalog",
];

#[derive(Clone)]
pub struct Persistence {
    pub postgres: PgPool,
    pub redis: RedisClient,
}

impl Persistence {
    /// Connects to Postgres and Redis, verifies Redis responsiveness, and runs migrations.
    ///
    /// # Errors
    /// Returns any database, migration, Redis client, or Redis ping initialization error.
    pub async fn connect(settings: &Settings) -> anyhow::Result<Self> {
        let postgres = PgPoolOptions::new()
            .max_connections(settings.database_max_connections)
            .connect(&settings.database_url)
            .await?;

        sqlx::migrate!("./migrations").run(&postgres).await?;
        validate_canonical_bootstrap_state(&postgres, settings).await?;

        let redis = RedisClient::open(settings.redis_url.clone())?;
        let mut conn = redis.get_multiplexed_tokio_connection().await?;
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;

        Ok(Self { postgres, redis })
    }
}

async fn validate_canonical_bootstrap_state(
    postgres: &PgPool,
    settings: &Settings,
) -> anyhow::Result<()> {
    if !settings.destructive_fresh_bootstrap_settings().required {
        return Ok(());
    }

    if !canonical_baseline_present(postgres).await? {
        anyhow::bail!(
            "canonical bootstrap validation failed: required tables `catalog_workspace`, `catalog_library`, `iam_principal`, `iam_user`, `ai_provider_catalog`, `ai_model_catalog`, and `ai_price_catalog` are missing after migration"
        );
    }

    anyhow::ensure!(
        canonical_ai_catalog_seeded(postgres).await?,
        "canonical bootstrap validation failed: ai_provider_catalog, ai_model_catalog, or ai_price_catalog is missing seeded rows after migration"
    );

    Ok(())
}

pub async fn canonical_baseline_present(postgres: &PgPool) -> anyhow::Result<bool> {
    for table_name in CANONICAL_BASELINE_TABLES {
        if !table_exists(postgres, table_name).await? {
            return Ok(false);
        }
    }

    Ok(true)
}

pub async fn canonical_ai_catalog_seeded(postgres: &PgPool) -> anyhow::Result<bool> {
    if !table_exists(postgres, "ai_provider_catalog").await?
        || !table_exists(postgres, "ai_model_catalog").await?
        || !table_exists(postgres, "ai_price_catalog").await?
    {
        return Ok(false);
    }

    let provider_count = sqlx::query_scalar::<_, i64>(
        "select count(*) from ai_provider_catalog where provider_kind = any($1)",
    )
    .bind(&SEEDED_PROVIDER_KINDS)
    .fetch_one(postgres)
    .await?;
    let model_count = sqlx::query_scalar::<_, i64>("select count(*) from ai_model_catalog")
        .fetch_one(postgres)
        .await?;
    let price_count = sqlx::query_scalar::<_, i64>("select count(*) from ai_price_catalog")
        .fetch_one(postgres)
        .await?;

    Ok(provider_count >= i64::try_from(SEEDED_PROVIDER_KINDS.len()).unwrap_or(0)
        && model_count > 0
        && price_count > 0)
}

pub async fn legacy_runtime_repair_tables_present(postgres: &PgPool) -> anyhow::Result<bool> {
    Ok(table_exists(postgres, "ingestion_job").await?
        && table_exists(postgres, "runtime_ingestion_run").await?)
}

pub async fn legacy_ui_bootstrap_tables_present(postgres: &PgPool) -> anyhow::Result<bool> {
    Ok(table_exists(postgres, "ui_user").await?
        && table_exists(postgres, "workspace").await?
        && table_exists(postgres, "project").await?)
}

pub async fn canonical_ui_bootstrap_tables_present(postgres: &PgPool) -> anyhow::Result<bool> {
    Ok(table_exists(postgres, "iam_principal").await?
        && table_exists(postgres, "iam_user").await?
        && table_exists(postgres, "iam_grant").await?
        && table_exists(postgres, "iam_workspace_membership").await?
        && table_exists(postgres, "catalog_workspace").await?
        && table_exists(postgres, "catalog_library").await?)
}

async fn table_exists(postgres: &PgPool, table_name: &str) -> anyhow::Result<bool> {
    let exists = sqlx::query_scalar::<_, bool>("select to_regclass($1) is not null")
        .bind(format!("public.{table_name}"))
        .fetch_one(postgres)
        .await?;
    Ok(exists)
}
