//! Durable lease for scheduled maintenance work.
//!
//! Every recurring sweeper is identified by a [`MaintenanceClass`] and a
//! [`Scope`]. The pair maps to exactly one row in `maintenance_job_run`
//! which carries the lifecycle: when it is next due, who is currently
//! holding the lease, the cursor for incremental work, retry attempts,
//! and the dead-letter mark.
//!
//! The scheduler tick calls [`acquire_next_due`] which atomically picks
//! the oldest-pending row in a class and flips it to `leased`. While the
//! sweeper runs, it refreshes [`heartbeat`] periodically; if it crashes
//! the row stays in `leased` state until [`reap_stale_leases`] returns
//! it to `pending`. On clean completion the sweeper calls [`complete`];
//! on failure it calls [`fail`] which either re-queues with backoff or
//! marks the row `dead_letter` once attempts are exhausted.
//!
//! `pg_advisory_xact_lock` alone is not enough for this contract because
//! a crashed sweeper would silently release the lock and leave partial
//! cross-store state without any visible status. The durable row is the
//! single source of truth for "what is currently running, what failed,
//! what to retry next".

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Type};
use thiserror::Error;
use uuid::Uuid;

/// Sentinel uuid used in the unique index on `maintenance_job_run` so that
/// scope_id NULL (`Scope::Instance`) still has a deterministic key.
const INSTANCE_SCOPE_SENTINEL: Uuid = Uuid::nil();

/// Default retry ceiling before a run is marked `dead_letter`.
pub const DEFAULT_MAX_ATTEMPTS: i32 = 3;

/// Default stale-lease cutoff for the reaper.
pub const DEFAULT_STALE_LEASE: Duration = Duration::from_secs(5 * 60);

/// Canonical scheduler class identifier. Stored as text in the
/// `maintenance_job_run.class` column so adding a new class is a code-only
/// change with no migration. Keep the string forms stable — they are
/// referenced from Grafana dashboards and from the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaintenanceClass {
    GcStaleChunks,
    GcStaleEvidence,
    GcArchivalEvidence,
    GcPgGraphZombies,
    GcOrphanLibrariesAudit,
    AuditStorageSummary,
    AuditIndexBloat,
    AuditNullHeadDocs,
    RetentionStageEvents,
    RetentionAttempts,
    RetentionPolicyDecisions,
    RetentionAsyncOperations,
    RetentionWebDiscoveredPages,
}

impl MaintenanceClass {
    /// Stable kebab-case string used in `maintenance_job_run.class`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GcStaleChunks => "gc.stale-chunks",
            Self::GcStaleEvidence => "gc.stale-evidence",
            Self::GcArchivalEvidence => "gc.archival-evidence",
            Self::GcPgGraphZombies => "gc.pg-graph-zombies",
            Self::GcOrphanLibrariesAudit => "gc.orphan-libraries-audit",
            Self::AuditStorageSummary => "audit.storage-summary",
            Self::AuditIndexBloat => "audit.index-bloat",
            Self::AuditNullHeadDocs => "audit.null-head-docs",
            Self::RetentionStageEvents => "retention.stage-events",
            Self::RetentionAttempts => "retention.attempts",
            Self::RetentionPolicyDecisions => "retention.policy-decisions",
            Self::RetentionAsyncOperations => "retention.async-operations",
            Self::RetentionWebDiscoveredPages => "retention.web-discovered-pages",
        }
    }

    /// Parse the stored class string back into the enum. Returns `None` for
    /// unknown classes (e.g. a row written by a newer binary).
    #[must_use]
    pub fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "gc.stale-chunks" => Self::GcStaleChunks,
            "gc.stale-evidence" => Self::GcStaleEvidence,
            "gc.archival-evidence" => Self::GcArchivalEvidence,
            "gc.pg-graph-zombies" => Self::GcPgGraphZombies,
            "gc.orphan-libraries-audit" => Self::GcOrphanLibrariesAudit,
            "audit.storage-summary" => Self::AuditStorageSummary,
            "audit.index-bloat" => Self::AuditIndexBloat,
            "audit.null-head-docs" => Self::AuditNullHeadDocs,
            "retention.stage-events" => Self::RetentionStageEvents,
            "retention.attempts" => Self::RetentionAttempts,
            "retention.policy-decisions" => Self::RetentionPolicyDecisions,
            "retention.async-operations" => Self::RetentionAsyncOperations,
            "retention.web-discovered-pages" => Self::RetentionWebDiscoveredPages,
            _ => return None,
        })
    }
}

/// State machine mirror for the `maintenance_run_state` PG enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Type, Serialize, Deserialize)]
#[sqlx(type_name = "maintenance_run_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceRunState {
    Pending,
    Leased,
    Completed,
    Failed,
    DeadLetter,
}

/// Scope of a maintenance run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// One singleton row per class. Used for instance-wide work like
    /// `audit.storage-summary` or `retention.stage-events`.
    Instance,
    /// Per-library row keyed by library uuid. Used for per-library work
    /// like `gc.stale-chunks`.
    Library(Uuid),
}

impl Scope {
    fn kind(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Library(_) => "library",
        }
    }

    fn id(self) -> Option<Uuid> {
        match self {
            Self::Instance => None,
            Self::Library(id) => Some(id),
        }
    }

    fn sentinel(self) -> Uuid {
        self.id().unwrap_or(INSTANCE_SCOPE_SENTINEL)
    }
}

/// Mirror of `maintenance_job_run`. Returned by [`acquire_next_due`] so the
/// caller knows the cursor and the row id to reference in heartbeat/complete.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct MaintenanceJobRun {
    pub id: Uuid,
    pub class: String,
    pub scope_kind: String,
    pub scope_id: Option<Uuid>,
    pub owner_node: Option<String>,
    pub state: MaintenanceRunState,
    pub cursor_json: serde_json::Value,
    pub attempts: i32,
    pub last_started_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub last_completed_at: Option<DateTime<Utc>>,
    pub rows_removed_total: i64,
    pub bytes_reclaimed_total: i64,
    pub error_code: Option<String>,
    pub error_text: Option<String>,
    pub next_due_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MaintenanceJobRun {
    /// Convenience: parse the persisted class string. Returns `None` for
    /// classes a newer binary may have introduced.
    #[must_use]
    pub fn class_enum(&self) -> Option<MaintenanceClass> {
        MaintenanceClass::from_str(&self.class)
    }

    /// Convenience: rebuild the typed scope from the persisted columns.
    #[must_use]
    pub fn scope(&self) -> Scope {
        match (self.scope_kind.as_str(), self.scope_id) {
            ("library", Some(id)) => Scope::Library(id),
            _ => Scope::Instance,
        }
    }
}

#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("postgres error in maintenance lease: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Create the row for `(class, scope)` if it does not yet exist. Idempotent.
///
/// Called once per known sweeper at scheduler boot to make sure the
/// `maintenance_job_run` table has the canonical set of rows. The first
/// `next_due_at` defaults to `now()` so a fresh boot picks the work up
/// on the next tick.
pub async fn ensure_row(
    pool: &PgPool,
    class: MaintenanceClass,
    scope: Scope,
    next_due_at: Option<DateTime<Utc>>,
) -> Result<(), LeaseError> {
    let kind = scope.kind();
    let scope_id = scope.id();
    sqlx::query(
        "insert into maintenance_job_run (class, scope_kind, scope_id, next_due_at) \
         values ($1, $2, $3, coalesce($4, now())) \
         on conflict (class, scope_kind, coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
         do nothing",
    )
    .bind(class.as_str())
    .bind(kind)
    .bind(scope_id)
    .bind(next_due_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Atomically pick the oldest-pending due row in `class` and lease it to
/// `owner_node`. Returns `Ok(None)` if no row is currently due.
///
/// Internally a `select … for update skip locked` keeps two competing
/// schedulers off the same row even under tight tick alignment, and the
/// `update … returning` flips the state atomically.
pub async fn acquire_next_due(
    pool: &PgPool,
    class: MaintenanceClass,
    owner_node: &str,
) -> Result<Option<MaintenanceJobRun>, LeaseError> {
    let row = sqlx::query_as::<_, MaintenanceJobRun>(
        "update maintenance_job_run \
         set state = 'leased', \
             owner_node = $2, \
             heartbeat_at = now(), \
             last_started_at = now(), \
             updated_at = now() \
         where id = ( \
             select id from maintenance_job_run \
             where class = $1 \
               and state = 'pending' \
               and next_due_at <= now() \
             order by next_due_at asc \
             for update skip locked \
             limit 1 \
         ) \
         returning *",
    )
    .bind(class.as_str())
    .bind(owner_node)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Refresh the heartbeat on a leased row. No-op if the row is not in
/// `leased` state (e.g. a reaper already stole it).
pub async fn heartbeat(pool: &PgPool, run_id: Uuid) -> Result<(), LeaseError> {
    sqlx::query(
        "update maintenance_job_run \
         set heartbeat_at = now(), updated_at = now() \
         where id = $1 and state = 'leased'",
    )
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update the persisted cursor under a leased row.
///
/// Cursor format is class-specific; the lease module is content-blind.
/// Used by sweepers that process work in pages and need to resume after
/// a crash without re-doing pages already finished.
pub async fn update_cursor(
    pool: &PgPool,
    run_id: Uuid,
    cursor: serde_json::Value,
) -> Result<(), LeaseError> {
    sqlx::query(
        "update maintenance_job_run \
         set cursor_json = $2, updated_at = now() \
         where id = $1 and state = 'leased'",
    )
    .bind(run_id)
    .bind(cursor)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a lease as completed and re-queue for `next_interval`.
///
/// Resets attempts and error state — the row is healthy again. Adds
/// `rows_removed` and `bytes_reclaimed` to the running totals; pass zero
/// when the sweeper had nothing to do.
pub async fn complete(
    pool: &PgPool,
    run_id: Uuid,
    rows_removed: i64,
    bytes_reclaimed: i64,
    next_interval: Duration,
) -> Result<(), LeaseError> {
    let interval_secs = next_interval.as_secs() as i64;
    sqlx::query(
        "update maintenance_job_run \
         set state = 'pending', \
             last_completed_at = now(), \
             heartbeat_at = null, \
             owner_node = null, \
             rows_removed_total = rows_removed_total + $2, \
             bytes_reclaimed_total = bytes_reclaimed_total + $3, \
             attempts = 0, \
             error_code = null, \
             error_text = null, \
             next_due_at = now() + make_interval(secs => $4::double precision), \
             updated_at = now() \
         where id = $1",
    )
    .bind(run_id)
    .bind(rows_removed)
    .bind(bytes_reclaimed)
    .bind(interval_secs as f64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Outcome of a [`fail`] call so the caller can emit the matching metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    /// Lease was returned to `pending` for another attempt after `backoff`.
    Retry { attempts: i32, backoff: Duration },
    /// Lease was marked `dead_letter`; the scheduler will not pick it up
    /// again until an operator clears it.
    DeadLetter { attempts: i32 },
}

/// Record a failure on a leased row.
///
/// Increments `attempts`. While attempts are below `max_attempts` the row
/// goes back to `pending` with an exponential-ish backoff (1m, 5m, 30m by
/// default). On the attempt that crosses `max_attempts` the row sticks at
/// `dead_letter` and is excluded from the scheduler until an operator
/// resets it via the CLI.
pub async fn fail(
    pool: &PgPool,
    run_id: Uuid,
    error_code: &str,
    error_text: &str,
    max_attempts: i32,
) -> Result<FailOutcome, LeaseError> {
    let mut transaction = pool.begin().await?;
    let row: Option<(i32,)> =
        sqlx::query_as("select attempts from maintenance_job_run where id = $1 for update")
            .bind(run_id)
            .fetch_optional(&mut *transaction)
            .await?;
    let current_attempts = row.map_or(0, |(value,)| value);
    let new_attempts = current_attempts + 1;
    let outcome = if new_attempts >= max_attempts {
        FailOutcome::DeadLetter { attempts: new_attempts }
    } else {
        FailOutcome::Retry { attempts: new_attempts, backoff: retry_backoff(new_attempts) }
    };
    match outcome {
        FailOutcome::DeadLetter { attempts } => {
            sqlx::query(
                "update maintenance_job_run \
                 set state = 'dead_letter', \
                     attempts = $2, \
                     error_code = $3, \
                     error_text = $4, \
                     heartbeat_at = null, \
                     owner_node = null, \
                     updated_at = now() \
                 where id = $1",
            )
            .bind(run_id)
            .bind(attempts)
            .bind(error_code)
            .bind(error_text)
            .execute(&mut *transaction)
            .await?;
        }
        FailOutcome::Retry { attempts, backoff } => {
            sqlx::query(
                "update maintenance_job_run \
                 set state = 'pending', \
                     attempts = $2, \
                     error_code = $3, \
                     error_text = $4, \
                     heartbeat_at = null, \
                     owner_node = null, \
                     next_due_at = now() + make_interval(secs => $5::double precision), \
                     updated_at = now() \
                 where id = $1",
            )
            .bind(run_id)
            .bind(attempts)
            .bind(error_code)
            .bind(error_text)
            .bind(backoff.as_secs() as f64)
            .execute(&mut *transaction)
            .await?;
        }
    }
    transaction.commit().await?;
    Ok(outcome)
}

/// Clear a dead-letter mark and return the row to the scheduler.
///
/// Wired into the operator CLI (`ironrag-maintenance repair clear-failure`)
/// so a human can resume a class after fixing the root cause.
pub async fn clear_dead_letter(
    pool: &PgPool,
    class: MaintenanceClass,
    scope: Scope,
) -> Result<bool, LeaseError> {
    let kind = scope.kind();
    let sentinel = scope.sentinel();
    let updated = sqlx::query(
        "update maintenance_job_run \
         set state = 'pending', \
             attempts = 0, \
             error_code = null, \
             error_text = null, \
             next_due_at = now(), \
             updated_at = now() \
         where class = $1 \
           and scope_kind = $2 \
           and coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid) = $3 \
           and state = 'dead_letter'",
    )
    .bind(class.as_str())
    .bind(kind)
    .bind(sentinel)
    .execute(pool)
    .await?;
    Ok(updated.rows_affected() > 0)
}

/// Return rows whose lease went stale (no heartbeat) back to `pending`
/// so a healthy scheduler can re-pick them up. Should be called by the
/// scheduler tick before [`acquire_next_due`]; safe to invoke from any
/// node — the operation is set-based and idempotent.
///
/// Returns the number of rows reaped.
pub async fn reap_stale_leases(pool: &PgPool, stale_after: Duration) -> Result<u64, LeaseError> {
    let secs = stale_after.as_secs() as f64;
    let result = sqlx::query(
        "update maintenance_job_run \
         set state = 'pending', \
             owner_node = null, \
             heartbeat_at = null, \
             updated_at = now() \
         where state = 'leased' \
           and heartbeat_at < now() - make_interval(secs => $1::double precision)",
    )
    .bind(secs)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Look up the current row for `(class, scope)` if any. Used by the CLI
/// for `audit run-status` and by integration tests.
pub async fn load(
    pool: &PgPool,
    class: MaintenanceClass,
    scope: Scope,
) -> Result<Option<MaintenanceJobRun>, LeaseError> {
    let row = sqlx::query_as::<_, MaintenanceJobRun>(
        "select * from maintenance_job_run \
         where class = $1 \
           and scope_kind = $2 \
           and coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid) = $3",
    )
    .bind(class.as_str())
    .bind(scope.kind())
    .bind(scope.sentinel())
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Count rows in each state for `class`. Used by the operator CLI to
/// render a per-class summary table.
pub async fn count_by_state(
    pool: &PgPool,
    class: MaintenanceClass,
) -> Result<StateCounts, LeaseError> {
    let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "select \
            count(*) filter (where state = 'pending')::bigint, \
            count(*) filter (where state = 'leased')::bigint, \
            count(*) filter (where state = 'completed')::bigint, \
            count(*) filter (where state = 'failed')::bigint, \
            count(*) filter (where state = 'dead_letter')::bigint \
         from maintenance_job_run \
         where class = $1",
    )
    .bind(class.as_str())
    .fetch_one(pool)
    .await?;
    Ok(StateCounts {
        pending: row.0,
        leased: row.1,
        completed: row.2,
        failed: row.3,
        dead_letter: row.4,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateCounts {
    pub pending: i64,
    pub leased: i64,
    pub completed: i64,
    pub failed: i64,
    pub dead_letter: i64,
}

/// Backoff schedule for failed runs. Pure function so it is easy to assert.
const fn retry_backoff(attempts: i32) -> Duration {
    match attempts {
        1 => Duration::from_secs(60),
        2 => Duration::from_secs(5 * 60),
        _ => Duration::from_secs(30 * 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_strings_round_trip() {
        for class in [
            MaintenanceClass::GcStaleChunks,
            MaintenanceClass::GcStaleEvidence,
            MaintenanceClass::GcArchivalEvidence,
            MaintenanceClass::GcPgGraphZombies,
            MaintenanceClass::GcOrphanLibrariesAudit,
            MaintenanceClass::AuditStorageSummary,
            MaintenanceClass::AuditIndexBloat,
            MaintenanceClass::AuditNullHeadDocs,
            MaintenanceClass::RetentionStageEvents,
            MaintenanceClass::RetentionAttempts,
            MaintenanceClass::RetentionPolicyDecisions,
            MaintenanceClass::RetentionAsyncOperations,
            MaintenanceClass::RetentionWebDiscoveredPages,
        ] {
            let text = class.as_str();
            assert_eq!(MaintenanceClass::from_str(text), Some(class), "{text}");
        }
        assert_eq!(MaintenanceClass::from_str("does.not.exist"), None);
    }

    #[test]
    fn instance_scope_uses_sentinel() {
        assert_eq!(Scope::Instance.kind(), "instance");
        assert!(Scope::Instance.id().is_none());
        assert_eq!(Scope::Instance.sentinel(), INSTANCE_SCOPE_SENTINEL);
    }

    #[test]
    fn library_scope_preserves_id() {
        let id = Uuid::from_u128(0xdeadbeef);
        let scope = Scope::Library(id);
        assert_eq!(scope.kind(), "library");
        assert_eq!(scope.id(), Some(id));
        assert_eq!(scope.sentinel(), id);
    }

    #[test]
    fn retry_backoff_grows() {
        assert_eq!(retry_backoff(1), Duration::from_secs(60));
        assert_eq!(retry_backoff(2), Duration::from_secs(5 * 60));
        assert_eq!(retry_backoff(3), Duration::from_secs(30 * 60));
        assert_eq!(retry_backoff(10), Duration::from_secs(30 * 60));
    }
}
