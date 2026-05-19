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
    domains::deployment::{DependencyKind, DependencyMode, ServiceRole, StartupAuthorityMode},
    infra::persistence::{
        canonical_baseline_present, validate_arango_bootstrap_state,
        validate_postgres_migration_state,
    },
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
    pub arangodb: DependencyStatus,
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
        let role = state
            .settings
            .service_role_kind()
            .map(ServiceRole::as_str)
            .unwrap_or(state.settings.service_role.as_str())
            .to_string();

        let postgres_status = dependency_status(
            state.settings.dependency_mode(DependencyKind::Postgres),
            sqlx::query("select 1")
                .fetch_one(&state.persistence.heartbeat_postgres)
                .await
                .map(|row| row.get::<i32, _>(0) == 1)
                .unwrap_or(false),
            "postgres unreachable".to_string(),
        );

        let redis_ok = match state.persistence.redis.get_multiplexed_async_connection().await {
            Ok(mut conn) => redis::cmd("PING")
                .query_async::<String>(&mut conn)
                .await
                .map(|value| value == "PONG")
                .unwrap_or(false),
            Err(_) => false,
        };
        let redis_status = dependency_status(
            state.settings.dependency_mode(DependencyKind::Redis),
            redis_ok,
            "redis unreachable".to_string(),
        );

        let arangodb_status = dependency_status(
            state.settings.dependency_mode(DependencyKind::ArangoDb),
            state.arango_client.ping().await.is_ok(),
            "arangodb unreachable".to_string(),
        );

        let storage_probe = state.content_storage.probe().await;
        let storage = StorageStatus {
            provider: state.content_storage.diagnostics().provider.as_str().to_string(),
            status: match storage_probe.status {
                ContentStorageProbeStatus::Ok => StorageHealth::Ok,
                ContentStorageProbeStatus::Down => StorageHealth::Down,
                ContentStorageProbeStatus::Unsupported => StorageHealth::Unsupported,
                ContentStorageProbeStatus::Misconfigured => StorageHealth::Misconfigured,
            },
            topology: state.content_storage.diagnostics().topology.as_str().to_string(),
            bucket: state.content_storage.diagnostics().bucket.clone(),
            root_path: state
                .content_storage
                .diagnostics()
                .root_path
                .as_ref()
                .map(|path| path.display().to_string()),
            endpoint: state.content_storage.diagnostics().endpoint.clone(),
            message: storage_probe.message,
        };

        let topology = if storage.status == StorageHealth::Unsupported {
            TopologyStatus {
                status: TopologySupport::NotSupported,
                message: Some(
                    "deployment topology is incompatible with the configured content storage provider"
                        .to_string(),
                ),
            }
        } else {
            TopologyStatus { status: TopologySupport::Supported, message: None }
        };

        let startup_authority = self.startup_authority_status(state).await;
        let worker_snapshot = state.worker_runtime.snapshot().await;

        // Log individual dependency health checks
        for (name, dep_status) in [
            ("postgres", &postgres_status),
            ("redis", &redis_status),
            ("arangodb", &arangodb_status),
        ] {
            tracing::debug!(stage = "readiness", dependency = %name, status = ?dep_status.status, "health check completed");
            if !matches!(dep_status.status, DependencyHealth::Ok) {
                tracing::warn!(stage = "readiness", dependency = %name, status = ?dep_status.status, "dependency degraded");
            }
        }

        let all_dependencies_ok =
            [postgres_status.status, redis_status.status, arangodb_status.status]
                .into_iter()
                .all(|status| matches!(status, DependencyHealth::Ok));
        let storage_ok = storage.status == StorageHealth::Ok;
        let topology_ok = topology.status == TopologySupport::Supported;
        let startup_ok = matches!(
            startup_authority.state,
            StartupAuthorityState::Succeeded | StartupAuthorityState::NotRequired
        );
        let worker_ok = if state.settings.runs_ingestion_workers() {
            worker_snapshot.status != WORKER_STATUS_ERROR
        } else {
            true
        };
        let all_ok = all_dependencies_ok && storage_ok && topology_ok && startup_ok && worker_ok;

        let message = if !all_dependencies_ok {
            Some("one or more dependencies are unavailable".to_string())
        } else if !storage_ok {
            Some("content storage provider is not ready".to_string())
        } else if !topology_ok {
            Some("deployment topology is unsupported for the selected storage provider".to_string())
        } else if !startup_ok {
            startup_authority.message.clone()
        } else if !worker_ok {
            worker_snapshot
                .message
                .clone()
                .or_else(|| Some("worker runtime is degraded".to_string()))
        } else {
            None
        };

        let overall_status = if all_ok {
            OverallReadiness::Ready
        } else if topology_ok {
            OverallReadiness::Degraded
        } else {
            OverallReadiness::Blocked
        };
        tracing::info!(stage = "readiness", overall = ?overall_status, "readiness probe completed");

        (
            all_ok,
            DeploymentReadinessSnapshot {
                status: overall_status,
                role,
                startup_authority,
                dependencies: DependencyStatusSet {
                    postgres: postgres_status,
                    redis: redis_status,
                    arangodb: arangodb_status,
                },
                storage,
                topology,
                message,
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
        let cached = self.startup_authority_cache.read().await;
        let cached = cached.as_ref()?;
        let ttl = startup_authority_status_cache_ttl(cached.status.state);
        if cached.checked_at.elapsed() <= ttl { Some(cached.status.clone()) } else { None }
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
        match validate_arango_bootstrap_state(&state.arango_client, &state.settings).await {
            Ok(()) => StartupAuthorityStatus {
                mode: mode.as_str().to_string(),
                state: StartupAuthorityState::Succeeded,
                message: None,
            },
            Err(error) => StartupAuthorityStatus {
                mode: mode.as_str().to_string(),
                state: StartupAuthorityState::Pending,
                message: Some(error.to_string()),
            },
        }
    }
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
