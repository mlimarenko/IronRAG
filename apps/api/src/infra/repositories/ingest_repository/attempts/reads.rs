use sqlx::PgPool;
use uuid::Uuid;

use super::rows::IngestAttemptRow;

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
