use super::*;

/// Explicitly acknowledges a stuck draining owner and abandons its delivery.
///
/// This is an operator-only recovery primitive, not an automatic lease reaper:
/// a remote endpoint may already have received the request even though the
/// owner never persisted its result. Callers must require an explicit
/// at-least-once/duplicate-delivery risk acknowledgement before invoking it.
pub async fn force_abandon_draining_webhook_deliveries(
    postgres: &PgPool,
    subscription_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let draining = sqlx::query_scalar::<_, Uuid>(
        "select id
         from webhook_subscription
         where id = $1
           and active = false
           and delete_requested_at is not null
         for update",
    )
    .bind(subscription_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if draining.is_none() {
        transaction.rollback().await?;
        return Ok(0);
    }

    let job_ids = sqlx::query_scalar::<_, Uuid>(
        "select job.id
         from ingest_job job
         join webhook_delivery_attempt delivery on delivery.job_id = job.id
         where delivery.subscription_id = $1
         order by job.id
         for update of job",
    )
    .bind(subscription_id)
    .fetch_all(&mut *transaction)
    .await?;
    let delivery_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from webhook_delivery_attempt
         where subscription_id = $1
           and delivery_state = 'delivering'
         order by id
         for update",
    )
    .bind(subscription_id)
    .fetch_all(&mut *transaction)
    .await?;
    if delivery_ids.is_empty() {
        transaction.commit().await?;
        return Ok(0);
    }

    sqlx::query(
        "update webhook_delivery_attempt
         set delivery_state = 'abandoned',
             delivery_lease_token = null,
             next_attempt_at = null,
             error_code = 'operator_force_abandoned',
             error_message = 'Webhook delivery was explicitly abandoned by an operator',
             updated_at = now()
         where id = any($1)",
    )
    .bind(&delivery_ids)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update ingest_job
         set queue_state = 'canceled',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = any($1)
           and queue_state in ('queued', 'leased', 'paused', 'failed')",
    )
    .bind(&job_ids)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update ingest_attempt
         set attempt_state = 'canceled',
             failure_class = 'webhook_delivery',
             failure_code = 'operator_force_abandoned',
             failure_message = 'Webhook delivery ownership was explicitly abandoned by an operator',
             finished_at = now(),
             retryable = false
         where job_id = any($1)
           and attempt_state in ('leased', 'running')",
    )
    .bind(&job_ids)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    u64::try_from(delivery_ids.len())
        .map_err(|_| sqlx::Error::Protocol("webhook abandon count overflow".to_string()))
}

// ============================================================================
// webhook_delivery_attempt
// ============================================================================

/// Creates a pending delivery attempt row (before the job is enqueued).
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the attempt row.
pub async fn create_webhook_delivery_attempt(
    postgres: &PgPool,
    input: &NewWebhookDeliveryAttempt,
) -> Result<WebhookDeliveryAttemptRow, sqlx::Error> {
    create_webhook_delivery_attempt_with_executor(postgres, input).await
}

async fn create_webhook_delivery_attempt_with_executor<'e, E>(
    executor: E,
    input: &NewWebhookDeliveryAttempt,
) -> Result<WebhookDeliveryAttemptRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "insert into webhook_delivery_attempt (
            subscription_id, workspace_id, library_id,
            event_type, event_id, occurred_at, payload_json, target_url
        ) values ($1, $2, $3, $4, $5, $6, $7, $8)
        returning
            id, subscription_id, workspace_id, library_id,
            event_type, event_id, occurred_at, payload_json, target_url,
            attempt_number, delivery_state::text as delivery_state,
            response_status, response_body_excerpt, error_code, error_message,
            job_id, delivery_lease_token, next_attempt_at,
            delivered_at, created_at, updated_at",
    )
    .bind(input.subscription_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(&input.event_type)
    .bind(&input.event_id)
    .bind(input.occurred_at)
    .bind(input.payload_json.clone())
    .bind(&input.target_url)
    .fetch_one(executor)
    .await
}

/// Loads one delivery attempt by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the attempt row.
pub async fn get_webhook_delivery_attempt_by_id(
    postgres: &PgPool,
    id: Uuid,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "select id, subscription_id, workspace_id, library_id,
                event_type, event_id, occurred_at, payload_json, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, response_body_excerpt, error_code, error_message,
                job_id, delivery_lease_token, next_attempt_at,
                delivered_at, created_at, updated_at
         from webhook_delivery_attempt
         where id = $1",
    )
    .bind(id)
    .fetch_optional(postgres)
    .await
}

/// Lists one bounded, newest-first keyset page of data-minimized delivery
/// attempts for one subscription and authorized workspace.
///
/// # Errors
/// Returns any `SQLx` error raised while querying attempt rows.
pub async fn list_webhook_delivery_attempts_by_subscription(
    postgres: &PgPool,
    subscription_id: Uuid,
    workspace_id: Uuid,
    state_filter: Option<&str>,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<WebhookDeliveryAttemptViewPage, sqlx::Error> {
    let (cursor_created_at, cursor_id) = cursor.unzip();
    let bounded_limit = limit.clamp(1, MAX_WEBHOOK_API_PAGE_SIZE);
    // Fetch `limit + 1` rows so a next-page cursor can be reported without a
    // second round-trip; the extra row is trimmed below.
    let fetch_limit = i64::from(bounded_limit) + 1;
    let mut rows = sqlx::query_as::<_, WebhookDeliveryAttemptViewRow>(
        "select id, subscription_id, event_type, event_id, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, error_message, delivered_at,
                next_attempt_at, created_at
         from webhook_delivery_attempt
         where subscription_id = $1
           and workspace_id = $2
           and (
                $3::text is null
                or delivery_state = $3::webhook_delivery_state
           )
           and (
                $4::timestamptz is null
                or (created_at, id) < ($4, $5)
           )
         order by created_at desc, id desc
         limit $6",
    )
    .bind(subscription_id)
    .bind(workspace_id)
    .bind(state_filter)
    .bind(cursor_created_at)
    .bind(cursor_id)
    .bind(fetch_limit)
    .fetch_all(postgres)
    .await?;
    let limit_usize = usize::try_from(bounded_limit).unwrap_or(usize::MAX);
    let next_cursor = if rows.len() > limit_usize {
        rows.truncate(limit_usize);
        rows.last().map(|row| (row.created_at, row.id))
    } else {
        None
    };
    Ok(WebhookDeliveryAttemptViewPage { rows, next_cursor })
}

/// Links a delivery attempt to its ingest job by setting the `job_id` FK.
///
/// Does NOT change `delivery_state` — state transitions are handled separately
/// by `claim_attempt_for_delivery` (when the worker leases the job) and
/// `record_webhook_delivery_result` (when the attempt completes).
///
/// # Errors
/// Returns any `SQLx` error raised while updating the attempt row.
pub async fn link_attempt_to_job(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    link_attempt_to_job_with_executor(postgres, attempt_id, job_id).await
}

async fn link_attempt_to_job_with_executor<'e, E>(
    executor: E,
    attempt_id: Uuid,
    job_id: Uuid,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set job_id = $2,
             updated_at = now()
         where id = $1
           and delivery_state in ('pending', 'failed')
           and delivery_lease_token is null
           and (job_id is null or job_id = $2)
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, occurred_at, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_code, error_message,
             job_id, delivery_lease_token, next_attempt_at,
             delivered_at, created_at, updated_at",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(executor)
    .await
}

/// Atomically creates a delivery attempt, its queue job, and their link.
///
/// The ingest queue's `(library_id, dedupe_key)` uniqueness contract makes
/// event publication idempotent. If a duplicate job is rejected, this
/// transaction also rolls back the otherwise orphaned attempt row.
pub async fn enqueue_webhook_delivery(
    postgres: &PgPool,
    attempt_input: &NewWebhookDeliveryAttempt,
    job_input: &NewIngestJob,
) -> Result<WebhookDeliveryAttemptRow, sqlx::Error> {
    if attempt_input.workspace_id != job_input.workspace_id
        || attempt_input.library_id != job_input.library_id
        || job_input.job_kind != "webhook_delivery"
    {
        return Err(sqlx::Error::Protocol(
            "webhook attempt and queue job scopes must match".to_string(),
        ));
    }
    let mut transaction = postgres.begin().await?;
    let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        attempt_input.workspace_id,
        attempt_input.library_id,
    )
    .await?;
    if !parent_locked {
        transaction.rollback().await?;
        return Err(sqlx::Error::Protocol(
            "webhook delivery library scope is unavailable".to_string(),
        ));
    }
    let attempt =
        match create_webhook_delivery_attempt_with_executor(&mut *transaction, attempt_input).await
        {
            Ok(attempt) => attempt,
            Err(error) if is_webhook_delivery_event_identity_conflict(&error) => {
                transaction.rollback().await?;
                return find_compatible_linked_delivery_attempt(postgres, attempt_input)
                    .await?
                    .ok_or(error);
            }
            Err(error) => return Err(error),
        };
    let job = match create_ingest_job_with_executor(&mut *transaction, job_input).await {
        Ok(job) => job,
        Err(error) if is_ingest_job_dedupe_conflict(&error) => {
            transaction.rollback().await?;
            return find_compatible_linked_delivery_attempt(postgres, attempt_input)
                .await?
                .ok_or(error);
        }
        Err(error) => return Err(error),
    };
    let linked = link_attempt_to_job_with_executor(&mut *transaction, attempt.id, job.id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    transaction.commit().await?;
    Ok(linked)
}

pub(super) fn is_ingest_job_dedupe_conflict(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database_error)
            if database_error.constraint() == Some("idx_ingest_job_dedupe_key")
    )
}

pub(super) fn is_webhook_delivery_event_identity_conflict(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database_error)
            if database_error.constraint()
                == Some("webhook_delivery_attempt_subscription_event_key")
    )
}

pub(super) async fn find_linked_delivery_attempt(
    postgres: &PgPool,
    subscription_id: Uuid,
    event_id: &str,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "select id, subscription_id, workspace_id, library_id,
                event_type, event_id, occurred_at, payload_json, target_url,
                attempt_number, delivery_state::text as delivery_state,
                response_status, response_body_excerpt, error_code, error_message,
                job_id, delivery_lease_token, next_attempt_at,
                delivered_at, created_at, updated_at
         from webhook_delivery_attempt
         where subscription_id = $1
           and event_id = $2
           and job_id is not null
         order by created_at asc, id asc
         limit 1",
    )
    .bind(subscription_id)
    .bind(event_id)
    .fetch_optional(postgres)
    .await
}

pub(super) async fn find_compatible_linked_delivery_attempt(
    postgres: &PgPool,
    expected: &NewWebhookDeliveryAttempt,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let existing =
        find_linked_delivery_attempt(postgres, expected.subscription_id, &expected.event_id)
            .await?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    if existing.workspace_id != expected.workspace_id
        || existing.library_id != Some(expected.library_id)
        || existing.event_type != expected.event_type
        || existing.occurred_at.timestamp_micros() != expected.occurred_at.timestamp_micros()
        || existing.payload_json != expected.payload_json
    {
        return Err(sqlx::Error::Protocol(
            "webhook delivery event identity was reused with incompatible content".to_string(),
        ));
    }
    Ok(Some(existing))
}

/// Claims a delivery attempt for one exact queue job and returns the coherent
/// subscription snapshot protected by the same transaction.
///
/// A recent `delivering` row cannot be claimed twice. A row older than the
/// configured five-minute stale-lease window may be reclaimed after a worker
/// crash.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the attempt row.
pub async fn claim_attempt_for_delivery(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
) -> Result<WebhookDeliveryClaimOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    // Lock order is subscription -> delivery attempt, matching deletion. A
    // delete that wins this lock removes/cancels the row before claim; a claim
    // that wins publishes `delivering` before delete decides whether it may
    // return success.
    let subscription = sqlx::query_as::<_, WebhookSubscriptionRow>(
        "select
            subscription.id,
            subscription.workspace_id,
            subscription.library_id,
            subscription.display_name,
            subscription.target_url,
            subscription.secret,
            subscription.event_types,
            subscription.custom_headers_json,
            subscription.active,
            subscription.created_by_principal_id,
            subscription.created_at,
            subscription.updated_at
         from webhook_subscription subscription
         join webhook_delivery_attempt delivery
           on delivery.subscription_id = subscription.id
         where delivery.id = $1
           and delivery.job_id = $2
         for share of subscription",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(subscription) = subscription else {
        transaction.commit().await?;
        return Ok(WebhookDeliveryClaimOutcome::Canceled);
    };

    let lease_token = Uuid::now_v7();
    let claimed = sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set delivery_state = 'delivering',
             target_url = $3,
             delivery_lease_token = $4,
             response_status = null,
             response_body_excerpt = null,
             error_code = null,
             error_message = null,
             next_attempt_at = null,
             updated_at = now()
         where id = $1
           and job_id = $2
           and (
                delivery_state in ('pending', 'failed')
                or (
                    delivery_state = 'delivering'
                    and delivery_lease_token is not null
                    and updated_at < now() - make_interval(secs => $5::double precision)
                )
           )
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, occurred_at, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_code, error_message,
             job_id, delivery_lease_token, next_attempt_at,
             delivered_at, created_at, updated_at",
    )
    .bind(attempt_id)
    .bind(job_id)
    .bind(&subscription.target_url)
    .bind(lease_token)
    .bind(WEBHOOK_DELIVERY_LEASE_TIMEOUT_SECONDS as f64)
    .fetch_optional(&mut *transaction)
    .await?;
    if let Some(attempt) = claimed {
        transaction.commit().await?;
        return Ok(WebhookDeliveryClaimOutcome::Claimed {
            attempt: Box::new(attempt),
            subscription: Box::new(subscription),
        });
    }

    let current = sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
        "select delivery_state::text,
                case
                    when delivery_state <> 'delivering' then null
                    when delivery_lease_token is null
                        then now() + make_interval(secs => $4::double precision)
                    when $3::bool then greatest(
                        updated_at + make_interval(secs => $4::double precision),
                        now()
                    )
                    else now() + make_interval(secs => $4::double precision)
                end as retry_at
         from webhook_delivery_attempt
         where id = $1
           and job_id = $2",
    )
    .bind(attempt_id)
    .bind(job_id)
    .bind(subscription.active)
    .bind(WEBHOOK_DELIVERY_LEASE_TIMEOUT_SECONDS as f64)
    .fetch_optional(&mut *transaction)
    .await?;
    transaction.commit().await?;
    let Some((delivery_state, retry_at)) = current else {
        return Ok(WebhookDeliveryClaimOutcome::Canceled);
    };
    if delivery_state == "delivering" {
        let retry_at = retry_at.ok_or_else(|| {
            sqlx::Error::Protocol("delivering webhook lease is missing database retry time".into())
        })?;
        return Ok(WebhookDeliveryClaimOutcome::InFlight { attempt_id, retry_at });
    }
    Ok(WebhookDeliveryClaimOutcome::Terminal { attempt_id, delivery_state })
}
