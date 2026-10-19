use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Defers a webhook queue job until the delivery lease can be reclaimed.
///
/// This is intentionally not the generic retry-budget path: a duplicate
/// worker that observes another delivery owner has not performed HTTP and
/// must not consume the job's failure budget or finalize it as succeeded.
/// Both the queue lease token and ingest-attempt state are compared in one
/// transaction so a stale worker cannot defer a newer owner.
pub async fn defer_webhook_delivery_in_flight(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
    expected_queue_lease_token: &str,
    retry_at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let owns_job = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_job
         where id = $1
           and job_kind = 'webhook_delivery'::ingest_job_kind
           and queue_state = 'leased'
           and queue_lease_token = $2
         for update",
    )
    .bind(job_id)
    .bind(expected_queue_lease_token)
    .fetch_optional(&mut *transaction)
    .await?;
    if owns_job.is_none() {
        transaction.rollback().await?;
        return Ok(false);
    }

    let attempt = sqlx::query_scalar::<_, Uuid>(
        "update ingest_attempt
         set attempt_state = 'failed',
             current_stage = 'webhook_delivery',
             heartbeat_at = now(),
             finished_at = now(),
             failure_class = 'webhook_delivery',
             failure_code = 'delivery_lease_in_flight',
             failure_message = 'Another worker still owns the webhook delivery lease',
             retryable = true
         where id = $1
           and job_id = $2
           and attempt_state = 'leased'
         returning id",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if attempt.is_none() {
        transaction.rollback().await?;
        return Ok(false);
    }

    let result = sqlx::query(
        "update ingest_job
         set queue_state = 'queued',
             available_at = greatest($3, now()),
             completed_at = null,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'leased'
           and queue_lease_token = $2",
    )
    .bind(job_id)
    .bind(expected_queue_lease_token)
    .bind(retry_at)
    .execute(&mut *transaction)
    .await?;
    if result.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(false);
    }
    transaction.commit().await?;
    Ok(true)
}
