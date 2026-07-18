use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    domains::webhook::WebhookEvent,
    infra::repositories::{catalog_repository, webhook_outbox_repository},
};

use super::jobs::UpdateIngestJob;

#[derive(Debug, Clone, FromRow)]
pub struct IngestAttemptRow {
    pub id: Uuid,
    pub job_id: Uuid,
    pub attempt_number: i32,
    pub worker_principal_id: Option<Uuid>,
    pub lease_token: Option<String>,
    pub knowledge_generation_id: Option<Uuid>,
    pub attempt_state: String,
    pub current_stage: Option<String>,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub progress_percent: i32,
    pub retryable: bool,
}

#[derive(Debug, Clone)]
pub struct NewIngestAttempt {
    pub job_id: Uuid,
    pub attempt_number: i32,
    pub worker_principal_id: Option<Uuid>,
    pub lease_token: Option<String>,
    pub knowledge_generation_id: Option<Uuid>,
    pub attempt_state: String,
    pub current_stage: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub progress_percent: i32,
    pub retryable: bool,
}

#[derive(Debug, Clone)]
pub struct UpdateIngestAttempt {
    pub worker_principal_id: Option<Uuid>,
    pub lease_token: Option<String>,
    pub knowledge_generation_id: Option<Uuid>,
    pub attempt_state: String,
    pub current_stage: Option<String>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub progress_percent: i32,
    pub retryable: bool,
}

pub async fn create_ingest_attempt(
    postgres: &PgPool,
    input: &NewIngestAttempt,
) -> Result<IngestAttemptRow, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "insert into ingest_attempt (
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7::ingest_attempt_state,
            $8,
            coalesce($9, now()),
            $10,
            $11,
            $12,
            $13,
            $14,
            $15,
            $16
        )
        returning
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable",
    )
    .bind(Uuid::now_v7())
    .bind(input.job_id)
    .bind(input.attempt_number)
    .bind(input.worker_principal_id)
    .bind(&input.lease_token)
    .bind(input.knowledge_generation_id)
    .bind(&input.attempt_state)
    .bind(&input.current_stage)
    .bind(input.started_at)
    .bind(input.heartbeat_at)
    .bind(input.finished_at)
    .bind(&input.failure_class)
    .bind(&input.failure_code)
    .bind(&input.failure_message)
    .bind(input.progress_percent)
    .bind(input.retryable)
    .fetch_one(postgres)
    .await
}

async fn insert_ingest_attempt_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    input: &NewIngestAttempt,
    attempt_number: i32,
) -> Result<IngestAttemptRow, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "insert into ingest_attempt (
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7::ingest_attempt_state,
            $8,
            coalesce($9, now()),
            $10,
            $11,
            $12,
            $13,
            $14,
            $15,
            $16
        )
        returning
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable",
    )
    .bind(Uuid::now_v7())
    .bind(input.job_id)
    .bind(attempt_number)
    .bind(input.worker_principal_id)
    .bind(&input.lease_token)
    .bind(input.knowledge_generation_id)
    .bind(&input.attempt_state)
    .bind(&input.current_stage)
    .bind(input.started_at)
    .bind(input.heartbeat_at)
    .bind(input.finished_at)
    .bind(&input.failure_class)
    .bind(&input.failure_code)
    .bind(&input.failure_message)
    .bind(input.progress_percent)
    .bind(input.retryable)
    .fetch_one(&mut **transaction)
    .await
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
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    let Some(library_id) = lock_attempt_parent_library(&mut tx, input.job_id).await? else {
        tx.commit().await?;
        return Ok(None);
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
        return Ok(None);
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
    Ok(Some(attempt))
}

/// Atomically claim a freshly admitted inline job and create its canonical
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

/// Lock and validate the canonical authority for an ingest-owned revision
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

pub async fn get_ingest_attempt_by_id(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "select
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable
         from ingest_attempt
         where id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(postgres)
    .await
}

/// Defers a webhook queue job until the delivery lease can be reclaimed.
///
/// This is intentionally not the generic retry-budget path: a duplicate
/// worker that observes another delivery owner has not performed HTTP and
/// must not consume the job's failure budget or finalize it as succeeded.
/// Both the queue lease token and ingest-attempt state are compared in one
/// transaction so a stale worker cannot defer a newer owner.
pub async fn defer_webhook_delivery_in_flight(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
    expected_queue_lease_token: &str,
    retry_at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let owns_job = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_job
         where id = $1
           and job_kind = 'webhook_delivery'::ingest_job_kind
           and queue_state = 'leased'
           and queue_lease_token = $2
         for update",
    )
    .bind(job_id)
    .bind(expected_queue_lease_token)
    .fetch_optional(&mut *transaction)
    .await?;
    if owns_job.is_none() {
        transaction.rollback().await?;
        return Ok(false);
    }

    let attempt = sqlx::query_scalar::<_, Uuid>(
        "update ingest_attempt
         set attempt_state = 'failed',
             current_stage = 'webhook_delivery',
             heartbeat_at = now(),
             finished_at = now(),
             failure_class = 'webhook_delivery',
             failure_code = 'delivery_lease_in_flight',
             failure_message = 'Another worker still owns the webhook delivery lease',
             retryable = true
         where id = $1
           and job_id = $2
           and attempt_state = 'leased'
         returning id",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if attempt.is_none() {
        transaction.rollback().await?;
        return Ok(false);
    }

    let result = sqlx::query(
        "update ingest_job
         set queue_state = 'queued',
             available_at = greatest($3, now()),
             completed_at = null,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'leased'
           and queue_lease_token = $2",
    )
    .bind(job_id)
    .bind(expected_queue_lease_token)
    .bind(retry_at)
    .execute(&mut *transaction)
    .await?;
    if result.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(false);
    }
    transaction.commit().await?;
    Ok(true)
}

#[derive(Debug, Clone)]
pub struct IngestAttemptPage {
    pub rows: Vec<IngestAttemptRow>,
    pub has_more: bool,
}

/// Keyset-paginated fetch for `GET /v1/ingest/jobs/{jobId}/attempts`.
///
/// Newest attempt first: `(attempt_number desc, id desc)`. `attempt_number`
/// is a per-job monotonic counter minted at attempt creation, so it is a
/// stronger keyset key than `started_at` (two attempts can share a
/// heartbeat-driven timestamp under clock coarsening; they never share a
/// number). `limit + 1` rows are fetched so `has_more` is derived without a
/// `COUNT(*)`.
pub async fn list_ingest_attempts_by_job_page(
    postgres: &PgPool,
    job_id: Uuid,
    cursor: Option<(i32, Uuid)>,
    limit: i64,
) -> Result<IngestAttemptPage, sqlx::Error> {
    let fetch_limit = limit + 1;
    let (cursor_attempt_number, cursor_id) =
        cursor.map_or((None, None), |(attempt_number, id)| (Some(attempt_number), Some(id)));

    let mut rows = sqlx::query_as::<_, IngestAttemptRow>(
        "select
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable
         from ingest_attempt
         where job_id = $1
           and ($2::int is null or (attempt_number, id) < ($2, $3))
         order by attempt_number desc, id desc
         limit $4",
    )
    .bind(job_id)
    .bind(cursor_attempt_number)
    .bind(cursor_id)
    .bind(fetch_limit)
    .fetch_all(postgres)
    .await?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(IngestAttemptPage { rows, has_more })
}

pub async fn list_ingest_attempts_by_job(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Vec<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "select
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable
         from ingest_attempt
         where job_id = $1
         order by attempt_number asc, started_at asc, id asc",
    )
    .bind(job_id)
    .fetch_all(postgres)
    .await
}

/// Batch variant: loads attempts for ALL given job IDs in one query.
/// Eliminates the N+1 pattern in document lifecycle assembly.
pub async fn list_ingest_attempts_by_jobs(
    postgres: &PgPool,
    job_ids: &[Uuid],
) -> Result<Vec<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "select
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable
         from ingest_attempt
         where job_id = ANY($1)
         order by job_id, attempt_number asc, started_at asc, id asc",
    )
    .bind(job_ids)
    .fetch_all(postgres)
    .await
}

pub async fn get_latest_ingest_attempt_by_job(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "select
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable
         from ingest_attempt
         where job_id = $1
         order by attempt_number desc, started_at desc, id desc
         limit 1",
    )
    .bind(job_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_latest_ingest_attempts_by_job_ids(
    postgres: &PgPool,
    job_ids: &[Uuid],
) -> Result<Vec<IngestAttemptRow>, sqlx::Error> {
    if job_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, IngestAttemptRow>(
        "select distinct on (job_id)
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable
         from ingest_attempt
         where job_id = any($1)
         order by job_id, attempt_number desc, started_at desc, id desc",
    )
    .bind(job_ids)
    .fetch_all(postgres)
    .await
}

pub async fn update_ingest_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
    input: &UpdateIngestAttempt,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "update ingest_attempt
         set worker_principal_id = $2,
             lease_token = $3,
             knowledge_generation_id = $4,
             attempt_state = $5::ingest_attempt_state,
             current_stage = $6,
             heartbeat_at = $7,
             finished_at = $8,
             failure_class = $9,
             failure_code = $10,
             failure_message = $11,
             progress_percent = $12,
             retryable = $13
         where id = $1
         returning
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable",
    )
    .bind(attempt_id)
    .bind(input.worker_principal_id)
    .bind(&input.lease_token)
    .bind(input.knowledge_generation_id)
    .bind(&input.attempt_state)
    .bind(&input.current_stage)
    .bind(input.heartbeat_at)
    .bind(input.finished_at)
    .bind(&input.failure_class)
    .bind(&input.failure_code)
    .bind(&input.failure_message)
    .bind(input.progress_percent)
    .bind(input.retryable)
    .fetch_optional(postgres)
    .await
}

pub async fn finalize_leased_ingest_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
    input: &UpdateIngestAttempt,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "update ingest_attempt
         set worker_principal_id = $2,
             lease_token = $3,
             knowledge_generation_id = $4,
             attempt_state = $5::ingest_attempt_state,
             current_stage = $6,
             heartbeat_at = $7,
             finished_at = $8,
             failure_class = $9,
             failure_code = $10,
             failure_message = $11,
             progress_percent = $12,
             retryable = $13
         where id = $1 and attempt_state = 'leased'
         returning
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable",
    )
    .bind(attempt_id)
    .bind(input.worker_principal_id)
    .bind(&input.lease_token)
    .bind(input.knowledge_generation_id)
    .bind(&input.attempt_state)
    .bind(&input.current_stage)
    .bind(input.heartbeat_at)
    .bind(input.finished_at)
    .bind(&input.failure_class)
    .bind(&input.failure_code)
    .bind(&input.failure_message)
    .bind(input.progress_percent)
    .bind(input.retryable)
    .fetch_optional(postgres)
    .await
}

pub async fn finalize_leased_ingest_attempt_and_update_job(
    postgres: &PgPool,
    attempt_id: Uuid,
    attempt_input: &UpdateIngestAttempt,
    job_input: &UpdateIngestJob,
    lifecycle_event: Option<&WebhookEvent>,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    if let Some(event) = lifecycle_event {
        let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut *tx,
            event.workspace_id,
            event.library_id,
        )
        .await?;
        if !parent_locked {
            tx.rollback().await?;
            return Ok(None);
        }
    }
    // Canonical queue lock order is parent scope (when present) -> job ->
    // attempt. Read the attempt identity without locking it, then acquire the
    // owning job row before any attempt UPDATE. Delete, cancel, recovery, and
    // webhook handoff paths use the same order.
    let locked_job_id = sqlx::query_scalar::<_, Uuid>(
        "select job.id
         from ingest_attempt attempt
         join ingest_job job on job.id = attempt.job_id
         where attempt.id = $1
           and attempt.attempt_state = 'leased'
           and job.queue_state = 'leased'
         for update of job",
    )
    .bind(attempt_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(locked_job_id) = locked_job_id else {
        tx.rollback().await?;
        return Ok(None);
    };
    let finalized_attempt = sqlx::query_as::<_, IngestAttemptRow>(
        "update ingest_attempt
         set worker_principal_id = $2,
             lease_token = $3,
             knowledge_generation_id = $4,
             attempt_state = $5::ingest_attempt_state,
             current_stage = $6,
             heartbeat_at = $7,
             finished_at = $8,
             failure_class = $9,
             failure_code = $10,
             failure_message = $11,
             progress_percent = $12,
             retryable = $13
         where id = $1 and attempt_state = 'leased'
         returning
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            failure_message,
            progress_percent,
            retryable",
    )
    .bind(attempt_id)
    .bind(attempt_input.worker_principal_id)
    .bind(&attempt_input.lease_token)
    .bind(attempt_input.knowledge_generation_id)
    .bind(&attempt_input.attempt_state)
    .bind(&attempt_input.current_stage)
    .bind(attempt_input.heartbeat_at)
    .bind(attempt_input.finished_at)
    .bind(&attempt_input.failure_class)
    .bind(&attempt_input.failure_code)
    .bind(&attempt_input.failure_message)
    .bind(attempt_input.progress_percent)
    .bind(attempt_input.retryable)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(finalized_attempt) = finalized_attempt else {
        tx.rollback().await?;
        return Ok(None);
    };
    if finalized_attempt.job_id != locked_job_id {
        tx.rollback().await?;
        return Err(sqlx::Error::Protocol(
            "ingest attempt changed jobs while finalizing its queue lease".to_string(),
        ));
    }

    let job_result = sqlx::query(
        "update ingest_job
         set mutation_id = $2,
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
             queue_lease_owner = case when $8::ingest_queue_state = 'leased' then queue_lease_owner else null end
         where id = $1
           and queue_state = 'leased'",
    )
    .bind(finalized_attempt.job_id)
    .bind(job_input.mutation_id)
    .bind(job_input.connector_id)
    .bind(job_input.async_operation_id)
    .bind(job_input.knowledge_document_id)
    .bind(job_input.knowledge_revision_id)
    .bind(&job_input.job_kind)
    .bind(&job_input.queue_state)
    .bind(job_input.priority)
    .bind(&job_input.dedupe_key)
    .bind(job_input.available_at)
    .bind(job_input.completed_at)
    .execute(&mut *tx)
    .await?;

    if job_result.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(None);
    }

    if let Some(event) = lifecycle_event {
        webhook_outbox_repository::enqueue_webhook_lifecycle_event_with_executor(&mut *tx, event)
            .await?;
        catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *tx,
            event.library_id,
        )
        .await?;
    }

    tx.commit().await?;
    Ok(Some(finalized_attempt))
}

pub async fn touch_attempt_heartbeat(
    postgres: &PgPool,
    attempt_id: Uuid,
    current_stage: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update ingest_attempt
         set heartbeat_at = now(),
             current_stage = coalesce($2, current_stage)
         where id = $1 and attempt_state = 'leased'",
    )
    .bind(attempt_id)
    .bind(current_stage)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn touch_attempt_heartbeat_and_load_job_state(
    postgres: &PgPool,
    attempt_id: Uuid,
    current_stage: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "with touched_attempt as (
             update ingest_attempt
             set heartbeat_at = now(),
                 current_stage = coalesce($2, current_stage)
             where id = $1 and attempt_state = 'leased'
             returning job_id
         )
         select j.queue_state::text
         from touched_attempt a
         join ingest_job j on j.id = a.job_id",
    )
    .bind(attempt_id)
    .bind(current_stage)
    .fetch_optional(postgres)
    .await
}

pub async fn update_leased_attempt_stage_progress(
    postgres: &PgPool,
    attempt_id: Uuid,
    current_stage: &str,
    progress_percent: i32,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update ingest_attempt
         set heartbeat_at = now(),
             current_stage = $2,
             progress_percent = greatest(progress_percent, $3)
         where id = $1 and attempt_state = 'leased'",
    )
    .bind(attempt_id)
    .bind(current_stage)
    .bind(progress_percent.clamp(0, 99))
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn abandon_paused_ingest_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'abandoned',
             heartbeat_at = now(),
             finished_at = now(),
             failure_class = 'operator_control',
             failure_code = 'paused_by_operator',
             failure_message = 'Processing was paused from the administration queue',
             retryable = true
         where id = $1 and attempt_state = 'leased'",
    )
    .bind(attempt_id)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() > 0)
}
