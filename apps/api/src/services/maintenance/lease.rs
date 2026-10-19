//! Durable lease for scheduled maintenance work.
//!
//! Every recurring sweeper is identified by a [`MaintenanceClass`] and a
//! [`Scope`]. The pair maps to exactly one row in `maintenance_job_run`
//! which carries the lifecycle: when it is next due, who is currently
//! holding the lease, the cursor for incremental work, retry attempts,
//! and the dead-letter mark.
//!
//! The scheduler tick calls [`acquire_next_due`] which atomically picks
//! one due row in a class — an in-flight (non-empty cursor) row first, then
//! the oldest due — and flips it to `leased`. While the
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
/// `scope_id` NULL (`Scope::Instance`) still has a deterministic key.
const INSTANCE_SCOPE_SENTINEL: Uuid = Uuid::nil();

/// Default retry ceiling before a run is marked `dead_letter`.
pub const DEFAULT_MAX_ATTEMPTS: i32 = 3;

/// Stable scheduler class identifier. Stored as text in the
/// `maintenance_job_run.class` column so adding a new class is a code-only
/// change with no migration. Keep the string forms stable — they are
/// referenced from Grafana dashboards and from the CLI.
///
/// Durable maintenance lease and coordination classes live here. A class may
/// be dispatched by the general scheduler or own a narrower typed coordination
/// row, such as graph re-extract admission; keep every stored wire value stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaintenanceClass {
    GcSupersededVectors,
    GcRuntimeGraph,
    GcHistoricalProjection,
    GraphReextractAdmission,
}

impl MaintenanceClass {
    /// Stable kebab-case string used in `maintenance_job_run.class`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GcSupersededVectors => "gc.superseded-vectors",
            Self::GcRuntimeGraph => "gc.runtime-graph",
            Self::GcHistoricalProjection => "gc.historical-projection",
            Self::GraphReextractAdmission => "graph.reextract-periodic-admission",
        }
    }

    /// Parse the stored class string back into the enum. Returns `None` for
    /// unknown classes (e.g. a row written by a newer binary).
    #[must_use]
    pub fn parse_wire(value: &str) -> Option<Self> {
        Some(match value {
            "gc.superseded-vectors" => Self::GcSupersededVectors,
            "gc.runtime-graph" => Self::GcRuntimeGraph,
            "gc.historical-projection" => Self::GcHistoricalProjection,
            "graph.reextract-periodic-admission" => Self::GraphReextractAdmission,
            _ => return None,
        })
    }
}

/// State machine mirror for the `maintenance_run_state` PG enum.
///
/// Only `pending`, `leased` and `dead_letter` are live. The Postgres enum still
/// declares `completed` and `failed`, but no Rust path writes them: a successful
/// sweep returns the row to `pending` with a future `next_due_at`, and a failure
/// either retries as `pending` or sticks at `dead_letter`. The variants are
/// omitted here so the write-dead states cannot be reintroduced by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Type, Serialize, Deserialize)]
#[sqlx(type_name = "maintenance_run_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceRunState {
    Pending,
    Leased,
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
    /// like `gc.superseded-vectors`.
    Library(Uuid),
}

impl Scope {
    const fn kind(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Library(_) => "library",
        }
    }

    const fn id(self) -> Option<Uuid> {
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
        MaintenanceClass::parse_wire(&self.class)
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
    #[error("maintenance lease {run_id} is no longer owned by this scheduler")]
    LostOwnership { run_id: Uuid },
}

const LIBRARY_ROW_SEEDING_SQL: &str = "insert into maintenance_job_run (class, scope_kind, scope_id, next_due_at) \
     select $1, 'library', library.id, now() \
     from catalog_library library \
     on conflict (class, scope_kind, coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
     do nothing";

const RETIRED_STALE_CHUNKS_CLASS: &str = "gc.stale-chunks";

/// Delete rows for the pre-IRG-62 maintenance class after old workers have
/// stopped. The old class combined destructive chunk deletion with graph
/// cleanup and cannot be migrated to either replacement cursor safely.
pub async fn retire_obsolete_rows(pool: &PgPool) -> Result<u64, LeaseError> {
    let deleted = sqlx::query("delete from maintenance_job_run where class = $1")
        .bind(RETIRED_STALE_CHUNKS_CLASS)
        .execute(pool)
        .await?;
    Ok(deleted.rows_affected())
}

/// Create the missing rows for `class` across every catalog library in one
/// statement. Idempotent, and cheap enough to repeat on every scheduler tick
/// so libraries created after boot become eligible without a restart.
///
/// The per-library variant this replaced issued one INSERT per library per
/// tick: on an installation with ~2.2k libraries and three scheduler
/// processes that was ~6.5k no-op writes every 30 s against the same Postgres
/// that serves query turns, and it was the single most frequent statement in
/// a measured 10-turn wave.
///
/// # Errors
/// Returns any `SQLx` error raised while seeding the rows.
pub async fn ensure_library_rows(
    pool: &PgPool,
    class: MaintenanceClass,
) -> Result<u64, LeaseError> {
    let result = sqlx::query(LIBRARY_ROW_SEEDING_SQL).bind(class.as_str()).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Create the row for `(class, scope)` if it does not yet exist. Idempotent.
///
/// Used for the non-library scopes; the library sweep seeds its rows in bulk
/// through [`ensure_library_rows`].
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

/// Which cursor population [`acquire_next_due`] serves first.
///
/// `cursor_json = '{}'` is the canonical finished/idle sentinel — both sweepers
/// write `{}` on a completed phase — so a row mid-multiphase carries a non-empty
/// cursor. This is an ordering key only, never a filter: when the preferred
/// population has no due row, the same statement leases the other one, so a
/// slot is never wasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquirePreference {
    /// Continue a row already mid-sweep (`cursor_json <> '{}'`).
    InFlight,
    /// Serve a row that has not started its interval (`cursor_json = '{}'`).
    Fresh,
}

impl AcquirePreference {
    const fn prefer_in_flight(self) -> bool {
        matches!(self, Self::InFlight)
    }
}

/// The single eligibility predicate shared by the acquire statement and the
/// non-terminal backlog sampler. The acquire statement applies it alone — there
/// is no second `next_due_at` filter to drift from it — so a row that
/// `next_due_at` reports as due is always the row the lease will take.
///
/// `next_due_at` is the time-based re-arm deadline. [`complete`] pushes it out
/// by the class's `full_pass_interval` once a sweep finishes, so an unchanged
/// library still receives its safety full pass exactly when its deadline
/// arrives; [`fail`] and the active-ingest re-queue use a short backoff and a
/// never-started row is seeded due now. Every one of those states is therefore
/// covered by the first arm, and `next_due_at` can no longer show a row as
/// overdue while the fence withholds it.
///
/// The second arm re-arms a started, finished row (`cursor_json = '{}'`) on
/// demand, without waiting for the deadline, when the library's
/// `source_truth_version` advanced after that row's own `last_started_at`. The
/// `class_interval` cooldown since `last_completed_at` — not `next_due_at` —
/// throttles this arm, so a continuously-changing library still runs at most
/// once per class interval while a generation bump that lands while the row is
/// leased re-arms it on the next tick.
///
/// `cooldown_param` is a SQL expression yielding seconds (`$3` in the acquire
/// statement, `$1` in the sampler). The changed-library fence is
/// `last_started_at`, not `last_completed_at`. A missing library row coalesces
/// to the maximum generation, so such a scope stays schedulable.
pub(crate) fn gate_eligible_predicate(cooldown_param: &str) -> String {
    format!(
        "( next_due_at <= now() \
         or ( cursor_json = '{{}}'::jsonb \
              and last_started_at is not null \
              and last_completed_at \
                    <= now() - make_interval(secs => {cooldown_param}::double precision) \
              and coalesce((select library.source_truth_version \
                           from catalog_library library \
                           where library.id = maintenance_job_run.scope_id), \
                          9223372036854775807) \
                 > (extract(epoch from last_started_at) * 1000000)::bigint ) )"
    )
}

/// Lease-acquire statement for [`acquire_next_due`]. Built once from the shared
/// [`gate_eligible_predicate`] so a unit test can assert the ordering without a
/// database and the sampler cannot drift from the gate.
///
/// The predicate is the gate alone: `next_due_at <= now()` now lives inside
/// [`gate_eligible_predicate`], so there is exactly one schedule expression and
/// a row the gate does not withhold is always taken. `$4` selects which cursor
/// population sorts first. A single unconditional in-flight-first ordering
/// starved the plain `{}` rows: with far more in-flight-due rows than the
/// per-tick batch, every slot went to a mid-sweep row and a never-started row
/// was never selected at all. Bounding the preference keeps a multi-phase sweep
/// from being starved behind thousands of overdue first-phase rows *and* lets
/// the plain rows progress. `next_due_at asc` still rotates within the
/// preferred population, because every completed row's `next_due_at` moves into
/// the future. The partial due-index drives the ordering; PostgreSQL sorts at
/// most the matched rows.
pub(crate) fn acquire_next_due_sql() -> &'static str {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!(
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
                   and {} \
                 order by ((cursor_json <> '{{}}'::jsonb) = $4) desc, next_due_at asc \
                 for update skip locked \
                 limit 1 \
             ) \
             returning *",
            gate_eligible_predicate("$3")
        )
    });
    &SQL
}

/// Atomically pick one due row in `class`, preferring `preference`'s cursor
/// population and then the oldest due — and lease it to `owner_node`. Returns
/// `Ok(None)` when no row in the class is currently fence-eligible.
///
/// `class_interval` is the cooldown bound for the changed-library arm of
/// `gate_eligible_predicate`; the full-pass deadline is carried by the row's
/// own `next_due_at` and needs no parameter here. A row mid-sweep continues once
/// its re-queue delay (`tick_interval` or the retry backoff) has passed, a row
/// never started is seeded due now, and a finished row is re-armed either by its
/// full-pass `next_due_at` or by a generation bump after the class-interval
/// cooldown.
///
/// Internally a `select … for update skip locked` keeps two competing
/// schedulers off the same row even under tight tick alignment, and the
/// `update … returning` flips the state atomically. Because the preference is
/// an ordering key rather than a filter, an acquire never returns `None` while
/// any fence-eligible row exists.
pub async fn acquire_next_due(
    pool: &PgPool,
    class: MaintenanceClass,
    owner_node: &str,
    preference: AcquirePreference,
    class_interval: Duration,
) -> Result<Option<MaintenanceJobRun>, LeaseError> {
    let row = sqlx::query_as::<_, MaintenanceJobRun>(acquire_next_due_sql())
        .bind(class.as_str())
        .bind(owner_node)
        .bind(class_interval.as_secs() as f64)
        .bind(preference.prefer_in_flight())
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// Refresh the heartbeat on a lease still held by `owner_node`.
/// Returns [`LeaseError::LostOwnership`] after a reaper or another scheduler
/// has taken the row, so stale work stops instead of looking healthy.
pub async fn heartbeat(pool: &PgPool, run_id: Uuid, owner_node: &str) -> Result<(), LeaseError> {
    let updated = sqlx::query(
        "update maintenance_job_run \
         set heartbeat_at = now(), updated_at = now() \
         where id = $1 and state = 'leased' and owner_node = $2",
    )
    .bind(run_id)
    .bind(owner_node)
    .execute(pool)
    .await?;
    ensure_owned_update(run_id, updated.rows_affected())?;
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
    owner_node: &str,
    cursor: serde_json::Value,
) -> Result<(), LeaseError> {
    let updated = sqlx::query(
        "update maintenance_job_run \
         set cursor_json = $2, updated_at = now() \
         where id = $1 and state = 'leased' and owner_node = $3",
    )
    .bind(run_id)
    .bind(cursor)
    .bind(owner_node)
    .execute(pool)
    .await?;
    ensure_owned_update(run_id, updated.rows_affected())?;
    Ok(())
}

/// Mark a lease as completed and re-queue for `next_interval`.
///
/// `next_interval` becomes the row's `next_due_at`, which is the gate's
/// time-based re-arm deadline: the scheduler passes the class's
/// `full_pass_interval` for a finished sweep, so the next `next_due_at` is the
/// safety full-pass deadline, and `tick_interval` while work remains.
///
/// Resets attempts and error state — the row is healthy again. Adds
/// `rows_removed` and `bytes_reclaimed` to the running totals; pass zero
/// when the sweeper had nothing to do. Guarded by `state = 'leased' and
/// owner_node = $owner` so a sweeper whose lease was already reaped and
/// re-acquired by another node cannot clobber the new occupant's row.
pub async fn complete(
    pool: &PgPool,
    run_id: Uuid,
    owner_node: &str,
    rows_removed: i64,
    bytes_reclaimed: i64,
    next_interval: Duration,
) -> Result<(), LeaseError> {
    let interval_secs = next_interval.as_secs() as i64;
    let updated = sqlx::query(
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
         where id = $1 and state = 'leased' and owner_node = $5",
    )
    .bind(run_id)
    .bind(rows_removed)
    .bind(bytes_reclaimed)
    .bind(interval_secs as f64)
    .bind(owner_node)
    .execute(pool)
    .await?;
    ensure_owned_update(run_id, updated.rows_affected())?;
    Ok(())
}

fn ensure_owned_update(run_id: Uuid, rows_affected: u64) -> Result<(), LeaseError> {
    if rows_affected == 1 { Ok(()) } else { Err(LeaseError::LostOwnership { run_id }) }
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
/// resets it via the CLI. Both terminal updates are guarded by
/// `state = 'leased' and owner_node = $owner` so a sweeper whose lease was
/// already reaped and re-acquired by another node cannot clobber the new
/// occupant's row.
pub async fn fail(
    pool: &PgPool,
    run_id: Uuid,
    owner_node: &str,
    error_code: &str,
    error_text: &str,
    max_attempts: i32,
) -> Result<FailOutcome, LeaseError> {
    let mut transaction = pool.begin().await?;
    let row: Option<(i32,)> = sqlx::query_as(
        "select attempts from maintenance_job_run \
         where id = $1 and state = 'leased' and owner_node = $2 \
         for update",
    )
    .bind(run_id)
    .bind(owner_node)
    .fetch_optional(&mut *transaction)
    .await?;
    let current_attempts = row.map(|(value,)| value).ok_or(LeaseError::LostOwnership { run_id })?;
    let new_attempts = current_attempts + 1;
    let outcome = if new_attempts >= max_attempts {
        FailOutcome::DeadLetter { attempts: new_attempts }
    } else {
        FailOutcome::Retry { attempts: new_attempts, backoff: retry_backoff(new_attempts) }
    };
    match outcome {
        FailOutcome::DeadLetter { attempts } => {
            let updated = sqlx::query(
                "update maintenance_job_run \
                 set state = 'dead_letter', \
                     attempts = $2, \
                     error_code = $3, \
                     error_text = $4, \
                     heartbeat_at = null, \
                     owner_node = null, \
                     updated_at = now() \
                 where id = $1 and state = 'leased' and owner_node = $5",
            )
            .bind(run_id)
            .bind(attempts)
            .bind(error_code)
            .bind(error_text)
            .bind(owner_node)
            .execute(&mut *transaction)
            .await?;
            ensure_owned_update(run_id, updated.rows_affected())?;
        }
        FailOutcome::Retry { attempts, backoff } => {
            let updated = sqlx::query(
                "update maintenance_job_run \
                 set state = 'pending', \
                     attempts = $2, \
                     error_code = $3, \
                     error_text = $4, \
                     heartbeat_at = null, \
                     owner_node = null, \
                     next_due_at = now() + make_interval(secs => $5::double precision), \
                     updated_at = now() \
                 where id = $1 and state = 'leased' and owner_node = $6",
            )
            .bind(run_id)
            .bind(attempts)
            .bind(error_code)
            .bind(error_text)
            .bind(backoff.as_secs() as f64)
            .bind(owner_node)
            .execute(&mut *transaction)
            .await?;
            ensure_owned_update(run_id, updated.rows_affected())?;
        }
    }
    transaction.commit().await?;
    Ok(outcome)
}

/// Clear a dead-letter mark and return the row to the scheduler.
///
/// Wired into the operator CLI (`ironrag-maintenance repair clear-failure`)
/// so a human can resume a class after fixing the root cause.
///
/// `last_started_at` is reset to NULL as part of the re-arm: the
/// changed-library gate in [`acquire_next_due`] schedules a never-started row
/// unconditionally, so an operator's explicit resume is not swallowed by the
/// fence until the safety full-pass interval elapses. `last_completed_at`
/// stays as the operational completion record.
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
             last_started_at = null, \
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

/// Count live rows (every state except `dead_letter`) in `class`, split by
/// whether the row is mid-sweep (`cursor_json <> '{}'`). The scheduler sizes its
/// per-tick batch from both populations so a multi-phase sweep and a
/// never-started row are each walked within `class_interval`.
pub async fn count_active_by_cursor(
    pool: &PgPool,
    class: MaintenanceClass,
) -> Result<(i64, i64), LeaseError> {
    let row: (i64, i64) =
        sqlx::query_as(COUNT_ACTIVE_BY_CURSOR_SQL).bind(class.as_str()).fetch_one(pool).await?;
    Ok(row)
}

/// Cursor-split live-row count statement for [`count_active_by_cursor`]. Named
/// so a unit test can prove the split excludes `dead_letter` rows.
pub(crate) const COUNT_ACTIVE_BY_CURSOR_SQL: &str = "select \
         count(*) filter (where cursor_json <> '{}'::jsonb)::bigint, \
         count(*) filter (where cursor_json = '{}'::jsonb)::bigint \
     from maintenance_job_run \
     where class = $1 and state <> 'dead_letter'";

/// Per-live-state counts statement for [`count_by_state`]. Named so a unit test
/// can prove it covers only the live states and never the write-dead
/// `completed`/`failed` variants.
pub(crate) const COUNT_BY_STATE_SQL: &str = "select \
         count(*) filter (where state = 'pending')::bigint, \
         count(*) filter (where state = 'leased')::bigint, \
         count(*) filter (where state = 'dead_letter')::bigint \
     from maintenance_job_run \
     where class = $1";

/// Count live rows in each state for `class`. Used by the operator CLI to
/// render a per-class summary table.
pub async fn count_by_state(
    pool: &PgPool,
    class: MaintenanceClass,
) -> Result<StateCounts, LeaseError> {
    let row: (i64, i64, i64) =
        sqlx::query_as(COUNT_BY_STATE_SQL).bind(class.as_str()).fetch_one(pool).await?;
    Ok(StateCounts { pending: row.0, leased: row.1, dead_letter: row.2 })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateCounts {
    pub pending: i64,
    pub leased: i64,
    pub dead_letter: i64,
}

/// Backoff schedule for failed runs. Pure function so it is easy to assert.
const fn retry_backoff(attempts: i32) -> Duration {
    match attempts {
        1 => Duration::from_mins(1),
        2 => Duration::from_mins(5),
        _ => Duration::from_mins(30),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_strings_round_trip() {
        for class in [
            MaintenanceClass::GcSupersededVectors,
            MaintenanceClass::GcRuntimeGraph,
            MaintenanceClass::GcHistoricalProjection,
            MaintenanceClass::GraphReextractAdmission,
        ] {
            let text = class.as_str();
            assert_eq!(MaintenanceClass::parse_wire(text), Some(class), "{text}");
        }
        assert_eq!(MaintenanceClass::parse_wire("does.not.exist"), None);
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

    /// The scheduler seeds library rows on EVERY tick so libraries created
    /// after boot become eligible without a restart. That is only affordable
    /// as a single set-based statement: the per-library loop it replaced cost
    /// one INSERT per library per tick, which on ~2.2k libraries across three
    /// scheduler processes was ~6.5k no-op writes every 30 s against the
    /// query-serving database.
    #[test]
    fn library_row_seeding_is_one_set_based_statement() {
        let sql = LIBRARY_ROW_SEEDING_SQL;
        assert!(sql.starts_with("insert into maintenance_job_run"));
        assert!(sql.contains("from catalog_library library"));
        assert!(sql.contains("do nothing"), "seeding must stay idempotent");
        // A per-row loop would bind the scope id; the set-based form derives
        // it from the catalog instead, so `class` is the only bound value.
        assert_eq!(sql.matches('$').count(), 1);
        assert!(!sql.contains("$2"));
    }

    #[test]
    fn retry_backoff_grows() {
        assert_eq!(retry_backoff(1), Duration::from_secs(60));
        assert_eq!(retry_backoff(2), Duration::from_secs(5 * 60));
        assert_eq!(retry_backoff(3), Duration::from_secs(30 * 60));
        assert_eq!(retry_backoff(10), Duration::from_secs(30 * 60));
    }
}
