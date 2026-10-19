//! Closes the `runtime_execution` / `runtime_graph_extraction` rows that an
//! ingest attempt leaves non-terminal when it terminates.
//!
//! Graph extraction writes two runtime rows per activated attempt
//! (`services/graph/extract/mod.rs`):
//! * `runtime_execution` with `owner_kind = 'graph_extraction_attempt'` and
//!   `owner_id` = the `runtime_graph_extraction.id` it belongs to;
//! * `runtime_graph_extraction` with `status = 'processing'` and the activating
//!   attempt recorded at
//!   `raw_output_json->'lifecycle'->>'activated_by_attempt_id'`.
//!
//! The attempt finalizers in `ingest_repository` historically updated only
//! `ingest_job` / `ingest_attempt` / `content_mutation` / `ops_async_operation`
//! and never these runtime rows. An attempt that reached a terminal state
//! therefore left its runtime execution `accepted` / `running` and its
//! extraction `processing` forever. Two entry points close that defect:
//!
//! * [`terminalize_runtime_rows_for_attempt`] — called from every attempt
//!   finalizer chokepoint inside the same transaction that terminalizes the
//!   attempt.
//! * [`reconcile_orphan_runtime_rows_once`] — a bounded, idempotent sweeper for
//!   the historical backlog and for rows whose activation attempt is already
//!   terminal or absent. State guards make both passes idempotent.
//!
//! There is no `attempt_id` column on either table and none is added: the
//! attempt identity lives in the extraction JSON and the owner link, exactly as
//! the graph-extraction pipeline writes it.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Matches the canonical lower/upper-case UUID text form. `activated_by_attempt_id`
/// is always written as a UUID string, but a malformed historical row must be
/// classified as "attempt absent", not abort the sweep with a cast error.
const UUID_TEXT_PATTERN: &str =
    "'^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'";

pub const RECONCILED_ORPHAN_FAILURE_CODE: &str = "reconciled_orphaned_attempt";

/// Counts of runtime rows closed by one [`terminalize_runtime_rows_for_attempt`]
/// call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeRowClosure {
    pub extractions_closed: u64,
    pub executions_closed: u64,
}

/// Counts of runtime rows closed by one
/// [`reconcile_orphan_runtime_rows_once`] batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrphanRuntimeCounts {
    pub extractions_terminalized: u64,
    pub executions_terminalized: u64,
}

/// Terminalizes every non-terminal runtime row activated by `attempt_id`.
///
/// * `runtime_graph_extraction` rows that are still `processing` and carry
///   `activated_by_attempt_id = attempt_id` move to `extraction_status`
///   (`canceled` for a canceled attempt, otherwise `failed`) and keep any
///   existing `error_message`.
/// * The `runtime_execution` rows those extractions point at move to
///   `execution_state` when they are still `accepted` / `running`; their
///   `active_stage` is cleared, `failure_code` is set and `completed_at` is
///   stamped.
///
/// The `&mut PgConnection` bound lets callers pass `&mut *tx` so the closure
/// commits with the attempt finalization, or `&mut *pool.acquire()` for a
/// standalone best-effort call. The `status` / `lifecycle_state` guards make
/// repeated calls no-ops.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime rows.
pub async fn terminalize_runtime_rows_for_attempt(
    connection: &mut PgConnection,
    attempt_id: Uuid,
    extraction_status: &str,
    execution_state: &str,
    failure_code: &str,
) -> Result<RuntimeRowClosure, sqlx::Error> {
    // Binds are per statement: this one uses $1..$3 only. Referencing $4 while
    // leaving $3 unbind-referenced makes PostgreSQL reject the prepared
    // statement with `could not determine data type of parameter $2`, so
    // `failure_code` is $3 here and `execution_state` is bound only by the
    // execution update below.
    let execution_ids = sqlx::query_scalar::<_, Uuid>(
        "update runtime_graph_extraction
         set status = $2,
             error_message = coalesce(error_message, $3)
         where status = 'processing'
           and raw_output_json->'lifecycle'->>'activated_by_attempt_id' = $1::text
         returning runtime_execution_id",
    )
    .bind(attempt_id)
    .bind(extraction_status)
    .bind(failure_code)
    .fetch_all(&mut *connection)
    .await?;

    let extractions_closed = u64::try_from(execution_ids.len()).unwrap_or(u64::MAX);

    let executions_closed = if execution_ids.is_empty() {
        0
    } else {
        sqlx::query(
            "update runtime_execution
             set lifecycle_state = $2::runtime_lifecycle_state,
                 active_stage = null,
                 failure_code = $3,
                 completed_at = now()
             where id = any($1)
               and lifecycle_state in ('accepted', 'running')",
        )
        .bind(&execution_ids)
        .bind(execution_state)
        .bind(failure_code)
        .execute(&mut *connection)
        .await?
        .rows_affected()
    };

    Ok(RuntimeRowClosure { extractions_closed, executions_closed })
}

/// One bounded sweep of runtime rows whose activation attempt is terminal or
/// absent.
///
/// * Extractions still `processing` and older than `stale_before` whose
///   activation attempt is absent or in `('succeeded', 'failed', 'canceled',
///   'abandoned')` move to `canceled` when that attempt was canceled, otherwise
///   `failed`.
/// * Executions still `accepted` / `running`, older than `stale_before`, owned
///   by `graph_extraction_attempt`, whose extraction (via `owner_id`) is terminal
///   or absent, or whose activation attempt is terminal or absent, move to
///   `canceled` when that attempt was canceled, otherwise `failed`, with
///   `failure_code = 'reconciled_orphaned_attempt'`.
///
/// Both updates are capped at `batch_limit` rows and are idempotent: terminal
/// rows no longer satisfy the selection guards.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime rows.
pub async fn reconcile_orphan_runtime_rows_once(
    postgres: &PgPool,
    stale_before: DateTime<Utc>,
    batch_limit: u32,
) -> Result<OrphanRuntimeCounts, sqlx::Error> {
    let limit = i64::from(batch_limit.max(1));
    let mut transaction = postgres.begin().await?;

    let reconciled_extractions = sqlx::query_scalar::<_, Uuid>(sqlx::AssertSqlSafe(format!(
        "with targets as materialized (
             select extraction.id,
                    attempt.attempt_state as attempt_state
             from runtime_graph_extraction as extraction
             left join ingest_attempt as attempt
                 on attempt.id = case
                     when extraction.raw_output_json->'lifecycle'->>'activated_by_attempt_id'
                          ~ {UUID_TEXT_PATTERN}
                     then (extraction.raw_output_json->'lifecycle'->>'activated_by_attempt_id')::uuid
                     else null
                 end
             where extraction.status = 'processing'
               and extraction.created_at < $1
               and (
                   attempt.id is null
                   or attempt.attempt_state in ('succeeded', 'failed', 'canceled', 'abandoned')
               )
             order by extraction.created_at, extraction.id
             limit $2
         )
         update runtime_graph_extraction as extraction
         set status = case
                 when targets.attempt_state = 'canceled' then 'canceled'
                 else 'failed'
             end,
             error_message = coalesce(extraction.error_message, '{RECONCILED_ORPHAN_FAILURE_CODE}')
         from targets
         where extraction.id = targets.id
           and extraction.status = 'processing'
         returning extraction.id",
    )))
    .bind(stale_before)
    .bind(limit)
    .fetch_all(&mut *transaction)
    .await?;

    let reconciled_executions = sqlx::query_scalar::<_, Uuid>(sqlx::AssertSqlSafe(format!(
        "with targets as materialized (
             select execution.id,
                    attempt.attempt_state as attempt_state
             from runtime_execution as execution
             left join runtime_graph_extraction as extraction
                 on extraction.id = execution.owner_id
             left join ingest_attempt as attempt
                 on attempt.id = case
                     when extraction.raw_output_json->'lifecycle'->>'activated_by_attempt_id'
                          ~ {UUID_TEXT_PATTERN}
                     then (extraction.raw_output_json->'lifecycle'->>'activated_by_attempt_id')::uuid
                     else null
                 end
             where execution.lifecycle_state in ('accepted', 'running')
               and execution.accepted_at < $1
               and execution.owner_kind = 'graph_extraction_attempt'
               -- Both traces must be terminal/absent. The extraction can reach
               -- `ready` while its execution is still running, so an
               -- extraction-terminal condition alone would close a live
               -- execution; the activating attempt is the authority.
               and (
                   attempt.id is null
                   or attempt.attempt_state in ('succeeded', 'failed', 'canceled', 'abandoned')
               )
               and (
                   extraction.id is null
                   or extraction.status in ('ready', 'failed', 'canceled')
               )
             order by execution.accepted_at, execution.id
             limit $2
         )
         update runtime_execution as execution
         set lifecycle_state = case
                 when targets.attempt_state = 'canceled' then 'canceled'
                 else 'failed'
             end::runtime_lifecycle_state,
             failure_code = '{RECONCILED_ORPHAN_FAILURE_CODE}',
             active_stage = null,
             completed_at = now()
         from targets
         where execution.id = targets.id
           and execution.lifecycle_state in ('accepted', 'running')
         returning execution.id",
    )))
    .bind(stale_before)
    .bind(limit)
    .fetch_all(&mut *transaction)
    .await?;

    transaction.commit().await?;

    Ok(OrphanRuntimeCounts {
        extractions_terminalized: u64::try_from(reconciled_extractions.len()).unwrap_or(u64::MAX),
        executions_terminalized: u64::try_from(reconciled_executions.len()).unwrap_or(u64::MAX),
    })
}
