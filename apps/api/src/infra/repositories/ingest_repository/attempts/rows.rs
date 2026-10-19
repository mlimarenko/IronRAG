use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

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

pub(super) async fn insert_ingest_attempt_in_transaction(
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
