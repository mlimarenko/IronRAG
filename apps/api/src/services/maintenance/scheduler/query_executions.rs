use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use tracing::{info, warn};

use crate::{app::state::AppState, infra::repositories::query_repository};

const STALE_QUERY_EXECUTION_BATCH_LIMIT: i64 = 100;

/// Runtime-clock cutoff, clamped to the minimum timestamp on underflow.
fn stale_before_from(stale_after: Duration) -> DateTime<Utc> {
    let stale_after = ChronoDuration::from_std(stale_after).unwrap_or(ChronoDuration::MAX);
    Utc::now().checked_sub_signed(stale_after).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

pub(super) async fn reap_stale_query_executions(state: &AppState, stale_after: Duration) {
    let stale_before = stale_before_from(stale_after);
    match query_repository::reap_stale_query_executions(
        &state.persistence.postgres,
        stale_before,
        STALE_QUERY_EXECUTION_BATCH_LIMIT,
    )
    .await
    {
        Ok(reaped) => {
            crate::observability::metrics::record_reap_stale_query_executions(reaped);
            if reaped > 0 {
                log_stale_query_executions_canceled(reaped);
            }
        }
        Err(error) => log_stale_query_execution_reaper_failed(&error),
    }
}

fn log_stale_query_executions_canceled(reaped: u64) {
    info!(reaped, "maintenance scheduler canceled stale query executions");
}

fn log_stale_query_execution_reaper_failed(error: &sqlx::Error) {
    warn!(?error, "stale query execution reaper failed; continuing");
}

/// Closes `query_execution` async operations whose linked runtime execution is
/// already terminal or absent, which [`reap_stale_query_executions`] never
/// selects because it requires the runtime to still be `accepted` / `running`.
pub(super) async fn reconcile_orphan_query_operations(state: &AppState, stale_after: Duration) {
    let stale_before = stale_before_from(stale_after);
    match query_repository::reconcile_orphan_query_operations_once(
        &state.persistence.postgres,
        stale_before,
        STALE_QUERY_EXECUTION_BATCH_LIMIT,
    )
    .await
    {
        Ok(count) if count > 0 => log_orphan_query_operations_reconciled(count),
        Ok(_) => {}
        Err(error) => log_orphan_query_operation_reconcile_failed(&error),
    }
}

fn log_orphan_query_operations_reconciled(count: u64) {
    info!(count, "maintenance scheduler reconciled orphaned query operations");
}

fn log_orphan_query_operation_reconcile_failed(error: &sqlx::Error) {
    warn!(?error, "orphan query operation reconcile failed; continuing");
}
