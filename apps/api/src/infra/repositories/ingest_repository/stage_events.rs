use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct IngestStageEventRow {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub stage_name: String,
    pub stage_state: String,
    pub ordinal: i32,
    pub message: Option<String>,
    pub details_json: Value,
    pub recorded_at: DateTime<Utc>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
    pub estimated_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub elapsed_ms: Option<i64>,
    pub started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewIngestStageEvent {
    pub attempt_id: Uuid,
    pub stage_name: String,
    pub stage_state: String,
    pub ordinal: i32,
    pub message: Option<String>,
    pub details_json: Value,
    pub recorded_at: Option<DateTime<Utc>>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
    pub estimated_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub elapsed_ms: Option<i64>,
}

pub async fn create_ingest_stage_event(
    postgres: &PgPool,
    input: &NewIngestStageEvent,
) -> Result<IngestStageEventRow, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "insert into ingest_stage_event (
            id,
            attempt_id,
            stage_name,
            stage_state,
            ordinal,
            message,
            details_json,
            recorded_at,
            provider_kind,
            model_name,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            estimated_cost,
            currency_code,
            elapsed_ms
        )
        values (
            $1,
            $2,
            $3,
            $4::ingest_stage_state,
            $5,
            $6,
            $7,
            coalesce($8, now()),
            $9,
            $10,
            $11,
            $12,
            $13,
            $14,
            $15,
            $16,
            $17
        )
        returning
            id,
            attempt_id,
            stage_name,
            stage_state::text as stage_state,
            ordinal,
            message,
            details_json,
            recorded_at,
            provider_kind,
            model_name,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            estimated_cost,
            currency_code,
            elapsed_ms,
            started_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.attempt_id)
    .bind(&input.stage_name)
    .bind(&input.stage_state)
    .bind(input.ordinal)
    .bind(&input.message)
    .bind(&input.details_json)
    .bind(input.recorded_at)
    .bind(&input.provider_kind)
    .bind(&input.model_name)
    .bind(input.prompt_tokens)
    .bind(input.completion_tokens)
    .bind(input.total_tokens)
    .bind(input.cached_tokens)
    .bind(input.estimated_cost)
    .bind(&input.currency_code)
    .bind(input.elapsed_ms)
    .fetch_one(postgres)
    .await
}

pub async fn get_ingest_stage_event_by_id(
    postgres: &PgPool,
    event_id: Uuid,
) -> Result<Option<IngestStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "select
            id,
            attempt_id,
            stage_name,
            stage_state::text as stage_state,
            ordinal,
            message,
            details_json,
            recorded_at,
            provider_kind,
            model_name,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            estimated_cost,
            currency_code,
            elapsed_ms,
            started_at
         from ingest_stage_event
         where id = $1",
    )
    .bind(event_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_ingest_stage_events_by_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<Vec<IngestStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "select
            id,
            attempt_id,
            stage_name,
            stage_state::text as stage_state,
            ordinal,
            message,
            details_json,
            recorded_at,
            provider_kind,
            model_name,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            estimated_cost,
            currency_code,
            elapsed_ms,
            started_at
         from ingest_stage_event
         where attempt_id = $1
         order by ordinal asc, recorded_at asc, id asc",
    )
    .bind(attempt_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_ingest_stage_events_by_job(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Vec<IngestStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "select
            event.id,
            event.attempt_id,
            event.stage_name,
            event.stage_state::text as stage_state,
            event.ordinal,
            event.message,
            event.details_json,
            event.recorded_at,
            event.provider_kind,
            event.model_name,
            event.prompt_tokens,
            event.completion_tokens,
            event.total_tokens,
            event.cached_tokens,
            event.estimated_cost,
            event.currency_code,
            event.elapsed_ms,
            event.started_at
         from ingest_stage_event as event
         join ingest_attempt as attempt on attempt.id = event.attempt_id
         where attempt.job_id = $1
         order by attempt.attempt_number asc, event.ordinal asc, event.recorded_at asc, event.id asc",
    )
    .bind(job_id)
    .fetch_all(postgres)
    .await
}

/// Batch variant: loads stage events for ALL given job IDs in one query.
pub async fn list_ingest_stage_events_by_jobs(
    postgres: &PgPool,
    job_ids: &[Uuid],
) -> Result<Vec<IngestStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "select
            event.id,
            event.attempt_id,
            event.stage_name,
            event.stage_state::text as stage_state,
            event.ordinal,
            event.message,
            event.details_json,
            event.recorded_at,
            event.provider_kind,
            event.model_name,
            event.prompt_tokens,
            event.completion_tokens,
            event.total_tokens,
            event.cached_tokens,
            event.estimated_cost,
            event.currency_code,
            event.elapsed_ms,
            event.started_at
         from ingest_stage_event as event
         join ingest_attempt as attempt on attempt.id = event.attempt_id
         where attempt.job_id = ANY($1)
         order by attempt.job_id, attempt.attempt_number asc, event.ordinal asc, event.recorded_at asc, event.id asc",
    )
    .bind(job_ids)
    .fetch_all(postgres)
    .await
}
