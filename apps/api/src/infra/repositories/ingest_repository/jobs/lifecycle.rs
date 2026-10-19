use super::*;

pub async fn pause_ingest_job(postgres: &PgPool, job_id: Uuid) -> Result<Option<()>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    let result = sqlx::query(
        "update ingest_job
         set queue_state = 'paused'::ingest_queue_state,
             available_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state in ('queued', 'leased')
           and completed_at is null",
    )
    .bind(job_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if result.rows_affected() == 0 { Ok(None) } else { Ok(Some(())) }
}

pub async fn resume_ingest_job(postgres: &PgPool, job_id: Uuid) -> Result<Option<()>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    let result = sqlx::query(
        "update ingest_job
         set queue_state = 'queued'::ingest_queue_state,
             available_at = now(),
             completed_at = null,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'paused'
           and completed_at is null
           and not exists (
               select 1
               from ingest_attempt a
               where a.job_id = ingest_job.id
                 and a.attempt_state in ('leased', 'running')
           )",
    )
    .bind(job_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if result.rows_affected() == 0 { Ok(None) } else { Ok(Some(())) }
}

pub async fn retry_or_requeue_ingest_job(
    postgres: &PgPool,
    job_id: Uuid,
    stale_threshold: chrono::Duration,
    available_at: DateTime<Utc>,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    let stale_seconds = stale_threshold.num_seconds().max(1);
    let mut tx = postgres.begin().await?;
    let target = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_job
         where id = $1
           and queue_state in ('queued', 'paused', 'leased')
         for update",
    )
    .bind(job_id)
    .fetch_optional(&mut *tx)
    .await?;

    if target.is_none() {
        tx.commit().await?;
        return Ok(None);
    }

    let row = sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "update ingest_job j
         set queue_state = 'queued'::ingest_queue_state,
             available_at = $3,
             completed_at = null,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where j.id = $1
           and j.queue_state in ('queued', 'paused', 'leased')
           and (
               j.queue_state in ('queued', 'paused')
               or (
                   j.queue_state = 'leased'
                   and coalesce(j.queue_leased_at, j.queued_at)
                       < now() - make_interval(secs => $2::double precision)
                   and not exists (
                       select 1
                       from ingest_attempt active_attempt
                       where active_attempt.job_id = j.id
                         and active_attempt.attempt_state in ('leased', 'running')
                   )
               )
           )
         returning
            {INGEST_JOB_COLUMNS}",
    )))
    .bind(job_id)
    .bind(stale_seconds as f64)
    .bind(available_at)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn cancel_ingest_job(postgres: &PgPool, job_id: Uuid) -> Result<u64, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    let (affected, canceled_attempt_ids) = sqlx::query_as::<_, (i64, Vec<Uuid>)>(
        "with target_job as materialized (
             select id from ingest_job
             WHERE id = $1
               AND queue_state IN ('queued', 'leased', 'paused')
               AND completed_at IS NULL
             for update
         ),
         job_canceled as (
             update ingest_job
             set queue_state = 'canceled',
                 completed_at = now(),
                 queue_leased_at = null,
                 queue_lease_token = null,
                 queue_lease_owner = null
             where id in (select id from target_job)
             returning id
         ),
         attempts_canceled AS (
             UPDATE ingest_attempt
             SET attempt_state = 'canceled',
                 failure_class = 'ingest_queue',
                 failure_code = 'canceled_by_operator',
                 failure_message = 'Processing was canceled from the administration queue',
                 finished_at = now()
             WHERE job_id IN (SELECT id FROM job_canceled)
               AND attempt_state IN ('leased', 'running')
             returning id
         )
         select
             (select count(*)::bigint from job_canceled),
             coalesce((select array_agg(id) from attempts_canceled), '{}'::uuid[])",
    )
    .bind(job_id)
    .fetch_one(&mut *tx)
    .await?;

    // The attempt rows are already canceled; close the graph-extraction runtime
    // rows they activated in the same transaction.
    for attempt_id in canceled_attempt_ids {
        crate::infra::repositories::runtime_graph_repository::terminalize_runtime_rows_for_attempt(
            &mut tx,
            attempt_id,
            "canceled",
            "canceled",
            "canceled_by_operator",
        )
        .await?;
    }

    tx.commit().await?;
    u64::try_from(affected)
        .map_err(|_| sqlx::Error::Protocol("negative canceled ingest job count".to_string()))
}

/// Outcome of one stale-lease recovery sweep, split by what it did to each row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleIngestLeaseRecovery {
    /// Jobs pushed back to `queued` for another worker.
    pub requeued: u64,
    /// Already-terminal jobs whose orphaned leased attempt was failed.
    pub orphaned: u64,
}

pub async fn recover_stale_ingest_leases(
    postgres: &PgPool,
    stale_threshold: chrono::Duration,
) -> Result<StaleIngestLeaseRecovery, sqlx::Error> {
    let stale_seconds = stale_threshold.num_seconds().max(1);

    // Two classes of stale-lease recovery in one statement:
    //
    //   1. Job and attempt both `leased`: a worker held the lease and
    //      stopped heartbeating (crash mid-stage, runtime starvation,
    //      network partition to Postgres). We mark the attempt
    //      `failed` with `lease_expired`/`stale_heartbeat` AND push
    //      the job back to `queued` so another worker picks it up.
    //
    //   2. Attempt `leased`, but job already reached a terminal state
    //      (`completed`, `failed`, `canceled`): the previous worker
    //      crashed between writing the terminal job row and
    //      finalising its own attempt. The job is done — re-queuing
    //      would double-process the document. We only clean up the
    //      orphaned attempt so it stops occupying an `active
    //      operations` slot on the dashboard and the dedicated
    //      heartbeat pool. Without this branch such attempts stayed
    //      `leased` forever (observed in the field: attempt rows
    //      with multi-hour heartbeat staleness pinned to jobs that
    //      completed hours earlier).
    //
    // Recovery marks stale leased attempts with the standard stale-heartbeat
    // failure metadata. Leased jobs are requeued only after the active-attempt
    // guard is rechecked in the same transaction.
    let mut tx = postgres.begin().await?;
    let locked_job_ids = sqlx::query_scalar::<_, Uuid>(
        "select j.id
         from ingest_job j
         where exists (
             select 1
             from ingest_attempt a
             where a.job_id = j.id
               and a.attempt_state = 'leased'
               and a.heartbeat_at < now() - make_interval(secs => $1::double precision)
               and j.queue_state in ('leased', 'completed', 'failed', 'canceled')
         )
         or (
             j.queue_state = 'leased'
             and coalesce(j.queue_leased_at, j.queued_at)
                 < now() - make_interval(secs => $1::double precision)
         )
         order by j.id
         for update of j",
    )
    .bind(stale_seconds as f64)
    .fetch_all(&mut *tx)
    .await?;

    if locked_job_ids.is_empty() {
        tx.commit().await?;
        return Ok(StaleIngestLeaseRecovery { requeued: 0, orphaned: 0 });
    }

    // Count the already-terminal-job attempts before the attempt UPDATE flips
    // them from `leased` to `failed`. They are the orphaned half of recovery;
    // jobs requeued below stay `queued` and are excluded by `j.queue_state`.
    let orphaned_count: i64 = sqlx::query_scalar(
        "select count(*)::bigint
         from ingest_attempt a
         join ingest_job j on j.id = a.job_id
         where a.job_id = any($2)
           and a.attempt_state = 'leased'
           and a.heartbeat_at < now() - make_interval(secs => $1::double precision)
           and j.queue_state in ('completed', 'failed', 'canceled')",
    )
    .bind(stale_seconds as f64)
    .bind(&locked_job_ids)
    .fetch_one(&mut *tx)
    .await?;
    let orphaned = u64::try_from(orphaned_count)
        .map_err(|_| sqlx::Error::Protocol("negative orphaned ingest attempt count".to_string()))?;

    let reaped_attempt_ids = sqlx::query_scalar::<_, Uuid>(
        "update ingest_attempt a
         set attempt_state = 'failed',
             failure_class = 'lease_expired',
             failure_code = 'stale_heartbeat',
             failure_message = 'Attempt heartbeat expired before processing finished',
             finished_at = now(),
             retryable = true
         where a.job_id = any($2)
           and a.attempt_state = 'leased'
           and a.heartbeat_at < now() - make_interval(secs => $1::double precision)
           and exists (
               select 1
               from ingest_job j
               where j.id = a.job_id
                 and j.queue_state in ('leased', 'completed', 'failed', 'canceled')
           )
         returning a.id",
    )
    .bind(stale_seconds as f64)
    .bind(&locked_job_ids)
    .fetch_all(&mut *tx)
    .await?;

    // The reaped attempts are now failed; close the graph-extraction runtime
    // rows they activated in the same transaction.
    for attempt_id in reaped_attempt_ids {
        crate::infra::repositories::runtime_graph_repository::terminalize_runtime_rows_for_attempt(
            &mut tx,
            attempt_id,
            "failed",
            "failed",
            "stale_heartbeat",
        )
        .await?;
    }

    let result = sqlx::query(
        "update ingest_job j
         set queue_state = 'queued',
             available_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where j.id = any($2)
           and j.queue_state = 'leased'
           and (
               coalesce(j.queue_leased_at, j.queued_at)
                   < now() - make_interval(secs => $1::double precision)
               or exists (
                   select 1
                   from ingest_attempt stale_attempt
                   where stale_attempt.job_id = j.id
                     and stale_attempt.attempt_state = 'failed'
                     and stale_attempt.heartbeat_at
                         < now() - make_interval(secs => $1::double precision)
               )
           )
           and not exists (
               select 1
               from ingest_attempt active_attempt
               where active_attempt.job_id = j.id
                 and active_attempt.attempt_state in ('leased', 'running')
           )",
    )
    .bind(stale_seconds as f64)
    .bind(&locked_job_ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StaleIngestLeaseRecovery { requeued: result.rows_affected(), orphaned })
}

/// Marks every **non-terminal** ingest job tied to `mutation_id` as failed AND
/// finalizes any attached leased attempts, mirroring the cancel path above.
///
/// Called when a mutation is reconciled as terminally failed. Without it the
/// reconcile terminalized the async operation, the mutation, its items and the
/// revision readiness while leaving the job `queued` — a retryable attempt is
/// enough to produce that shape. The next worker to claim such a job cannot
/// lease an attempt (the lease refuses to drag a terminal operation back to
/// `processing`), the failure is deferred back to lifecycle recovery, and the
/// job returns to the queue at the head of its library. Two of those exhaust
/// `ingestion_max_parallel_jobs_per_library` and nothing else in the library is
/// ever claimed again.
///
/// `failed` rather than `canceled`: the pipeline rejected this work, the
/// operator did not withdraw it, and the dashboard's attention bucket should
/// say so.
pub async fn fail_jobs_for_mutation(
    postgres: &PgPool,
    mutation_id: Uuid,
    failure_code: &str,
    failure_message: &str,
) -> Result<u64, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let (affected, revisions) = sqlx::query_as::<_, (i64, Option<Vec<Uuid>>)>(
        "with target_jobs as materialized (
             select j.id from ingest_job j
             where j.mutation_id = $1
               and j.queue_state in ('queued', 'leased', 'paused')
             order by j.id
             for update of j
         ),
         jobs_failed as (
             update ingest_job
             set queue_state = 'failed',
                 completed_at = now(),
                 queue_leased_at = null,
                 queue_lease_token = null,
                 queue_lease_owner = null
             where id in (select id from target_jobs)
              returning id, knowledge_revision_id
         ),
         attempts_failed as (
             update ingest_attempt
             set attempt_state = 'failed',
                 failure_class = 'content_mutation',
                 failure_code = $2,
                 failure_message = $3,
                 finished_at = now(),
                 retryable = false
             where job_id in (select id from jobs_failed)
               and attempt_state in ('leased', 'running')
             returning id
         )
          select count(*)::bigint
                 + (select count(*) * 0 from attempts_failed),
                 array_agg(distinct knowledge_revision_id) filter (where knowledge_revision_id is not null)
          from jobs_failed",
    )
    .bind(mutation_id)
    .bind(failure_code)
    .bind(failure_message)
    .fetch_one(&mut *transaction)
    .await?;
    if let Some(revisions) = revisions {
        for revision_id in revisions {
            super::reconcile_terminal_failed_preparations_in_transaction(
                &mut transaction,
                Some(revision_id),
                None,
            )
            .await?;
        }
    }
    transaction.commit().await?;
    u64::try_from(affected)
        .map_err(|_| sqlx::Error::Protocol("negative failed ingest job count".to_string()))
}

/// Terminal disposition applied to the mutations, items, async operations and
/// graph-extraction runtime rows attached to the jobs a document cancellation
/// retires.
///
/// The cancellation itself only knows *that* the document's work is being
/// stopped; the caller knows *why*, and the reason decides the terminal state
/// and failure code every owner row must carry. Without the disposition the
/// `content_mutation` / `content_mutation_item` / `ops_async_operation` rows
/// outlived their canceled jobs forever.
#[derive(Debug, Clone, Copy)]
pub struct CancelDisposition<'a> {
    /// Terminal `content_mutation.mutation_state` (`canceled` or `failed`).
    pub mutation_state: &'a str,
    /// Terminal `content_mutation_item.item_state` (`skipped` or `failed`).
    pub item_state: &'a str,
    /// Terminal `ops_async_operation.status` (`canceled` or `failed`).
    pub operation_status: &'a str,
    /// `failure_code` written to the mutation, its operation and its runtime rows.
    pub failure_code: &'a str,
    /// `failure_message` written to the canceled attempts and pending items.
    pub failure_message: &'a str,
    /// Whether this cancellation also owns the mutation's terminal state.
    ///
    /// The retry (`SUPERSEDED_BY_RETRY`) and unrecoverable
    /// (`UNRECOVERABLE_NO_SOURCE`) callers immediately run
    /// `reconcile_failed_ingest_mutation`, which owns the mutation, its items,
    /// its operation, the document head and revision readiness. Terminalizing
    /// the mutation here first would make that reconcile a no-op and silently
    /// drop the head/readiness repair, so those two defer (`false`). The
    /// deletion and operator-cancel callers have no reconcile to follow and
    /// must own the mutation here (`true`).
    pub finalize_mutation: bool,
}

impl CancelDisposition<'static> {
    /// Operator requested cancellation over the batch-cancel surface.
    pub const CANCELED_BY_REQUEST: Self = Self {
        mutation_state: "canceled",
        item_state: "skipped",
        operation_status: "canceled",
        failure_code: "canceled_by_request",
        failure_message: "Processing was canceled by request",
        finalize_mutation: true,
    };

    /// The owning document is being deleted, so its work is retired permanently.
    pub const DOCUMENT_DELETED: Self = Self {
        mutation_state: "canceled",
        item_state: "skipped",
        operation_status: "failed",
        failure_code: "document_deleted",
        failure_message: "mutation skipped because document was deleted",
        finalize_mutation: true,
    };

    /// A user retry supersedes the inflight mutation.
    pub const SUPERSEDED_BY_RETRY: Self = Self {
        mutation_state: "failed",
        item_state: "failed",
        operation_status: "failed",
        failure_code: "superseded_by_retry",
        failure_message: "document retry requested by user while previous ingest was still inflight",
        finalize_mutation: false,
    };

    /// An orphan document with no recoverable source is retired.
    pub const UNRECOVERABLE_NO_SOURCE: Self = Self {
        mutation_state: "failed",
        item_state: "failed",
        operation_status: "failed",
        failure_code: "unrecoverable_no_source",
        failure_message: "document has no content_revision rows; nothing to ingest from",
        finalize_mutation: false,
    };
}

/// Marks every **non-terminal** ingest job tied to `document_id` as canceled
/// AND finalizes every owner row of those jobs in one SQL round trip: the
/// attached leased attempts, the `content_mutation` that owns each canceled
/// job (with its pending items and its async operation), and the
/// graph-extraction runtime rows those attempts activated.
///
/// Covers `queued` / `paused` (not currently claimed) and `leased` (a worker
/// currently holds it) jobs. For non-leased rows this is atomically terminal.
/// For leased rows, setting `queue_state='canceled'` is the signal the worker
/// observes on its next heartbeat tick so it can cooperatively drain the
/// current pipeline stage; the attempt-level UPDATE below immediately
/// bookkeeps the attempt as `canceled`, so the stale-lease reaper and the UI
/// activity deriver both see a consistent terminal attempt without waiting for
/// the worker to finish its in-flight LLM call. A subsequent worker-side
/// finalize call becomes a harmless no-op because its WHERE clause filters on
/// `attempt_state='leased'`.
///
/// Terminal states (`completed`, `failed`, already `canceled`) are left alone
/// because nothing useful can be canceled from them.
pub async fn cancel_jobs_for_document(
    postgres: &PgPool,
    document_id: Uuid,
    disposition: CancelDisposition<'_>,
) -> Result<u64, sqlx::Error> {
    cancel_jobs_for_document_with_executor(postgres, document_id, disposition).await
}

pub async fn cancel_jobs_for_document_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
    disposition: CancelDisposition<'_>,
) -> Result<u64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let affected = sqlx::query_scalar::<_, i64>(
        "with target_jobs as materialized (
             SELECT j.id FROM ingest_job j
             WHERE j.mutation_id IN (
                 SELECT m.id FROM content_mutation m
                 JOIN content_mutation_item mi ON mi.mutation_id = m.id
                 WHERE mi.document_id = $1
             )
             AND j.queue_state IN ('queued', 'leased', 'paused')
             AND j.completed_at IS NULL
             order by j.id
             for update of j
         ),
         jobs_canceled as (
             update ingest_job
             set queue_state = 'canceled',
                 completed_at = now(),
                 queue_leased_at = null,
                 queue_lease_token = null,
                 queue_lease_owner = null
             where id in (select id from target_jobs)
             returning id, mutation_id
         ),
         attempts_canceled AS (
             UPDATE ingest_attempt
             SET attempt_state = 'canceled',
                 failure_class = 'content_mutation',
                 failure_code = $2,
                 failure_message = $3,
                 finished_at = now()
             WHERE job_id IN (SELECT id FROM jobs_canceled)
               AND attempt_state IN ('leased', 'running')
             returning id
         ),
         runtime_extractions AS (
             UPDATE runtime_graph_extraction
             SET status = 'canceled',
                 error_message = coalesce(error_message, $2)
             WHERE status = 'processing'
               AND raw_output_json->'lifecycle'->>'activated_by_attempt_id'
                   IN (SELECT id::text FROM attempts_canceled)
             returning runtime_execution_id
         ),
         runtime_executions AS (
             UPDATE runtime_execution
             SET lifecycle_state = 'canceled',
                 active_stage = null,
                 failure_code = $2,
                 completed_at = now()
             WHERE id IN (SELECT runtime_execution_id FROM runtime_extractions)
               AND lifecycle_state IN ('accepted', 'running')
             returning id
         ),
         mutations_finalized AS (
             UPDATE content_mutation
             SET mutation_state = $4::content_mutation_state,
                 completed_at = now(),
                 failure_code = $2
             WHERE $7
               AND id IN (
                   SELECT DISTINCT mutation_id FROM jobs_canceled
                   WHERE mutation_id IS NOT NULL
               )
               AND mutation_state IN ('accepted', 'running')
               AND NOT EXISTS (
                   SELECT 1 FROM ingest_job remaining
                   WHERE remaining.mutation_id = content_mutation.id
                     AND remaining.queue_state IN ('queued', 'leased', 'paused')
                     AND remaining.completed_at IS NULL
                     AND remaining.id NOT IN (SELECT id FROM jobs_canceled)
               )
               AND NOT EXISTS (
                   SELECT 1 FROM content_mutation_item sibling
                   WHERE sibling.mutation_id = content_mutation.id
                     AND sibling.document_id <> $1
               )
             returning id
         ),
         items_finalized AS (
             UPDATE content_mutation_item
             SET item_state = $5::content_mutation_item_state,
                 message = coalesce(message, $3)
             WHERE mutation_id IN (SELECT id FROM mutations_finalized)
               AND item_state = 'pending'
             returning id
         ),
         document_items_skipped AS (
             UPDATE content_mutation_item
             SET item_state = 'skipped',
                 message = coalesce(message, $3)
             WHERE $7
               AND document_id = $1
               AND item_state = 'pending'
               AND mutation_id IN (
                   SELECT DISTINCT mutation_id FROM jobs_canceled
                   WHERE mutation_id IS NOT NULL
               )
               AND mutation_id NOT IN (SELECT id FROM mutations_finalized)
             returning id
         ),
         operations_finalized AS (
             UPDATE ops_async_operation
             SET status = $6::ops_async_operation_status,
                 completed_at = now(),
                 failure_code = $2
             WHERE subject_kind = 'content_mutation'
               AND subject_id IN (SELECT id FROM mutations_finalized)
               AND status IN ('accepted', 'processing')
             returning id
         )
         select count(*)::bigint
                + (select count(*) * 0 from attempts_canceled)
                + (select count(*) * 0 from runtime_executions)
                + (select count(*) * 0 from mutations_finalized)
                + (select count(*) * 0 from items_finalized)
                + (select count(*) * 0 from document_items_skipped)
                + (select count(*) * 0 from operations_finalized)
         from jobs_canceled",
    )
    .bind(document_id)
    .bind(disposition.failure_code)
    .bind(disposition.failure_message)
    .bind(disposition.mutation_state)
    .bind(disposition.item_state)
    .bind(disposition.operation_status)
    .bind(disposition.finalize_mutation)
    .fetch_one(executor)
    .await?;
    u64::try_from(affected)
        .map_err(|_| sqlx::Error::Protocol("negative canceled ingest job count".to_string()))
}
