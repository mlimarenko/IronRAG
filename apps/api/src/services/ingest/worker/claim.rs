use std::{sync::Arc, time::Instant};

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{app::state::AppState, infra::repositories::ingest_repository};

use super::attempt::execute_ingest_job;

const PROCESS_CLAIM_MEMORY_RESERVED_MIB: u64 = 256;
const PROCESS_CLAIM_MEMORY_PER_JOB_MIB: u64 = 512;

pub(super) struct IngestJobOutcome {
    pub(super) job_id: Uuid,
    pub(super) worker_id: String,
    pub(super) job_kind: String,
    pub(super) library_id: Uuid,
    pub(super) started_at: Instant,
    pub(super) result: anyhow::Result<()>,
}

fn ingest_worker_id(service_name: &str, worker_index: usize) -> String {
    format!("{service_name}:{worker_index}:{}", Uuid::now_v7())
}

pub(super) fn memory_bound_process_claim_limit(memory_soft_limit_mib: u64) -> usize {
    if memory_soft_limit_mib == 0 {
        return usize::MAX;
    }
    let job_slots = memory_soft_limit_mib
        .saturating_sub(PROCESS_CLAIM_MEMORY_RESERVED_MIB)
        .checked_div(PROCESS_CLAIM_MEMORY_PER_JOB_MIB)
        .unwrap_or(0);
    usize::try_from(job_slots.max(1)).unwrap_or(usize::MAX)
}

/// The parallelism and memory ceilings one claim pass enforces, so the claim
/// loop signature carries one value instead of five positional limits.
pub(super) struct ClaimLimits {
    pub(super) process_claim_limit: usize,
    pub(super) global_limit: usize,
    pub(super) workspace_limit: usize,
    pub(super) library_limit: usize,
    pub(super) memory_soft_limit_mib: u64,
}

pub(super) async fn fill_available_job_slots(
    state: Arc<AppState>,
    active_jobs: &mut JoinSet<IngestJobOutcome>,
    next_worker_index: &mut usize,
    limits: ClaimLimits,
    shutdown_cancellation_token: &CancellationToken,
) {
    // SQL retains the deployment-wide cap; this loop also enforces pool and RSS ceilings.
    while active_jobs.len() < limits.process_claim_limit {
        if claims_exceed_memory_limit(active_jobs.len(), limits.memory_soft_limit_mib) {
            break;
        }
        state.worker_runtime.touch().await;
        let queue_lease_token = format!("queue-{}", Uuid::now_v7());
        let queue_lease_owner = ingest_worker_id(&state.settings.service_name, *next_worker_index);
        let Some(job) =
            claim_ingest_job(&state, &limits, &queue_lease_token, &queue_lease_owner).await
        else {
            break;
        };
        spawn_claimed_ingest_job(
            &state,
            active_jobs,
            next_worker_index,
            job,
            queue_lease_owner,
            shutdown_cancellation_token,
        );
    }
}

fn claims_exceed_memory_limit(active_job_count: usize, memory_soft_limit_mib: u64) -> bool {
    if memory_soft_limit_mib == 0 || active_job_count == 0 {
        return false;
    }
    let Some(rss_bytes) = crate::shared::telemetry::current_process_rss_bytes() else {
        return false;
    };
    let rss_mib = rss_bytes / (1024 * 1024);
    if rss_mib < memory_soft_limit_mib {
        return false;
    }
    warn!(
        rss_mib,
        memory_soft_limit_mib,
        active_jobs = active_job_count,
        "ingest dispatcher holding claims: worker RSS over soft limit"
    );
    true
}

async fn claim_ingest_job(
    state: &AppState,
    limits: &ClaimLimits,
    queue_lease_token: &str,
    queue_lease_owner: &str,
) -> Option<ingest_repository::IngestJobRow> {
    match ingest_repository::claim_next_queued_ingest_job(
        &state.persistence.postgres,
        queue_lease_token,
        queue_lease_owner,
        limits.library_limit as i64,
        limits.workspace_limit as i64,
        limits.global_limit as i64,
    )
    .await
    {
        Ok(job) => job,
        Err(error) => {
            state.worker_runtime.mark_error(format!("failed to claim ingest job: {error}")).await;
            warn!(?error, "failed to claim ingest job");
            None
        }
    }
}

fn spawn_claimed_ingest_job(
    state: &Arc<AppState>,
    active_jobs: &mut JoinSet<IngestJobOutcome>,
    next_worker_index: &mut usize,
    job: ingest_repository::IngestJobRow,
    worker_id: String,
    shutdown_cancellation_token: &CancellationToken,
) {
    let started_at = Instant::now();
    let job_id = job.id;
    let job_kind = job.job_kind.clone();
    let library_id = job.library_id;
    *next_worker_index = next_worker_index.saturating_add(1);
    info!(%worker_id, %job_id, job_kind = %job_kind, library_id = %library_id, "claimed ingest job");
    active_jobs.spawn({
        let state = state.clone();
        let worker_id = worker_id.clone();
        let job_cancellation_token = shutdown_cancellation_token.child_token();
        async move {
            // Task-local provider lanes do not cross spawn: establish the ingest lane here.
            let result = Box::pin(crate::integrations::provider_budget::with_lane(
                crate::integrations::provider_budget::ProviderLane::Ingest,
                execute_ingest_job(state, &worker_id, job, job_cancellation_token),
            ))
            .await;
            IngestJobOutcome { job_id, worker_id, job_kind, library_id, started_at, result }
        }
    });
}
