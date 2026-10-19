use super::*;

enum QueuedJobMove {
    Swap { target_id: Uuid, target_rank: i64 },
    SetRank(i64),
}

pub async fn update_ingest_job(
    postgres: &PgPool,
    job_id: Uuid,
    input: &UpdateIngestJob,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
        "update ingest_job
         set {INGEST_JOB_UPDATE_ASSIGNMENTS}
         where id = $1
         returning
            {INGEST_JOB_COLUMNS}",
    )))
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
    // The zombie-lease problem is handled by `recover_stale_ingest_leases`
    // (the stale-lease reaper) on its own tick. The dispatcher should not
    // try to detect zombies — its only job is to enforce limits.
    // `queue_rank` is the operator-visible order. Fairness and priority remain
    // deterministic tie-breakers, but they must not contradict the queue shown
    // in the administration UI.
    let mut tx = postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('ingest.queue.claim', 0))")
        .execute(&mut *tx)
        .await?;

    let claimed = sqlx::query_as::<_, IngestJobRow>(sqlx::AssertSqlSafe(format!(
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
            {INGEST_JOB_COLUMNS}",
    )))
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
