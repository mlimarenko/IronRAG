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
    max_jobs_per_workspace: i64,
    max_jobs_global: i64,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    // The dispatcher counts ALL leased jobs against the limits, including
    // those whose attempt row has not yet written its first heartbeat.
    // Filtering on heartbeat_at introduces a TOCTOU gap: a fresh lease
    // sets queue_state='leased' before the attempt heartbeat exists, so a
    // concurrent claim sees zero active leases and bypasses the cap.
    //
    // The zombie-lease problem is handled by `recover_stale_canonical_leases`
    // (the stale-lease reaper) on its own tick. The dispatcher should not
    // try to detect zombies — its only job is to enforce limits.
    // `queue_rank` is the operator-visible order. Fairness and priority remain
    // deterministic tie-breakers, but they must not contradict the queue shown
    // in the administration UI.
    sqlx::query_as::<_, IngestJobRow>(
        "with active_leases as (
             select j.id, j.library_id, j.workspace_id
             from ingest_job j
             where j.queue_state = 'leased'
         ),
         library_running as (
             select library_id, count(*)::bigint as running_count
             from active_leases
             group by library_id
         )
         update ingest_job
         set queue_state = 'leased'::ingest_queue_state
         where id = (
             select j.id from ingest_job j
             left join library_running lr on lr.library_id = j.library_id
             where j.queue_state = 'queued'
               and j.available_at <= now()
               and (select count(*) from active_leases) < $3::bigint
               and (
                   select count(*) from active_leases al
                   where al.workspace_id = j.workspace_id
               ) < $2::bigint
               and (
                   select count(*) from active_leases al
                   where al.library_id = j.library_id
               ) < $1::bigint
             order by
                 j.queue_rank asc,
                 j.priority asc,
                 coalesce(lr.running_count, 0) asc,
                 j.available_at asc,
                 j.queued_at asc,
                 j.id asc
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
    .bind(max_jobs_per_workspace)
    .bind(max_jobs_global)
    .fetch_optional(postgres)
    .await
}

pub async fn list_active_ingest_queue(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<IngestQueueItemRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestQueueItemRow>(
        "with active_leases as (
             select j.id, j.library_id, j.workspace_id
             from ingest_job j
             where j.queue_state = 'leased'
         ),
         library_running as (
             select library_id, count(*)::bigint as running_count
             from active_leases
             group by library_id
         ),
         operator_ordered as (
             select
                 j.id,
                 row_number() over (
                     order by
                         j.queue_rank asc,
                         j.priority asc,
                         coalesce(lr.running_count, 0) asc,
                         j.available_at asc,
                         j.queued_at asc,
                         j.id asc
                 )::bigint as queue_position
             from ingest_job j
             left join library_running lr on lr.library_id = j.library_id
             where j.queue_state in ('queued', 'paused')
               and ($1::uuid is null or j.workspace_id = $1)
               and ($2::uuid is null or j.library_id = $2)
         )
         select
             j.id as job_id,
             j.workspace_id,
             ws.display_name as workspace_name,
             j.library_id,
             lib.display_name as library_name,
             j.knowledge_document_id,
             doc.external_key as document_name,
             j.job_kind::text as job_kind,
             j.queue_state::text as queue_state,
             j.priority,
             j.queue_rank,
             operator_ordered.queue_position,
             j.queued_at,
             j.available_at,
             j.completed_at,
             attempt.id as attempt_id,
             attempt.attempt_number,
             attempt.attempt_state::text as attempt_state,
             attempt.current_stage,
             attempt.started_at,
             attempt.heartbeat_at,
             attempt.finished_at,
             attempt.progress_percent,
             attempt.failure_code,
             attempt.failure_message
         from ingest_job j
         join catalog_workspace ws on ws.id = j.workspace_id
         join catalog_library lib on lib.id = j.library_id
         left join content_document doc on doc.id = j.knowledge_document_id
         left join operator_ordered on operator_ordered.id = j.id
         left join lateral (
             select a.*
             from ingest_attempt a
             where a.job_id = j.id
             order by a.attempt_number desc, a.started_at desc, a.id desc
             limit 1
         ) attempt on true
         where j.queue_state in ('queued', 'leased', 'paused')
           and ($1::uuid is null or j.workspace_id = $1)
           and ($2::uuid is null or j.library_id = $2)
         order by
             case j.queue_state::text when 'leased' then 0 else 1 end,
             operator_ordered.queue_position asc nulls last,
             coalesce(attempt.started_at, j.queued_at) asc,
             j.queue_rank asc,
             j.id asc",
    )
    .bind(workspace_id)
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn move_queued_ingest_job(
    postgres: &PgPool,
    job_id: Uuid,
    direction: QueueMoveDirection,
) -> Result<Option<()>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    let rows = sqlx::query_as::<_, (Uuid, i64)>(
        "select id, queue_rank
         from ingest_job
         where queue_state in ('queued', 'paused')
         order by queue_rank asc, priority asc, available_at asc, queued_at asc, id asc
         for update",
    )
    .fetch_all(&mut *tx)
    .await?;

    let Some(index) = rows.iter().position(|(id, _)| *id == job_id) else {
        tx.commit().await?;
        return Ok(None);
    };

    let target_rank = match direction {
        QueueMoveDirection::Up => {
            if index == 0 {
                None
            } else {
                Some(rows[index - 1].1)
            }
        }
        QueueMoveDirection::Down => rows.get(index + 1).map(|(_, rank)| *rank),
        QueueMoveDirection::Top => {
            if index == 0 {
                None
            } else {
                rows.first().map(|(_, rank)| *rank - 1_000_000)
            }
        }
        QueueMoveDirection::Bottom => {
            if index + 1 >= rows.len() {
                None
            } else {
                rows.last().map(|(_, rank)| *rank + 1_000_000)
            }
        }
    };

    if let Some(target_rank) = target_rank {
        match direction {
            QueueMoveDirection::Up | QueueMoveDirection::Down => {
                let (target_id, _) = if direction == QueueMoveDirection::Up {
                    rows[index - 1]
                } else {
                    rows[index + 1]
                };
                sqlx::query(
                    "update ingest_job
                     set queue_rank = case
                         when id = $1 then $4
                         when id = $3 then $2
                         else queue_rank
                     end
                     where id in ($1, $3)",
                )
                .bind(job_id)
                .bind(rows[index].1)
                .bind(target_id)
                .bind(target_rank)
                .execute(&mut *tx)
                .await?;
            }
            QueueMoveDirection::Top | QueueMoveDirection::Bottom => {
                if target_rank != rows[index].1 {
                    sqlx::query("update ingest_job set queue_rank = $2 where id = $1")
                        .bind(job_id)
                        .bind(target_rank)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
    }

    tx.commit().await?;
    Ok(Some(()))
}

pub async fn pause_ingest_job(postgres: &PgPool, job_id: Uuid) -> Result<Option<()>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    let result = sqlx::query(
        "update ingest_job
         set queue_state = 'paused'::ingest_queue_state,
             available_at = now()
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
             completed_at = null
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

pub async fn cancel_ingest_job(postgres: &PgPool, job_id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "WITH target_job AS (
             SELECT id FROM ingest_job
             WHERE id = $1
               AND queue_state IN ('queued', 'leased', 'paused')
               AND completed_at IS NULL
         ),
         attempts_canceled AS (
             UPDATE ingest_attempt
             SET attempt_state = 'canceled',
                 failure_class = 'ingest_queue',
                 failure_code = 'canceled_by_operator',
                 failure_message = 'Processing was canceled from the administration queue',
                 finished_at = now()
             WHERE job_id IN (SELECT id FROM target_job)
               AND attempt_state IN ('leased', 'running')
         )
         UPDATE ingest_job
         SET queue_state = 'canceled', completed_at = now()
         WHERE id IN (SELECT id FROM target_job)",
    )
    .bind(job_id)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}

pub async fn recover_stale_canonical_leases(
    postgres: &PgPool,
    stale_threshold: chrono::Duration,
) -> Result<u64, sqlx::Error> {
    let cutoff = Utc::now() - stale_threshold;

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
    // Both branches write through `retryable = true` so operator
    // tooling that surfaces stalled documents still treats them as
    // recoverable; orphaned-attempt rows inherit that flag too but
    // their underlying job is already finalised so no retry runs.
    let result = sqlx::query(
        "with stale_attempts as (
             select a.id as attempt_id, a.job_id, j.queue_state::text as job_state
             from ingest_attempt a
             join ingest_job j on j.id = a.job_id
             where a.attempt_state = 'leased'
               and a.heartbeat_at < $1
               and j.queue_state in ('leased', 'completed', 'failed', 'canceled')
         ),
         failed_attempts as (
             update ingest_attempt
             set attempt_state = 'failed',
                 failure_class = 'lease_expired',
                 failure_code = 'stale_heartbeat',
                 failure_message = 'Attempt heartbeat expired before processing finished',
                 finished_at = now(),
                 retryable = true
             where id in (select attempt_id from stale_attempts)
         )
         update ingest_job
         set queue_state = 'queued',
             available_at = now()
         where id in (
             select job_id from stale_attempts where job_state = 'leased'
         )",
    )
    .bind(cutoff)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}

/// Marks every **non-terminal** ingest job tied to `document_id` as canceled
/// AND finalizes any attached leased attempts in one SQL round trip.
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
) -> Result<u64, sqlx::Error> {
    cancel_jobs_for_document_with_executor(postgres, document_id).await
}

pub async fn cancel_jobs_for_document_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<u64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query(
        "WITH target_jobs AS (
             SELECT j.id FROM ingest_job j
             WHERE j.mutation_id IN (
                 SELECT m.id FROM content_mutation m
                 JOIN content_mutation_item mi ON mi.mutation_id = m.id
                 WHERE mi.document_id = $1
             )
             AND j.queue_state IN ('queued', 'leased', 'paused')
             AND j.completed_at IS NULL
         ),
         attempts_canceled AS (
             UPDATE ingest_attempt
             SET attempt_state = 'canceled',
                 failure_class = 'content_mutation',
                 failure_code = 'canceled_by_request',
                 failure_message = 'Processing was canceled by request',
                 finished_at = now()
             WHERE job_id IN (SELECT id FROM target_jobs)
               AND attempt_state IN ('leased', 'running')
         )
         UPDATE ingest_job
         SET queue_state = 'canceled', completed_at = now()
         WHERE id IN (SELECT id FROM target_jobs)",
    )
    .bind(document_id)
    .execute(executor)
    .await?;
    Ok(result.rows_affected())
}
