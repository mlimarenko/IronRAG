use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct IngestJobRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
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
    #[sqlx(default)]
    pub queue_leased_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    pub queue_lease_token: Option<String>,
    #[sqlx(default)]
    pub queue_lease_owner: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewIngestJob {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
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
    pub queue_leased_at: Option<DateTime<Utc>>,
    pub queue_lease_token: Option<String>,
    pub queue_lease_owner: Option<String>,
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

enum QueuedJobMove {
    Swap { target_id: Uuid, target_rank: i64 },
    SetRank(i64),
}

pub async fn create_ingest_job(
    postgres: &PgPool,
    input: &NewIngestJob,
) -> Result<IngestJobRow, sqlx::Error> {
    create_ingest_job_with_executor(postgres, input).await
}

/// Creates a queue job using the caller's pool connection or transaction.
///
/// Transaction-aware callers use this to commit a job and its owning domain
/// record atomically.
pub async fn create_ingest_job_with_executor<'e, E>(
    executor: E,
    input: &NewIngestJob,
) -> Result<IngestJobRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, IngestJobRow>(
        "insert into ingest_job (
            id,
            workspace_id,
            library_id,
            mutation_id,
            mutation_item_id,
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
            $9,
            $10::ingest_job_kind,
            $11::ingest_queue_state,
            $12,
            $13,
            coalesce($14, now()),
            coalesce($15, now()),
            $16
        )
        returning
            id,
            workspace_id,
            library_id,
            mutation_id,
            mutation_item_id,
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
    .bind(input.mutation_item_id)
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
    .fetch_one(executor)
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
            mutation_item_id,
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
            completed_at,
            queue_leased_at,
            queue_lease_token,
            queue_lease_owner
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
            mutation_item_id,
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
            completed_at,
            queue_leased_at,
            queue_lease_token,
            queue_lease_owner
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
            mutation_item_id,
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
            mutation_item_id,
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
            mutation_item_id,
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
            mutation_item_id,
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
            mutation_item_id,
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

#[derive(Debug, Clone)]
pub struct IngestJobPage {
    pub rows: Vec<IngestJobRow>,
    pub has_more: bool,
}

/// Keyset-paginated fetch for `GET /v1/ingest/libraries/{libraryId}/jobs`.
///
/// * `limit` is clamped by the caller to a sane page size; this function
///   fetches `limit + 1` rows so `has_more` is derived without a `COUNT(*)`.
/// * `cursor` is `(queued_at, id)` of the last row on the previous page;
///   rows strictly older on the `(queued_at desc, id desc)` keyset are
///   returned. Unlike `list_active_ingest_queue` (a live "currently active"
///   snapshot), this is the full paginated history, including terminal
///   (`completed` / `failed` / `canceled`) jobs.
/// * `status_filter` holds `ingest_queue_state` values; empty = no filter.
pub async fn list_ingest_jobs_page(
    postgres: &PgPool,
    library_id: Uuid,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
    status_filter: &[String],
) -> Result<IngestJobPage, sqlx::Error> {
    let fetch_limit = limit + 1;
    let (cursor_queued_at, cursor_id) =
        cursor.map_or((None, None), |(queued_at, id)| (Some(queued_at), Some(id)));

    let mut rows = sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
            mutation_item_id,
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
           and ($2::timestamptz is null or (queued_at, id) < ($2, $3))
           and (cardinality($4::text[]) = 0 or queue_state::text = any($4))
         order by queued_at desc, id desc
         limit $5",
    )
    .bind(library_id)
    .bind(cursor_queued_at)
    .bind(cursor_id)
    .bind(status_filter)
    .bind(fetch_limit)
    .fetch_all(postgres)
    .await?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(IngestJobPage { rows, has_more })
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
                    mutation_item_id,
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
                    mutation_item_id,
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
                    mutation_item_id,
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
                    mutation_item_id,
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
             completed_at = $12,
             queue_leased_at = case when $8::ingest_queue_state = 'leased' then queue_leased_at else null end,
             queue_lease_token = case when $8::ingest_queue_state = 'leased' then queue_lease_token else null end,
             queue_lease_owner = case when $8::ingest_queue_state = 'leased' then queue_lease_owner else null end
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            mutation_id,
            mutation_item_id,
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
    queue_lease_token: &str,
    queue_lease_owner: &str,
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
    let mut tx = postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('ingest.queue.claim', 0))")
        .execute(&mut *tx)
        .await?;

    let claimed = sqlx::query_as::<_, IngestJobRow>(
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
         set queue_state = 'leased'::ingest_queue_state,
             queue_leased_at = now(),
             queue_lease_token = $1,
             queue_lease_owner = $2
         where id = (
             select j.id from ingest_job j
             left join library_running lr on lr.library_id = j.library_id
             where j.queue_state = 'queued'
               and j.available_at <= now()
               and (select count(*) from active_leases) < $5::bigint
               and (
                   select count(*) from active_leases al
                   where al.workspace_id = j.workspace_id
               ) < $4::bigint
               and (
                   select count(*) from active_leases al
                   where al.library_id = j.library_id
               ) < $3::bigint
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
            mutation_item_id,
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
            completed_at,
            queue_leased_at,
            queue_lease_token,
            queue_lease_owner",
    )
    .bind(queue_lease_token)
    .bind(queue_lease_owner)
    .bind(max_jobs_per_library)
    .bind(max_jobs_per_workspace)
    .bind(max_jobs_global)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(claimed)
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
             j.queue_leased_at,
             j.queue_lease_token,
             j.queue_lease_owner,
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
    let rows = queued_ingest_job_rows(&mut tx).await?;

    let Some(index) = rows.iter().position(|(id, _)| *id == job_id) else {
        tx.commit().await?;
        return Ok(None);
    };

    if let Some(job_move) = queued_job_move(&rows, index, direction) {
        apply_queued_job_move(&mut tx, job_id, rows[index].1, job_move).await?;
    }

    tx.commit().await?;
    Ok(Some(()))
}

async fn queued_ingest_job_rows(
    tx: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<Vec<(Uuid, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (Uuid, i64)>(
        "select id, queue_rank
         from ingest_job
         where queue_state in ('queued', 'paused')
         order by queue_rank asc, priority asc, available_at asc, queued_at asc, id asc
         for update",
    )
    .fetch_all(&mut **tx)
    .await
}

fn queued_job_move(
    rows: &[(Uuid, i64)],
    index: usize,
    direction: QueueMoveDirection,
) -> Option<QueuedJobMove> {
    match direction {
        QueueMoveDirection::Up => index.checked_sub(1).map(|target_index| QueuedJobMove::Swap {
            target_id: rows[target_index].0,
            target_rank: rows[target_index].1,
        }),
        QueueMoveDirection::Down => rows
            .get(index + 1)
            .map(|target| QueuedJobMove::Swap { target_id: target.0, target_rank: target.1 }),
        QueueMoveDirection::Top => index
            .checked_sub(1)
            .and_then(|_| rows.first())
            .map(|(_, rank)| QueuedJobMove::SetRank(rank - 1_000_000)),
        QueueMoveDirection::Bottom => rows
            .get(index + 1)
            .and_then(|_| rows.last().map(|(_, rank)| QueuedJobMove::SetRank(rank + 1_000_000))),
    }
}

async fn apply_queued_job_move(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    job_id: Uuid,
    current_rank: i64,
    job_move: QueuedJobMove,
) -> Result<(), sqlx::Error> {
    match job_move {
        QueuedJobMove::Swap { target_id, target_rank } => {
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
            .bind(current_rank)
            .bind(target_id)
            .bind(target_rank)
            .execute(&mut **tx)
            .await?;
        }
        QueuedJobMove::SetRank(target_rank) if target_rank != current_rank => {
            sqlx::query("update ingest_job set queue_rank = $2 where id = $1")
                .bind(job_id)
                .bind(target_rank)
                .execute(&mut **tx)
                .await?;
        }
        QueuedJobMove::SetRank(_) => {}
    }
    Ok(())
}

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

    let row = sqlx::query_as::<_, IngestJobRow>(
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
            id,
            workspace_id,
            library_id,
            mutation_id,
            mutation_item_id,
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
            completed_at,
            queue_leased_at,
            queue_lease_token,
            queue_lease_owner",
    )
    .bind(job_id)
    .bind(stale_seconds as f64)
    .bind(available_at)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn cancel_ingest_job(postgres: &PgPool, job_id: Uuid) -> Result<u64, sqlx::Error> {
    let affected = sqlx::query_scalar::<_, i64>(
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
         select count(*)::bigint
                + (select count(*) * 0 from attempts_canceled)
         from job_canceled",
    )
    .bind(job_id)
    .fetch_one(postgres)
    .await?;
    u64::try_from(affected)
        .map_err(|_| sqlx::Error::Protocol("negative canceled ingest job count".to_string()))
}

pub async fn recover_stale_canonical_leases(
    postgres: &PgPool,
    stale_threshold: chrono::Duration,
) -> Result<u64, sqlx::Error> {
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
    // Recovery marks stale leased attempts with the canonical stale-heartbeat
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
        return Ok(0);
    }

    sqlx::query(
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
           )",
    )
    .bind(stale_seconds as f64)
    .bind(&locked_job_ids)
    .execute(&mut *tx)
    .await?;

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
             returning id
         ),
         attempts_canceled AS (
             UPDATE ingest_attempt
             SET attempt_state = 'canceled',
                 failure_class = 'content_mutation',
                 failure_code = 'canceled_by_request',
                 failure_message = 'Processing was canceled by request',
                 finished_at = now()
             WHERE job_id IN (SELECT id FROM jobs_canceled)
               AND attempt_state IN ('leased', 'running')
             returning id
         )
         select count(*)::bigint
                + (select count(*) * 0 from attempts_canceled)
         from jobs_canceled",
    )
    .bind(document_id)
    .fetch_one(executor)
    .await?;
    u64::try_from(affected)
        .map_err(|_| sqlx::Error::Protocol("negative canceled ingest job count".to_string()))
}
