use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct IngestJobRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
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
}

#[derive(Debug, Clone)]
pub struct NewIngestJob {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
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
    pub retryable: bool,
}

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
}

pub async fn create_ingest_job(
    postgres: &PgPool,
    input: &NewIngestJob,
) -> Result<IngestJobRow, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "insert into ingest_job (
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            $9::ingest_job_kind,
            $10::ingest_queue_state,
            $11,
            $12,
            coalesce($13, now()),
            coalesce($14, now()),
            $15
        )
        returning
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
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
    .fetch_one(postgres)
    .await
}

pub async fn get_ingest_job_by_id(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at
         from ingest_job
         where id = $1",
    )
    .bind(job_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_ingest_job_by_dedupe_key(
    postgres: &PgPool,
    library_id: Uuid,
    dedupe_key: &str,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at
         from ingest_job
         where library_id = $1
           and dedupe_key = $2
         order by queued_at desc, id desc
         limit 1",
    )
    .bind(library_id)
    .bind(dedupe_key)
    .fetch_optional(postgres)
    .await
}

pub async fn get_latest_ingest_job_by_mutation_id(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at
         from ingest_job
         where mutation_id = $1
         order by queued_at desc, id desc
         limit 1",
    )
    .bind(mutation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_latest_ingest_job_by_async_operation_id(
    postgres: &PgPool,
    async_operation_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at
         from ingest_job
         where async_operation_id = $1
         order by queued_at desc, id desc
         limit 1",
    )
    .bind(async_operation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_latest_ingest_job_by_knowledge_revision_id(
    postgres: &PgPool,
    knowledge_revision_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at
         from ingest_job
         where knowledge_revision_id = $1
         order by queued_at desc, id desc
         limit 1",
    )
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
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at
         from ingest_job
         where workspace_id = $1
           and library_id = $2
           and knowledge_document_id = $3
         order by queued_at desc, id desc",
    )
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

    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at
         from ingest_job
         where workspace_id = $1
           and library_id = $2
           and mutation_id = any($3)
         order by queued_at desc, id desc",
    )
    .bind(workspace_id)
    .bind(library_id)
    .bind(mutation_ids)
    .fetch_all(postgres)
    .await
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
            sqlx::query_as::<_, IngestJobRow>(
                "select
                    id,
                    workspace_id,
                    library_id,
                    mutation_id,
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
                    completed_at
                 from ingest_job
                 where workspace_id = $1
                   and library_id = $2
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $3 offset $4",
            )
            .bind(workspace_id)
            .bind(library_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (Some(workspace_id), None) => {
            sqlx::query_as::<_, IngestJobRow>(
                "select
                    id,
                    workspace_id,
                    library_id,
                    mutation_id,
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
                    completed_at
                 from ingest_job
                 where workspace_id = $1
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $2 offset $3",
            )
            .bind(workspace_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (None, Some(library_id)) => {
            sqlx::query_as::<_, IngestJobRow>(
                "select
                    id,
                    workspace_id,
                    library_id,
                    mutation_id,
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
                    completed_at
                 from ingest_job
                 where library_id = $1
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $2 offset $3",
            )
            .bind(library_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, IngestJobRow>(
                "select
                    id,
                    workspace_id,
                    library_id,
                    mutation_id,
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
                    completed_at
                 from ingest_job
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $1 offset $2",
            )
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn update_ingest_job(
    postgres: &PgPool,
    job_id: Uuid,
    input: &UpdateIngestJob,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
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
             completed_at = $12
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at",
    )
    .bind(job_id)
    .bind(input.mutation_id)
    .bind(input.connector_id)
    .bind(input.async_operation_id)
    .bind(input.knowledge_document_id)
    .bind(input.knowledge_revision_id)
    .bind(&input.job_kind)
    .bind(&input.queue_state)
    .bind(input.priority)
    .bind(&input.dedupe_key)
    .bind(input.available_at)
    .bind(input.completed_at)
    .fetch_optional(postgres)
    .await
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
            $14
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
    .bind(input.retryable)
    .fetch_one(postgres)
    .await
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
            retryable
         from ingest_attempt
         where job_id = $1
         order by attempt_number asc, started_at asc, id asc",
    )
    .bind(job_id)
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
             retryable = $11
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
    .bind(input.retryable)
    .fetch_optional(postgres)
    .await
}

/// Lightweight heartbeat touch — only updates `heartbeat_at` and optionally
/// `current_stage` on an `ingest_attempt` row.
pub async fn touch_attempt_heartbeat(
    postgres: &PgPool,
    attempt_id: Uuid,
    current_stage: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingest_attempt
         set heartbeat_at = now(),
             current_stage = coalesce($2, current_stage)
         where id = $1 and attempt_state = 'leased'",
    )
    .bind(attempt_id)
    .bind(current_stage)
    .execute(postgres)
    .await?;
    Ok(())
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
            recorded_at
        )
        values (
            $1,
            $2,
            $3,
            $4::ingest_stage_state,
            $5,
            $6,
            $7,
            coalesce($8, now())
        )
        returning
            id,
            attempt_id,
            stage_name,
            stage_state::text as stage_state,
            ordinal,
            message,
            details_json,
            recorded_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.attempt_id)
    .bind(&input.stage_name)
    .bind(&input.stage_state)
    .bind(input.ordinal)
    .bind(&input.message)
    .bind(&input.details_json)
    .bind(input.recorded_at)
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
            recorded_at
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
            recorded_at
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
            event.recorded_at
         from ingest_stage_event as event
         join ingest_attempt as attempt on attempt.id = event.attempt_id
         where attempt.job_id = $1
         order by attempt.attempt_number asc, event.ordinal asc, event.recorded_at asc, event.id asc",
    )
    .bind(job_id)
    .fetch_all(postgres)
    .await
}

/// Atomically claim the next available canonical `ingest_job` by transitioning
/// its `queue_state` from `queued` → `leased`.  Uses `FOR UPDATE SKIP LOCKED`
/// so concurrent workers never claim the same row.
pub async fn claim_next_queued_ingest_job(
    postgres: &PgPool,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "update ingest_job
         set queue_state = 'leased'::ingest_queue_state
         where id = (
             select id from ingest_job
             where queue_state = 'queued'
               and available_at <= now()
             order by priority asc, available_at asc, queued_at asc, id asc
             limit 1
             for update skip locked
         )
         returning
            id,
            workspace_id,
            library_id,
            mutation_id,
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
            completed_at",
    )
    .fetch_optional(postgres)
    .await
}

/// Recover canonical `ingest_job` rows stuck in `leased` whose corresponding
/// `ingest_attempt` heartbeat is stale (older than `stale_threshold`).
/// Returns the number of jobs reset back to `queued`.
pub async fn recover_stale_canonical_leases(
    postgres: &PgPool,
    stale_threshold: chrono::Duration,
) -> Result<u64, sqlx::Error> {
    let cutoff = Utc::now() - stale_threshold;
    let result = sqlx::query(
        "with stale_attempts as (
             select distinct a.job_id
             from ingest_attempt a
             join ingest_job j on j.id = a.job_id
             where j.queue_state = 'leased'
               and a.attempt_state = 'leased'
               and a.heartbeat_at < $1
         ),
         failed_attempts as (
             update ingest_attempt
             set attempt_state = 'failed',
                 failure_class = 'lease_expired',
                 failure_code = 'stale_heartbeat',
                 finished_at = now(),
                 retryable = true
             where job_id in (select job_id from stale_attempts)
               and attempt_state = 'leased'
         )
         update ingest_job
         set queue_state = 'queued',
             available_at = now()
         where id in (select job_id from stale_attempts)",
    )
    .bind(cutoff)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}
