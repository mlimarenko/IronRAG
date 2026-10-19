use sqlx::PgPool;
use uuid::Uuid;

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
             update ingest_attempt as attempt
             set heartbeat_at = now(),
                 current_stage = coalesce($2, current_stage)
             where attempt.id = $1
               and attempt.attempt_state = 'leased'
               and not exists (
                   select 1
                   from ingest_attempt newer
                   where newer.job_id = attempt.job_id
                     and newer.attempt_number > attempt.attempt_number
               )
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
