use super::*;

/// Transaction-aware lifecycle outbox insert used by content-state writers.
pub async fn enqueue_webhook_lifecycle_event_with_executor<'e, E>(
    executor: E,
    event: &WebhookEvent,
) -> Result<WebhookLifecycleOutboxRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let candidate_id = Uuid::now_v7();
    sqlx::query_as::<_, WebhookLifecycleOutboxRow>(
        "with event_row as (
            insert into webhook_lifecycle_outbox (
                id, event_id, event_type, occurred_at,
                workspace_id, library_id, payload_json
            ) values ($1, $2, $3, $4, $5, $6, $7)
            on conflict (event_id) do update
            set event_id = excluded.event_id
            where webhook_lifecycle_outbox.event_type = excluded.event_type
              and webhook_lifecycle_outbox.occurred_at = excluded.occurred_at
              and webhook_lifecycle_outbox.workspace_id = excluded.workspace_id
              and webhook_lifecycle_outbox.library_id = excluded.library_id
              and webhook_lifecycle_outbox.payload_json = excluded.payload_json
            returning
                id, event_id, event_type, occurred_at,
                workspace_id, library_id, payload_json,
                dispatch_state, dispatch_attempts, available_at,
                lease_owner, lease_token, leased_at, lease_expires_at,
                last_error_code, last_error, dispatched_at,
                resolution_reason_code, resolved_at, created_at, updated_at,
                id = $1 as newly_created
         ), recipient_snapshot as (
            insert into webhook_lifecycle_outbox_recipient (outbox_id, subscription_id)
            select event_row.id, subscription.id
            from event_row
            join webhook_subscription as subscription
              on subscription.workspace_id = event_row.workspace_id
             and subscription.active = true
             and event_row.event_type = any(subscription.event_types)
             and (
                 subscription.library_id is null
                 or subscription.library_id = event_row.library_id
             )
            where event_row.newly_created
            on conflict (outbox_id, subscription_id) do nothing
            returning outbox_id
         )
         select
            id, event_id, event_type, occurred_at,
            workspace_id, library_id, payload_json,
            dispatch_state, dispatch_attempts, available_at,
            lease_owner, lease_token, leased_at, lease_expires_at,
            last_error_code, last_error, dispatched_at,
            resolution_reason_code, resolved_at, created_at, updated_at
         from event_row",
    )
    .bind(candidate_id)
    .bind(&event.event_id)
    .bind(&event.event_type)
    .bind(event.occurred_at)
    .bind(event.workspace_id)
    .bind(event.library_id)
    .bind(event.payload_json.clone())
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| {
        sqlx::Error::Protocol(
            "webhook lifecycle event identity was reused for different event data".to_string(),
        )
    })
}

/// Resolves the still-active recipients captured when an event was inserted.
///
/// Event type and library scope are intentionally not re-evaluated: membership
/// is immutable event-time state. A deleted or subsequently deactivated
/// subscription is a terminal skip, while delivery uses its current target so
/// target URL and encrypted delivery configuration stay coherent.
pub async fn list_active_webhook_lifecycle_recipient_targets(
    postgres: &PgPool,
    outbox_id: Uuid,
) -> Result<Vec<WebhookSubscriptionTargetRow>, sqlx::Error> {
    let targets = sqlx::query_as::<_, WebhookSubscriptionTargetRow>(
        "select recipient.subscription_id as id, subscription.target_url
         from webhook_lifecycle_outbox_recipient as recipient
         join webhook_subscription as subscription
           on subscription.id = recipient.subscription_id
         where recipient.outbox_id = $1
           and subscription.active = true
         order by recipient.subscription_id asc
         limit $2",
    )
    .bind(outbox_id)
    .bind(MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE + 1)
    .fetch_all(postgres)
    .await?;
    if i64::try_from(targets.len()).unwrap_or(i64::MAX)
        > MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE
    {
        return Err(sqlx::Error::Protocol(
            "webhook lifecycle recipient snapshot exceeds the active subscription quota"
                .to_string(),
        ));
    }
    Ok(targets)
}

/// Leases a bounded batch of due or expired lifecycle events with
/// `FOR UPDATE SKIP LOCKED`.
pub async fn lease_webhook_lifecycle_outbox_batch(
    postgres: &PgPool,
    lease_owner: &str,
    lease_duration: chrono::Duration,
    limit: i64,
) -> Result<WebhookLifecycleOutboxLease, sqlx::Error> {
    let lease_owner = lease_owner.trim();
    if lease_owner.is_empty() || lease_owner.chars().count() > 255 {
        return Err(sqlx::Error::Protocol(
            "webhook lifecycle outbox lease owner must contain 1 to 255 characters".to_string(),
        ));
    }
    let limit = limit.clamp(1, MAX_WEBHOOK_LIFECYCLE_OUTBOX_BATCH_LIMIT);
    let lease_seconds = lease_duration.num_seconds().clamp(1, 60 * 60);
    let lease_token = Uuid::now_v7();
    let events = sqlx::query_as::<_, WebhookLifecycleOutboxRow>(
        "with candidates as (
            select id
            from webhook_lifecycle_outbox
            where (dispatch_state = 'pending' and available_at <= now())
               or (dispatch_state = 'dispatching' and lease_expires_at <= now())
            order by available_at asc, created_at asc, id asc
            for update skip locked
            limit $1
         )
         update webhook_lifecycle_outbox as outbox
         set dispatch_state = 'dispatching',
             dispatch_attempts = outbox.dispatch_attempts + 1,
             lease_owner = $2,
             lease_token = $3,
             leased_at = now(),
             lease_expires_at = now() + make_interval(secs => $4::double precision),
             updated_at = now()
         from candidates
         where outbox.id = candidates.id
         returning
            outbox.id, outbox.event_id, outbox.event_type, outbox.occurred_at,
            outbox.workspace_id, outbox.library_id, outbox.payload_json,
            outbox.dispatch_state, outbox.dispatch_attempts, outbox.available_at,
            outbox.lease_owner, outbox.lease_token, outbox.leased_at,
            outbox.lease_expires_at, outbox.last_error_code, outbox.last_error,
            outbox.dispatched_at, outbox.resolution_reason_code, outbox.resolved_at,
            outbox.created_at, outbox.updated_at",
    )
    .bind(limit)
    .bind(lease_owner)
    .bind(lease_token)
    .bind(lease_seconds as f64)
    .fetch_all(postgres)
    .await?;
    Ok(WebhookLifecycleOutboxLease { lease_token, events })
}

/// Extends one active lifecycle-outbox lease using the `PostgreSQL` clock.
///
/// The state and token predicates are the fencing guard. A stale worker gets
/// `false` and must cancel the remaining fanout instead of extending or
/// completing work now owned by another relay.
pub async fn renew_webhook_lifecycle_outbox_lease(
    postgres: &PgPool,
    outbox_id: Uuid,
    lease_token: Uuid,
    lease_duration: chrono::Duration,
) -> Result<bool, sqlx::Error> {
    let lease_seconds = lease_duration.num_seconds().clamp(1, 60 * 60);
    let result = sqlx::query(
        "update webhook_lifecycle_outbox
         set lease_expires_at = now() + make_interval(secs => $3::double precision),
             updated_at = now()
         where id = $1
           and dispatch_state = 'dispatching'
           and lease_token = $2",
    )
    .bind(outbox_id)
    .bind(lease_token)
    .bind(lease_seconds as f64)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Marks one event dispatched only while the caller owns its current lease.
pub async fn mark_webhook_lifecycle_outbox_dispatched(
    postgres: &PgPool,
    outbox_id: Uuid,
    lease_token: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update webhook_lifecycle_outbox
         set dispatch_state = 'dispatched',
             lease_owner = null,
             lease_token = null,
             leased_at = null,
             lease_expires_at = null,
             last_error_code = null,
             last_error = null,
             dispatched_at = now(),
             resolution_reason_code = null,
             resolved_at = null,
             updated_at = now()
         where id = $1
           and dispatch_state = 'dispatching'
           and lease_token = $2",
    )
    .bind(outbox_id)
    .bind(lease_token)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Releases a retryable failed event, or dead-letters a permanent failure and
/// any retryable failure that reached `max_attempts`.
/// Returns the new state only while the caller still owns the current lease.
pub async fn fail_webhook_lifecycle_outbox_dispatch(
    postgres: &PgPool,
    input: WebhookLifecycleOutboxFailure<'_>,
) -> Result<Option<String>, sqlx::Error> {
    let WebhookLifecycleOutboxFailure {
        outbox_id,
        lease_token,
        retry_at,
        error_code,
        error_message,
        max_attempts,
        retryable,
    } = input;
    if error_code.is_empty()
        || error_code.len() > 64
        || !error_code.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))
        })
    {
        return Err(sqlx::Error::Protocol(
            "webhook lifecycle outbox failure code is not normalized".to_string(),
        ));
    }
    sqlx::query_scalar::<_, String>(
        "update webhook_lifecycle_outbox
         set dispatch_state = case
                 when not $7 or dispatch_attempts >= $6 then 'dead_letter'
                 else 'pending'
             end,
             available_at = case
                 when not $7 or dispatch_attempts >= $6 then available_at
                 else $3
             end,
             lease_owner = null,
             lease_token = null,
             leased_at = null,
             lease_expires_at = null,
             last_error_code = $4,
             last_error = $5,
             dispatched_at = null,
             resolution_reason_code = null,
             resolved_at = null,
             updated_at = now()
         where id = $1
           and dispatch_state = 'dispatching'
           and lease_token = $2
         returning dispatch_state",
    )
    .bind(outbox_id)
    .bind(lease_token)
    .bind(retry_at)
    .bind(error_code)
    .bind(error_message)
    .bind(max_attempts.max(1))
    .bind(retryable)
    .fetch_optional(postgres)
    .await
}

/// Failure disposition and retry deadline for the currently leased lifecycle event.
pub struct WebhookLifecycleOutboxFailure<'a> {
    pub outbox_id: Uuid,
    pub lease_token: Uuid,
    pub retry_at: DateTime<Utc>,
    pub error_code: &'a str,
    pub error_message: &'a str,
    pub max_attempts: i32,
    pub retryable: bool,
}

/// Deletes a bounded batch of old successfully dispatched lifecycle events.
/// Dead-letter and explicitly resolved rows are retained for operator audit.
pub async fn prune_dispatched_webhook_lifecycle_outbox(
    postgres: &PgPool,
    dispatched_before: DateTime<Utc>,
    limit: i64,
) -> Result<u64, sqlx::Error> {
    let limit = limit.clamp(1, 10_000);
    let result = sqlx::query(
        "with expired as (
            select id
            from webhook_lifecycle_outbox
            where dispatch_state = 'dispatched'
              and dispatched_at < $1
            order by dispatched_at asc, id asc
            limit $2
         )
         delete from webhook_lifecycle_outbox as outbox
         using expired
         where outbox.id = expired.id",
    )
    .bind(dispatched_before)
    .bind(limit)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}
