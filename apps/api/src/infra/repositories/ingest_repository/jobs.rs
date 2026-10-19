mod lifecycle;
mod queue;

pub use self::{lifecycle::*, queue::*};

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{Acquire, Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

/// Settles a bounded batch of historical failed content jobs that predate
/// atomic preparation finalization. Also used for the exact revision inside
/// the live failure publication transaction, after its job goes terminal.
pub const TERMINAL_PREPARATION_RECONCILE_BATCH: u32 = 256;

pub async fn reconcile_terminal_failed_preparations(postgres: &PgPool) -> Result<u64, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let changed =
        reconcile_terminal_failed_preparations_in_transaction(&mut transaction, None, None).await?;
    transaction.commit().await?;
    Ok(changed)
}

pub async fn reconcile_terminal_failed_preparations_in_transaction(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    revision_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "with candidates as (
             select structured.revision_id
             from knowledge_structured_revision structured
             join knowledge_revision revision on revision.revision_id = structured.revision_id
             join knowledge_document document on document.document_id = revision.document_id
             join content_document_head head on head.document_id = revision.document_id
             where ($1::uuid is null or structured.revision_id = $1)
               and ($3::uuid is null or structured.library_id = $3)
               and structured.preparation_state = 'preparing'
               and structured.revision_id is distinct from document.readable_revision_id
               and structured.revision_id is distinct from document.active_revision_id
               and structured.revision_id is distinct from head.readable_revision_id
               and structured.revision_id is distinct from head.active_revision_id
               and exists (
                   select 1 from ingest_job job
                   where job.knowledge_revision_id = structured.revision_id
                     and job.job_kind = 'content_mutation'
                     and job.queue_state = 'failed'
               )
               and not exists (
                   select 1 from ingest_job job
                   where job.knowledge_revision_id = structured.revision_id
                     and job.job_kind = 'content_mutation'
                     and job.queue_state in ('queued', 'leased', 'paused', 'completed')
               )
             order by structured.revision_id
              limit $2
             for update of structured skip locked
         )
         update knowledge_structured_revision structured
         set preparation_state = 'failed', updated_at = now()
         from candidates
         where structured.revision_id = candidates.revision_id",
    )
    .bind(revision_id)
    .bind(i64::from(TERMINAL_PREPARATION_RECONCILE_BATCH))
    .bind(library_id)
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected())
}

#[derive(Debug, Clone, FromRow)]
pub struct IngestJobRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
    pub connector_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub queued_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    pub queue_leased_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    pub queue_lease_token: Option<String>,
    #[sqlx(default)]
    pub queue_lease_owner: Option<String>,
}

pub(crate) const INGEST_JOB_COLUMNS: &str = "
            id,
            workspace_id,
            library_id,
            mutation_id,
            mutation_item_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at,
            queue_leased_at,
            queue_lease_token,
            queue_lease_owner";

pub(crate) const INGEST_JOB_UPDATE_ASSIGNMENTS: &str = "
             mutation_id = $2,
             connector_id = $3,
             async_operation_id = $4,
             knowledge_document_id = $5,
             knowledge_revision_id = $6,
             job_kind = $7::ingest_job_kind,
             queue_state = $8::ingest_queue_state,
             priority = $9,
             dedupe_key = $10,
             available_at = $11,
             completed_at = $12,
             queue_leased_at = case when $8::ingest_queue_state = 'leased' then queue_leased_at else null end,
             queue_lease_token = case when $8::ingest_queue_state = 'leased' then queue_lease_token else null end,
             queue_lease_owner = case when $8::ingest_queue_state = 'leased' then queue_lease_owner else null end";

#[derive(Debug, Clone)]
pub struct NewIngestJob {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
    pub connector_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub queued_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpdateIngestJob {
    pub mutation_id: Option<Uuid>,
    pub connector_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub available_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IngestQueueItemRow {
    pub job_id: Uuid,
    pub workspace_id: Uuid,
    pub workspace_name: String,
    pub library_id: Uuid,
    pub library_name: String,
    pub knowledge_document_id: Option<Uuid>,
    pub document_name: Option<String>,
    pub job_kind: String,
    pub queue_state: String,
    pub priority: i32,
    pub queue_rank: i64,
    pub queue_position: Option<i64>,
    pub queued_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub queue_leased_at: Option<DateTime<Utc>>,
    pub queue_lease_token: Option<String>,
    pub queue_lease_owner: Option<String>,
    pub attempt_id: Option<Uuid>,
    pub attempt_number: Option<i32>,
    pub attempt_state: Option<String>,
    pub current_stage: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub progress_percent: Option<i32>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMoveDirection {
    Up,
    Down,
    Top,
    Bottom,
}

pub async fn create_ingest_job(
    postgres: &PgPool,
    input: &NewIngestJob,
) -> Result<IngestJobRow, sqlx::Error> {
    create_ingest_job_with_executor(postgres, input).await
}

/// Enqueues a job for an active library unless the same dedupe key already
/// belongs to a live job.
///
/// A terminal collision is retired inside the enqueue transaction so the new
/// job can reuse the stable key. Graph-refresh keys retain the prior key plus a
/// terminal job-id suffix because automatic retry/dead-letter policy consumes
/// that durable versioned history; other job kinds clear the retired key.
/// Locking the catalog parent prevents a delete from archiving the library
/// between the lifecycle check and the insert. The savepoint keeps the
/// transaction usable after PostgreSQL reports the initial unique-index
/// violation.
pub async fn enqueue_active_library_ingest_job_with_live_dedupe(
    postgres: &PgPool,
    input: &NewIngestJob,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    enqueue_active_library_ingest_job_with_live_dedupe_and_graph_capacity(postgres, input, None)
        .await
}

/// Enqueues an automatic graph-refresh job only while the cluster-wide live
/// graph-refresh count remains below `max_live_graph_refresh_jobs`.
///
/// The same advisory transaction lock serializes the check-and-insert with
/// graph-refresh operator enqueues and scheduler tick reservation. Explicit
/// jobs may intentionally exceed the automatic ceiling, but if they linearize
/// first they consume capacity observed by the automatic insert.
pub async fn enqueue_automatic_graph_refresh_job_with_live_dedupe(
    postgres: &PgPool,
    input: &NewIngestJob,
    max_live_graph_refresh_jobs: u32,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    if input.job_kind != "graph_refresh" {
        return Err(sqlx::Error::Protocol(
            "automatic graph-refresh admission received another job kind".to_string(),
        ));
    }
    enqueue_active_library_ingest_job_with_live_dedupe_and_graph_capacity(
        postgres,
        input,
        Some(max_live_graph_refresh_jobs),
    )
    .await
}

async fn enqueue_active_library_ingest_job_with_live_dedupe_and_graph_capacity(
    postgres: &PgPool,
    input: &NewIngestJob,
    max_live_graph_refresh_jobs: Option<u32>,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    let dedupe_key = input.dedupe_key.as_deref();
    let mut transaction = postgres.begin().await?;
    if !admit_graph_refresh_capacity(&mut transaction, input, max_live_graph_refresh_jobs).await? {
        transaction.commit().await?;
        return Ok(None);
    }
    let active_library = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1
           and lifecycle_state = 'active'::catalog_library_lifecycle_state
         for key share",
    )
    .bind(input.library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if active_library.is_none() {
        transaction.commit().await?;
        return Ok(None);
    }
    let Some(dedupe_key) = dedupe_key else {
        let job = create_ingest_job_with_executor(&mut *transaction, input).await?;
        transaction.commit().await?;
        return Ok(Some(job));
    };

    loop {
        let mut savepoint = transaction.begin().await?;
        match create_ingest_job_with_executor(&mut *savepoint, input).await {
            Ok(job) => {
                savepoint.commit().await?;
                transaction.commit().await?;
                return Ok(Some(job));
            }
            Err(sqlx::Error::Database(database_error))
                if database_error.is_unique_violation()
                    && database_error.constraint() == Some("idx_ingest_job_dedupe_key") =>
            {
                savepoint.rollback().await?;
            }
            Err(error) => {
                savepoint.rollback().await?;
                return Err(error);
            }
        }

        if retire_terminal_dedupe_collision(&mut transaction, input.library_id, dedupe_key).await? {
            transaction.commit().await?;
            return Ok(None);
        }
    }
}

/// Takes the cluster-wide graph-refresh admission advisory lock and enforces the
/// automatic live-refresh ceiling for an automatic graph-refresh enqueue.
/// Returns `false` when the ceiling is already reached and the caller must not
/// insert. Non-graph-refresh kinds return `true` without taking the lock.
async fn admit_graph_refresh_capacity(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    input: &NewIngestJob,
    max_live_graph_refresh_jobs: Option<u32>,
) -> Result<bool, sqlx::Error> {
    if input.job_kind != "graph_refresh" {
        return Ok(true);
    }
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(GRAPH_REEXTRACT_ADMISSION_LOCK_NAMESPACE)
        .execute(&mut **transaction)
        .await?;
    let Some(max_live_graph_refresh_jobs) = max_live_graph_refresh_jobs else {
        return Ok(true);
    };
    let live_jobs = count_live_graph_refresh_jobs(transaction).await?;
    Ok(live_jobs < i64::from(max_live_graph_refresh_jobs))
}

/// Resolves the terminal collision left by a rejected duplicate key. A still
/// live dedupe key means the caller must not enqueue (returns `true`); a
/// terminal collision is retired in place so the new job reuses the key
/// (returns `false`). Graph-refresh keys keep their prior key plus the
/// terminating job-id suffix; other kinds clear the key.
async fn retire_terminal_dedupe_collision(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    library_id: Uuid,
    dedupe_key: &str,
) -> Result<bool, sqlx::Error> {
    let collision = sqlx::query_as::<_, (Uuid, Option<DateTime<Utc>>, String)>(
        "select id, completed_at, job_kind::text
         from ingest_job
         where library_id = $1 and dedupe_key = $2
         for update",
    )
    .bind(library_id)
    .bind(dedupe_key)
    .fetch_optional(&mut **transaction)
    .await?;

    match collision {
        Some((_, None, _)) => Ok(true),
        Some((job_id, Some(_), job_kind)) => {
            sqlx::query(
                "update ingest_job
                 set dedupe_key = case
                     when $2 = 'graph_refresh'
                     then dedupe_key || ':terminal:' || id::text
                     else null
                 end
                 where id = $1 and completed_at is not null",
            )
            .bind(job_id)
            .bind(job_kind)
            .execute(&mut **transaction)
            .await?;
            Ok(false)
        }
        None => Ok(false),
    }
}

async fn count_live_graph_refresh_jobs(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from ingest_job
         where job_kind = 'graph_refresh'::ingest_job_kind
           and queue_state in (
               'queued'::ingest_queue_state,
               'leased'::ingest_queue_state,
               'paused'::ingest_queue_state
           )
           and completed_at is null",
    )
    .fetch_one(&mut **transaction)
    .await
}

/// Creates a queue job using the caller's pool connection or transaction.
///
/// Transaction-aware callers use this to commit a job and its owning domain
/// record atomically.
pub async fn create_ingest_job_with_executor<'e, E>(
    executor: E,
    input: &NewIngestJob,
) -> Result<IngestJobRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "insert into ingest_job (
            id,
            workspace_id,
            library_id,
            mutation_id,
            mutation_item_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind,
            queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7,
            $8,
            $9,
            $10::ingest_job_kind,
            $11::ingest_queue_state,
            $12,
            $13,
            coalesce($14, now()),
            coalesce($15, now()),
            $16
        )
        returning
            {INGEST_JOB_COLUMNS}",
    )))
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
    .bind(input.mutation_item_id)
    .bind(input.connector_id)
    .bind(input.async_operation_id)
    .bind(input.knowledge_document_id)
    .bind(input.knowledge_revision_id)
    .bind(&input.job_kind)
    .bind(&input.queue_state)
    .bind(input.priority)
    .bind(&input.dedupe_key)
    .bind(input.queued_at)
    .bind(input.available_at)
    .bind(input.completed_at)
    .fetch_one(executor)
    .await
}

pub async fn get_ingest_job_by_id(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "select
            {INGEST_JOB_COLUMNS}
         from ingest_job
         where id = $1",
    )))
    .bind(job_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_ingest_job_by_dedupe_key(
    postgres: &PgPool,
    library_id: Uuid,
    dedupe_key: &str,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "select
            {INGEST_JOB_COLUMNS}
         from ingest_job
         where library_id = $1
           and dedupe_key = $2
         order by queued_at desc, id desc
         limit 1",
    )))
    .bind(library_id)
    .bind(dedupe_key)
    .fetch_optional(postgres)
    .await
}

/// Reports whether one library still has non-terminal work of `job_kind`.
/// Paused jobs intentionally count as live: an operator pause must not cause a
/// background scheduler to enqueue another repair batch behind it.
pub async fn has_live_ingest_jobs_by_kind(
    postgres: &PgPool,
    library_id: Uuid,
    job_kind: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists (
             select 1
             from ingest_job
             where library_id = $1
               and job_kind = $2::ingest_job_kind
               and queue_state in ('queued', 'leased', 'paused')
               and completed_at is null
         )",
    )
    .bind(library_id)
    .bind(job_kind)
    .fetch_one(postgres)
    .await
}

const GRAPH_REEXTRACT_ADMISSION_LOCK_NAMESPACE: &str =
    "ironrag:graph-reextract-periodic-admission-tick";
const GRAPH_REEXTRACT_CURSOR_LIBRARY_KEY: &str = "last_inspected_library_id";
const GRAPH_REEXTRACT_RESERVATION_TOKEN_KEY: &str = "reservation_token";
/// A healthy bounded admission pass should finish well inside this window.
/// Keeping the lease much longer than the two-minute tick prevents overlapping
/// replicas from repeatedly superseding one another, while still allowing a
/// crashed scheduler to recover without operator intervention.
const GRAPH_REEXTRACT_RESERVATION_LEASE: Duration = Duration::from_mins(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphReextractAdmissionReservation {
    pub start_index: usize,
    pub token: Uuid,
    /// Automatic jobs which may still be inserted without exceeding the
    /// cluster-wide live graph-refresh ceiling observed under the admission
    /// lock. Queued, leased, and operator-paused work in every library consumes
    /// this capacity.
    pub remaining_capacity: u32,
}

#[must_use]
pub(crate) fn next_graph_reextract_start_index(
    sorted_library_ids: &[Uuid],
    last_inspected_library_id: Option<Uuid>,
) -> usize {
    if sorted_library_ids.is_empty() {
        return 0;
    }
    let Some(last_inspected_library_id) = last_inspected_library_id else {
        return 0;
    };
    match sorted_library_ids.binary_search(&last_inspected_library_id) {
        Ok(index) => index.saturating_add(1) % sorted_library_ids.len(),
        Err(index) => index % sorted_library_ids.len(),
    }
}

/// Reserves one cluster-wide periodic graph-repair admission tick.
///
/// The transaction uses a non-blocking advisory lock, reads the durable UUID
/// rotation cursor in the existing singleton `maintenance_job_run` state row,
/// stamps a reservation token, and records the next cluster-wide due time. It
/// commits before any per-library scan or enqueue begins. A second short
/// conditional update advances past the last library actually inspected.
/// Scheduler replicas and restarted processes therefore share one fair
/// rotation without holding a control connection while the main pool performs
/// the potentially long scan.
pub async fn try_reserve_graph_reextract_admission_tick(
    postgres: &PgPool,
    sorted_library_ids: &[Uuid],
    tick_interval: Duration,
    max_live_graph_refresh_jobs: u32,
) -> Result<Option<GraphReextractAdmissionReservation>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let admission_class =
        crate::services::maintenance::lease::MaintenanceClass::GraphReextractAdmission.as_str();
    let acquired = sqlx::query_scalar::<_, bool>(
        "select pg_try_advisory_xact_lock(hashtextextended($1::text, 0))",
    )
    .bind(GRAPH_REEXTRACT_ADMISSION_LOCK_NAMESPACE)
    .fetch_one(&mut *transaction)
    .await?;
    if !acquired {
        transaction.rollback().await?;
        return Ok(None);
    }

    sqlx::query(
        "insert into maintenance_job_run (class, scope_kind, scope_id, next_due_at)
         values ($1, 'instance', null, clock_timestamp())
         on conflict do nothing",
    )
    .bind(admission_class)
    .execute(&mut *transaction)
    .await?;

    let reservation_lease_seconds = GRAPH_REEXTRACT_RESERVATION_LEASE.as_secs() as f64;
    let (cursor_json, is_due, reservation_is_active) =
        sqlx::query_as::<_, (serde_json::Value, bool, bool)>(
            "select cursor_json,
                next_due_at <= clock_timestamp(),
                cursor_json ? $2::text
                  and last_started_at is not null
                  and last_started_at
                        > clock_timestamp() - make_interval(secs => $3::double precision)
         from maintenance_job_run
         where class = $1
           and scope_kind = 'instance'
           and scope_id is null
         for update",
        )
        .bind(admission_class)
        .bind(GRAPH_REEXTRACT_RESERVATION_TOKEN_KEY)
        .bind(reservation_lease_seconds)
        .fetch_one(&mut *transaction)
        .await?;
    if !is_due || reservation_is_active {
        transaction.commit().await?;
        return Ok(None);
    }

    // This count shares the cluster admission transaction and advisory lock
    // with the durable reservation. Scheduler replicas therefore cannot each
    // observe the same free worker wave and refill it independently. Explicit
    // operator jobs are intentionally included, as are paused jobs: they own
    // queue capacity until an operator resumes or terminalizes them.
    let live_graph_refresh_jobs = count_live_graph_refresh_jobs(&mut transaction).await?;
    let live_graph_refresh_jobs = u32::try_from(live_graph_refresh_jobs.max(0)).unwrap_or(u32::MAX);
    let remaining_capacity = max_live_graph_refresh_jobs.saturating_sub(live_graph_refresh_jobs);

    let last_inspected_library_id = cursor_json
        .get(GRAPH_REEXTRACT_CURSOR_LIBRARY_KEY)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    let start_index =
        next_graph_reextract_start_index(sorted_library_ids, last_inspected_library_id);
    let token = Uuid::now_v7();
    let next_cursor_json = last_inspected_library_id.map_or_else(
        || serde_json::json!({ "reservation_token": token.to_string() }),
        |library_id| {
            serde_json::json!({
                "last_inspected_library_id": library_id.to_string(),
                "reservation_token": token.to_string(),
            })
        },
    );
    let tick_interval_seconds = tick_interval.as_secs().max(1) as f64;
    sqlx::query(
        "update maintenance_job_run
         set cursor_json = $2,
             last_started_at = clock_timestamp(),
             next_due_at = clock_timestamp() + make_interval(secs => $3::double precision),
             updated_at = clock_timestamp()
         where class = $1
           and scope_kind = 'instance'
           and scope_id is null",
    )
    .bind(admission_class)
    .bind(next_cursor_json)
    .bind(tick_interval_seconds)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    Ok(Some(GraphReextractAdmissionReservation { start_index, token, remaining_capacity }))
}

/// Advances the shared cursor past the last library actually inspected by a
/// reserved tick. The token predicate prevents an expired/crashed reservation
/// from overwriting the cursor after a recovery replica has replaced it.
pub async fn complete_graph_reextract_admission_tick(
    postgres: &PgPool,
    reservation_token: Uuid,
    last_inspected_library_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    let admission_class =
        crate::services::maintenance::lease::MaintenanceClass::GraphReextractAdmission.as_str();
    let result = sqlx::query(
        "update maintenance_job_run
         set cursor_json = case
                 when $3::uuid is null then cursor_json - $4::text
                 else jsonb_build_object(
                     'last_inspected_library_id', $3::uuid::text
                 )
             end,
             last_completed_at = clock_timestamp(),
             updated_at = clock_timestamp()
         where class = $1
           and scope_kind = 'instance'
           and scope_id is null
           and cursor_json ->> $4::text = $2::uuid::text",
    )
    .bind(admission_class)
    .bind(reservation_token)
    .bind(last_inspected_library_id)
    .bind(GRAPH_REEXTRACT_RESERVATION_TOKEN_KEY)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn get_latest_ingest_job_by_mutation_id(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "select
            {INGEST_JOB_COLUMNS}
         from ingest_job
         where mutation_id = $1
         order by queued_at desc, id desc
         limit 1",
    )))
    .bind(mutation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_latest_ingest_job_by_async_operation_id(
    postgres: &PgPool,
    async_operation_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "select
            {INGEST_JOB_COLUMNS}
         from ingest_job
         where async_operation_id = $1
         order by queued_at desc, id desc
         limit 1",
    )))
    .bind(async_operation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_latest_ingest_job_by_knowledge_revision_id(
    postgres: &PgPool,
    knowledge_revision_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "select
            {INGEST_JOB_COLUMNS}
         from ingest_job
         where knowledge_revision_id = $1
         order by queued_at desc, id desc
         limit 1",
    )))
    .bind(knowledge_revision_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_ingest_jobs_by_knowledge_document_id(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    knowledge_document_id: Uuid,
) -> Result<Vec<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "select
            {INGEST_JOB_COLUMNS}
         from ingest_job
         where workspace_id = $1
           and library_id = $2
           and knowledge_document_id = $3
         order by queued_at desc, id desc",
    )))
    .bind(workspace_id)
    .bind(library_id)
    .bind(knowledge_document_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_ingest_jobs_by_mutation_ids(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    mutation_ids: &[Uuid],
) -> Result<Vec<IngestJobRow>, sqlx::Error> {
    if mutation_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "select
            {INGEST_JOB_COLUMNS}
         from ingest_job
         where workspace_id = $1
           and library_id = $2
           and mutation_id = any($3)
         order by queued_at desc, id desc",
    )))
    .bind(workspace_id)
    .bind(library_id)
    .bind(mutation_ids)
    .fetch_all(postgres)
    .await
}

#[derive(Debug, Clone)]
pub struct IngestJobPage {
    pub rows: Vec<IngestJobRow>,
    pub has_more: bool,
}

/// Keyset-paginated fetch for `GET /v1/ingest/libraries/{libraryId}/jobs`.
///
/// * `limit` is clamped by the caller to a sane page size; this function
///   fetches `limit + 1` rows so `has_more` is derived without a `COUNT(*)`.
/// * `cursor` is `(queued_at, id)` of the last row on the previous page;
///   rows strictly older on the `(queued_at desc, id desc)` keyset are
///   returned. Unlike `list_active_ingest_queue` (a live "currently active"
///   snapshot), this is the full paginated history, including terminal
///   (`completed` / `failed` / `canceled`) jobs.
/// * `status_filter` holds `ingest_queue_state` values; empty = no filter.
pub async fn list_ingest_jobs_page(
    postgres: &PgPool,
    library_id: Uuid,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
    status_filter: &[String],
) -> Result<IngestJobPage, sqlx::Error> {
    let fetch_limit = limit + 1;
    let (cursor_queued_at, cursor_id) =
        cursor.map_or((None, None), |(queued_at, id)| (Some(queued_at), Some(id)));

    let mut rows = sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "select
            {INGEST_JOB_COLUMNS}
         from ingest_job
         where library_id = $1
           and ($2::timestamptz is null or (queued_at, id) < ($2, $3))
           and (cardinality($4::text[]) = 0 or queue_state::text = any($4))
         order by queued_at desc, id desc
         limit $5",
    )))
    .bind(library_id)
    .bind(cursor_queued_at)
    .bind(cursor_id)
    .bind(status_filter)
    .bind(fetch_limit)
    .fetch_all(postgres)
    .await?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(IngestJobPage { rows, has_more })
}

pub async fn list_ingest_jobs(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<IngestJobRow>, sqlx::Error> {
    let effective_limit = limit.unwrap_or(500);
    let effective_offset = offset.unwrap_or(0);

    match (workspace_id, library_id) {
        (Some(workspace_id), Some(library_id)) => {
            sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
                "select
                    {INGEST_JOB_COLUMNS}
                 from ingest_job
                 where workspace_id = $1
                   and library_id = $2
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $3 offset $4",
            )))
            .bind(workspace_id)
            .bind(library_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (Some(workspace_id), None) => {
            sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
                "select
                    {INGEST_JOB_COLUMNS}
                 from ingest_job
                 where workspace_id = $1
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $2 offset $3",
            )))
            .bind(workspace_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (None, Some(library_id)) => {
            sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
                "select
                    {INGEST_JOB_COLUMNS}
                 from ingest_job
                 where library_id = $1
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $2 offset $3",
            )))
            .bind(library_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
                "select
                    {INGEST_JOB_COLUMNS}
                 from ingest_job
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $1 offset $2",
            )))
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
    }
}

#[cfg(test)]
mod graph_reextract_admission_tests {
    use super::*;

    #[test]
    fn durable_cursor_rotates_after_present_and_deleted_library_ids() {
        let library_ids = [Uuid::from_u128(10), Uuid::from_u128(20), Uuid::from_u128(30)];

        assert_eq!(next_graph_reextract_start_index(&library_ids, None), 0);
        assert_eq!(next_graph_reextract_start_index(&library_ids, Some(Uuid::from_u128(10))), 1);
        assert_eq!(next_graph_reextract_start_index(&library_ids, Some(Uuid::from_u128(20))), 2);
        assert_eq!(next_graph_reextract_start_index(&library_ids, Some(Uuid::from_u128(30))), 0);
        assert_eq!(
            next_graph_reextract_start_index(&library_ids, Some(Uuid::from_u128(15))),
            1,
            "a deleted cursor id must resume at its sorted insertion point"
        );
        assert_eq!(
            next_graph_reextract_start_index(&library_ids, Some(Uuid::from_u128(40))),
            0,
            "a deleted cursor beyond the current tail must wrap"
        );
    }

    #[test]
    fn durable_cursor_handles_an_empty_library_set() {
        assert_eq!(next_graph_reextract_start_index(&[], Some(Uuid::from_u128(10))), 0);
    }
}
