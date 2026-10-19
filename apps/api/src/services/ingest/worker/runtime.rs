use std::{sync::Arc, time::Duration};

use tokio::{
    sync::broadcast,
    task::{JoinHandle, JoinSet},
    time,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::{app::state::AppState, infra::persistence};

use super::{
    claim::{
        ClaimLimits, IngestJobOutcome, fill_available_job_slots, memory_bound_process_claim_limit,
    },
    failure::fail_ingest_job,
    maintenance::{
        reclaim_orphaned_leases_on_startup, run_graph_backfill_loop, run_graph_maintenance_loop,
        run_ingest_lease_recovery_loop, spawn_external_stale_lease_reaper,
    },
};

/// How often each worker polls the ingest queue for new jobs.
const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[must_use]
pub fn spawn_ingestion_worker(
    state: AppState,
    shutdown: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_ingestion_worker_pool(Arc::new(state), shutdown).await;
    })
}

async fn run_ingestion_worker_pool(state: Arc<AppState>, mut shutdown: broadcast::Receiver<()>) {
    let capacity = ingestion_dispatcher_capacity(&state);
    let shutdown_cancellation_token = CancellationToken::new();
    let mut active_jobs = JoinSet::new();
    state.worker_runtime.mark_idle().await;
    log_dispatcher_start(&capacity);

    // Startup sweep: reclaim any `leased` rows orphaned by a previous process
    // that crashed or was restarted before it could finalize. This closes the
    // ~30s window where the periodic recovery loop would otherwise sit idle
    // before its first tick — without it, documents stay visibly stuck after
    // every backend or worker restart until the steady-state reaper catches
    // up.
    reclaim_orphaned_leases_on_startup(&state).await;

    let lease_recovery_handle =
        tokio::spawn(run_ingest_lease_recovery_loop(state.clone(), shutdown.resubscribe()));

    // Periodic self-healing pass for documents stuck with `ready` extraction
    // but no graph node — see services/graph/backfill.rs for the background.
    // The per-job hook in pipeline.rs only fires when ingest traffic is flowing;
    // on an idle queue this loop makes sure the gap still closes on its own.
    let graph_backfill_handle =
        tokio::spawn(run_graph_backfill_loop(state.clone(), shutdown.resubscribe()));
    let graph_maintenance_handle =
        tokio::spawn(run_graph_maintenance_loop(state.clone(), shutdown.resubscribe()));

    // Out-of-band stale-lease reaper. Unlike the in-tokio
    // recovery loop above, this runs on a dedicated OS thread with its
    // own mini tokio runtime and its own pg pool handle. Whenever the
    // main runtime becomes saturated by CPU-bound ingest work (graph
    // reconcile, merge loop, large JSON parse) the in-tokio reaper
    // starves together with the heartbeat task — the whole point of
    // this sentinel is to still fire reliably in that failure mode and
    // unstick the system by releasing orphaned leases. It also writes
    // to the heartbeat-dedicated postgres pool so it never contends
    // with the main connection pool that the stuck worker is holding.
    spawn_external_stale_lease_reaper(state.persistence.heartbeat_postgres.clone());

    dispatch_until_shutdown(
        &state,
        &mut shutdown,
        &shutdown_cancellation_token,
        &mut active_jobs,
        capacity.limits,
    )
    .await;

    while let Some(result) = active_jobs.join_next().await {
        handle_job_join_result(&state, result).await;
    }

    state.worker_runtime.mark_idle().await;

    join_maintenance_tasks(
        &state,
        lease_recovery_handle,
        graph_backfill_handle,
        graph_maintenance_handle,
    )
    .await;
}

struct IngestDispatcherCapacity {
    limits: ClaimLimits,
    process_db_limit: usize,
    configured_process_claim_limit: usize,
    memory_bound_process_claim_limit: usize,
    memory_soft_limit_source: &'static str,
}

fn ingestion_dispatcher_capacity(state: &AppState) -> IngestDispatcherCapacity {
    let global_limit = state.settings.ingestion_max_parallel_jobs_global.max(1);
    let process_db_limit = persistence::worker_process_db_job_limit(&state.settings);
    let memory_soft_limit_mib = crate::shared::telemetry::resolve_memory_soft_limit_mib(
        state.settings.ingestion_memory_soft_limit_mib,
    );
    let configured_process_claim_limit = global_limit.min(process_db_limit).max(1);
    let memory_bound_process_claim_limit = memory_bound_process_claim_limit(memory_soft_limit_mib);
    let limits = ClaimLimits {
        process_claim_limit: configured_process_claim_limit
            .min(memory_bound_process_claim_limit)
            .max(1),
        global_limit,
        workspace_limit: state.settings.ingestion_max_parallel_jobs_per_workspace.max(1),
        library_limit: state.settings.ingestion_max_parallel_jobs_per_library.max(1),
        memory_soft_limit_mib,
    };
    IngestDispatcherCapacity {
        limits,
        process_db_limit,
        configured_process_claim_limit,
        memory_bound_process_claim_limit,
        memory_soft_limit_source: memory_soft_limit_source(
            state.settings.ingestion_memory_soft_limit_mib,
            memory_soft_limit_mib,
        ),
    }
}

fn log_dispatcher_start(capacity: &IngestDispatcherCapacity) {
    let ClaimLimits {
        global_limit,
        process_claim_limit,
        workspace_limit,
        library_limit,
        memory_soft_limit_mib,
    } = capacity.limits;
    let IngestDispatcherCapacity {
        process_db_limit,
        configured_process_claim_limit,
        memory_bound_process_claim_limit,
        memory_soft_limit_source,
        ..
    } = *capacity;
    info!(
        global_limit,
        process_claim_limit,
        configured_process_claim_limit,
        memory_bound_process_claim_limit,
        process_db_limit,
        workspace_limit,
        library_limit,
        memory_soft_limit_mib,
        memory_soft_limit_source,
        "starting ingestion dispatcher"
    );
}

fn memory_soft_limit_source(configured_mib: u64, resolved_mib: u64) -> &'static str {
    if configured_mib > 0 {
        "config"
    } else if resolved_mib > 0 {
        "auto:cgroup"
    } else {
        "disabled"
    }
}

async fn dispatch_until_shutdown(
    state: &Arc<AppState>,
    shutdown: &mut broadcast::Receiver<()>,
    shutdown_cancellation_token: &CancellationToken,
    active_jobs: &mut JoinSet<IngestJobOutcome>,
    limits: ClaimLimits,
) {
    let mut next_worker_index = 0usize;
    loop {
        fill_available_job_slots(
            state.clone(),
            active_jobs,
            &mut next_worker_index,
            ClaimLimits {
                process_claim_limit: limits.process_claim_limit,
                global_limit: limits.global_limit,
                workspace_limit: limits.workspace_limit,
                library_limit: limits.library_limit,
                memory_soft_limit_mib: limits.memory_soft_limit_mib,
            },
            shutdown_cancellation_token,
        )
        .await;
        sync_worker_runtime_snapshot(state, active_jobs.len()).await;
        if !wait_for_dispatcher_event(state, shutdown, shutdown_cancellation_token, active_jobs)
            .await
        {
            break;
        }
    }
}

async fn wait_for_dispatcher_event(
    state: &Arc<AppState>,
    shutdown: &mut broadcast::Receiver<()>,
    shutdown_cancellation_token: &CancellationToken,
    active_jobs: &mut JoinSet<IngestJobOutcome>,
) -> bool {
    tokio::select! {
        _ = shutdown.recv() => {
            stop_ingest_dispatcher(shutdown_cancellation_token);
            false
        }
        maybe_result = active_jobs.join_next(), if !active_jobs.is_empty() => {
            if let Some(result) = maybe_result {
                handle_job_join_result(state, result).await;
            }
            true
        }
        () = time::sleep(WORKER_POLL_INTERVAL) => {
            state.worker_runtime.touch().await;
            true
        }
    }
}

fn stop_ingest_dispatcher(shutdown_cancellation_token: &CancellationToken) {
    shutdown_cancellation_token.cancel();
    info!("stopping ingestion dispatcher");
}

async fn join_maintenance_tasks(
    state: &AppState,
    lease_recovery_handle: JoinHandle<()>,
    graph_backfill_handle: JoinHandle<()>,
    graph_maintenance_handle: JoinHandle<()>,
) {
    join_lease_recovery_task(state, lease_recovery_handle).await;
    join_graph_backfill_task(graph_backfill_handle).await;
    join_graph_maintenance_task(graph_maintenance_handle).await;
}

async fn join_graph_backfill_task(graph_backfill_handle: JoinHandle<()>) {
    if let Err(error) = graph_backfill_handle.await {
        error!(?error, "graph backfill loop crashed");
    }
}

async fn join_graph_maintenance_task(graph_maintenance_handle: JoinHandle<()>) {
    if let Err(error) = graph_maintenance_handle.await {
        error!(?error, "graph maintenance loop crashed");
    }
}

async fn join_lease_recovery_task(state: &AppState, lease_recovery_handle: JoinHandle<()>) {
    if let Err(error) = lease_recovery_handle.await {
        state
            .worker_runtime
            .mark_error(format!("ingestion lease recovery task crashed: {error}"))
            .await;
        error!(?error, "ingestion lease recovery task crashed");
    }
}

async fn sync_worker_runtime_snapshot(state: &Arc<AppState>, active_job_count: usize) {
    if active_job_count == 0 {
        state.worker_runtime.mark_idle().await;
        return;
    }

    state.worker_runtime.mark_active(format!("processing {active_job_count} ingest jobs")).await;
}

async fn handle_job_join_result(
    state: &Arc<AppState>,
    result: Result<IngestJobOutcome, tokio::task::JoinError>,
) {
    match result {
        Ok(outcome) => handle_job_outcome(state, outcome).await,
        Err(error) => {
            state
                .worker_runtime
                .mark_error(format!("ingestion worker task crashed: {error}"))
                .await;
            error!(?error, "ingestion worker task crashed");
        }
    }
}

async fn handle_job_outcome(state: &Arc<AppState>, outcome: IngestJobOutcome) {
    match outcome.result {
        Ok(()) => {
            state.worker_runtime.touch().await;
        }
        Err(error) => {
            state
                .worker_runtime
                .mark_error(format!("ingest job {} failed: {error}", outcome.job_id))
                .await;
            let elapsed_ms = outcome.started_at.elapsed().as_millis();
            error!(
                worker_id = %outcome.worker_id,
                job_id = %outcome.job_id,
                job_kind = %outcome.job_kind,
                library_id = %outcome.library_id,
                elapsed_ms,
                ?error,
                "ingest job failed",
            );
            fail_ingest_job(state, outcome.job_id, &outcome.worker_id, &error).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory_soft_limit_source;

    #[test]
    fn memory_limit_source_keeps_config_precedence_over_auto_detection() {
        assert_eq!(memory_soft_limit_source(1024, 1024), "config");
        assert_eq!(memory_soft_limit_source(1024, 0), "config");
        assert_eq!(memory_soft_limit_source(0, 1024), "auto:cgroup");
        assert_eq!(memory_soft_limit_source(0, 0), "disabled");
    }
}
