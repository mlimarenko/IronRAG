#![allow(clippy::missing_errors_doc)]

use std::time::Duration;

use redis::Client as RedisClient;
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::app::config::Settings;
use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        DOCUMENT_COLLECTIONS, KNOWLEDGE_CHUNK_VECTOR_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_INDEX,
        KNOWLEDGE_ENTITY_VECTOR_COLLECTION, KNOWLEDGE_ENTITY_VECTOR_INDEX, KNOWLEDGE_GRAPH_NAME,
        KNOWLEDGE_PERSISTENT_INDEXES, KNOWLEDGE_SEARCH_VIEW,
    },
};

// Forces the crate to rebuild whenever the migration set changes, including file deletions.
const _SQLX_MIGRATIONS_FINGERPRINT: &str = env!("IRONRAG_MIGRATIONS_FINGERPRINT");

const SEEDED_PROVIDER_KINDS: [&str; 3] = ["openai", "deepseek", "qwen"];
const CANONICAL_BASELINE_TABLES: [&str; 9] = [
    "catalog_workspace",
    "catalog_library",
    "iam_principal",
    "iam_user",
    "iam_grant",
    "iam_workspace_membership",
    "ai_provider_catalog",
    "ai_model_catalog",
    "ai_price_catalog",
];

#[derive(Clone)]
pub struct Persistence {
    pub postgres: PgPool,
    /// Small dedicated pool reserved for latency-critical control plane
    /// traffic that must never starve behind the main working pool. Sized
    /// so heartbeat/cancel polls can always grab a connection even while
    /// the ingest worker has saturated `postgres` with fan-out graph
    /// merges. Canonically used by the ingest worker heartbeat loop to
    /// keep `ingest_attempt.heartbeat_at` fresh under CPU-bound stages.
    pub heartbeat_postgres: PgPool,
    pub redis: RedisClient,
}

impl Persistence {
    /// Connects to Postgres and Redis and verifies Redis responsiveness.
    ///
    /// # Errors
    /// Returns any database, Redis client, or Redis ping initialization error.
    pub async fn connect(settings: &Settings) -> anyhow::Result<Self> {
        // Main working pool. `acquire_timeout` caps how long a request
        // may block waiting for a free slot before returning
        // `PoolTimedOut` — without it, a spike of concurrent
        // grounded_answer calls (each holding a connection for a
        // retrieval + audit write) could stack up behind a cold
        // runtime_graph_edge load and surface as 30-60 s timeouts at
        // the MCP transport. 8 s is long enough to absorb the typical
        // slow-query tail, short enough that clients see a real
        // error before the MCP tool-call budget is exhausted.
        // `min_connections` keeps a handful of sockets warm so the
        // first query after an idle window doesn't pay the TLS/auth
        // handshake tax. `idle_timeout` reclaims slots that have
        // been unused long enough to have been closed by Postgres
        // (`idle_session_timeout`) or a pgbouncer in between.
        let postgres = PgPoolOptions::new()
            .max_connections(settings.database_max_connections)
            .min_connections(4)
            .acquire_timeout(Duration::from_secs(8))
            .idle_timeout(Some(Duration::from_secs(300)))
            .connect(&settings.database_url)
            .await?;

        // Independent control-plane pool. Sized to cover concurrent
        // heartbeat tasks (one per in-flight ingest attempt, up to the
        // worker's slot count) plus the dedicated stale-lease reaper that
        // shares this pool.
        //
        // Sizing history:
        //   * 2 — starved at 3+ heartbeats vs reaper.
        //   * 6 — worked at single-digit concurrency; starved on
        //     2026-04-21 under 24-slot merge load. Live `PoolTimedOut`
        //     storm on the reaper surfaced 23 "stale" leases that were
        //     actually healthy jobs whose heartbeats couldn't claim a
        //     slot in time. Canonical root cause is row-lock
        //     contention on `ingest_attempt` (state-transition UPDATEs
        //     from the main pool hold row locks that block heartbeat
        //     UPDATEs from this pool), not pool count alone — the
        //     schema-level split of heartbeat into its own row is
        //     tracked as a follow-up. Meanwhile, raising the pool to
        //     24 gives every worker slot a dedicated connection so
        //     heartbeats + the reaper don't compete for the same 6
        //     sockets. The pool is still an order of magnitude smaller
        //     than `database_max_connections` so it cannot meaningfully
        //     starve the main pool.
        let heartbeat_postgres = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(24)
            .acquire_timeout(Duration::from_secs(15))
            .connect(&settings.database_url)
            .await?;

        let redis = RedisClient::open(settings.redis_url.clone())?;
        let mut conn = redis.get_multiplexed_async_connection().await?;
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;

        Ok(Self { postgres, heartbeat_postgres, redis })
    }

    /// Test-only constructor that reuses the same Postgres pool for the
    /// heartbeat path. Production always uses a dedicated tiny pool via
    /// [`Persistence::connect`]; integration tests don't exercise the
    /// starvation scenario the dedicated pool guards against, so sharing
    /// one pool keeps fixture setup simple while still populating every
    /// field of the struct.
    #[must_use]
    pub fn for_tests(postgres: PgPool, redis: RedisClient) -> Self {
        Self { postgres: postgres.clone(), heartbeat_postgres: postgres, redis }
    }
}

pub async fn run_postgres_migrations(postgres: &PgPool) -> anyhow::Result<()> {
    // `sqlx::migrate!` expands at compile time. When a new `.sql` file is
    // added without any Rust change, cargo may skip re-expanding the macro
    // and bake a stale migration list into the binary. If you ever see
    // `migration N was previously applied but is missing in the resolved
    // migrations` at startup, nudge this function (e.g. touch a comment)
    // before rebuilding so the proc macro re-scans `./migrations`.
    sqlx::migrate!("./migrations").run(postgres).await?;
    Ok(())
}

pub async fn validate_canonical_bootstrap_state(
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

pub async fn validate_arango_bootstrap_state(
    arango_client: &ArangoClient,
    settings: &Settings,
) -> anyhow::Result<()> {
    for collection in DOCUMENT_COLLECTIONS {
        anyhow::ensure!(
            arango_client.collection_exists(collection).await?,
            "canonical bootstrap validation failed: required Arango collection `{collection}` is missing",
        );
    }

    for index in KNOWLEDGE_PERSISTENT_INDEXES {
        anyhow::ensure!(
            arango_client
                .persistent_index_matches(
                    index.collection,
                    index.name,
                    index.fields,
                    index.unique,
                    index.sparse
                )
                .await?,
            "canonical bootstrap validation failed: required Arango persistent index `{}` on `{}` is missing or mismatched",
            index.name,
            index.collection,
        );
    }

    if settings.arangodb_bootstrap_views {
        anyhow::ensure!(
            arango_client.view_exists(KNOWLEDGE_SEARCH_VIEW).await?,
            "canonical bootstrap validation failed: required Arango view `{KNOWLEDGE_SEARCH_VIEW}` is missing",
        );
    }

    if settings.arangodb_bootstrap_graph {
        anyhow::ensure!(
            arango_client.graph_exists(KNOWLEDGE_GRAPH_NAME).await?,
            "canonical bootstrap validation failed: required Arango named graph `{KNOWLEDGE_GRAPH_NAME}` is missing",
        );
    }

    if settings.arangodb_bootstrap_vector_indexes {
        anyhow::ensure!(
            arango_client
                .vector_index_exists(
                    KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    KNOWLEDGE_CHUNK_VECTOR_INDEX
                )
                .await?,
            "canonical bootstrap validation failed: chunk vector index `{KNOWLEDGE_CHUNK_VECTOR_INDEX}` is missing",
        );
        anyhow::ensure!(
            arango_client
                .vector_index_exists(
                    KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                    KNOWLEDGE_ENTITY_VECTOR_INDEX
                )
                .await?,
            "canonical bootstrap validation failed: entity vector index `{KNOWLEDGE_ENTITY_VECTOR_INDEX}` is missing",
        );
    }

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
    .bind(SEEDED_PROVIDER_KINDS)
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

async fn table_exists(postgres: &PgPool, table_name: &str) -> anyhow::Result<bool> {
    let exists = sqlx::query_scalar::<_, bool>("select to_regclass($1) is not null")
        .bind(format!("public.{table_name}"))
        .fetch_one(postgres)
        .await?;
    Ok(exists)
}
