use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::infra::repositories::ingest_repository;

pub(super) struct AttemptHeartbeatGuard {
    running: Arc<AtomicBool>,
}

impl AttemptHeartbeatGuard {
    pub(super) const fn new(running: Arc<AtomicBool>) -> Self {
        Self { running }
    }
}

impl Drop for AttemptHeartbeatGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// The owned cancellation handles an attempt heartbeat thread needs to observe
/// and signal job cancellation, so the spawn signature carries one value instead
/// of four positional handles.
pub(super) struct AttemptHeartbeatSignal {
    pub(super) cancellation_token: CancellationToken,
    pub(super) user_cancel_requested: Arc<AtomicBool>,
    pub(super) operator_pause_requested: Arc<AtomicBool>,
    pub(super) lease_lost_requested: Arc<AtomicBool>,
}

pub(super) fn spawn_attempt_heartbeat_observer(
    heartbeat_pg: sqlx::PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
    heartbeat_interval: Duration,
    heartbeat_running: Arc<AtomicBool>,
    signal: AttemptHeartbeatSignal,
) {
    let AttemptHeartbeatSignal {
        cancellation_token,
        user_cancel_requested,
        operator_pause_requested,
        lease_lost_requested,
    } = signal;
    let thread_name = format!("ironrag-heartbeat-{}", attempt_id.simple());
    let spawn_failure_cancellation = cancellation_token.clone();
    let spawn_failure_lease_lost = Arc::clone(&lease_lost_requested);
    let spawn_result = std::thread::Builder::new().name(thread_name).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(%error, %attempt_id, "attempt heartbeat observer failed to start runtime");
                lease_lost_requested.store(true, Ordering::Relaxed);
                cancellation_token.cancel();
                return;
            }
        };

        runtime.block_on(async move {
            // Runs outside the main Tokio runtime so CPU-heavy graph reconcile
            // cannot starve heartbeats and trigger a false stale-lease requeue.
            while heartbeat_running.load(Ordering::Relaxed) {
                tokio::time::sleep(heartbeat_interval).await;
                if !heartbeat_running.load(Ordering::Relaxed) {
                    break;
                }

                match ingest_repository::touch_attempt_heartbeat_and_load_job_state(
                    &heartbeat_pg,
                    attempt_id,
                    None,
                )
                .await
                {
                    Ok(Some(queue_state)) if queue_state == "leased" => {}
                    Ok(Some(queue_state)) if queue_state == "canceled" => {
                        info!(
                            %job_id,
                            %attempt_id,
                            "cancellation observed on heartbeat tick, signalling pipeline abort"
                        );
                        user_cancel_requested.store(true, Ordering::Relaxed);
                        cancellation_token.cancel();
                    }
                    Ok(Some(queue_state)) if queue_state == "paused" => {
                        info!(
                            %job_id,
                            %attempt_id,
                            "operator pause observed on heartbeat tick, signalling pipeline pause"
                        );
                        operator_pause_requested.store(true, Ordering::Relaxed);
                        cancellation_token.cancel();
                    }
                    Ok(Some(queue_state)) => {
                        lease_lost_requested.store(true, Ordering::Relaxed);
                        cancellation_token.cancel();
                        warn!(
                            %job_id,
                            %attempt_id,
                            queue_state = %queue_state,
                            "attempt heartbeat observed job lease moved away; cancelling stale worker pipeline"
                        );
                        break;
                    }
                    Ok(None) => {
                        lease_lost_requested.store(true, Ordering::Relaxed);
                        cancellation_token.cancel();
                        warn!(
                            %job_id,
                            %attempt_id,
                            "attempt heartbeat observed lost lease; cancelling stale worker pipeline"
                        );
                        break;
                    }
                    Err(error) => {
                        warn!(
                            ?error,
                            %attempt_id,
                            "failed to touch attempt heartbeat and poll queue state"
                        );
                    }
                }
            }
        });
    });
    if let Err(error) = spawn_result {
        tracing::error!(%error, %attempt_id, "failed to spawn attempt heartbeat observer");
        spawn_failure_lease_lost.store(true, Ordering::Relaxed);
        spawn_failure_cancellation.cancel();
    }
}
