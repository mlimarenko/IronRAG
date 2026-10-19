use super::*;

fn validate_webhook_delivery_completion(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    validate_webhook_completion_state(completion)?;
    validate_webhook_completion_failure(completion)?;
    validate_webhook_completion_delivery(completion)?;
    validate_webhook_completion_failure_fields(completion)
}

fn webhook_completion_protocol_error(message: &str) -> sqlx::Error {
    sqlx::Error::Protocol(message.into())
}

fn validate_webhook_completion_state(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    if !matches!(completion.delivery_state, "delivered" | "failed" | "abandoned")
        || completion.attempt_number < 1
    {
        return Err(webhook_completion_protocol_error(
            "webhook completion must be terminal for one attempted delivery",
        ));
    }
    Ok(())
}

fn validate_webhook_completion_failure(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    if completion.error_code.is_some() != completion.error_summary.is_some() {
        return Err(webhook_completion_protocol_error(
            "webhook completion error code and summary must be supplied together",
        ));
    }
    match (completion.delivery_state, completion.error_code.is_some()) {
        ("delivered", true) => Err(webhook_completion_protocol_error(
            "delivered webhook completion cannot contain a failure",
        )),
        ("delivered", false) | (_, true) => Ok(()),
        (_, false) => Err(webhook_completion_protocol_error(
            "failed webhook completion requires a typed failure",
        )),
    }
}

fn validate_webhook_completion_delivery(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    if completion.delivery_state == "delivered"
        && (!completion.response_status.is_some_and(|status| (200..300).contains(&status))
            || completion.next_attempt_at.is_some())
    {
        return Err(webhook_completion_protocol_error(
            "delivered webhook completion requires a successful status and no retry",
        ));
    }
    if completion.delivery_state == "abandoned" && completion.next_attempt_at.is_some() {
        return Err(webhook_completion_protocol_error(
            "abandoned webhook completion cannot schedule a retry",
        ));
    }
    Ok(())
}

fn validate_webhook_completion_failure_fields(
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<(), sqlx::Error> {
    if completion.error_code.is_some_and(|code| !webhook_failure_code(code)) {
        return Err(webhook_completion_protocol_error(
            "webhook completion failure code is not normalized",
        ));
    }
    if completion.error_summary.is_some_and(|summary| summary.len() > 512) {
        return Err(webhook_completion_protocol_error(
            "webhook completion failure summary exceeds 512 bytes",
        ));
    }
    Ok(())
}

fn webhook_failure_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))
        })
}

async fn record_webhook_delivery_result_with_executor<'e, E>(
    executor: E,
    completion: &WebhookDeliveryCompletion<'_>,
    replacement_job_id: Option<Uuid>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, WebhookDeliveryAttemptRow>(
        "update webhook_delivery_attempt
         set delivery_state         = $4::webhook_delivery_state,
             attempt_number         = $5,
             response_status        = $6,
             response_body_excerpt  = null,
             error_code             = $7,
             error_message          = $8,
             next_attempt_at        = $9,
             job_id                 = coalesce($10, job_id),
             delivery_lease_token   = null,
             delivered_at           = case when $4 = 'delivered' then now() else delivered_at end,
             updated_at             = now()
         where id = $1
           and job_id = $2
           and delivery_lease_token = $3
           and delivery_state = 'delivering'
         returning
             id, subscription_id, workspace_id, library_id,
             event_type, event_id, occurred_at, payload_json, target_url,
             attempt_number, delivery_state::text as delivery_state,
             response_status, response_body_excerpt, error_code, error_message,
             job_id, delivery_lease_token, next_attempt_at,
             delivered_at, created_at, updated_at",
    )
    .bind(completion.attempt_id)
    .bind(completion.job_id)
    .bind(completion.lease_token)
    .bind(completion.delivery_state)
    .bind(completion.attempt_number)
    .bind(completion.response_status)
    .bind(completion.error_code)
    .bind(completion.error_summary)
    .bind(completion.next_attempt_at)
    .bind(replacement_job_id)
    .fetch_optional(executor)
    .await
}

/// Records a delivery result only while the caller owns the exact attempt/job
/// lease. A stale worker receives `None` and cannot overwrite a newer result.
///
/// # Errors
/// Returns a protocol error for an invalid completion or any `SQLx` error raised
/// while updating the attempt row.
pub async fn record_webhook_delivery_result(
    postgres: &PgPool,
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    validate_webhook_delivery_completion(completion)?;
    record_webhook_delivery_result_with_executor(postgres, completion, None).await
}

/// Releases an owned delivery claim when execution is canceled before a
/// response can be classified. The attempt number is intentionally unchanged:
/// no delivery result was observed, and the current ingest lease decides
/// whether the queue job is canceled, paused, or retried.
pub async fn release_webhook_delivery_claim(
    postgres: &PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
    lease_token: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update webhook_delivery_attempt
         set delivery_state = 'pending',
             delivery_lease_token = null,
             response_status = null,
             response_body_excerpt = null,
             error_code = null,
             error_message = null,
             next_attempt_at = null,
             updated_at = now()
         where id = $1
           and job_id = $2
           and delivery_lease_token = $3
           and delivery_state = 'delivering'",
    )
    .bind(attempt_id)
    .bind(job_id)
    .bind(lease_token)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Atomically records a retryable delivery result, links the delivery to its
/// replacement job, and terminalizes the exact current ingest attempt/job.
///
/// There is no committed state in which the delivery points at the replacement
/// while the old queue lease remains recoverable. A process crash or an
/// ambiguous database response therefore cannot make the old job consume its
/// retry budget after `job_id` has moved.
pub async fn record_webhook_delivery_failure_and_handoff_retry(
    postgres: &PgPool,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
    job_input: &NewIngestJob,
) -> Result<WebhookRetryHandoffOutcome, sqlx::Error> {
    ensure_retry_handoff_input(completion, job_input)?;

    let mut transaction = postgres.begin().await?;
    let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        job_input.workspace_id,
        job_input.library_id,
    )
    .await?;
    if !parent_locked {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    }
    match lock_subscription_for_retry_completion(&mut transaction, completion).await? {
        None => {
            transaction.rollback().await?;
            return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
        }
        Some(false) => {
            return record_terminal_retry_completion(transaction, completion).await;
        }
        Some(true) => {}
    }

    handoff_retry_job(transaction, completion, handoff, job_input).await
}

fn ensure_retry_handoff_input(
    completion: &WebhookDeliveryCompletion<'_>,
    job_input: &NewIngestJob,
) -> Result<(), sqlx::Error> {
    validate_webhook_delivery_completion(completion)?;
    if completion.delivery_state != "failed" || completion.next_attempt_at.is_none() {
        return Err(sqlx::Error::Protocol(
            "webhook retry handoff requires a failed completion with a retry timestamp".into(),
        ));
    }
    if job_input.dedupe_key.is_none() {
        return Err(sqlx::Error::Protocol("webhook retry handoff requires a dedupe key".into()));
    }
    Ok(())
}

/// Verifies that the caller still owns the exact leased ingest job and attempt
/// and, in that order, schedules the replacement retry job, records the failed
/// delivery against it, and terminalizes the old attempt and job. The whole
/// phase commits or rolls back as one unit; the caller has already proven the
/// owning library and subscription are current.
async fn handoff_retry_job(
    mut transaction: sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
    job_input: &NewIngestJob,
) -> Result<WebhookRetryHandoffOutcome, sqlx::Error> {
    if !verify_retry_handoff_ownership(&mut transaction, completion, handoff).await? {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    }
    let Some(recorded) =
        apply_retry_handoff(&mut transaction, completion, handoff, job_input).await?
    else {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    };
    transaction.commit().await?;
    Ok(WebhookRetryHandoffOutcome::RetryScheduled(recorded))
}

/// Proves the caller still owns both the leased webhook-delivery job and its
/// leased ingest attempt before any retry row is produced.
async fn verify_retry_handoff_ownership(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
) -> Result<bool, sqlx::Error> {
    if !owns_leased_webhook_delivery_job(transaction, completion, handoff).await? {
        return Ok(false);
    }
    owns_leased_ingest_attempt(transaction, completion, handoff).await
}

/// Schedules the replacement retry job, records the failed delivery against it
/// and terminalizes the old attempt and job. `None` means the completion token
/// went stale mid-phase, so the caller rolls back.
async fn apply_retry_handoff(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
    job_input: &NewIngestJob,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let retry_job_id = get_or_create_compatible_retry_job(transaction, job_input).await?;
    let recorded = record_webhook_delivery_result_with_executor(
        &mut **transaction,
        completion,
        Some(retry_job_id),
    )
    .await?;
    let Some(recorded) = recorded else {
        return Ok(None);
    };
    if !finalize_handed_off_attempt(transaction, completion, handoff).await? {
        return Ok(None);
    }
    if !finalize_handed_off_job(transaction, completion, handoff).await? {
        return Ok(None);
    }
    Ok(Some(recorded))
}

async fn owns_leased_webhook_delivery_job(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
) -> Result<bool, sqlx::Error> {
    let owned = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_job
         where id = $1
           and job_kind = 'webhook_delivery'::ingest_job_kind
           and queue_state = 'leased'
           and queue_lease_token = $2
         for update",
    )
    .bind(completion.job_id)
    .bind(handoff.expected_queue_lease_token)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(owned.is_some())
}

async fn owns_leased_ingest_attempt(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
) -> Result<bool, sqlx::Error> {
    let owned = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_attempt
         where id = $1
           and job_id = $2
           and attempt_state = 'leased'
         for update",
    )
    .bind(handoff.ingest_attempt_id)
    .bind(completion.job_id)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(owned.is_some())
}

async fn finalize_handed_off_attempt(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
) -> Result<bool, sqlx::Error> {
    let finalized = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'succeeded',
             current_stage = 'webhook_delivery',
             heartbeat_at = now(),
             finished_at = now(),
             failure_class = null,
             failure_code = null,
             failure_message = null,
             progress_percent = 100,
             retryable = false
         where id = $1
           and job_id = $2
           and attempt_state = 'leased'",
    )
    .bind(handoff.ingest_attempt_id)
    .bind(completion.job_id)
    .execute(&mut **transaction)
    .await?;
    Ok(finalized.rows_affected() == 1)
}

async fn finalize_handed_off_job(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    handoff: &WebhookRetryHandoff<'_>,
) -> Result<bool, sqlx::Error> {
    let finalized = sqlx::query(
        "update ingest_job
         set queue_state = 'completed',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and job_kind = 'webhook_delivery'::ingest_job_kind
           and queue_state = 'leased'
           and queue_lease_token = $2",
    )
    .bind(completion.job_id)
    .bind(handoff.expected_queue_lease_token)
    .execute(&mut **transaction)
    .await?;
    Ok(finalized.rows_affected() == 1)
}

/// Records the terminal completion for a delivery whose subscription is no
/// longer active: the retry timestamp is dropped, ownership is re-checked under
/// the same lease, and the transaction commits or rolls back as one unit.
async fn record_terminal_retry_completion(
    mut transaction: sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<WebhookRetryHandoffOutcome, sqlx::Error> {
    let terminal_completion = WebhookDeliveryCompletion {
        attempt_id: completion.attempt_id,
        job_id: completion.job_id,
        lease_token: completion.lease_token,
        delivery_state: completion.delivery_state,
        attempt_number: completion.attempt_number,
        response_status: completion.response_status,
        error_code: completion.error_code,
        error_summary: completion.error_summary,
        next_attempt_at: None,
    };
    let recorded =
        record_webhook_delivery_result_with_executor(&mut *transaction, &terminal_completion, None)
            .await?;
    let Some(recorded) = recorded else {
        transaction.rollback().await?;
        return Ok(WebhookRetryHandoffOutcome::OwnershipLost);
    };
    transaction.commit().await?;
    Ok(WebhookRetryHandoffOutcome::CompletionRecorded(recorded))
}

async fn get_or_create_compatible_retry_job(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    input: &NewIngestJob,
) -> Result<Uuid, sqlx::Error> {
    let retry_job = sqlx::query_as::<_, (Uuid, Uuid, Uuid, String, String, Option<String>)>(
        "insert into ingest_job (
            id, workspace_id, library_id, mutation_id, connector_id,
            async_operation_id, knowledge_document_id, knowledge_revision_id,
            job_kind, queue_state, priority, dedupe_key,
            queued_at, available_at, completed_at
         ) values (
            $1, $2, $3, $4, $5, $6, $7, $8,
            $9::ingest_job_kind, $10::ingest_queue_state, $11, $12,
            coalesce($13, now()), coalesce($14, now()), $15
         )
         on conflict (library_id, dedupe_key) where dedupe_key is not null
         do update set dedupe_key = excluded.dedupe_key
         returning id, workspace_id, library_id,
                   job_kind::text, queue_state::text, dedupe_key",
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
    .fetch_one(&mut **transaction)
    .await?;
    if retry_job.1 != input.workspace_id
        || retry_job.2 != input.library_id
        || retry_job.3 != input.job_kind
        || retry_job.5 != input.dedupe_key
        || !matches!(retry_job.4.as_str(), "queued" | "leased" | "paused")
    {
        return Err(sqlx::Error::Protocol(
            "webhook retry dedupe key resolved to an incompatible queue job".into(),
        ));
    }
    Ok(retry_job.0)
}

/// Atomically records a retryable failure and transfers the attempt to a new
/// queue job. The retry row is rolled back when the completion token is stale,
/// so a worker that lost ownership cannot schedule follow-up work.
///
/// # Errors
/// Returns a protocol error for invalid retry input or any SQLx error raised by
/// the fenced completion/queue transaction.
#[cfg(feature = "test-support")]
pub async fn record_webhook_delivery_failure_and_enqueue_retry_detached(
    postgres: &PgPool,
    completion: &WebhookDeliveryCompletion<'_>,
    job_input: &NewIngestJob,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    validate_webhook_retry_input(completion, job_input)?;
    let mut transaction = postgres.begin().await?;
    match lock_subscription_for_retry_completion(&mut transaction, completion).await? {
        None => {
            transaction.rollback().await?;
            return Ok(None);
        }
        Some(false) => {
            return record_inactive_detached_retry(transaction, completion).await;
        }
        Some(true) => {}
    }
    let retry_job = match create_ingest_job_with_executor(&mut *transaction, job_input).await {
        Ok(job) => job,
        Err(error) if is_ingest_job_dedupe_conflict(&error) => {
            transaction.rollback().await?;
            return record_existing_detached_retry(postgres, completion, job_input, error).await;
        }
        Err(error) => return Err(error),
    };
    record_detached_retry_in_transaction(transaction, completion, Some(retry_job.id)).await
}

#[cfg(feature = "test-support")]
fn validate_webhook_retry_input(
    completion: &WebhookDeliveryCompletion<'_>,
    job_input: &NewIngestJob,
) -> Result<(), sqlx::Error> {
    validate_webhook_delivery_completion(completion)?;
    if completion.delivery_state != "failed" || completion.next_attempt_at.is_none() {
        return Err(sqlx::Error::Protocol(
            "webhook retry requires a failed completion with a next-attempt timestamp".into(),
        ));
    }
    if job_input.dedupe_key.is_none() {
        return Err(sqlx::Error::Protocol("webhook retry job requires a dedupe key".into()));
    }
    Ok(())
}

#[cfg(feature = "test-support")]
async fn record_inactive_detached_retry(
    transaction: sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let terminal_completion = terminal_webhook_completion(completion);
    record_detached_retry_in_transaction(transaction, &terminal_completion, None).await
}

#[cfg(feature = "test-support")]
fn terminal_webhook_completion<'a>(
    completion: &'a WebhookDeliveryCompletion<'a>,
) -> WebhookDeliveryCompletion<'a> {
    WebhookDeliveryCompletion {
        attempt_id: completion.attempt_id,
        job_id: completion.job_id,
        lease_token: completion.lease_token,
        delivery_state: completion.delivery_state,
        attempt_number: completion.attempt_number,
        response_status: completion.response_status,
        error_code: completion.error_code,
        error_summary: completion.error_summary,
        next_attempt_at: None,
    }
}

#[cfg(feature = "test-support")]
async fn record_existing_detached_retry(
    postgres: &PgPool,
    completion: &WebhookDeliveryCompletion<'_>,
    job_input: &NewIngestJob,
    conflict: sqlx::Error,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let dedupe_key = job_input
        .dedupe_key
        .as_deref()
        .ok_or_else(|| sqlx::Error::Protocol("webhook retry job requires a dedupe key".into()))?;
    let existing = get_ingest_job_by_dedupe_key(postgres, job_input.library_id, dedupe_key)
        .await?
        .ok_or(conflict)?;
    validate_compatible_retry_job(&existing, job_input, dedupe_key)?;
    let mut transaction = postgres.begin().await?;
    match lock_subscription_for_retry_completion(&mut transaction, completion).await? {
        None => {
            transaction.rollback().await?;
            Ok(None)
        }
        Some(false) => record_inactive_detached_retry(transaction, completion).await,
        Some(true) => {
            record_detached_retry_in_transaction(transaction, completion, Some(existing.id)).await
        }
    }
}

#[cfg(feature = "test-support")]
fn validate_compatible_retry_job(
    existing: &crate::infra::repositories::ingest_repository::IngestJobRow,
    input: &NewIngestJob,
    dedupe_key: &str,
) -> Result<(), sqlx::Error> {
    let is_compatible = existing.workspace_id == input.workspace_id
        && existing.library_id == input.library_id
        && existing.job_kind == input.job_kind
        && existing.dedupe_key.as_deref() == Some(dedupe_key)
        && matches!(existing.queue_state.as_str(), "queued" | "leased" | "paused");
    if !is_compatible {
        return Err(sqlx::Error::Protocol(
            "webhook retry dedupe key resolved to an incompatible queue job".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "test-support")]
async fn record_detached_retry_in_transaction(
    mut transaction: sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
    retry_job_id: Option<Uuid>,
) -> Result<Option<WebhookDeliveryAttemptRow>, sqlx::Error> {
    let recorded =
        record_webhook_delivery_result_with_executor(&mut *transaction, completion, retry_job_id)
            .await?;
    if recorded.is_none() {
        transaction.rollback().await?;
        return Ok(None);
    }
    transaction.commit().await?;
    Ok(recorded)
}

async fn lock_subscription_for_retry_completion(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    completion: &WebhookDeliveryCompletion<'_>,
) -> Result<Option<bool>, sqlx::Error> {
    let subscription_active = sqlx::query_scalar::<_, bool>(
        "select subscription.active
         from webhook_delivery_attempt delivery
         join webhook_subscription subscription
           on subscription.id = delivery.subscription_id
         where delivery.id = $1
           and delivery.job_id = $2
           and delivery.delivery_lease_token = $3
           and delivery.delivery_state = 'delivering'
         for key share of subscription",
    )
    .bind(completion.attempt_id)
    .bind(completion.job_id)
    .bind(completion.lease_token)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(subscription_active)
}
