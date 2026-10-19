//! Bounded sweep that closes `runtime_execution` / `runtime_graph_extraction`
//! rows orphaned by an ingest attempt that terminated before its runtime rows
//! did.
//!
//! This is the operator- and scheduler-facing wrapper over
//! [`runtime_graph_repository::reconcile_orphan_runtime_rows_once`]; it resolves
//! the relative `stale_after` window into an absolute cutoff so callers never
//! read the wall clock themselves. The sweep state guards make it idempotent,
//! so a repeated run over the same backlog reports zero the second time.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::infra::repositories::runtime_graph_repository::{self, OrphanRuntimeCounts};

/// Upper bound on the look-back window, keeping the cutoff arithmetic inside
/// `chrono`'s representable range for any caller-supplied duration.
const MAX_STALE_AFTER_SECONDS: i64 = 86_400 * 365 * 1_000;

/// Look-back the maintenance scheduler applies before treating a non-terminal
/// runtime row whose activating attempt is already terminal (or absent) as an
/// orphan. Kept comfortably above the longest pipeline stage so an attempt that
/// is still running is never mistakenly swept.
pub const ORPHAN_RUNTIME_STALE_AFTER: Duration = Duration::from_mins(10);

/// Maximum runtime rows closed per table in one sweep; bounds the transaction.
pub const ORPHAN_RUNTIME_BATCH_LIMIT: u32 = 500;

/// Runs one orphan-runtime sweep: rows older than `stale_after` whose
/// activating attempt is terminal or absent are closed, at most `batch_limit`
/// at a time.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime rows.
pub async fn reconcile_orphan_runtime_rows_once(
    postgres: &PgPool,
    stale_after: Duration,
    batch_limit: u32,
) -> Result<OrphanRuntimeCounts, sqlx::Error> {
    runtime_graph_repository::reconcile_orphan_runtime_rows_once(
        postgres,
        stale_cutoff(stale_after),
        batch_limit,
    )
    .await
}

fn stale_cutoff(stale_after: Duration) -> DateTime<Utc> {
    let seconds =
        i64::try_from(stale_after.as_secs()).unwrap_or(i64::MAX).min(MAX_STALE_AFTER_SECONDS);
    Utc::now() - chrono::Duration::seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_cutoff_subtracts_the_window() {
        let now = Utc::now();
        let cutoff = stale_cutoff(Duration::from_secs(60));

        assert!(cutoff <= now, "cutoff {cutoff} must not be after now {now}");
        assert!(
            cutoff >= now - chrono::Duration::seconds(61),
            "cutoff {cutoff} must be within the 60s window of now {now}",
        );
    }

    #[test]
    fn stale_cutoff_saturates_an_oversized_window() {
        let now = Utc::now();
        let cutoff = stale_cutoff(Duration::from_secs(u64::MAX));

        assert!(cutoff <= now, "oversized window must still produce a past cutoff");
        assert!(
            cutoff >= now - chrono::Duration::seconds(MAX_STALE_AFTER_SECONDS),
            "oversized window must be clamped to MAX_STALE_AFTER_SECONDS",
        );
    }

    #[test]
    fn stale_cutoff_of_zero_is_now() {
        let before = Utc::now();
        let cutoff = stale_cutoff(Duration::ZERO);
        let after = Utc::now();

        assert!(cutoff >= before && cutoff <= after);
    }
}
