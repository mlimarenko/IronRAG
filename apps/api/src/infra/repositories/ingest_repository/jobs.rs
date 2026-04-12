use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres};
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

pub async fn claim_next_queued_ingest_job(
    postgres: &PgPool,
    max_jobs_per_library: i64,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "update ingest_job
         set queue_state = 'leased'::ingest_queue_state
         where id = (
             select id from ingest_job j
             where j.queue_state = 'queued'
               and j.available_at <= now()
               and (
                   $1::bigint <= 0
                   or (
                       select count(*) from ingest_job leased
                       where leased.queue_state = 'leased'
                         and leased.library_id = j.library_id
                   ) < $1::bigint
               )
             order by j.priority asc, j.available_at asc, j.queued_at asc, j.id asc
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
    .bind(max_jobs_per_library)
    .fetch_optional(postgres)
    .await
}

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

pub async fn cancel_queued_jobs_for_document(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    cancel_queued_jobs_for_document_with_executor(postgres, document_id).await
}

pub async fn cancel_queued_jobs_for_document_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<u64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query(
        "UPDATE ingest_job
         SET queue_state = 'canceled', completed_at = now()
         WHERE mutation_id IN (
             SELECT m.id FROM content_mutation m
             JOIN content_mutation_item mi ON mi.mutation_id = m.id
             WHERE mi.document_id = $1
         )
         AND queue_state = 'queued'
         AND completed_at IS NULL",
    )
    .bind(document_id)
    .execute(executor)
    .await?;
    Ok(result.rows_affected())
}
