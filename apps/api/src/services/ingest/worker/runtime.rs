use std::{sync::Arc, time::Instant};

use tokio::{sync::broadcast, time};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{app::state::AppState, infra::repositories::ingest_repository};

use super::{
    CANONICAL_LEASE_RECOVERY_INTERVAL, CANONICAL_STALE_LEASE_SECONDS, WORKER_POLL_INTERVAL,
    execute_canonical_ingest_job, fail_canonical_ingest_job,
};

pub(super) async fn run_ingestion_worker_pool(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
) {
    let worker_concurrency = state.settings.ingestion_worker_concurrency.max(1);

    let mut handles = Vec::new();

    state.worker_runtime.mark_idle().await;
    info!(worker_concurrency, "starting ingestion worker pool on the canonical queue only");

    handles.push(tokio::spawn(run_canonical_lease_recovery_loop(
        state.clone(),
        shutdown.resubscribe(),
    )));

    for worker_index in 0..worker_concurrency {
        let worker_id = canonical_worker_id(&state.settings.service_name, worker_index);
        handles.push(tokio::spawn(run_canonical_ingest_worker_loop(
            state.clone(),
            shutdown.resubscribe(),
            worker_id,
        )));
    }

    if handles.is_empty() {
        let _ = shutdown.recv().await;
        return;
    }

    for handle in handles {
        if let Err(error) = handle.await {
            state
                .worker_runtime
                .mark_error(format!("ingestion worker task crashed: {error}"))
                .await;
            error!(?error, "ingestion worker task crashed");
        }
    }
}

fn canonical_worker_id(service_name: &str, worker_index: usize) -> String {
    format!("{service_name}:canonical:{worker_index}:{}", Uuid::now_v7())
}

async fn run_canonical_ingest_worker_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
    worker_id: String,
) {
    info!(%worker_id, "starting canonical ingest worker loop");

    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!(%worker_id, "stopping canonical ingest worker loop");
                break;
            }
            _ = time::sleep(WORKER_POLL_INTERVAL) => {
                state.worker_runtime.touch().await;
                match ingest_repository::claim_next_queued_ingest_job(
                    &state.persistence.postgres,
                    state.settings.ingestion_max_jobs_per_library as i64,
                ).await {
                    Ok(Some(job)) => {
                        let job_id = job.id;
                        let started_at = Instant::now();
                        state
                            .worker_runtime
                            .mark_active(format!("processing canonical ingest job {job_id}"))
                            .await;
                        info!(
                            %worker_id,
                            %job_id,
                            job_kind = %job.job_kind,
                            library_id = %job.library_id,
                            "claimed canonical ingest job",
                        );
                        if let Err(error) = execute_canonical_ingest_job(
                            state.clone(), &worker_id, job,
                        ).await {
                            state
                                .worker_runtime
                                .mark_error(format!("canonical ingest job {job_id} failed: {error}"))
                                .await;
                            let elapsed_ms = started_at.elapsed().as_millis();
                            error!(
                                %worker_id,
                                %job_id,
                                elapsed_ms,
                                ?error,
                                "canonical ingest job failed",
                            );
                            fail_canonical_ingest_job(&state, job_id, &worker_id, &error).await;
                        } else {
                            state.worker_runtime.mark_idle().await;
                        }
                    }
                    Ok(None) => {
                        state.worker_runtime.mark_idle().await;
                    }
                    Err(error) => {
                        state
                            .worker_runtime
                            .mark_error(format!("failed to claim canonical ingest job: {error}"))
                            .await;
                        warn!(%worker_id, ?error, "failed to claim canonical ingest job");
                    }
                }
            }
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
