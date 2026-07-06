use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

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

pub async fn create_ingest_attempt_for_queue_lease(
    postgres: &PgPool,
    input: &NewIngestAttempt,
    expected_queue_lease_token: &str,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    let target_job = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_job
         where id = $1
           and queue_state = 'leased'
           and queue_lease_token = $2
         for update",
    )
    .bind(input.job_id)
    .bind(expected_queue_lease_token)
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

    let attempt = sqlx::query_as::<_, IngestAttemptRow>(
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
    .bind(next_attempt_number)
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
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Some(attempt))
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
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
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
