//! `retention.*` sweepers — TTL-based batched DELETEs against the
//! INSERT-only history tables.
//!
//! Each retention sweeper:
//!
//! * Filters by the table's canonical timestamp column (e.g.
//!   `ingest_stage_event.recorded_at`).
//! * Issues the DELETE in batches of up to [`BATCH_SIZE`] rows so the
//!   transaction never holds more than that many tuples worth of
//!   `AccessExclusiveLock`. Between batches a short [`BATCH_SLEEP`]
//!   yields the database to other writers.
//! * Returns the total number of rows removed once the table is empty
//!   for the configured TTL window.
//!
//! Indexes on the filter columns are pre-created in migration 0017 so
//! the predicate compiles to an index range scan rather than a
//! sequential scan that would blow up the lock footprint.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use thiserror::Error;
use tokio::time::sleep;
use tracing::info;

/// Maximum rows removed per DELETE batch. Chosen so the WAL impact of
/// one batch stays well below typical Postgres bgwriter ceilings.
pub const BATCH_SIZE: i64 = 10_000;

/// Wall-clock pause between batches to keep the database responsive
/// for concurrent ingest writers. 100 ms is short enough to finish a
/// 100 k-row sweep in seconds and long enough to let WAL writers
/// breathe.
pub const BATCH_SLEEP: Duration = Duration::from_millis(100);

/// Default retention window for `ingest_stage_event` rows. Anything
/// older than this is no longer load-bearing for ingest debugging.
pub const DEFAULT_STAGE_EVENT_RETENTION: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// One-shot report for a retention sweep.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RetentionReport {
    pub rows_removed: i64,
    pub batches: i64,
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("postgres error during retention sweep: {0}")]
    Sqlx(#[from] sqlx::Error),
}

impl RetentionError {
    /// Stable string for scheduler dead-letter rows and Prometheus
    /// labels.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Sqlx(_) => "postgres",
        }
    }
}

/// Sweep rows from `ingest_stage_event` older than `older_than`.
///
/// Batched DELETE keyed by `recorded_at`. The cascade on
/// `ingest_stage_provider_call.stage_event_id` cleans up dependent
/// rows automatically, so the sweeper only needs to walk one table.
pub async fn stage_events(
    pool: &PgPool,
    older_than: Duration,
) -> Result<RetentionReport, RetentionError> {
    sweep_batched(pool, "ingest_stage_event", "recorded_at", older_than).await
}

/// Generic batched DELETE keyed by a timestamp column.
///
/// Returns once a batch returns zero rows. Logs progress at INFO so an
/// operator running the sweep manually sees forward motion.
async fn sweep_batched(
    pool: &PgPool,
    table: &'static str,
    column: &'static str,
    older_than: Duration,
) -> Result<RetentionReport, RetentionError> {
    let secs = older_than.as_secs() as f64;
    // Format separately because we cannot bind a table identifier as
    // a parameter. The values for `table` and `column` come from
    // module-level &'static str constants — never user input — so
    // this avoids the SQL-injection lane that would otherwise apply
    // to dynamic identifiers.
    let sql = format!(
        "with deleted as ( \
             delete from {table} \
             where ctid in ( \
                 select ctid from {table} \
                 where {column} < now() - make_interval(secs => $1::double precision) \
                 limit $2 \
             ) \
             returning 1 \
         ) \
         select count(*)::bigint from deleted",
    );

    let mut report = RetentionReport::default();
    loop {
        let removed: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(&*sql))
            .bind(secs)
            .bind(BATCH_SIZE)
            .fetch_one(pool)
            .await?;
        if removed == 0 {
            return Ok(report);
        }
        report.rows_removed += removed;
        report.batches += 1;
        info!(
            table,
            batch = report.batches,
            rows_removed = removed,
            total_rows_removed = report.rows_removed,
            "retention batch complete",
        );
        sleep(BATCH_SLEEP).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_retention_is_ninety_days() {
        assert_eq!(DEFAULT_STAGE_EVENT_RETENTION.as_secs(), 90 * 24 * 60 * 60);
    }

    #[test]
    fn error_code_is_stable() {
        let error = RetentionError::Sqlx(sqlx::Error::PoolClosed);
        assert_eq!(error.code(), "postgres");
    }
}
