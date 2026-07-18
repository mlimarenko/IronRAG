use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use tokio::sync::{Mutex, RwLock};

use crate::{
    app::state::AppState,
    domains::deployment::{DependencyKind, DependencyMode, StartupAuthorityMode},
    infra::persistence::{canonical_baseline_present, validate_postgres_migration_state},
    services::content::storage::types::ContentStorageProbeStatus,
};

#[derive(Debug, Clone, Copy, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DependencyHealth {
    Ok,
    Down,
    Misconfigured,
}

const WORKER_STATUS_IDLE: &str = "idle";
const WORKER_STATUS_ACTIVE: &str = "active";
const WORKER_STATUS_ERROR: &str = "error";

const DEPENDENCY_MODE_MISCONFIGURED: &str = "misconfigured";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StartupAuthorityState {
    Succeeded,
    Pending,
    NotRequired,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverallReadiness {
    Ready,
    Degraded,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageHealth {
    Ok,
    Down,
    Unsupported,
    Misconfigured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TopologySupport {
    Supported,
    NotSupported,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerRuntimeState {
    snapshot: Arc<RwLock<WorkerRuntimeSnapshot>>,
}

#[derive(Clone, Debug)]
pub struct WorkerRuntimeSnapshot {
    pub status: &'static str,
    pub message: Option<String>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}

impl Default for WorkerRuntimeSnapshot {
    fn default() -> Self {
        Self { status: WORKER_STATUS_IDLE, message: None, last_heartbeat_at: None }
    }
}

impl WorkerRuntimeState {
    pub async fn mark_idle(&self) {
        let mut snapshot = self.snapshot.write().await;
        snapshot.status = WORKER_STATUS_IDLE;
        snapshot.message = None;
        snapshot.last_heartbeat_at = Some(Utc::now());
    }

    pub async fn mark_active(&self, message: impl Into<String>) {
        let mut snapshot = self.snapshot.write().await;
        snapshot.status = WORKER_STATUS_ACTIVE;
        snapshot.message = Some(message.into());
        snapshot.last_heartbeat_at = Some(Utc::now());
    }

    pub async fn mark_error(&self, message: impl Into<String>) {
        let mut snapshot = self.snapshot.write().await;
        snapshot.status = WORKER_STATUS_ERROR;
        snapshot.message = Some(message.into());
        snapshot.last_heartbeat_at = Some(Utc::now());
    }

    pub async fn touch(&self) {
        let mut snapshot = self.snapshot.write().await;
        snapshot.last_heartbeat_at = Some(Utc::now());
    }

    pub async fn is_idle(&self) -> bool {
        self.snapshot.read().await.status == WORKER_STATUS_IDLE
    }

    pub async fn snapshot(&self) -> WorkerRuntimeSnapshot {
        self.snapshot.read().await.clone()
    }
}

const READINESS_SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(1);
const READINESS_SNAPSHOT_MAX_STALE: Duration = Duration::from_secs(5);
const STARTUP_AUTHORITY_STABLE_CACHE_TTL: Duration = Duration::from_secs(30);
const STARTUP_AUTHORITY_TRANSIENT_CACHE_TTL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct DeploymentDiagnosticsService {
    readiness_cache: Arc<RwLock<Option<CachedReadinessSnapshot>>>,
    readiness_cold_start_lock: Arc<Mutex<()>>,
    readiness_refresh_in_flight: Arc<AtomicBool>,
    startup_authority_cache: Arc<RwLock<Option<CachedStartupAuthorityStatus>>>,
    startup_authority_refresh_lock: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct CachedReadinessSnapshot {
    checked_at: Instant,
    ready: bool,
    snapshot: DeploymentReadinessSnapshot,
}

#[derive(Clone)]
struct CachedStartupAuthorityStatus {
    checked_at: Instant,
    status: StartupAuthorityStatus,
}

struct ReadinessRefreshGuard {
    in_flight: Arc<AtomicBool>,
}

impl Drop for ReadinessRefreshGuard {
    fn drop(&mut self) {
        self.in_flight.store(false, Ordering::Release);
    }
}

impl Default for DeploymentDiagnosticsService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DependencyStatus {
    pub mode: String,
    pub status: DependencyHealth,
    pub message: Option<String>,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DependencyStatusSet {
    pub postgres: DependencyStatus,
    pub redis: DependencyStatus,
    pub knowledge_plane: DependencyStatus,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartupAuthorityStatus {
    pub mode: String,
    pub state: StartupAuthorityState,
    pub message: Option<String>,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatus {
    pub provider: String,
    pub status: StorageHealth,
    pub topology: String,
    pub bucket: Option<String>,
    pub root_path: Option<String>,
    pub endpoint: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopologyStatus {
    pub status: TopologySupport,
    pub message: Option<String>,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentReadinessSnapshot {
    pub status: OverallReadiness,
    pub role: String,
    pub startup_authority: StartupAuthorityStatus,
    pub dependencies: DependencyStatusSet,
    pub storage: StorageStatus,
    pub topology: TopologyStatus,
    pub message: Option<String>,
    pub checked_at: String,
}

impl DeploymentDiagnosticsService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            readiness_cache: Arc::new(RwLock::new(None)),
            readiness_cold_start_lock: Arc::new(Mutex::new(())),
            readiness_refresh_in_flight: Arc::new(AtomicBool::new(false)),
            startup_authority_cache: Arc::new(RwLock::new(None)),
            startup_authority_refresh_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn readiness_snapshot(
        &self,
        state: &AppState,
    ) -> (bool, DeploymentReadinessSnapshot) {
        if let Some(cached) = self.cached_readiness_snapshot().await {
            let cached_age = cached.checked_at.elapsed();
            if cached_age > READINESS_SNAPSHOT_MAX_STALE {
                return self.refresh_readiness_snapshot_synchronously(state).await;
            }
            if cached_age > READINESS_SNAPSHOT_CACHE_TTL {
                self.refresh_readiness_snapshot_in_background(state.clone());
            }
            return (cached.ready, cached.snapshot);
        }

        self.refresh_readiness_snapshot_synchronously(state).await
    }

    async fn refresh_readiness_snapshot_synchronously(
        &self,
        state: &AppState,
    ) -> (bool, DeploymentReadinessSnapshot) {
        let _cold_start = self.readiness_cold_start_lock.lock().await;
        if let Some(cached) = self.cached_readiness_snapshot().await {
            if cached.checked_at.elapsed() > READINESS_SNAPSHOT_MAX_STALE {
                let (ready, snapshot) = self.compute_readiness_snapshot(state).await;
                self.store_readiness_snapshot(ready, snapshot.clone()).await;
                return (ready, snapshot);
            }
            return (cached.ready, cached.snapshot);
        }

        let (ready, snapshot) = self.compute_readiness_snapshot(state).await;
        self.store_readiness_snapshot(ready, snapshot.clone()).await;
        (ready, snapshot)
    }

    async fn cached_readiness_snapshot(&self) -> Option<CachedReadinessSnapshot> {
        self.readiness_cache.read().await.clone()
    }

    fn refresh_readiness_snapshot_in_background(&self, state: AppState) {
        if self.readiness_refresh_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }

        let service = self.clone();
        let guard = ReadinessRefreshGuard { in_flight: self.readiness_refresh_in_flight.clone() };
        tokio::spawn(async move {
            let _guard = guard;
            let (ready, snapshot) = service.compute_readiness_snapshot(&state).await;
            service.store_readiness_snapshot(ready, snapshot).await;
        });
    }

    async fn store_readiness_snapshot(&self, ready: bool, snapshot: DeploymentReadinessSnapshot) {
        let mut cache = self.readiness_cache.write().await;
        *cache = Some(CachedReadinessSnapshot { checked_at: Instant::now(), ready, snapshot });
    }

    async fn compute_readiness_snapshot(
        &self,
        state: &AppState,
    ) -> (bool, DeploymentReadinessSnapshot) {
        let role = configured_service_role(state);
        let postgres_status = postgres_dependency_status(state).await;
        let redis_status = redis_dependency_status(state).await;
        let knowledge_plane_status = knowledge_plane_dependency_status(state).await;
        let storage = storage_status(state).await;
        let topology = topology_status(&storage);
        let startup_authority = self.startup_authority_status(state).await;
        let worker_snapshot = state.worker_runtime.snapshot().await;

        log_dependency_statuses([
            ("postgres", &postgres_status),
            ("redis", &redis_status),
            ("knowledge_plane", &knowledge_plane_status),
        ]);

        let readiness = ReadinessAssessment::new(ReadinessInputs {
            runs_ingestion_workers: state.settings.runs_ingestion_workers(),
            postgres: &postgres_status,
            redis: &redis_status,
            knowledge_plane: &knowledge_plane_status,
            storage: &storage,
            topology: &topology,
            startup_authority: &startup_authority,
            worker: &worker_snapshot,
        });
        tracing::info!(stage = "readiness", overall = ?readiness.overall_status, "readiness probe completed");

        (
            readiness.is_ready,
            DeploymentReadinessSnapshot {
                status: readiness.overall_status,
                role,
                startup_authority,
                dependencies: DependencyStatusSet {
                    postgres: postgres_status,
                    redis: redis_status,
                    knowledge_plane: knowledge_plane_status,
                },
                storage,
                topology,
                message: readiness.message,
                checked_at: Utc::now().to_rfc3339(),
            },
        )
    }

    async fn startup_authority_status(&self, state: &AppState) -> StartupAuthorityStatus {
        if let Some(cached) = self.cached_startup_authority_status().await {
            return cached;
        }

        let _refresh = self.startup_authority_refresh_lock.lock().await;
        if let Some(cached) = self.cached_startup_authority_status().await {
            return cached;
        }

        let status = self.compute_startup_authority_status(state).await;
        let mut cache = self.startup_authority_cache.write().await;
        *cache = Some(CachedStartupAuthorityStatus {
            checked_at: Instant::now(),
            status: status.clone(),
        });
        status
    }

    async fn cached_startup_authority_status(&self) -> Option<StartupAuthorityStatus> {
        let cache = self.startup_authority_cache.read().await;
        let status = {
            let cached = cache.as_ref()?;
            let ttl = startup_authority_status_cache_ttl(cached.status.state);
            (cached.checked_at.elapsed() <= ttl).then(|| cached.status.clone())
        };
        drop(cache);
        status
    }

    async fn compute_startup_authority_status(&self, state: &AppState) -> StartupAuthorityStatus {
        let mode = state
            .settings
            .startup_authority_mode_kind()
            .unwrap_or(StartupAuthorityMode::NotRequired);
        let control_plane_postgres = &state.persistence.heartbeat_postgres;
        if !canonical_baseline_present(control_plane_postgres).await.unwrap_or(false) {
            return StartupAuthorityStatus {
                mode: mode.as_str().to_string(),
                state: StartupAuthorityState::Pending,
                message: Some("postgres baseline has not been initialized yet".to_string()),
            };
        }
        if let Err(error) = validate_postgres_migration_state(control_plane_postgres).await {
            return StartupAuthorityStatus {
                mode: mode.as_str().to_string(),
                state: StartupAuthorityState::Pending,
                message: Some(format!(
                    "postgres migration state is not compatible with the current binary: {error}"
                )),
            };
        }
        if matches!(mode, StartupAuthorityMode::NotRequired) {
            return StartupAuthorityStatus {
                mode: mode.as_str().to_string(),
                state: StartupAuthorityState::NotRequired,
                message: None,
            };
        }
        if state.settings.runs_startup_authority() {
            return StartupAuthorityStatus {
                mode: mode.as_str().to_string(),
                state: StartupAuthorityState::Running,
                message: Some("startup authority is executing".to_string()),
            };
        }
        if state.settings.knowledge_plane_backend != "postgres" {
            return StartupAuthorityStatus {
                mode: mode.as_str().to_string(),
                state: StartupAuthorityState::Pending,
                message: Some(format!(
                    "unsupported knowledge_plane_backend `{}`",
                    state.settings.knowledge_plane_backend
                )),
            };
        }
        StartupAuthorityStatus {
            mode: mode.as_str().to_string(),
            state: StartupAuthorityState::Succeeded,
            message: None,
        }
    }
}

async fn postgres_dependency_status(state: &AppState) -> DependencyStatus {
    let postgres_ready = sqlx::query("select 1")
        .fetch_one(&state.persistence.heartbeat_postgres)
        .await
        .is_ok_and(|row| row.get::<i32, _>(0) == 1);
    dependency_status(
        state.settings.dependency_mode(DependencyKind::Postgres),
        postgres_ready,
        "postgres unreachable".to_string(),
    )
}

async fn redis_dependency_status(state: &AppState) -> DependencyStatus {
    let redis_ready = match state.persistence.redis.get_multiplexed_async_connection().await {
        Ok(mut connection) => redis::cmd("PING")
            .query_async::<String>(&mut connection)
            .await
            .is_ok_and(|value| value == "PONG"),
        Err(_) => false,
    };
    dependency_status(
        state.settings.dependency_mode(DependencyKind::Redis),
        redis_ready,
        "redis unreachable".to_string(),
    )
}

async fn knowledge_plane_dependency_status(state: &AppState) -> DependencyStatus {
    match state.settings.knowledge_plane_backend.as_str() {
        "postgres" => dependency_status(
            state.settings.dependency_mode(DependencyKind::Postgres),
            postgres_knowledge_plane_ready(state).await,
            "postgres knowledge plane not ready".to_string(),
        ),
        backend => DependencyStatus {
            mode: DEPENDENCY_MODE_MISCONFIGURED.to_string(),
            status: DependencyHealth::Misconfigured,
            message: Some(format!("unsupported knowledge_plane_backend `{backend}`")),
        },
    }
}

async fn storage_status(state: &AppState) -> StorageStatus {
    let probe = state.content_storage.probe().await;
    let diagnostics = state.content_storage.diagnostics();
    StorageStatus {
        provider: diagnostics.provider.as_str().to_string(),
        status: storage_health(probe.status),
        topology: diagnostics.topology.as_str().to_string(),
        bucket: diagnostics.bucket.clone(),
        root_path: diagnostics.root_path.as_ref().map(|path| path.display().to_string()),
        endpoint: diagnostics.endpoint.clone(),
        message: probe.message,
    }
}

const fn storage_health(status: ContentStorageProbeStatus) -> StorageHealth {
    match status {
        ContentStorageProbeStatus::Ok => StorageHealth::Ok,
        ContentStorageProbeStatus::Down => StorageHealth::Down,
        ContentStorageProbeStatus::Unsupported => StorageHealth::Unsupported,
        ContentStorageProbeStatus::Misconfigured => StorageHealth::Misconfigured,
    }
}

fn topology_status(storage: &StorageStatus) -> TopologyStatus {
    if storage.status == StorageHealth::Unsupported {
        return TopologyStatus {
            status: TopologySupport::NotSupported,
            message: Some(
                "deployment topology is incompatible with the configured content storage provider"
                    .to_string(),
            ),
        };
    }
    TopologyStatus { status: TopologySupport::Supported, message: None }
}

fn configured_service_role(state: &AppState) -> String {
    state
        .settings
        .service_role_kind()
        .map_or_else(|_| state.settings.service_role.as_str(), |role| role.as_str())
        .to_string()
}

fn log_dependency_statuses(dependencies: [(&str, &DependencyStatus); 3]) {
    for (name, status) in dependencies {
        tracing::debug!(stage = "readiness", dependency = %name, status = ?status.status, "health check completed");
        if !matches!(status.status, DependencyHealth::Ok) {
            tracing::warn!(stage = "readiness", dependency = %name, status = ?status.status, "dependency degraded");
        }
    }
}

struct ReadinessAssessment {
    is_ready: bool,
    overall_status: OverallReadiness,
    message: Option<String>,
}

struct ReadinessInputs<'a> {
    runs_ingestion_workers: bool,
    postgres: &'a DependencyStatus,
    redis: &'a DependencyStatus,
    knowledge_plane: &'a DependencyStatus,
    storage: &'a StorageStatus,
    topology: &'a TopologyStatus,
    startup_authority: &'a StartupAuthorityStatus,
    worker: &'a WorkerRuntimeSnapshot,
}

impl ReadinessAssessment {
    fn new(inputs: ReadinessInputs<'_>) -> Self {
        let dependencies_ready =
            [inputs.postgres.status, inputs.redis.status, inputs.knowledge_plane.status]
                .into_iter()
                .all(|status| matches!(status, DependencyHealth::Ok));
        let storage_ready = inputs.storage.status == StorageHealth::Ok;
        let topology_supported = inputs.topology.status == TopologySupport::Supported;
        let startup_ready = matches!(
            inputs.startup_authority.state,
            StartupAuthorityState::Succeeded | StartupAuthorityState::NotRequired
        );
        let worker_ready =
            !inputs.runs_ingestion_workers || inputs.worker.status != WORKER_STATUS_ERROR;
        let is_ready = dependencies_ready
            && storage_ready
            && topology_supported
            && startup_ready
            && worker_ready;
        let message = readiness_message(
            dependencies_ready,
            storage_ready,
            topology_supported,
            startup_ready,
            worker_ready,
            inputs.startup_authority,
            inputs.worker,
        );
        let overall_status = overall_readiness_status(is_ready, topology_supported);

        Self { is_ready, overall_status, message }
    }
}

fn readiness_message(
    dependencies_ready: bool,
    storage_ready: bool,
    topology_supported: bool,
    startup_ready: bool,
    worker_ready: bool,
    startup_authority: &StartupAuthorityStatus,
    worker: &WorkerRuntimeSnapshot,
) -> Option<String> {
    if !dependencies_ready {
        return Some("one or more dependencies are unavailable".to_string());
    }
    if !storage_ready {
        return Some("content storage provider is not ready".to_string());
    }
    if !topology_supported {
        return Some(
            "deployment topology is unsupported for the selected storage provider".to_string(),
        );
    }
    if !startup_ready {
        return startup_authority.message.clone();
    }
    if !worker_ready {
        return worker.message.clone().or_else(|| Some("worker runtime is degraded".to_string()));
    }
    None
}

const fn overall_readiness_status(is_ready: bool, topology_supported: bool) -> OverallReadiness {
    if is_ready {
        OverallReadiness::Ready
    } else if topology_supported {
        OverallReadiness::Degraded
    } else {
        OverallReadiness::Blocked
    }
}

async fn postgres_knowledge_plane_ready(state: &AppState) -> bool {
    sqlx::query_scalar::<_, bool>(
        r"
        with ping as (select 1 as ok)
        select
            (select ok from ping) = 1
            and exists (select 1 from pg_extension where extname = 'vector')
            and to_regclass('public.knowledge_chunk') is not null
            and exists (
                select 1
                from information_schema.tables
                where table_schema = 'public'
                  and table_name like 'knowledge\_%' escape '\'
            )
        ",
    )
    .fetch_one(&state.persistence.heartbeat_postgres)
    .await
    .unwrap_or(false)
}

const fn startup_authority_status_cache_ttl(state: StartupAuthorityState) -> Duration {
    match state {
        StartupAuthorityState::Succeeded | StartupAuthorityState::NotRequired => {
            STARTUP_AUTHORITY_STABLE_CACHE_TTL
        }
        StartupAuthorityState::Pending | StartupAuthorityState::Running => {
            STARTUP_AUTHORITY_TRANSIENT_CACHE_TTL
        }
    }
}

fn dependency_status(
    mode: Result<DependencyMode, String>,
    ok: bool,
    default_message: String,
) -> DependencyStatus {
    match mode {
        Ok(mode @ (DependencyMode::Bundled | DependencyMode::External)) => DependencyStatus {
            mode: mode.as_str().to_string(),
            status: if ok { DependencyHealth::Ok } else { DependencyHealth::Down },
            message: if ok { None } else { Some(default_message) },
        },
        Ok(DependencyMode::Disabled) => DependencyStatus {
            mode: DependencyMode::Disabled.as_str().to_string(),
            status: DependencyHealth::Misconfigured,
            message: Some("dependency is disabled but the runtime requires it".to_string()),
        },
        Err(error) => DependencyStatus {
            mode: DEPENDENCY_MODE_MISCONFIGURED.to_string(),
            status: DependencyHealth::Misconfigured,
            message: Some(error),
        },
    }
}
