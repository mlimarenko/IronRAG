//! Background scheduler for recurring maintenance sweepers.
//!
//! The scheduler lives in the worker role only (see
//! `Settings::runs_maintenance_scheduler`). Each tick:
//!
//! 1. Reaps `leased` rows whose heartbeat has gone stale.
//! 2. Bootstraps any missing `maintenance_job_run` rows for known
//!    (class, scope) pairs, so a freshly-added library shows up in
//!    the next tick without operator intervention.
//! 3. For every class enabled in this build, atomically picks the
//!    oldest pending row whose `next_due_at` has passed and runs the
//!    matching sweeper under a tokio task that refreshes the lease
//!    heartbeat every 30 s.
//!
//! Rolling-budget cadence: each tick takes **at most one** (class,
//! scope) pair per class. With the default 30 s tick interval and one
//! class enabled (`gc.stale-chunks`) the scheduler walks the full
//! library set in `library_count * 30 s` wall time. That keeps load
//! predictable instead of stampeding every library at the same wall
//! clock.
//!
//! `ActiveIngest` is treated as a soft retry signal — the (class,
//! scope) pair is re-queued with a short backoff and `attempts` is NOT
//! incremented, so a library with continuously-in-flight ingest never
//! drifts into dead-letter just because the sweeper could not get the
//! advisory lock.

use std::time::Duration;

use sqlx::PgPool;
use tokio::{sync::broadcast, task::JoinHandle, time::sleep};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::catalog_repository,
    services::maintenance::{
        gc::{self, GcStaleChunksError, GcStaleChunksOptions, LibraryGcReport},
        lease::{self, MaintenanceClass, MaintenanceJobRun, Scope},
    },
};

/// Default heartbeat refresh cadence while a sweeper is running.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Backoff used when the sweeper refuses to run because the library is
/// busy with ingest. Short enough that the next tick within a few
/// minutes can pick the work up once ingest quiesces.
pub const ACTIVE_INGEST_RETRY: Duration = Duration::from_secs(5 * 60);

/// Build a scheduler-task handle for the lifetime of the process.
/// Returns `None` when the role / kill-switch combination means the
/// scheduler must not run.
#[must_use]
pub fn spawn_maintenance_scheduler(
    state: AppState,
    mut shutdown: broadcast::Receiver<()>,
) -> Option<JoinHandle<()>> {
    if !state.settings.runs_maintenance_scheduler() {
        return None;
    }
    let config = SchedulerConfig::from_settings(&state.settings);
    let owner_node = node_owner_identity();
    info!(
        tick_interval_secs = config.tick_interval.as_secs(),
        class_interval_secs = config.class_interval.as_secs(),
        stale_lease_after_secs = config.stale_lease_after.as_secs(),
        owner_node = %owner_node,
        "maintenance scheduler starting",
    );
    Some(tokio::spawn(async move {
        // Bootstrap is best-effort — if it fails the next tick will retry.
        if let Err(error) = bootstrap_rows(&state).await {
            warn!(?error, "maintenance scheduler bootstrap failed; will retry on next tick");
        }
        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    info!("maintenance scheduler received shutdown signal; exiting");
                    return;
                }
                () = sleep(config.tick_interval) => {}
            }
            if let Err(error) = run_tick(&state, &config, &owner_node).await {
                warn!(?error, "maintenance scheduler tick failed; continuing");
            }
        }
    }))
}

/// Configuration captured at scheduler startup; immutable while running.
#[derive(Debug, Clone, Copy)]
struct SchedulerConfig {
    tick_interval: Duration,
    class_interval: Duration,
    stale_lease_after: Duration,
}

impl SchedulerConfig {
    fn from_settings(settings: &crate::app::config::Settings) -> Self {
        Self {
            tick_interval: Duration::from_secs(settings.maintenance_tick_interval_seconds.max(1)),
            class_interval: Duration::from_secs(
                settings.maintenance_class_interval_seconds.max(60),
            ),
            stale_lease_after: Duration::from_secs(
                settings.maintenance_stale_lease_seconds.max(60),
            ),
        }
    }
}

/// Stable identifier for the node running this scheduler. Uses the
/// `HOSTNAME` env so containers report a useful label out of the box.
fn node_owner_identity() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| format!("ironrag-worker-{}", Uuid::now_v7()))
}

async fn bootstrap_rows(state: &AppState) -> anyhow::Result<()> {
    let pool = &state.persistence.postgres;
    let libraries = catalog_repository::list_libraries(pool, None).await?;
    for library in &libraries {
        if let Err(error) = lease::ensure_row(
            pool,
            MaintenanceClass::GcStaleChunks,
            Scope::Library(library.id),
            None,
        )
        .await
        {
            warn!(
                library_id = %library.id,
                class = MaintenanceClass::GcStaleChunks.as_str(),
                ?error,
                "failed to ensure maintenance_job_run row at bootstrap",
            );
        }
    }
    Ok(())
}

async fn run_tick(
    state: &AppState,
    config: &SchedulerConfig,
    owner_node: &str,
) -> anyhow::Result<()> {
    let pool = &state.persistence.postgres;
    match lease::reap_stale_leases(pool, config.stale_lease_after).await {
        Ok(reaped) if reaped > 0 => {
            info!(reaped, "maintenance scheduler reaped stale leases");
        }
        Ok(_) => {}
        Err(error) => warn!(?error, "stale-lease reaper failed; continuing"),
    }
    // New libraries show up between bootstrap and this tick — ensure
    // their rows now so they are eligible for acquisition immediately.
    if let Err(error) = bootstrap_rows(state).await {
        warn!(?error, "scheduler tick bootstrap refresh failed");
    }
    process_class(state, config, owner_node, MaintenanceClass::GcStaleChunks).await;
    Ok(())
}

async fn process_class(
    state: &AppState,
    config: &SchedulerConfig,
    owner_node: &str,
    class: MaintenanceClass,
) {
    let pool = &state.persistence.postgres;
    let run = match lease::acquire_next_due(pool, class, owner_node).await {
        Ok(Some(run)) => run,
        Ok(None) => return,
        Err(error) => {
            warn!(class = class.as_str(), ?error, "lease acquire failed");
            return;
        }
    };
    let heartbeat_handle = spawn_heartbeat(pool.clone(), run.id);
    let dispatch = dispatch(state, class, &run).await;
    heartbeat_handle.abort();
    handle_outcome(state, config, class, run, dispatch).await;
}

fn spawn_heartbeat(pool: PgPool, run_id: Uuid) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            sleep(HEARTBEAT_INTERVAL).await;
            if lease::heartbeat(&pool, run_id).await.is_err() {
                // Heartbeat failure is non-fatal; the reaper will return
                // the lease to pending if the sweeper actually crashes.
                return;
            }
        }
    })
}

/// One concrete sweep outcome for the scheduler. Either the canonical
/// success counts (`rows_removed`, `bytes_reclaimed`), a soft retry
/// signal that should not consume an attempt, or a real failure.
enum DispatchOutcome {
    Ok { rows_removed: i64, bytes_reclaimed: i64 },
    ActiveIngest,
    Failed { error_code: &'static str, error_text: String },
}

async fn dispatch(
    state: &AppState,
    class: MaintenanceClass,
    run: &MaintenanceJobRun,
) -> DispatchOutcome {
    match (class, run.scope()) {
        (MaintenanceClass::GcStaleChunks, Scope::Library(library_id)) => {
            dispatch_gc_stale_chunks(state, library_id).await
        }
        (class, scope) => DispatchOutcome::Failed {
            error_code: "unsupported_class_scope",
            error_text: format!(
                "scheduler dispatch has no implementation for class={} scope={:?}",
                class.as_str(),
                scope,
            ),
        },
    }
}

async fn dispatch_gc_stale_chunks(state: &AppState, library_id: Uuid) -> DispatchOutcome {
    let pool = &state.persistence.postgres;
    // Resolve the workspace_id from the canonical catalog row. A
    // library that vanished between scheduler bootstrap and dispatch
    // (e.g. operator deleted it) shows up here as missing; that is a
    // benign "no work" signal, not a failure.
    let libraries = match catalog_repository::list_libraries(pool, None).await {
        Ok(libraries) => libraries,
        Err(error) => {
            return DispatchOutcome::Failed {
                error_code: "catalog_lookup",
                error_text: error.to_string(),
            };
        }
    };
    let Some(library) = libraries.into_iter().find(|library| library.id == library_id) else {
        return DispatchOutcome::Ok { rows_removed: 0, bytes_reclaimed: 0 };
    };

    match gc::run_for_library(
        state,
        library.workspace_id,
        library.id,
        GcStaleChunksOptions::default(),
    )
    .await
    {
        Ok(report) => DispatchOutcome::Ok { rows_removed: total_rows(&report), bytes_reclaimed: 0 },
        Err(GcStaleChunksError::ActiveIngest { active_jobs, .. }) => {
            info!(
                library_id = %library_id,
                active_jobs,
                "gc.stale-chunks deferred: library has active ingest",
            );
            DispatchOutcome::ActiveIngest
        }
        Err(error) => {
            DispatchOutcome::Failed { error_code: error.code(), error_text: error.to_string() }
        }
    }
}

fn total_rows(report: &LibraryGcReport) -> i64 {
    report.total_rows_removed()
}

async fn handle_outcome(
    state: &AppState,
    config: &SchedulerConfig,
    class: MaintenanceClass,
    run: MaintenanceJobRun,
    outcome: DispatchOutcome,
) {
    let pool = &state.persistence.postgres;
    match outcome {
        DispatchOutcome::Ok { rows_removed, bytes_reclaimed } => {
            if let Err(error) =
                lease::complete(pool, run.id, rows_removed, bytes_reclaimed, config.class_interval)
                    .await
            {
                warn!(class = class.as_str(), run_id = %run.id, ?error, "lease complete failed");
            } else if rows_removed > 0 {
                info!(
                    class = class.as_str(),
                    scope = ?run.scope(),
                    rows_removed,
                    bytes_reclaimed,
                    "maintenance sweep completed",
                );
            }
        }
        DispatchOutcome::ActiveIngest => {
            // Re-queue WITHOUT touching attempts — this is not a real
            // failure. `complete` resets attempts and pushes
            // `next_due_at` out by `class_interval`; pass a short
            // backoff so the busy library is checked again soon.
            if let Err(error) = lease::complete(pool, run.id, 0, 0, ACTIVE_INGEST_RETRY).await {
                warn!(class = class.as_str(), run_id = %run.id, ?error, "active-ingest re-queue failed");
            }
        }
        DispatchOutcome::Failed { error_code, error_text } => {
            match lease::fail(pool, run.id, error_code, &error_text, lease::DEFAULT_MAX_ATTEMPTS)
                .await
            {
                Ok(lease::FailOutcome::Retry { attempts, backoff }) => warn!(
                    class = class.as_str(),
                    scope = ?run.scope(),
                    attempts,
                    backoff_secs = backoff.as_secs(),
                    error_code,
                    error_text,
                    "maintenance sweep failed; re-queued",
                ),
                Ok(lease::FailOutcome::DeadLetter { attempts }) => warn!(
                    class = class.as_str(),
                    scope = ?run.scope(),
                    attempts,
                    error_code,
                    error_text,
                    "maintenance sweep dead-lettered after exhausting attempts",
                ),
                Err(error) => warn!(
                    class = class.as_str(),
                    run_id = %run.id,
                    ?error,
                    "lease fail update failed",
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> crate::app::config::Settings {
        crate::app::config::Settings::from_env().expect("settings defaults should load")
    }

    #[test]
    fn config_clamps_durations_to_safe_minimums() {
        let mut settings = test_settings();
        settings.maintenance_tick_interval_seconds = 0;
        settings.maintenance_class_interval_seconds = 0;
        settings.maintenance_stale_lease_seconds = 0;
        let config = SchedulerConfig::from_settings(&settings);
        assert!(config.tick_interval >= Duration::from_secs(1));
        assert!(config.class_interval >= Duration::from_secs(60));
        assert!(config.stale_lease_after >= Duration::from_secs(60));
    }

    #[test]
    fn config_passes_through_when_above_minimum() {
        let mut settings = test_settings();
        settings.maintenance_tick_interval_seconds = 45;
        settings.maintenance_class_interval_seconds = 7200;
        settings.maintenance_stale_lease_seconds = 600;
        let config = SchedulerConfig::from_settings(&settings);
        assert_eq!(config.tick_interval, Duration::from_secs(45));
        assert_eq!(config.class_interval, Duration::from_secs(7200));
        assert_eq!(config.stale_lease_after, Duration::from_secs(600));
    }
}
