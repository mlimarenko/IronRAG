use std::{sync::Arc, time::Instant};

use tokio::{sync::broadcast, task::JoinSet, time};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{app::state::AppState, infra::repositories::ingest_repository};

use super::{
    CANONICAL_LEASE_RECOVERY_INTERVAL, CANONICAL_STALE_LEASE_SECONDS,
    CANONICAL_STARTUP_LEASE_RECOVERY_SECONDS, WORKER_POLL_INTERVAL, execute_canonical_ingest_job,
    fail_canonical_ingest_job,
};

pub(super) async fn run_ingestion_worker_pool(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
) {
    let global_limit = state.settings.ingestion_max_parallel_jobs_global.max(1);
    let workspace_limit = state.settings.ingestion_max_parallel_jobs_per_workspace.max(1);
    let library_limit = state.settings.ingestion_max_parallel_jobs_per_library.max(1);
    let memory_soft_limit_mib = crate::shared::telemetry::resolve_memory_soft_limit_mib(
        state.settings.ingestion_memory_soft_limit_mib,
    );
    let memory_soft_limit_source = if state.settings.ingestion_memory_soft_limit_mib > 0 {
        "config"
    } else if memory_soft_limit_mib > 0 {
        "auto:cgroup"
    } else {
        "disabled"
    };
    let mut next_worker_index = 0usize;
    let mut active_jobs = JoinSet::new();

    state.worker_runtime.mark_idle().await;
    info!(
        global_limit,
        workspace_limit,
        library_limit,
        memory_soft_limit_mib,
        memory_soft_limit_source,
        "starting canonical ingestion dispatcher",
    );

    // Startup sweep: reclaim any `leased` rows orphaned by a previous process
    // that crashed or was restarted before it could finalize. This closes the
    // ~30s window where the periodic recovery loop would otherwise sit idle
    // before its first tick — without it, documents stay visibly stuck after
    // every backend or worker restart until the steady-state reaper catches
    // up.
    reclaim_orphaned_leases_on_startup(&state).await;

    let lease_recovery_handle =
        tokio::spawn(run_canonical_lease_recovery_loop(state.clone(), shutdown.resubscribe()));

    loop {
        fill_available_job_slots(
            state.clone(),
            &mut active_jobs,
            &mut next_worker_index,
            global_limit,
            workspace_limit,
            library_limit,
            memory_soft_limit_mib,
        )
        .await;
        sync_worker_runtime_snapshot(&state, active_jobs.len()).await;

        tokio::select! {
            _ = shutdown.recv() => {
                info!("stopping canonical ingestion dispatcher");
                break;
            }
            maybe_result = active_jobs.join_next(), if !active_jobs.is_empty() => {
                if let Some(result) = maybe_result {
                    handle_job_join_result(&state, result).await;
                }
            }
            _ = time::sleep(WORKER_POLL_INTERVAL) => {
                state.worker_runtime.touch().await;
            }
        }
    }

    while let Some(result) = active_jobs.join_next().await {
        handle_job_join_result(&state, result).await;
    }

    state.worker_runtime.mark_idle().await;

    if let Err(error) = lease_recovery_handle.await {
        state
            .worker_runtime
            .mark_error(format!("ingestion lease recovery task crashed: {error}"))
            .await;
        error!(?error, "ingestion lease recovery task crashed");
    }
}

struct CanonicalJobOutcome {
    job_id: Uuid,
    worker_id: String,
    job_kind: String,
    library_id: Uuid,
    started_at: Instant,
    result: anyhow::Result<()>,
}

fn canonical_worker_id(service_name: &str, worker_index: usize) -> String {
    format!("{service_name}:canonical:{worker_index}:{}", Uuid::now_v7())
}

async fn fill_available_job_slots(
    state: Arc<AppState>,
    active_jobs: &mut JoinSet<CanonicalJobOutcome>,
    next_worker_index: &mut usize,
    global_limit: usize,
    workspace_limit: usize,
    library_limit: usize,
    memory_soft_limit_mib: u64,
) {
    while active_jobs.len() < global_limit {
        // Memory-aware backpressure. The static parallelism limits above are
        // the *ceiling* — actual concurrency also drops automatically when
        // the worker process RSS approaches the soft limit, so a burst of
        // heavy docs cannot stack past the cgroup. When one of the in-flight
        // jobs finishes and frees memory, the dispatcher resumes claiming.
        if memory_soft_limit_mib > 0 && !active_jobs.is_empty() {
            if let Some(rss_bytes) = crate::shared::telemetry::current_process_rss_bytes() {
                let rss_mib = rss_bytes / (1024 * 1024);
                if rss_mib >= memory_soft_limit_mib {
                    warn!(
                        rss_mib,
                        memory_soft_limit_mib,
                        active_jobs = active_jobs.len(),
                        "ingest dispatcher holding claims: worker RSS over soft limit",
                    );
                    break;
                }
            }
        }
        state.worker_runtime.touch().await;
        match ingest_repository::claim_next_queued_ingest_job(
            &state.persistence.postgres,
            library_limit as i64,
            workspace_limit as i64,
            global_limit as i64,
        )
        .await
        {
            Ok(Some(job)) => {
                let started_at = Instant::now();
                let job_id = job.id;
                let job_kind = job.job_kind.clone();
                let library_id = job.library_id;
                let worker_id =
                    canonical_worker_id(&state.settings.service_name, *next_worker_index);
                *next_worker_index = next_worker_index.saturating_add(1);
                info!(
                    %worker_id,
                    %job_id,
                    job_kind = %job_kind,
                    library_id = %library_id,
                    "claimed canonical ingest job",
                );
                active_jobs.spawn({
                    let state = state.clone();
                    let worker_id = worker_id.clone();
                    async move {
                        let result = execute_canonical_ingest_job(state, &worker_id, job).await;
                        CanonicalJobOutcome {
                            job_id,
                            worker_id,
                            job_kind,
                            library_id,
                            started_at,
                            result,
                        }
                    }
                });
            }
            Ok(None) => break,
            Err(error) => {
                state
                    .worker_runtime
                    .mark_error(format!("failed to claim canonical ingest job: {error}"))
                    .await;
                warn!(?error, "failed to claim canonical ingest job");
                break;
            }
        }
    }
}

async fn sync_worker_runtime_snapshot(state: &Arc<AppState>, active_job_count: usize) {
    if active_job_count == 0 {
        state.worker_runtime.mark_idle().await;
        return;
    }

    state
        .worker_runtime
        .mark_active(format!("processing {active_job_count} canonical ingest jobs"))
        .await;
}

async fn handle_job_join_result(
    state: &Arc<AppState>,
    result: Result<CanonicalJobOutcome, tokio::task::JoinError>,
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

async fn handle_job_outcome(state: &Arc<AppState>, outcome: CanonicalJobOutcome) {
    match outcome.result {
        Ok(()) => {
            state.worker_runtime.touch().await;
        }
        Err(error) => {
            state
                .worker_runtime
                .mark_error(format!("canonical ingest job {} failed: {error}", outcome.job_id))
                .await;
            let elapsed_ms = outcome.started_at.elapsed().as_millis();
            error!(
                worker_id = %outcome.worker_id,
                job_id = %outcome.job_id,
                job_kind = %outcome.job_kind,
                library_id = %outcome.library_id,
                elapsed_ms,
                ?error,
                "canonical ingest job failed",
            );
            fail_canonical_ingest_job(state, outcome.job_id, &outcome.worker_id, &error).await;
        }
    }
}

/// One-shot startup reclamation. Runs synchronously before the dispatcher
/// starts claiming new jobs so the first thing the new process does is flush
/// any orphaned `leased` rows from the crashed/restarted predecessor. Uses a
/// shorter threshold than the steady-state loop because at boot we know this
/// process holds zero active leases — two missed heartbeats is already enough
/// evidence that the old owner is gone.
async fn reclaim_orphaned_leases_on_startup(state: &Arc<AppState>) {
    let threshold = chrono::Duration::seconds(CANONICAL_STARTUP_LEASE_RECOVERY_SECONDS);
    match ingest_repository::recover_stale_canonical_leases(
        &state.persistence.postgres,
        threshold,
    )
    .await
    {
        Ok(0) => {
            info!("startup lease sweep: no orphaned canonical leases to reclaim");
        }
        Ok(recovered) => {
            warn!(
                recovered,
                threshold_seconds = CANONICAL_STARTUP_LEASE_RECOVERY_SECONDS,
                "startup lease sweep: reclaimed orphaned canonical ingest leases after worker pool boot"
            );
        }
        Err(error) => {
            error!(
                ?error,
                "startup lease sweep failed; dispatcher will proceed and rely on the periodic recovery loop"
            );
        }
    }
}

async fn run_canonical_lease_recovery_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
) {
    info!("starting canonical lease recovery loop");
    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!("stopping canonical lease recovery loop");
                break;
            }
            _ = time::sleep(CANONICAL_LEASE_RECOVERY_INTERVAL) => {
                let threshold = chrono::Duration::seconds(CANONICAL_STALE_LEASE_SECONDS);
                match ingest_repository::recover_stale_canonical_leases(
                    &state.persistence.postgres,
                    threshold,
                ).await {
                    Ok(0) => {}
                    Ok(recovered) => {
                        warn!(recovered, "recovered stale canonical ingest job leases");
                    }
                    Err(error) => {
                        warn!(?error, "failed to recover stale canonical leases");
                    }
                }
            }
        }
    }
}
