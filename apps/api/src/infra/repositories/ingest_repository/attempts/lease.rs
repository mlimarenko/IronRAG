use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::rows::{IngestAttemptRow, NewIngestAttempt, insert_ingest_attempt_in_transaction};

/// What happened when a claimed queue job tried to open its attempt.
#[derive(Debug)]
pub enum QueueLeaseOutcome {
    /// The attempt exists and the linked operation is `processing`.
    Leased(Box<IngestAttemptRow>),
    /// The queue lease moved to another worker before the attempt existed.
    LeaseMoved,
    /// The linked async operation is already terminal, so this job can never
    /// open a valid attempt. It has been retired inside the same transaction.
    Retired,
}

/// Whether the operation linked to a job can still accept a new attempt.
enum LinkedAsyncOperation {
    /// No linked operation, or one that is still `accepted` / `processing`.
    Open,
    /// Already `ready` / `failed` / `canceled`.
    Terminal,
}

/// Reads the linked async operation under a row lock so the decision below
/// cannot race a concurrent reconcile.
async fn classify_linked_async_operation(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: Uuid,
) -> Result<LinkedAsyncOperation, sqlx::Error> {
    let status = sqlx::query_scalar::<_, String>(
        // `status` is the `ops_async_operation_status` enum; it has to be cast
        // or the row fails to decode and the lease reports a database error
        // instead of retiring the job.
        "select operation.status::text
         from ingest_job as job
         join ops_async_operation as operation
           on operation.id = job.async_operation_id
          and operation.workspace_id = job.workspace_id
          and operation.library_id = job.library_id
         where job.id = $1
         for update of operation",
    )
    .bind(job_id)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(match status.as_deref() {
        None | Some("accepted" | "processing") => LinkedAsyncOperation::Open,
        Some(_) => LinkedAsyncOperation::Terminal,
    })
}

/// Retires a job whose linked operation already went terminal.
///
/// Without this the job stays claimable forever: the lease below refuses to
/// drag a terminal operation back to `processing`, the worker's failure path
/// defers content-mutation failures to lifecycle recovery, and the reaper
/// returns the job to the head of its library queue. Each such job then holds
/// one `ingestion_max_parallel_jobs_per_library` slot permanently, so a
/// library can be wedged by a couple of rows while its documents sit in
/// `queued` indefinitely.
async fn retire_job_behind_terminal_operation(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: Uuid,
) -> Result<(), sqlx::Error> {
    let revision_id = sqlx::query_scalar::<_, Option<Uuid>>(
        "update ingest_job
         set queue_state = 'failed',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
          where id = $1
          returning knowledge_revision_id",
    )
    .bind(job_id)
    .fetch_one(&mut **transaction)
    .await?;
    if let Some(revision_id) = revision_id {
        super::super::jobs::reconcile_terminal_failed_preparations_in_transaction(
            transaction,
            Some(revision_id),
            None,
        )
        .await?;
    }
    Ok(())
}

/// Moves the async operation owned by `job_id` into processing inside the
/// same transaction that creates the leased attempt.  Keeping these writes in
/// one commit prevents a durable attempt from being exposed while its
/// operator-facing lifecycle still says `accepted` (or while an operation
/// update failed after the attempt commit).
async fn mark_linked_async_operation_processing(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: Uuid,
) -> Result<(), sqlx::Error> {
    let linked_operation_id = sqlx::query_scalar::<_, Option<Uuid>>(
        "select async_operation_id
         from ingest_job
         where id = $1",
    )
    .bind(job_id)
    .fetch_one(&mut **transaction)
    .await?;
    let Some(operation_id) = linked_operation_id else {
        return Ok(());
    };

    let updated = sqlx::query(
        "update ops_async_operation as operation
         set status = 'processing',
             completed_at = null,
             failure_code = null
         from ingest_job as job
         where operation.id = $1
           and job.id = $2
           and job.async_operation_id = operation.id
           and job.workspace_id = operation.workspace_id
           and job.library_id = operation.library_id
           and operation.status in ('accepted', 'processing')",
    )
    .bind(operation_id)
    .bind(job_id)
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol(
            "linked ingest async-operation identity changed before attempt lease".to_string(),
        ));
    }
    Ok(())
}

/// Locks the parent library before a lease path locks its job row. This keeps
/// queue and inline attempt creation on the same library -> job -> attempt
/// order used by publication and vector-plane fencing.
async fn lock_attempt_parent_library(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let library_id = sqlx::query_scalar::<_, Uuid>(
        "select library_id
         from ingest_job
         where id = $1",
    )
    .bind(job_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(library_id) = library_id else {
        return Ok(None);
    };
    sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1
         for share",
    )
    .bind(library_id)
    .fetch_optional(&mut **transaction)
    .await
}

pub async fn create_ingest_attempt_for_queue_lease(
    postgres: &PgPool,
    input: &NewIngestAttempt,
    expected_queue_lease_token: &str,
) -> Result<QueueLeaseOutcome, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    let Some(library_id) = lock_attempt_parent_library(&mut tx, input.job_id).await? else {
        tx.commit().await?;
        return Ok(QueueLeaseOutcome::LeaseMoved);
    };
    let target_job = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_job
         where id = $1
           and library_id = $3
           and queue_state = 'leased'
           and queue_lease_token = $2
         for update",
    )
    .bind(input.job_id)
    .bind(expected_queue_lease_token)
    .bind(library_id)
    .fetch_optional(&mut *tx)
    .await?;
    if target_job.is_none() {
        tx.commit().await?;
        return Ok(QueueLeaseOutcome::LeaseMoved);
    }

    if matches!(
        classify_linked_async_operation(&mut tx, input.job_id).await?,
        LinkedAsyncOperation::Terminal
    ) {
        retire_job_behind_terminal_operation(&mut tx, input.job_id).await?;
        tx.commit().await?;
        return Ok(QueueLeaseOutcome::Retired);
    }

    let next_attempt_number = sqlx::query_scalar::<_, i32>(
        "select coalesce(max(attempt_number), 0) + 1
         from ingest_attempt
         where job_id = $1",
    )
    .bind(input.job_id)
    .fetch_one(&mut *tx)
    .await?;

    let attempt = insert_ingest_attempt_in_transaction(&mut tx, input, next_attempt_number).await?;
    mark_linked_async_operation_processing(&mut tx, input.job_id).await?;

    tx.commit().await?;
    Ok(QueueLeaseOutcome::Leased(Box::new(attempt)))
}

/// Atomically claim a freshly admitted inline job and create its corresponding
/// leased attempt. The library -> job -> attempt lock order matches vector
/// write/cleanup fencing, so a retry cannot appear between authority
/// validation and a short vector transaction.
pub async fn create_ingest_attempt_for_inline_lease(
    postgres: &PgPool,
    input: &NewIngestAttempt,
    queue_lease_token: &str,
    queue_lease_owner: &str,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some(library_id) = lock_attempt_parent_library(&mut transaction, input.job_id).await?
    else {
        transaction.commit().await?;
        return Ok(None);
    };
    let target_job = sqlx::query_scalar::<_, Uuid>(
        "select job.id
         from ingest_job job
         where job.id = $1
           and job.library_id = $2
           and job.queue_state = 'queued'
           and not exists (
               select 1
               from ingest_attempt attempt
               where attempt.job_id = job.id
                 and attempt.attempt_state = 'leased'
           )
         for update",
    )
    .bind(input.job_id)
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if target_job.is_none() {
        transaction.commit().await?;
        return Ok(None);
    }

    sqlx::query(
        "update ingest_job
         set queue_state = 'leased',
             queue_leased_at = now(),
             queue_lease_token = $2,
             queue_lease_owner = $3
         where id = $1",
    )
    .bind(input.job_id)
    .bind(queue_lease_token)
    .bind(queue_lease_owner)
    .execute(&mut *transaction)
    .await?;
    let next_attempt_number = sqlx::query_scalar::<_, i32>(
        "select coalesce(max(attempt_number), 0) + 1
         from ingest_attempt
         where job_id = $1",
    )
    .bind(input.job_id)
    .fetch_one(&mut *transaction)
    .await?;
    let attempt =
        insert_ingest_attempt_in_transaction(&mut transaction, input, next_attempt_number).await?;
    mark_linked_async_operation_processing(&mut transaction, input.job_id).await?;
    transaction.commit().await?;
    Ok(Some(attempt))
}

/// Lock and validate the current authority for an ingest-owned revision
/// mutation. Callers must lock the parent library first; this helper then
/// follows the shared library -> job -> attempt order used by vector writes,
/// readiness, cleanup, and head publication.
pub async fn lock_latest_leased_revision_attempt(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    attempt_id: Uuid,
    revision_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let job = sqlx::query_as::<_, (Uuid, Option<Uuid>)>(
        "select job.id, job.knowledge_revision_id
         from ingest_job job
         where job.library_id = $1
           and job.queue_state = 'leased'
           and job.id = (
               select attempt.job_id
               from ingest_attempt attempt
               where attempt.id = $2
           )
         for share",
    )
    .bind(library_id)
    .bind(attempt_id)
    .fetch_optional(&mut **transaction)
    .await?
    .filter(|(_, owning_revision_id)| *owning_revision_id == Some(revision_id));
    let Some((job_id, _)) = job else {
        return Ok(false);
    };
    let latest_attempt = sqlx::query_scalar::<_, i32>(
        "select attempt.attempt_number
         from ingest_attempt attempt
         where attempt.id = $1
           and attempt.job_id = $2
           and attempt.attempt_state = 'leased'
           and not exists (
               select 1
               from ingest_attempt newer
               where newer.job_id = attempt.job_id
                 and newer.attempt_number > attempt.attempt_number
           )
         for share",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(latest_attempt.is_some())
}
