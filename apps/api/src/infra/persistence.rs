use std::{collections::HashMap, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use redis::Client as RedisClient;
use sha2::{Digest as _, Sha256};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};

use super::tls_pin_proxy::{TlsPinProxies, UpstreamProtocol, start_tls_pin_proxy};
use crate::{app::config::Settings, domains::deployment::ServiceRole};

// Forces the crate to rebuild whenever the required schema baseline changes.
const _SQLX_MIGRATIONS_FINGERPRINT: &str = env!("IRONRAG_MIGRATIONS_FINGERPRINT");

const POSTGRES_POOL_MIN_BUDGET: u32 = 4;
const POSTGRES_POOL_IDLE_TIMEOUT: Duration = Duration::from_mins(1);
const POSTGRES_POOL_MAX_LIFETIME: Duration = Duration::from_mins(30);
const POSTGRES_MAIN_MIN_CONNECTIONS: u32 = 1;
const API_CONTROL_POOL_CONNECTIONS: u32 = 2;
const STARTUP_PROCESS_POOL_BUDGET: u32 = 4;
const STARTUP_CONTROL_POOL_CONNECTIONS: u32 = 1;
// The worker's historical four-connection control target is now split into
// three heartbeat/reaper connections plus one scheduler connection when the
// process budget permits. The minimum four-connection layout is one main, two
// heartbeat/reaper, and one scheduler connection.
const WORKER_CONTROL_POOL_MIN_CONNECTIONS: u32 = 3;
const WORKER_CONTROL_POOL_MAX_CONNECTIONS: u32 = 12;
const WORKER_CONTROL_RESERVED_CONNECTIONS: u32 = 1;
const WORKER_SCHEDULER_CONTROL_CONNECTIONS: u32 = 1;
const REQUIRED_SNAPSHOT_TABLES: [&str; 8] = [
    "catalog_workspace",
    "catalog_library",
    "iam_principal",
    "iam_user",
    "iam_grant",
    "iam_workspace_membership",
    "ai_provider_catalog",
    "ai_model_catalog",
];

static POSTGRES_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PostgresPoolPlan {
    main_max_connections: u32,
    main_min_connections: u32,
    control_max_connections: u32,
    control_min_connections: u32,
    scheduler_control_max_connections: u32,
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
            let runtime_replicas = u32::try_from(runtime_replicas).unwrap_or(u32::MAX);
            // Settings validation requires at least POSTGRES_POOL_MIN_BUDGET
            // per runtime replica. The lower clamp is therefore a defensive
            // floor for non-runtime callers, not extra deployment capacity.
            configured_budget.saturating_div(runtime_replicas).max(POSTGRES_POOL_MIN_BUDGET)
        }
    };
    let scheduler_control_max_connections = match role {
        ServiceRole::Worker => WORKER_SCHEDULER_CONTROL_CONNECTIONS,
        ServiceRole::Api | ServiceRole::Startup => 0,
    };
    let shared_pool_budget = process_budget.saturating_sub(scheduler_control_max_connections);
    let control_target = match role {
        ServiceRole::Api => API_CONTROL_POOL_CONNECTIONS,
        ServiceRole::Startup => STARTUP_CONTROL_POOL_CONNECTIONS,
        ServiceRole::Worker => {
            let worker_slots =
                u32::try_from(worker_global_limit).unwrap_or(WORKER_CONTROL_POOL_MAX_CONNECTIONS);
            let balanced_control_ceiling = shared_pool_budget
                .saturating_add(WORKER_CONTROL_RESERVED_CONNECTIONS)
                .saturating_div(2)
                .max(WORKER_CONTROL_POOL_MIN_CONNECTIONS);
            worker_slots
                .saturating_add(WORKER_CONTROL_RESERVED_CONNECTIONS)
                .clamp(WORKER_CONTROL_POOL_MIN_CONNECTIONS, WORKER_CONTROL_POOL_MAX_CONNECTIONS)
                .min(balanced_control_ceiling)
        }
    };
    let control_max_connections =
        control_target.min(shared_pool_budget.saturating_sub(POSTGRES_MAIN_MIN_CONNECTIONS));
    let main_max_connections = shared_pool_budget
        .saturating_sub(control_max_connections)
        .max(POSTGRES_MAIN_MIN_CONNECTIONS);

    PostgresPoolPlan {
        main_max_connections,
        main_min_connections: POSTGRES_MAIN_MIN_CONNECTIONS.min(main_max_connections),
        control_max_connections,
        control_min_connections: match role {
            ServiceRole::Worker => WORKER_CONTROL_RESERVED_CONNECTIONS.min(control_max_connections),
            ServiceRole::Api | ServiceRole::Startup => 0,
        },
        scheduler_control_max_connections,
        idle_timeout: POSTGRES_POOL_IDLE_TIMEOUT,
        max_lifetime: POSTGRES_POOL_MAX_LIFETIME,
    }
}

pub(crate) fn postgres_pool_plan_attestation(settings: &Settings) -> serde_json::Value {
    let Some(role) = settings.service_role_kind().ok() else {
        return serde_json::Value::Null;
    };
    let plan = postgres_pool_plan(
        role,
        settings.database_max_connections,
        settings.api_replicas,
        settings.worker_replicas,
        settings.ingestion_max_parallel_jobs_global,
    );
    serde_json::json!({
        "mainMaxConnections": plan.main_max_connections,
        "mainMinConnections": plan.main_min_connections,
        "controlMaxConnections": plan.control_max_connections,
        "controlMinConnections": plan.control_min_connections,
        "schedulerControlMaxConnections": plan.scheduler_control_max_connections,
        "workerProcessDbJobLimit": plan.worker_job_slot_limit(),
        "idleTimeoutSeconds": plan.idle_timeout.as_secs(),
        "maxLifetimeSeconds": plan.max_lifetime.as_secs(),
    })
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
    /// stay off the main working pool. Consistently used by the ingest worker
    /// heartbeat loop to keep `ingest_attempt.heartbeat_at` fresh under
    /// CPU-bound stages.
    pub heartbeat_postgres: PgPool,
    /// One-connection worker-only pool for graph-repair scheduler leadership
    /// and durable rotation reservations. It is independent of the heartbeat
    /// pool, so even a slow or stuck admission transaction cannot consume a
    /// worker heartbeat slot or the stale-reaper reserve. Non-worker roles do
    /// not run this scheduler and reuse the heartbeat handle without opening an
    /// otherwise-idle extra connection.
    pub scheduler_postgres: PgPool,
    pub redis: RedisClient,
    pub dependency_tls: DependencyTlsAttestation,
    _tls_pin_proxies: Vec<TlsPinProxies>,
}

#[derive(Clone, Default)]
pub struct DependencyTlsAttestation {
    pub database_leaf_sha256: Option<String>,
    pub redis_leaf_sha256: Option<String>,
}

fn single_pem_certificate_der(pem: &[u8]) -> anyhow::Result<Vec<u8>> {
    let text =
        std::str::from_utf8(pem).map_err(|_| anyhow::anyhow!("TLS leaf PEM is not UTF-8"))?;
    let mut inside = false;
    let mut certificate_count = 0_u8;
    let mut encoded = String::new();
    for line in text.lines().map(str::trim) {
        match line {
            "-----BEGIN CERTIFICATE-----" if !inside => {
                inside = true;
                certificate_count = certificate_count.saturating_add(1);
            }
            "-----END CERTIFICATE-----" if inside => inside = false,
            "" => {}
            _ if inside => encoded.push_str(line),
            _ => anyhow::bail!("TLS leaf PEM contains non-certificate content"),
        }
    }
    anyhow::ensure!(!inside, "TLS leaf PEM has no closing certificate marker");
    anyhow::ensure!(certificate_count == 1, "TLS leaf PEM must contain exactly one certificate");
    let der = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("TLS leaf PEM contains invalid base64"))?;
    anyhow::ensure!(!der.is_empty(), "TLS leaf certificate is empty");
    Ok(der)
}

#[cfg(test)]
fn single_pem_certificate_sha256(pem: &[u8]) -> anyhow::Result<String> {
    Ok(hex::encode(Sha256::digest(single_pem_certificate_der(pem)?)))
}

fn load_tls_leaf(path: &str) -> anyhow::Result<(Vec<u8>, String)> {
    let pem = std::fs::read(path)
        .map_err(|error| anyhow::anyhow!("cannot read TLS leaf certificate {path}: {error}"))?;
    let der = single_pem_certificate_der(&pem)?;
    let sha256 = hex::encode(Sha256::digest(&der));
    Ok((der, sha256))
}

struct DependencyTlsMaterial {
    database_leaf_der: Option<Vec<u8>>,
    redis_leaf_der: Option<Vec<u8>>,
    attestation: DependencyTlsAttestation,
}

/// Resolves the optional pinned TLS leaf material for the database and Redis
/// upstreams from the configured certificate paths.
fn resolve_tls_leaves(settings: &Settings) -> anyhow::Result<DependencyTlsMaterial> {
    let (database_leaf_der, database_leaf_sha256) = settings
        .database_tls_leaf_certificate
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(load_tls_leaf)
        .transpose()?
        .map_or((None, None), |(der, sha256)| (Some(der), Some(sha256)));
    let (redis_leaf_der, redis_leaf_sha256) = settings
        .redis_tls_leaf_certificate
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(load_tls_leaf)
        .transpose()?
        .map_or((None, None), |(der, sha256)| (Some(der), Some(sha256)));
    Ok(DependencyTlsMaterial {
        database_leaf_der,
        redis_leaf_der,
        attestation: DependencyTlsAttestation { database_leaf_sha256, redis_leaf_sha256 },
    })
}

/// Builds the Postgres connect options for this process, starting the loopback
/// TLS-pin proxy when a database leaf is pinned.
async fn postgres_connect_options(
    settings: &Settings,
    database_leaf_der: Option<Vec<u8>>,
    tls_pin_proxies: &mut Vec<TlsPinProxies>,
) -> anyhow::Result<PgConnectOptions> {
    let database_leaf_pinned = database_leaf_der.is_some();
    let database_url = if let Some(der) = database_leaf_der {
        let (url, proxy) =
            start_tls_pin_proxy(&settings.database_url, UpstreamProtocol::Postgres, der).await?;
        tls_pin_proxies.push(proxy);
        url.to_string()
    } else {
        settings.database_url.clone()
    };
    let mut postgres_options = database_url
        .parse::<PgConnectOptions>()
        .map_err(|error| anyhow::anyhow!("invalid database URL: {error}"))?;
    if database_leaf_pinned {
        // The loopback is plaintext only inside this process; the proxy performs
        // the upstream hostname, chain, and exact peer-leaf verification.
        postgres_options = postgres_options.ssl_mode(PgSslMode::Disable);
    }
    Ok(postgres_options)
}

/// Role-scaled worker process DB job slot limit; non-worker roles reserve none.
fn worker_process_db_job_limit_for(role: ServiceRole, plan: &PostgresPoolPlan) -> usize {
    match role {
        ServiceRole::Worker => plan.worker_job_slot_limit(),
        ServiceRole::Api | ServiceRole::Startup => 0,
    }
}

/// Builds the worker-only scheduler pool, sharing the heartbeat handle when the
/// role reserves no scheduler connection.
async fn scheduler_postgres_pool(
    plan: &PostgresPoolPlan,
    heartbeat_postgres: &PgPool,
    postgres_options: PgConnectOptions,
) -> anyhow::Result<PgPool> {
    if plan.scheduler_control_max_connections == 0 {
        return Ok(heartbeat_postgres.clone());
    }
    let pool = PgPoolOptions::new()
        .min_connections(0)
        .max_connections(plan.scheduler_control_max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Some(plan.idle_timeout))
        .max_lifetime(Some(plan.max_lifetime))
        .connect_with(postgres_options)
        .await?;
    Ok(pool)
}

/// Opens the Redis client, starting the pin proxy for a pinned leaf, and proves
/// responsiveness with one `PING`.
async fn connect_redis(
    settings: &Settings,
    redis_leaf_der: Option<Vec<u8>>,
    tls_pin_proxies: &mut Vec<TlsPinProxies>,
) -> anyhow::Result<RedisClient> {
    let redis = if let Some(der) = redis_leaf_der {
        let (url, proxy) =
            start_tls_pin_proxy(&settings.redis_url, UpstreamProtocol::Redis, der).await?;
        tls_pin_proxies.push(proxy);
        RedisClient::open(url.as_str())?
    } else {
        RedisClient::open(settings.redis_url.clone())?
    };
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let _: String = redis::cmd("PING").query_async(&mut conn).await?;
    Ok(redis)
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
        let worker_process_db_job_limit = worker_process_db_job_limit_for(service_role, &pool_plan);
        tracing::info!(
            service_role = %service_role,
            configured_database_max_connections = settings.database_max_connections,
            api_replicas = settings.api_replicas,
            worker_replicas = settings.worker_replicas,
            main_max_connections = pool_plan.main_max_connections,
            control_max_connections = pool_plan.control_max_connections,
            scheduler_control_max_connections = pool_plan.scheduler_control_max_connections,
            worker_process_db_job_limit,
            idle_timeout_seconds = pool_plan.idle_timeout.as_secs(),
            max_lifetime_seconds = pool_plan.max_lifetime.as_secs(),
            "configured postgres connection pools"
        );

        let DependencyTlsMaterial {
            database_leaf_der,
            redis_leaf_der,
            attestation: dependency_tls,
        } = resolve_tls_leaves(settings)?;
        let mut tls_pin_proxies = Vec::new();
        let postgres_options =
            postgres_connect_options(settings, database_leaf_der, &mut tls_pin_proxies).await?;

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
            .connect_with(postgres_options.clone())
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
            .connect_with(postgres_options.clone())
            .await?;

        let scheduler_postgres =
            scheduler_postgres_pool(&pool_plan, &heartbeat_postgres, postgres_options).await?;

        let redis = connect_redis(settings, redis_leaf_der, &mut tls_pin_proxies).await?;

        Ok(Self {
            postgres,
            heartbeat_postgres,
            scheduler_postgres,
            redis,
            dependency_tls,
            _tls_pin_proxies: tls_pin_proxies,
        })
    }

    /// Test-only constructor that reuses one Postgres pool for all three paths.
    /// Production always creates distinct worker control pools through
    /// [`Persistence::connect`]; integration tests that exercise their
    /// starvation contract construct explicit pools, while ordinary fixtures
    /// can keep setup simple and still populate every handle.
    #[must_use]
    pub fn for_tests(postgres: PgPool, redis: RedisClient) -> Self {
        Self {
            postgres: postgres.clone(),
            heartbeat_postgres: postgres.clone(),
            scheduler_postgres: postgres,
            redis,
            dependency_tls: DependencyTlsAttestation::default(),
            _tls_pin_proxies: Vec::new(),
        }
    }
}

pub async fn run_postgres_migrations(postgres: &PgPool) -> anyhow::Result<()> {
    if !prepare_clean_database_baseline(postgres).await? {
        return Ok(());
    }

    // The sole schema path is the fresh `0001_init.sql` baseline.
    // `sqlx::migrate!` expands at compile time; the migration fingerprint
    // above ensures Cargo rebuilds when that baseline changes.
    POSTGRES_MIGRATOR.run(postgres).await?;
    Ok(())
}

async fn prepare_clean_database_baseline(postgres: &PgPool) -> anyhow::Result<bool> {
    let has_sqlx_history =
        sqlx::query_scalar::<_, bool>("select to_regclass('public._sqlx_migrations') is not null")
            .fetch_one(postgres)
            .await?;
    if has_sqlx_history {
        validate_postgres_migration_state(postgres).await?;
        return Ok(false);
    }

    let user_table_count =
        sqlx::query_scalar::<_, i64>("select count(*) from pg_tables where schemaname = 'public'")
            .fetch_one(postgres)
            .await?;
    anyhow::ensure!(
        user_table_count == 0,
        "clean database baseline v1 requires an empty database; found {user_table_count} existing public tables"
    );
    Ok(true)
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

    validate_baseline_objects_present(postgres).await?;

    Ok(())
}

/// Objects the current baseline declares that a checksum comparison cannot see.
///
/// Each entry is `(description, sql predicate returning bool)`. A recorded
/// checksum does not prove the DDL was applied; these predicates detect missing
/// objects before readiness or a transition can certify the database.
///
/// Keep the list to objects added by baseline edits that runtime queries or
/// security invariants require. Transition commands run the groups up to their
/// own target revision before stamping the checksum, so readiness and the
/// additive path share one contract without a delta demanding a future object.
///
/// The groups are ordered exactly as the additive revision chain added them,
/// and `REQUIRED_BASELINE_OBJECT_GROUPS` lists them in that order.
///
/// A predicate must return `false` for an absent object rather than raise, so
/// it reaches the `ensure!` below with the operator path named. Resolve a
/// possibly-absent object with `to_regclass`/`to_regprocedure`, never with a
/// direct `::regclass`/`::regprocedure` cast, which errors on a missing object.
const REQUIRED_BASELINE_OBJECTS_IRG75: &[(&str, &str)] = &[(
    "ops_async_operation.result_json",
    "select exists (
             select 1 from information_schema.columns
             where table_schema = 'public'
               and table_name = 'ops_async_operation'
               and column_name = 'result_json'
          )",
)];

/// Objects the integration-provisioning revision (BOT-24) added.
const REQUIRED_BASELINE_OBJECTS_PROVISIONING: &[(&str, &str)] = &[
    (
        "iam_service_identity.provisioning_authority",
        "select exists (
             select 1 from information_schema.columns
             where table_schema = 'public'
               and table_name = 'iam_service_identity'
               and column_name = 'provisioning_authority'
               and data_type = 'text'
               and is_nullable = 'YES'
         )",
    ),
    (
        "catalog_workspace.provisioned_by_principal_id",
        "select exists (
             select 1 from information_schema.columns
             where table_schema = 'public'
               and table_name = 'catalog_workspace'
               and column_name = 'provisioned_by_principal_id'
               and data_type = 'uuid'
               and is_nullable = 'YES'
         )",
    ),
    (
        "catalog_library.provisioned_by_principal_id",
        "select exists (
             select 1 from information_schema.columns
             where table_schema = 'public'
               and table_name = 'catalog_library'
               and column_name = 'provisioned_by_principal_id'
               and data_type = 'uuid'
               and is_nullable = 'YES'
         )",
    ),
    (
        "iam_service_identity provisioning-authority constraint",
        "select coalesce((
             select pg_get_constraintdef(oid)
             from pg_constraint
             where conname = 'iam_service_identity_provisioning_authority_check'
               and conrelid = 'public.iam_service_identity'::regclass
               and contype = 'c'
          ) = 'CHECK (((provisioning_authority IS NULL) OR (provisioning_authority = ''integration''::text)))', false)",
    ),
    (
        "catalog_workspace provisioning-owner foreign key",
        "select exists (
             select 1 from pg_constraint
             where conname = 'catalog_workspace_provisioned_by_principal_id_fkey'
               and conrelid = 'public.catalog_workspace'::regclass
               and contype = 'f'
               and confrelid = 'public.iam_principal'::regclass
               and pg_get_constraintdef(oid) = 'FOREIGN KEY (provisioned_by_principal_id) REFERENCES iam_principal(id)'
         )",
    ),
    (
        "catalog_library provisioning-owner foreign key",
        "select exists (
             select 1 from pg_constraint
             where conname = 'catalog_library_provisioned_by_principal_id_fkey'
               and conrelid = 'public.catalog_library'::regclass
               and contype = 'f'
               and confrelid = 'public.iam_principal'::regclass
               and pg_get_constraintdef(oid) = 'FOREIGN KEY (provisioned_by_principal_id) REFERENCES iam_principal(id)'
         )",
    ),
    (
        "prevent_catalog_provisioning_owner_change function",
        "select exists (
             select 1 from pg_proc
             where oid = to_regprocedure('public.prevent_catalog_provisioning_owner_change()')
               and prorettype = 'trigger'::regtype
          )",
    ),
    (
        "iam_service_identity provisioning-authority index",
        "select coalesce(pg_get_indexdef(to_regclass('public.idx_iam_service_identity_provisioning_authority')) =
          'CREATE INDEX idx_iam_service_identity_provisioning_authority ON public.iam_service_identity USING btree (provisioning_authority) WHERE (provisioning_authority IS NOT NULL)', false)",
    ),
    (
        "catalog_workspace provisioning-owner index",
        "select coalesce(pg_get_indexdef(to_regclass('public.idx_catalog_workspace_provisioning_owner')) =
          'CREATE INDEX idx_catalog_workspace_provisioning_owner ON public.catalog_workspace USING btree (provisioned_by_principal_id) WHERE (provisioned_by_principal_id IS NOT NULL)', false)",
    ),
    (
        "catalog_library provisioning-owner index",
        "select coalesce(pg_get_indexdef(to_regclass('public.idx_catalog_library_provisioning_owner')) =
          'CREATE INDEX idx_catalog_library_provisioning_owner ON public.catalog_library USING btree (provisioned_by_principal_id, workspace_id) WHERE (provisioned_by_principal_id IS NOT NULL)', false)",
    ),
    (
        "catalog_workspace provisioning-owner immutability trigger",
        "select exists (
             select 1 from pg_trigger
             where tgname = 'trg_catalog_workspace_provisioning_owner_immutable'
               and tgrelid = 'public.catalog_workspace'::regclass
               and not tgisinternal
               and tgenabled = 'O'
               and tgfoid = to_regprocedure('public.prevent_catalog_provisioning_owner_change()')
         )",
    ),
    (
        "catalog_library provisioning-owner immutability trigger",
        "select exists (
             select 1 from pg_trigger
             where tgname = 'trg_catalog_library_provisioning_owner_immutable'
               and tgrelid = 'public.catalog_library'::regclass
               and not tgisinternal
               and tgenabled = 'O'
               and tgfoid = to_regprocedure('public.prevent_catalog_provisioning_owner_change()')
         )",
    ),
];

/// The nullable `ops_async_operation.failure_disposition` column the
/// IRG-173 revision adds. Additive, no backfill: existing rows read `null`.
const REQUIRED_BASELINE_OBJECTS_IRG173: &[(&str, &str)] = &[(
    "ops_async_operation.failure_disposition",
    "select exists (
             select 1 from information_schema.columns
             where table_schema = 'public'
               and table_name = 'ops_async_operation'
               and column_name = 'failure_disposition'
               and data_type = 'text'
               and is_nullable = 'YES'
          )",
)];

/// The `query_rerank` AI binding purpose the shipped baseline adds (IRG-163).
const REQUIRED_BASELINE_OBJECTS_QUERY_RERANK: &[(&str, &str)] = &[(
    "ai_binding_purpose query_rerank",
    "select exists (
             select 1 from pg_enum
             where enumtypid = 'public.ai_binding_purpose'::regtype
               and enumlabel = 'query_rerank'
         )",
)];

/// The decision-catalog enum values the shipped baseline adds (IRG-182): the
/// `graph_entity_type` AI binding purpose and the `typesafe_system_one` provider
/// API style.
const REQUIRED_BASELINE_OBJECTS_DECISION_CATALOG: &[(&str, &str)] = &[
    (
        "ai_binding_purpose graph_entity_type",
        "select exists (
             select 1 from pg_enum
             where enumtypid = 'public.ai_binding_purpose'::regtype
               and enumlabel = 'graph_entity_type'
         )",
    ),
    (
        "ai_provider_api_style typesafe_system_one",
        "select exists (
             select 1 from pg_enum
             where enumtypid = 'public.ai_provider_api_style'::regtype
               and enumlabel = 'typesafe_system_one'
         )",
    ),
];

/// The typed REQUIRED-admission visibility policy the IRG-92 revision adds
/// (IRG-92): the `admission_visibility_policy` enum and the
/// `content_mutation.visibility_policy` column. Additive, `NOT NULL DEFAULT
/// 'readable'`: a pre-migration row reads the canonical legacy value.
const REQUIRED_BASELINE_OBJECTS_ADMISSION_VISIBILITY_POLICY: &[(&str, &str)] = &[
    (
        "admission_visibility_policy enum type",
        "select exists (
             select 1 from pg_type
             where typname = 'admission_visibility_policy'
               and typnamespace = 'public'::regnamespace
               and typtype = 'e'
         )",
    ),
    (
        "content_mutation.visibility_policy",
        "select exists (
             select 1 from information_schema.columns
             where table_schema = 'public'
               and table_name = 'content_mutation'
               and column_name = 'visibility_policy'
               and data_type = 'USER-DEFINED'
               and udt_name = 'admission_visibility_policy'
               and is_nullable = 'NO'
               and column_default = '''readable''::admission_visibility_policy'
         )",
    ),
];

/// The object groups the IRG-75 revision declares: the base rung of the
/// cumulative ladder. It is the integration-provisioning delta's prerequisite
/// set, validated before that delta may run or report a pending dry-run.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS_IRG75: &[&[(&str, &str)]] =
    &[REQUIRED_BASELINE_OBJECTS_IRG75];

/// The object groups the integration-provisioning revision declares: the
/// cumulative set through its own target.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS_PROVISIONING: &[&[(&str, &str)]] =
    &[REQUIRED_BASELINE_OBJECTS_IRG75, REQUIRED_BASELINE_OBJECTS_PROVISIONING];

/// The manually installed IRG-112 FK indexes also belong to the predecessor
/// revision of IRG-173; a checksum alone cannot certify their presence.
const REQUIRED_BASELINE_OBJECTS_IRG112: &[(&str, &str)] = &[
    (
        "idx_knowledge_chunk_primary_block_fk",
        "select coalesce(pg_get_indexdef(to_regclass('public.idx_knowledge_chunk_primary_block_fk')) =
          'CREATE INDEX idx_knowledge_chunk_primary_block_fk ON public.knowledge_chunk USING btree (primary_block_id) WHERE (primary_block_id IS NOT NULL)', false)",
    ),
    (
        "idx_knowledge_evidence_block_fk",
        "select coalesce(pg_get_indexdef(to_regclass('public.idx_knowledge_evidence_block_fk')) =
          'CREATE INDEX idx_knowledge_evidence_block_fk ON public.knowledge_evidence USING btree (block_id) WHERE (block_id IS NOT NULL)', false)",
    ),
    (
        "idx_knowledge_structured_block_parent_block_fk",
        "select coalesce(pg_get_indexdef(to_regclass('public.idx_knowledge_structured_block_parent_block_fk')) =
          'CREATE INDEX idx_knowledge_structured_block_parent_block_fk ON public.knowledge_structured_block USING btree (parent_block_id) WHERE (parent_block_id IS NOT NULL)', false)",
    ),
];

/// IRG-112 is a documented manual hop, not a second automated delta.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS_IRG112: &[&[(&str, &str)]] = &[
    REQUIRED_BASELINE_OBJECTS_IRG75,
    REQUIRED_BASELINE_OBJECTS_PROVISIONING,
    REQUIRED_BASELINE_OBJECTS_IRG112,
];

/// The object groups the IRG-173 `failure_disposition` revision declares: the
/// cumulative set through its own target.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS_IRG173: &[&[(&str, &str)]] = &[
    REQUIRED_BASELINE_OBJECTS_IRG75,
    REQUIRED_BASELINE_OBJECTS_PROVISIONING,
    REQUIRED_BASELINE_OBJECTS_IRG112,
    REQUIRED_BASELINE_OBJECTS_IRG173,
];

/// The object groups the `query_rerank` revision declares: the cumulative set
/// through its own target.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS_QUERY_RERANK: &[&[(&str, &str)]] = &[
    REQUIRED_BASELINE_OBJECTS_IRG75,
    REQUIRED_BASELINE_OBJECTS_PROVISIONING,
    REQUIRED_BASELINE_OBJECTS_IRG112,
    REQUIRED_BASELINE_OBJECTS_IRG173,
    REQUIRED_BASELINE_OBJECTS_QUERY_RERANK,
];

/// The object groups the decision-catalog revision declares (IRG-182): the
/// cumulative set through its own target.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS_DECISION_CATALOG: &[&[(&str, &str)]] = &[
    REQUIRED_BASELINE_OBJECTS_IRG75,
    REQUIRED_BASELINE_OBJECTS_PROVISIONING,
    REQUIRED_BASELINE_OBJECTS_IRG112,
    REQUIRED_BASELINE_OBJECTS_IRG173,
    REQUIRED_BASELINE_OBJECTS_QUERY_RERANK,
    REQUIRED_BASELINE_OBJECTS_DECISION_CATALOG,
];

/// The object groups the admission-visibility-policy revision declares (IRG-92):
/// the cumulative set through its own target.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS_ADMISSION_VISIBILITY_POLICY: &[&[(
    &str,
    &str,
)]] = &[
    REQUIRED_BASELINE_OBJECTS_IRG75,
    REQUIRED_BASELINE_OBJECTS_PROVISIONING,
    REQUIRED_BASELINE_OBJECTS_IRG112,
    REQUIRED_BASELINE_OBJECTS_IRG173,
    REQUIRED_BASELINE_OBJECTS_QUERY_RERANK,
    REQUIRED_BASELINE_OBJECTS_DECISION_CATALOG,
    REQUIRED_BASELINE_OBJECTS_ADMISSION_VISIBILITY_POLICY,
];

const REQUIRED_BASELINE_OBJECTS_CONTENT_VISIBILITY_INTENT: &[(&str, &str)] = &[
    (
        "retired admission visibility policy absent",
        "select to_regtype('public.admission_visibility_policy') is null and not exists (select 1 from information_schema.columns where table_schema='public' and table_name='content_mutation' and column_name='visibility_policy')",
    ),
    (
        "same-document visibility FK columns",
        "select count(*)=3 from pg_constraint c join (values ('content_visibility_intent','content_visibility_intent_item_fkey',array['mutation_item_id','document_id'],'content_mutation_item',array['id','document_id']), ('content_mutation_item','content_mutation_item_visibility_intent_fkey',array['visibility_intent_id','document_id'],'content_visibility_intent',array['mutation_item_id','document_id']), ('content_document_head','content_document_head_visibility_intent_fkey',array['visibility_intent_id','document_id'],'content_visibility_intent',array['mutation_item_id','document_id']) ) expected(tbl,name,src,target,dst) on c.conrelid=to_regclass('public.'||expected.tbl) and c.confrelid=to_regclass('public.'||expected.target) and c.conname=expected.name where c.contype='f' and (select array_agg(a.attname::text order by k.ordinality) from unnest(c.conkey) with ordinality k(attnum,ordinality) join pg_attribute a on a.attrelid=c.conrelid and a.attnum=k.attnum)=expected.src and (select array_agg(a.attname::text order by k.ordinality) from unnest(c.confkey) with ordinality k(attnum,ordinality) join pg_attribute a on a.attrelid=c.confrelid and a.attnum=k.attnum)=expected.dst",
    ),
    (
        "visibility operation enum",
        "select exists (select 1 from pg_enum e join pg_type t on t.oid = e.enumtypid where t.typnamespace = 'public'::regnamespace and t.typname = 'content_mutation_operation_kind' and e.enumlabel = 'visibility')",
    ),
    (
        "visibility typed columns",
        "select count(*) = 6 from information_schema.columns c join (values ('content_document','revision_sequence','int4','NO'), ('content_mutation','request_identity','jsonb','NO'), ('content_mutation_item','visibility_intent_id','uuid','YES'), ('content_document_head','visibility_intent_id','uuid','YES'), ('content_revision','admission_identity','jsonb','YES'), ('content_visibility_intent','revision_floor','int4','NO')) expected(tbl,col,typ,nullable) on c.table_name=expected.tbl and c.column_name=expected.col and c.udt_name=expected.typ and c.is_nullable=expected.nullable where c.table_schema='public'",
    ),
    (
        "visibility ownership and retention constraints",
        "select count(*) = 10 from pg_constraint c join (values ('content_document','content_document_revision_sequence_check','c',false,' '), ('content_visibility_intent','content_visibility_intent_pkey','p',false,' '), ('content_visibility_intent','content_visibility_intent_revision_floor_check','c',false,' '), ('content_visibility_intent','content_visibility_intent_document_key','u',false,' '), ('content_visibility_intent','content_visibility_intent_item_fkey','f',false,'c'), ('content_visibility_intent','content_visibility_intent_document_fkey','f',false,'c'), ('content_mutation_item','content_mutation_item_id_document_key','u',false,' '), ('content_mutation_item','content_mutation_item_published_lineage_key','u',false,' '), ('content_mutation_item','content_mutation_item_visibility_intent_fkey','f',true,'a'), ('content_document_head','content_document_head_visibility_intent_fkey','f',true,'a') ) expected(tbl,name,typ,deferred,del) on c.conrelid=to_regclass('public.'||expected.tbl) and c.conname=expected.name and c.contype::text=expected.typ and c.condeferrable=expected.deferred and c.condeferred=expected.deferred and (c.contype <> 'f' or c.confdeltype::text=expected.del) where c.convalidated",
    ),
    (
        "revision-owned publication authority",
        "select not exists (select 1 from information_schema.columns where table_schema='public' and table_name='content_document_head' and column_name='readable_mutation_item_id') and exists (select 1 from pg_trigger t join pg_proc p on p.oid=t.tgfoid where t.tgrelid='public.content_revision'::regclass and t.tgname='content_revision_admission_immutable' and not t.tgisinternal and t.tgenabled='O' and t.tgtype=23 and p.proname='preserve_revision_admission_identity') and exists (select 1 from pg_trigger t join pg_proc p on p.oid=t.tgfoid where t.tgrelid='public.content_document_head'::regclass and t.tgname='content_document_head_revision_admission' and t.tgenabled='O' and t.tgdeferrable and t.tginitdeferred and p.proname='verify_head_revision_admission')",
    ),
    (
        "visibility result index",
        "select exists (select 1 from pg_index where indexrelid=to_regclass('public.content_mutation_item_document_result_idx') and indrelid='public.content_mutation_item'::regclass and indisvalid and indnkeyatts=2)",
    ),
    (
        "current revision view",
        "select exists (select 1 from pg_class c join pg_attribute a on a.attrelid=c.oid where c.oid=to_regclass('public.content_current_revision') and c.relkind='v' and a.attname='source_file_name' and a.atttypid='text'::regtype and a.attnum=6 and not a.attisdropped)",
    ),
    (
        "immutable semantic identity trigger",
        "select exists (select 1 from pg_trigger t join pg_proc p on p.oid=t.tgfoid where t.tgrelid='public.content_mutation'::regclass and t.tgname='content_mutation_request_identity_immutable' and not t.tgisinternal and t.tgenabled='O' and t.tgtype=23 and p.pronamespace='public'::regnamespace and p.proname='preserve_content_request_identity')",
    ),
    (
        "canonical mutation reference, historical audit and current-view certificate",
        "with owned as (
            select oid,relname,relkind from pg_class where relnamespace='public'::regnamespace
            and relname in ('content_document','content_revision','content_mutation_reference','content_mutation_predecessor_audit','content_mutation','content_mutation_item','content_document_head','content_visibility_intent','content_current_revision','ingest_job','content_web_ingest_run')
        ), definitions as (
            select 'column:'||o.relname||':'||a.attname as key, format_type(a.atttypid,a.atttypmod)||':'||a.attnotnull::text||':'||coalesce(pg_get_expr(d.adbin,d.adrelid),'') as definition
            from owned o join pg_attribute a on a.attrelid=o.oid and a.attnum>0 and not a.attisdropped left join pg_attrdef d on d.adrelid=o.oid and d.adnum=a.attnum
            union all select 'constraint:'||o.relname||':'||c.conname,pg_get_constraintdef(c.oid)||':'||c.convalidated::text from owned o join pg_constraint c on c.conrelid=o.oid
            union all select 'index:'||o.relname||':'||i.indexrelid::regclass::text,pg_get_indexdef(i.indexrelid)||':'||i.indisvalid::text from owned o join pg_index i on i.indrelid=o.oid
            union all select 'trigger:'||o.relname||':'||t.tgname,pg_get_triggerdef(t.oid)||':'||t.tgenabled::text from owned o join pg_trigger t on t.tgrelid=o.oid where not t.tgisinternal
            union all select 'view:'||o.relname,pg_get_viewdef(o.oid) from owned o where o.relkind='v'
            union all select 'function:'||p.proname,pg_get_functiondef(p.oid) from pg_proc p where p.pronamespace='public'::regnamespace and p.proname in ('preserve_content_request_identity','preserve_content_mutation_reference','verify_content_mutation_representation','preserve_content_lineage_owner','verify_content_mutation_lineage','preserve_revision_admission_identity','verify_head_revision_admission')
        ) select encode(digest(jsonb_agg(jsonb_build_array(key,definition) order by key)::text,'sha256'),'hex') = 'a8e0f1952ae98818a656fc7bc733094c62925b05dc1a8f0d17d51e63534d9488' from definitions",
    ),
];

/// Current intent target retains decision-catalog groups, retires the historical
/// policy group, and declares the canonical intent objects and policy absence.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS_CONTENT_VISIBILITY_INTENT: &[&[(&str, &str)]] = &[
    REQUIRED_BASELINE_OBJECTS_IRG75,
    REQUIRED_BASELINE_OBJECTS_PROVISIONING,
    REQUIRED_BASELINE_OBJECTS_IRG112,
    REQUIRED_BASELINE_OBJECTS_IRG173,
    REQUIRED_BASELINE_OBJECTS_QUERY_RERANK,
    REQUIRED_BASELINE_OBJECTS_DECISION_CATALOG,
    REQUIRED_BASELINE_OBJECTS_CONTENT_VISIBILITY_INTENT,
];

/// Readiness validates the current intent revision, not retired predecessor
/// objects. Historical group constants remain exact migration certificates.
pub(crate) const REQUIRED_BASELINE_OBJECT_GROUPS: &[&[(&str, &str)]] =
    REQUIRED_BASELINE_OBJECT_GROUPS_CONTENT_VISIBILITY_INTENT;

/// Fails when the recorded baseline checksum is current but an object that
/// baseline declares is absent.
///
/// Kept separate from the checksum walk so the operator transition can run the
/// same check before it stamps a checksum, rather than discovering the gap as a
/// query error in production. Extend the matching revision group in the same
/// commit that adds an object to the baseline.
pub async fn validate_baseline_objects_present(postgres: &PgPool) -> anyhow::Result<()> {
    let mut connection = postgres.acquire().await?;
    validate_baseline_objects_present_on(&mut connection).await
}

/// The same check against an open connection, so the operator transition can
/// run it inside the transaction that stamps the checksum — the only place
/// where "the DDL ran" and "the checksum says it ran" can still be made to
/// agree atomically.
pub async fn validate_baseline_objects_present_on(
    connection: &mut sqlx::PgConnection,
) -> anyhow::Result<()> {
    validate_baseline_object_groups_on(connection, REQUIRED_BASELINE_OBJECT_GROUPS).await
}

/// Validates a chosen list of revision groups. A transition passes only the
/// groups its own target revision declares, so the whole-list readiness check
/// and the additive path share one implementation without sharing a future
/// revision's demands.
pub(crate) async fn validate_baseline_object_groups_on(
    connection: &mut sqlx::PgConnection,
    groups: &[&[(&str, &str)]],
) -> anyhow::Result<()> {
    for group in groups {
        for (description, predicate) in *group {
            let present: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(*predicate))
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("failed to check for baseline object {description}: {error}")
                })?;
            anyhow::ensure!(
                present,
                "baseline object `{description}` is absent from this database; the current \
                 baseline checksum must not stand for it. Apply the additive path in \
                 docs/ru/CLI.md, then re-run — readiness refuses to serve and \
                 the transition command refuses to stamp until it is present"
            );
        }
    }
    Ok(())
}

pub async fn validate_required_bootstrap_state(
    postgres: &PgPool,
    settings: &Settings,
) -> anyhow::Result<()> {
    if !settings.destructive_fresh_bootstrap_settings().required {
        return Ok(());
    }

    if !required_baseline_present(postgres).await? {
        anyhow::bail!(
            "bootstrap validation failed: required tables `catalog_workspace`, `catalog_library`, `iam_principal`, `iam_user`, `ai_provider_catalog`, and `ai_model_catalog` are missing after migration"
        );
    }

    anyhow::ensure!(
        required_ai_catalog_seeded(postgres).await?,
        "bootstrap validation failed: ai_provider_catalog or ai_model_catalog is missing seeded rows after migration"
    );

    Ok(())
}

pub async fn required_baseline_present(postgres: &PgPool) -> anyhow::Result<bool> {
    for table_name in REQUIRED_SNAPSHOT_TABLES {
        if !table_exists(postgres, table_name).await? {
            return Ok(false);
        }
    }

    Ok(true)
}

pub async fn required_ai_catalog_seeded(postgres: &PgPool) -> anyhow::Result<bool> {
    if !table_exists(postgres, "ai_provider_catalog").await?
        || !table_exists(postgres, "ai_model_catalog").await?
    {
        return Ok(false);
    }

    // Pricing lives on the model row itself, so an active provider with an
    // active model is a complete catalog chain.
    let has_complete_catalog_chain = sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1
            from ai_provider_catalog provider
            join ai_model_catalog model on model.provider_catalog_id = provider.id
            where provider.lifecycle_state = 'active'
              and model.lifecycle_state = 'active'
        )",
    )
    .fetch_one(postgres)
    .await?;

    Ok(has_complete_catalog_chain)
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
    fn db_job_slots_are_reserved_only_for_workers() {
        for budget in [4, 20, 60] {
            let plan = postgres_pool_plan(ServiceRole::Worker, budget, 1, 1, 64);
            assert_eq!(
                worker_process_db_job_limit_for(ServiceRole::Worker, &plan),
                plan.worker_job_slot_limit(),
            );
            assert_eq!(worker_process_db_job_limit_for(ServiceRole::Api, &plan), 0);
            assert_eq!(worker_process_db_job_limit_for(ServiceRole::Startup, &plan), 0);
        }
    }

    #[test]
    fn tls_leaf_attestation_hashes_the_single_der_certificate() -> anyhow::Result<()> {
        let der = b"one exact leaf certificate";
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            BASE64_STANDARD.encode(der)
        );

        assert_eq!(
            single_pem_certificate_sha256(pem.as_bytes())?,
            hex::encode(Sha256::digest(der))
        );
        Ok(())
    }

    #[test]
    fn tls_leaf_attestation_rejects_a_certificate_bundle() {
        let certificate = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            BASE64_STANDARD.encode(b"certificate")
        );
        let bundle = format!("{certificate}{certificate}");

        assert!(single_pem_certificate_sha256(bundle.as_bytes()).is_err());
    }

    #[test]
    fn api_pool_plan_honours_configured_budget() {
        let plan = postgres_pool_plan(ServiceRole::Api, 60, 1, 1, 64);

        assert_eq!(plan.main_max_connections, 28);
        assert_eq!(plan.control_max_connections, 2);
        assert_eq!(plan.scheduler_control_max_connections, 0);
        assert_eq!(plan.main_min_connections, 1);
        assert_eq!(plan.control_min_connections, 0);
        assert_eq!(plan.idle_timeout, Duration::from_secs(60));
    }

    #[test]
    fn api_pool_plan_splits_a_stage_sized_budget_across_replicas() {
        let plan = postgres_pool_plan(ServiceRole::Api, 50, 1, 1, 64);

        assert_eq!(plan.main_max_connections, 23);
        assert_eq!(plan.control_max_connections, 2);
    }

    #[test]
    fn worker_pool_plan_keeps_heartbeat_pool_inside_process_budget() {
        let plan = postgres_pool_plan(ServiceRole::Worker, 20, 1, 1, 64);

        assert_eq!(plan.main_max_connections, 4);
        assert_eq!(plan.control_max_connections, 5);
        assert_eq!(plan.control_min_connections, 1);
        assert_eq!(plan.scheduler_control_max_connections, 1);
        assert_eq!(plan.worker_job_slot_limit(), 4);
    }

    #[test]
    fn worker_pool_plan_divides_budget_across_runtime_replicas() {
        let plan = postgres_pool_plan(ServiceRole::Worker, 20, 2, 2, 64);

        assert_eq!(plan.main_max_connections, 1);
        assert_eq!(plan.control_max_connections, 3);
        assert_eq!(plan.scheduler_control_max_connections, 1);
        assert_eq!(plan.worker_job_slot_limit(), 1);
    }

    #[test]
    fn minimum_worker_budget_keeps_distinct_main_heartbeat_and_scheduler_lanes() {
        let plan = postgres_pool_plan(ServiceRole::Worker, 4, 0, 1, 64);

        assert_eq!(plan.main_max_connections, 1);
        assert_eq!(plan.control_max_connections, 2);
        assert_eq!(plan.scheduler_control_max_connections, 1);
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
                    let per_worker = worker_plan.main_max_connections
                        + worker_plan.control_max_connections
                        + worker_plan.scheduler_control_max_connections;
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
    fn worker_guard_capacity_leaves_main_queries_and_reaper_lane_available() {
        for api_replicas in 0usize..=8 {
            for worker_replicas in 1usize..=8 {
                let runtime_replicas = api_replicas + worker_replicas;
                let min_valid_budget = POSTGRES_POOL_MIN_BUDGET * runtime_replicas as u32;
                for configured_budget in min_valid_budget..=128 {
                    for worker_global_limit in [1usize, 2, 4, 8, 16, 64] {
                        let plan = postgres_pool_plan(
                            ServiceRole::Worker,
                            configured_budget,
                            api_replicas,
                            worker_replicas,
                            worker_global_limit,
                        );
                        let job_limit = u32::try_from(plan.worker_job_slot_limit()).unwrap();

                        assert!(
                            plan.main_max_connections >= job_limit,
                            "guarded_jobs={job_limit} must retain one main query lane per job: {plan:?}",
                        );
                        assert!(
                            plan.control_max_connections
                                >= job_limit + WORKER_CONTROL_RESERVED_CONNECTIONS,
                            "control pool must hold every long graph guard plus the reaper reserve: {plan:?}",
                        );
                        assert_eq!(
                            plan.scheduler_control_max_connections,
                            WORKER_SCHEDULER_CONTROL_CONNECTIONS,
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn startup_pool_plan_does_not_inherit_runtime_budget() {
        let plan = postgres_pool_plan(ServiceRole::Startup, 60, 1, 1, 64);

        assert_eq!(plan.main_max_connections, 3);
        assert_eq!(plan.control_max_connections, 1);
        assert_eq!(plan.scheduler_control_max_connections, 0);
    }
}
