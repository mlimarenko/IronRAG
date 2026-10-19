//! Bounded, global vector-lifecycle inventory sampler.
//!
//! The scheduler owns the cadence: at most once per
//! [`VECTOR_LIFECYCLE_INVENTORY_INTERVAL`] it takes one global read-only
//! inventory snapshot and emits the `inventory.*` and `backlog` series. It does
//! not walk libraries per tick and it adds no new `maintenance_job_run` row —
//! the durable lease remains the only source of GC scheduling, and this sampler
//! merely reads it.
//!
//! A probe failure is logged with the contract's bounded `error.code`; a failed
//! observation never advances inventory last-success, so a persistent failure
//! keeps the freshness alert honest instead of faking progress.

use std::time::Duration;

use tokio::sync::broadcast;
use tracing::warn;

use crate::{
    app::state::AppState,
    services::maintenance::{
        vector_lifecycle_inventory::sample_global_inventory,
        vector_lifecycle_metrics::{GcErrorCode, VectorLifecycleInstruments},
    },
    shared::cancellation::AbortOnDrop,
};

/// Minimum cadence between two inventory samples, per worker. The interval is
/// deliberately independent of the tick: the probe is an aggregate over shared
/// vector shards, so it must not run at tick frequency.
pub(super) const VECTOR_LIFECYCLE_INVENTORY_INTERVAL: Duration = Duration::from_mins(30);

const INVENTORY_WINDOW_BUDGET: Duration = Duration::from_secs(120);

/// One supervised, non-overlapping sampler; inventory never stalls maintenance
/// dispatch and is cancelled when the owning scheduler exits.
pub(super) fn spawn(state: AppState, mut shutdown: broadcast::Receiver<()>) -> AbortOnDrop<()> {
    AbortOnDrop::new(tokio::spawn(async move {
        let mut cadence = tokio::time::interval(VECTOR_LIFECYCLE_INVENTORY_INTERVAL);
        cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.recv() => return,
                _ = cadence.tick() => {}
            }
            tokio::select! {
                _ = shutdown.recv() => return,
                result = tokio::time::timeout(INVENTORY_WINDOW_BUDGET, sample_and_publish(&state)) => {
                    if result.is_err() {
                        warn!(error_code = GcErrorCode::Postgres.label(), "vector inventory window exhausted its time budget");
                    }
                }
            }
        }
    }))
}

/// Take one global inventory snapshot and emit it.
pub(super) async fn sample_and_publish(state: &AppState) {
    let pool = &state.persistence.postgres;
    match sample_global_inventory(pool).await {
        Ok(snapshot) => VectorLifecycleInstruments::global().record_inventory_snapshot(&snapshot),
        Err(error_code) => {
            warn!(
                error_code = error_code.label(),
                "vector inventory probe failed; success timestamp unchanged"
            );
        }
    }
}
