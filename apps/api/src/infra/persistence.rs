#![allow(clippy::missing_errors_doc)]

use std::{collections::HashMap, time::Duration};

use redis::Client as RedisClient;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        DOCUMENT_COLLECTIONS, KNOWLEDGE_CHUNK_VECTOR_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_INDEX,
        KNOWLEDGE_ENTITY_VECTOR_COLLECTION, KNOWLEDGE_ENTITY_VECTOR_INDEX, KNOWLEDGE_GRAPH_NAME,
        KNOWLEDGE_PERSISTENT_INDEXES, KNOWLEDGE_SEARCH_VIEW,
    },
};
use crate::{app::config::Settings, domains::deployment::ServiceRole};

// Forces the crate to rebuild whenever the migration set changes, including file deletions.
const _SQLX_MIGRATIONS_FINGERPRINT: &str = env!("IRONRAG_MIGRATIONS_FINGERPRINT");

const SEEDED_PROVIDER_KINDS: [&str; 3] = ["openai", "deepseek", "qwen"];
const POSTGRES_POOL_MIN_BUDGET: u32 = 4;
const POSTGRES_POOL_MAX_PROCESS_BUDGET: u32 = 16;
const POSTGRES_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const POSTGRES_POOL_MAX_LIFETIME: Duration = Duration::from_secs(1_800);
const POSTGRES_MAIN_MIN_CONNECTIONS: u32 = 1;
const API_CONTROL_POOL_CONNECTIONS: u32 = 2;
const STARTUP_PROCESS_POOL_BUDGET: u32 = 4;
const STARTUP_CONTROL_POOL_CONNECTIONS: u32 = 1;
const WORKER_CONTROL_POOL_MIN_CONNECTIONS: u32 = 4;
const WORKER_CONTROL_POOL_MAX_CONNECTIONS: u32 = 12;
const WORKER_CONTROL_RESERVED_CONNECTIONS: u32 = 1;
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

static POSTGRES_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PostgresPoolPlan {
    main_max_connections: u32,
    main_min_connections: u32,
    control_max_connections: u32,
    control_min_connections: u32,
    idle_timeout: Duration,
    max_lifetime: Duration,
}

fn postgres_pool_plan(
    role: ServiceRole,
    configured_budget: u32,
    api_replicas: usize,
    worker_replicas: usize,
    worker_global_limit: usize,
) -> PostgresPoolPlan {
    let process_budget = match role {
        ServiceRole::Startup => STARTUP_PROCESS_POOL_BUDGET,
        ServiceRole::Api | ServiceRole::Worker => {
            let runtime_replicas = api_replicas.saturating_add(worker_replicas).max(1);
            let runtime_replicas =
                u32::try_from(runtime_replicas).unwrap_or(POSTGRES_POOL_MAX_PROCESS_BUDGET);
            // Settings validation requires at least POSTGRES_POOL_MIN_BUDGET
            // per runtime replica. The lower clamp is therefore a defensive
            // floor for non-runtime callers, not extra deployment capacity.
            configured_budget
                .saturating_div(runtime_replicas)
                .clamp(POSTGRES_POOL_MIN_BUDGET, POSTGRES_POOL_MAX_PROCESS_BUDGET)
        }
    };
    let control_target = match role {
        ServiceRole::Api => API_CONTROL_POOL_CONNECTIONS,
        ServiceRole::Startup => STARTUP_CONTROL_POOL_CONNECTIONS,
        ServiceRole::Worker => {
            let worker_slots =
                u32::try_from(worker_global_limit).unwrap_or(WORKER_CONTROL_POOL_MAX_CONNECTIONS);
            let balanced_control_ceiling = process_budget
                .saturating_div(2)
                .saturating_add(WORKER_CONTROL_RESERVED_CONNECTIONS)
                .max(WORKER_CONTROL_POOL_MIN_CONNECTIONS);
            worker_slots
                .saturating_add(WORKER_CONTROL_RESERVED_CONNECTIONS)
                .clamp(WORKER_CONTROL_POOL_MIN_CONNECTIONS, WORKER_CONTROL_POOL_MAX_CONNECTIONS)
                .min(balanced_control_ceiling)
        }
    };
    let control_max_connections =
        control_target.min(process_budget.saturating_sub(POSTGRES_MAIN_MIN_CONNECTIONS));
    let main_max_connections =
        process_budget.saturating_sub(control_max_connections).max(POSTGRES_MAIN_MIN_CONNECTIONS);

    PostgresPoolPlan {
        main_max_connections,
        main_min_connections: POSTGRES_MAIN_MIN_CONNECTIONS.min(main_max_connections),
        control_max_connections,
        control_min_connections: match role {
            ServiceRole::Worker => WORKER_CONTROL_RESERVED_CONNECTIONS.min(control_max_connections),
            ServiceRole::Api | ServiceRole::Startup => 0,
        },
        idle_timeout: POSTGRES_POOL_IDLE_TIMEOUT,
        max_lifetime: POSTGRES_POOL_MAX_LIFETIME,
    }
}

impl PostgresPoolPlan {
    fn worker_job_slot_limit(self) -> usize {
        let heartbeat_slots =
            self.control_max_connections.saturating_sub(WORKER_CONTROL_RESERVED_CONNECTIONS);
        let slot_limit = self.main_max_connections.min(heartbeat_slots).max(1);
        usize::try_from(slot_limit).unwrap_or(usize::MAX)
    }
}

#[must_use]
pub fn worker_process_db_job_limit(settings: &Settings) -> usize {
    let plan = postgres_pool_plan(
        ServiceRole::Worker,
        settings.database_max_connections,
        settings.api_replicas,
        settings.worker_replicas,
        settings.ingestion_max_parallel_jobs_global,
    );
    plan.worker_job_slot_limit()
}

#[derive(Clone)]
pub struct Persistence {
    pub postgres: PgPool,
    /// Small dedicated pool reserved for latency-critical control plane
    /// traffic. Worker dispatch is capped from the same plan, leaving a
    /// reserved slot for the stale-lease reaper while heartbeat/cancel polls
    /// stay off the main working pool. Canonically used by the ingest worker
    /// heartbeat loop to keep `ingest_attempt.heartbeat_at` fresh under
    /// CPU-bound stages.
    pub heartbeat_postgres: PgPool,
    pub redis: RedisClient,
}

impl Persistence {
    /// Connects to Postgres and Redis and verifies Redis responsiveness.
    ///
    /// # Errors
    /// Returns any database, Redis client, or Redis ping initialization error.
    pub async fn connect(settings: &Settings) -> anyhow::Result<Self> {
        let service_role = settings
            .service_role_kind()
            .map_err(|error| anyhow::anyhow!("invalid service_role in settings: {error}"))?;
        let pool_plan = postgres_pool_plan(
            service_role,
            settings.database_max_connections,
            settings.api_replicas,
            settings.worker_replicas,
            settings.ingestion_max_parallel_jobs_global,
        );
        let worker_process_db_job_limit = match service_role {
            ServiceRole::Worker => pool_plan.worker_job_slot_limit(),
            ServiceRole::Api | ServiceRole::Startup => 0,
        };
        tracing::info!(
            service_role = %service_role,
            configured_database_max_connections = settings.database_max_connections,
            api_replicas = settings.api_replicas,
            worker_replicas = settings.worker_replicas,
            main_max_connections = pool_plan.main_max_connections,
            control_max_connections = pool_plan.control_max_connections,
            worker_process_db_job_limit,
            idle_timeout_seconds = pool_plan.idle_timeout.as_secs(),
            max_lifetime_seconds = pool_plan.max_lifetime.as_secs(),
            "configured postgres connection pools"
        );

        // Main working pool. `acquire_timeout` caps how long a request
        // may block waiting for a free slot before returning
        // `PoolTimedOut` — without it, a spike of concurrent
        // grounded_answer calls (each holding a connection for a
        // retrieval + audit write) could stack up behind a cold
        // runtime_graph_edge load and surface as 30-60 s timeouts at
        // the MCP transport. 8 s is long enough to absorb the typical
        // slow-query tail, short enough that clients see a real
        // error before the MCP tool-call budget is exhausted.
        // `idle_timeout` aggressively reclaims post-spike idle
        // backends; on swapless single-node installs, keeping dozens
        // of Postgres processes resident after a query/ingest burst
        // can push the host into global OOM even though the app has
        // returned to idle.
        let postgres = PgPoolOptions::new()
            .max_connections(pool_plan.main_max_connections)
            .min_connections(pool_plan.main_min_connections)
            .acquire_timeout(Duration::from_secs(8))
            .idle_timeout(Some(pool_plan.idle_timeout))
            .max_lifetime(Some(pool_plan.max_lifetime))
            .connect(&settings.database_url)
            .await?;

        // Independent control-plane pool. Sized to cover concurrent
        // heartbeat tasks (one per in-flight ingest attempt, up to the
        // worker's DB-derived process slot count) plus the dedicated
        // stale-lease reaper that shares this pool.
        //
        // The control-plane pool is role-aware and is counted inside
        // the same per-process DB budget as the main pool. API/startup
        // need only a tiny reserve for diagnostics and readiness. The
        // worker gets enough slots for its effective claim limit plus a
        // reserved stale-lease reaper lane, without permanently adding a
        // fixed 24 idle Postgres backends on every process.
        let heartbeat_postgres = PgPoolOptions::new()
            .min_connections(pool_plan.control_min_connections)
            .max_connections(pool_plan.control_max_connections)
            .acquire_timeout(Duration::from_secs(15))
            .idle_timeout(Some(pool_plan.idle_timeout))
            .max_lifetime(Some(pool_plan.max_lifetime))
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
    // migrations` at startup, nudge this function before rebuilding so
    // the proc macro re-scans `./migrations`.
    POSTGRES_MIGRATOR.run(postgres).await?;
    Ok(())
}

pub async fn validate_postgres_migration_state(postgres: &PgPool) -> anyhow::Result<()> {
    let rows = sqlx::query("select version, checksum, success from _sqlx_migrations")
        .fetch_all(postgres)
        .await?;
    let mut applied = HashMap::<i64, Vec<u8>>::with_capacity(rows.len());
    for row in rows {
        let version: i64 = row.get("version");
        let success: bool = row.get("success");
        anyhow::ensure!(success, "migration {version} is marked dirty");
        applied.insert(version, row.get("checksum"));
    }

    for migration in
        POSTGRES_MIGRATOR.iter().filter(|migration| migration.migration_type.is_up_migration())
    {
        let Some(applied_checksum) = applied.remove(&migration.version) else {
            anyhow::bail!("migration {} has not been applied", migration.version);
        };
        anyhow::ensure!(
            applied_checksum.as_slice() == migration.checksum.as_ref(),
            "migration {} was previously applied but has been modified",
            migration.version
        );
    }

    if let Some(version) = applied.keys().min().copied() {
        anyhow::bail!("migration {version} was previously applied but is missing in the binary");
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_pool_plan_caps_old_oversized_budget() {
        let plan = postgres_pool_plan(ServiceRole::Api, 60, 1, 1, 64);

        assert_eq!(plan.main_max_connections, 14);
        assert_eq!(plan.control_max_connections, 2);
        assert_eq!(plan.main_min_connections, 1);
        assert_eq!(plan.control_min_connections, 0);
        assert_eq!(plan.idle_timeout, Duration::from_secs(60));
    }

    #[test]
    fn worker_pool_plan_keeps_heartbeat_pool_inside_process_budget() {
        let plan = postgres_pool_plan(ServiceRole::Worker, 20, 1, 1, 64);

        assert_eq!(plan.main_max_connections, 4);
        assert_eq!(plan.control_max_connections, 6);
        assert_eq!(plan.control_min_connections, 1);
        assert_eq!(plan.worker_job_slot_limit(), 4);
    }

    #[test]
    fn worker_pool_plan_divides_budget_across_runtime_replicas() {
        let plan = postgres_pool_plan(ServiceRole::Worker, 20, 2, 2, 64);

        assert_eq!(plan.main_max_connections, 1);
        assert_eq!(plan.control_max_connections, 4);
        assert_eq!(plan.worker_job_slot_limit(), 1);
    }

    #[test]
    fn runtime_pool_plan_never_exceeds_valid_deployment_budget() {
        for api_replicas in 0usize..=8 {
            for worker_replicas in 0usize..=8 {
                let runtime_replicas = api_replicas + worker_replicas;
                if runtime_replicas == 0 {
                    continue;
                }
                let min_valid_budget = POSTGRES_POOL_MIN_BUDGET * runtime_replicas as u32;
                for configured_budget in min_valid_budget..=128 {
                    let api_plan = postgres_pool_plan(
                        ServiceRole::Api,
                        configured_budget,
                        api_replicas,
                        worker_replicas,
                        64,
                    );
                    let worker_plan = postgres_pool_plan(
                        ServiceRole::Worker,
                        configured_budget,
                        api_replicas,
                        worker_replicas,
                        64,
                    );
                    let per_api = api_plan.main_max_connections + api_plan.control_max_connections;
                    let per_worker =
                        worker_plan.main_max_connections + worker_plan.control_max_connections;
                    let total =
                        (api_replicas as u32 * per_api) + (worker_replicas as u32 * per_worker);

                    assert!(
                        total <= configured_budget,
                        "api_replicas={api_replicas}, worker_replicas={worker_replicas}, configured_budget={configured_budget}, total={total}"
                    );
                }
            }
        }
    }

    #[test]
    fn startup_pool_plan_does_not_inherit_runtime_budget() {
        let plan = postgres_pool_plan(ServiceRole::Startup, 60, 1, 1, 64);

        assert_eq!(plan.main_max_connections, 3);
        assert_eq!(plan.control_max_connections, 1);
    }
}
