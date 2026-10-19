use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;
use zeroize::Zeroize as _;

use crate::{
    infra::repositories::{
        catalog_repository,
        ingest_repository::{NewIngestJob, create_ingest_job_with_executor},
    },
    shared::secret_encryption::{EncryptedSecret, SecretPurpose},
};

#[cfg(feature = "test-support")]
use crate::infra::repositories::ingest_repository::get_ingest_job_by_dedupe_key;

mod deliveries;
mod delivery_completion;
mod rows;

pub use deliveries::*;
pub use delivery_completion::*;
pub use rows::*;

/// Hard fanout bound for one workspace. Existing rows remain readable during
/// upgrade, while every new activation is serialized and enforced here.
pub const MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE: i64 = 100;
/// Hard storage bound for active and inactive subscriptions combined. Inactive
/// rows retain delivery/audit history and therefore are never age-purged
/// implicitly; callers must delete them explicitly before creating more.
pub const MAX_TOTAL_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE: i64 = 1_000;
/// Repository-side ceiling for public list surfaces. HTTP callers may choose a
/// smaller page, but no accidental internal caller can issue an unbounded scan.
pub const MAX_WEBHOOK_API_PAGE_SIZE: u32 = 200;
pub const WEBHOOK_DELIVERY_LEASE_TIMEOUT_SECONDS: i64 = 300;
const ACTIVE_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR: &str = "active webhook subscription quota exceeded";
const TOTAL_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR: &str = "total webhook subscription quota exceeded";
const DRAINING_WEBHOOK_SUBSCRIPTION_REACTIVATION_ERROR: &str =
    "draining webhook subscription cannot be reactivated";

#[must_use]
pub fn is_active_webhook_subscription_quota_error(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Protocol(message) if message == ACTIVE_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR)
        || matches!(
            error,
            sqlx::Error::Database(database_error)
                if database_error.constraint()
                    == Some("webhook_subscription_active_workspace_quota")
        )
}

#[must_use]
pub fn is_total_webhook_subscription_quota_error(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Protocol(message) if message == TOTAL_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR)
        || matches!(
            error,
            sqlx::Error::Database(database_error)
                if database_error.constraint()
                    == Some("webhook_subscription_total_workspace_quota")
        )
}

#[must_use]
pub fn is_draining_webhook_subscription_reactivation_error(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Protocol(message)
            if message == DRAINING_WEBHOOK_SUBSCRIPTION_REACTIVATION_ERROR
    )
}

fn validate_encrypted_webhook_secret_context(
    subscription_id: Uuid,
    secret: Option<&EncryptedSecret>,
) -> Result<(), sqlx::Error> {
    if secret.is_some_and(|secret| {
        !secret.is_bound_to(SecretPurpose::WebhookSigningSecret, subscription_id)
    }) {
        return Err(sqlx::Error::Protocol(
            "encrypted webhook credential context does not match target row".to_string(),
        ));
    }
    Ok(())
}

fn validate_encrypted_webhook_headers_context(
    subscription_id: Uuid,
    headers: Option<&EncryptedSecret>,
) -> Result<(), sqlx::Error> {
    if headers.is_some_and(|headers| {
        !headers.is_bound_to(SecretPurpose::WebhookCustomHeaders, subscription_id)
    }) {
        return Err(sqlx::Error::Protocol(
            "encrypted webhook custom-header context does not match target row".to_string(),
        ));
    }
    Ok(())
}

// ============================================================================
// webhook_subscription CRUD
// ============================================================================

/// Creates a new outbound webhook subscription.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the subscription row.
pub async fn create_webhook_subscription(
    postgres: &PgPool,
    input: &NewWebhookSubscription,
) -> Result<WebhookSubscriptionViewRow, sqlx::Error> {
    validate_encrypted_webhook_secret_context(input.id, Some(&input.secret))?;
    validate_encrypted_webhook_headers_context(input.id, Some(&input.custom_headers_json))?;
    let mut transaction = postgres.begin().await?;
    let row = sqlx::query_as::<_, WebhookSubscriptionViewRow>(
        "insert into webhook_subscription (
            id, workspace_id, library_id, display_name, target_url, secret,
            event_types, custom_headers_json, created_by_principal_id
        ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        returning
            id, workspace_id, library_id, display_name, target_url,
            event_types, active, created_at, updated_at",
    )
    .bind(input.id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(&input.display_name)
    .bind(&input.target_url)
    .bind(input.secret.as_str())
    .bind(&input.event_types)
    .bind(serde_json::Value::String(input.custom_headers_json.as_str().to_owned()))
    .bind(input.created_by_principal_id)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(row)
}

/// Loads a secret-free subscription view by id, restricted to workspace ids
/// already authorized by the caller. `None` means instance-wide access for a
/// system administrator; an empty slice intentionally matches no rows.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the subscription row.
pub async fn get_webhook_subscription_view_by_id_in_workspace_scope(
    postgres: &PgPool,
    id: Uuid,
    authorized_workspace_ids: Option<&[Uuid]>,
) -> Result<Option<WebhookSubscriptionViewRow>, sqlx::Error> {
    sqlx::query_as::<_, WebhookSubscriptionViewRow>(
        "select id, workspace_id, library_id, display_name, target_url,
                event_types, active, created_at, updated_at
         from webhook_subscription
         where id = $1
           and ($2::uuid[] is null or workspace_id = any($2))",
    )
    .bind(id)
    .bind(authorized_workspace_ids)
    .fetch_optional(postgres)
    .await
}

/// Lists secret-free active subscription targets matching one lifecycle event.
/// Workspace-wide subscriptions and subscriptions scoped to `library_id` are
/// both included.
///
/// # Errors
/// Returns any `SQLx` error raised while querying subscription rows.
pub async fn list_active_webhook_subscription_targets_for_event(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    event_type: &str,
) -> Result<Vec<WebhookSubscriptionTargetRow>, sqlx::Error> {
    let targets = sqlx::query_as::<_, WebhookSubscriptionTargetRow>(
        "select id, target_url
         from webhook_subscription
         where workspace_id = $1
           and active = true
           and $3 = any(event_types)
           and (library_id is null or library_id = $2)
         order by id
         limit $4",
    )
    .bind(workspace_id)
    .bind(library_id)
    .bind(event_type)
    .bind(MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE + 1)
    .fetch_all(postgres)
    .await?;
    if i64::try_from(targets.len()).unwrap_or(i64::MAX)
        > MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE
    {
        return Err(sqlx::Error::Protocol(
            "webhook event fanout exceeds the active subscription quota".to_string(),
        ));
    }
    Ok(targets)
}

/// Lists one bounded keyset page of secret-free subscriptions for a workspace
/// (all states), preserving the existing oldest-first ordering.
///
/// # Errors
/// Returns any `SQLx` error raised while querying subscription rows.
pub async fn list_webhook_subscriptions_by_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<WebhookSubscriptionViewPage, sqlx::Error> {
    let (cursor_created_at, cursor_id) = cursor.unzip();
    let bounded_limit = limit.clamp(1, MAX_WEBHOOK_API_PAGE_SIZE);
    // Fetch `limit + 1` rows so a next-page cursor can be reported without a
    // second round-trip; the extra row is trimmed below.
    let fetch_limit = i64::from(bounded_limit) + 1;
    let mut rows = sqlx::query_as::<_, WebhookSubscriptionViewRow>(
        "select id, workspace_id, library_id, display_name, target_url,
                event_types, active, created_at, updated_at
         from webhook_subscription
         where workspace_id = $1
           and (
                $2::timestamptz is null
                or (created_at, id) > ($2, $3)
         )
         order by created_at asc, id asc
         limit $4",
    )
    .bind(workspace_id)
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
    Ok(WebhookSubscriptionViewPage { rows, next_cursor })
}

/// Patches a webhook subscription; only supplied fields are updated.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the subscription row.
pub async fn update_webhook_subscription(
    postgres: &PgPool,
    id: Uuid,
    input: &UpdateWebhookSubscription,
) -> Result<Option<WebhookSubscriptionViewRow>, sqlx::Error> {
    update_webhook_subscription_in_scope(postgres, id, None, input).await
}

/// Applies a subscription patch only inside the already-authorized workspace.
/// This is the only update entry point used by the public global-id API.
pub async fn update_webhook_subscription_in_workspace(
    postgres: &PgPool,
    id: Uuid,
    workspace_id: Uuid,
    input: &UpdateWebhookSubscription,
) -> Result<Option<WebhookSubscriptionViewRow>, sqlx::Error> {
    update_webhook_subscription_in_scope(postgres, id, Some(workspace_id), input).await
}

async fn update_webhook_subscription_in_scope(
    postgres: &PgPool,
    id: Uuid,
    workspace_id: Option<Uuid>,
    input: &UpdateWebhookSubscription,
) -> Result<Option<WebhookSubscriptionViewRow>, sqlx::Error> {
    validate_encrypted_webhook_secret_context(id, input.secret.as_ref())?;
    validate_encrypted_webhook_headers_context(id, input.custom_headers_json.as_ref())?;
    let mut transaction = postgres.begin().await?;
    if input.active == Some(true) {
        let delete_requested_at = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "select delete_requested_at
             from webhook_subscription
             where id = $1
               and ($2::uuid is null or workspace_id = $2)
             for update",
        )
        .bind(id)
        .bind(workspace_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(delete_requested_at) = delete_requested_at else {
            transaction.rollback().await?;
            return Ok(None);
        };
        if delete_requested_at.is_some() {
            transaction.rollback().await?;
            return Err(sqlx::Error::Protocol(
                DRAINING_WEBHOOK_SUBSCRIPTION_REACTIVATION_ERROR.to_string(),
            ));
        }
    }
    let row = sqlx::query_as::<_, WebhookSubscriptionViewRow>(
        "update webhook_subscription
         set display_name        = coalesce($2, display_name),
             target_url          = coalesce($3, target_url),
             secret              = coalesce($4, secret),
             event_types         = coalesce($5, event_types),
             custom_headers_json = coalesce($6, custom_headers_json),
             active              = coalesce($7, active),
             updated_at          = now()
         where id = $1
           and ($8::uuid is null or workspace_id = $8)
           and not (coalesce($7, false) and delete_requested_at is not null)
         returning
             id, workspace_id, library_id, display_name, target_url,
             event_types, active, created_at, updated_at",
    )
    .bind(id)
    .bind(input.display_name.as_deref())
    .bind(input.target_url.as_deref())
    .bind(input.secret.as_ref().map(EncryptedSecret::as_str))
    .bind(input.event_types.as_deref())
    .bind(
        input
            .custom_headers_json
            .as_ref()
            .map(|headers| serde_json::Value::String(headers.as_str().to_owned())),
    )
    .bind(input.active)
    .bind(workspace_id)
    .fetch_optional(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(row)
}

/// Atomically fences new claims, cancels linked queue work, then hard-deletes
/// a subscription only when no current delivery lease can still perform HTTP.
///
/// Locking the subscription first prevents a concurrent delivery-attempt
/// insert from slipping between cancellation and the cascading delete. Leased
/// workers observe the persisted `canceled` job state, while the delivery
/// ownership CAS prevents any already in-flight response from scheduling a
/// retry after its attempt row is deleted. A current delivery returns
/// [`DeleteWebhookSubscriptionOutcome::Draining`] and leaves the now-inactive
/// subscription in place; the operator retries deletion after that lease
/// terminalizes. Lease age alone is never treated as owner acknowledgement.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the subscription row.
pub async fn delete_webhook_subscription(
    postgres: &PgPool,
    id: Uuid,
) -> Result<DeleteWebhookSubscriptionOutcome, sqlx::Error> {
    delete_webhook_subscription_in_scope(postgres, id, None).await
}

/// Deletes a subscription only inside the already-authorized workspace. This
/// prevents global UUIDs from becoming a cross-tenant existence oracle.
pub async fn delete_webhook_subscription_in_workspace(
    postgres: &PgPool,
    id: Uuid,
    workspace_id: Uuid,
) -> Result<DeleteWebhookSubscriptionOutcome, sqlx::Error> {
    delete_webhook_subscription_in_scope(postgres, id, Some(workspace_id)).await
}

async fn delete_webhook_subscription_in_scope(
    postgres: &PgPool,
    id: Uuid,
    workspace_id: Option<Uuid>,
) -> Result<DeleteWebhookSubscriptionOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let locked = sqlx::query_scalar::<_, Uuid>(
        "select id
         from webhook_subscription
         where id = $1
           and ($2::uuid is null or workspace_id = $2)
         for update",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if locked.is_none() {
        transaction.commit().await?;
        return Ok(DeleteWebhookSubscriptionOutcome::NotFound);
    }

    // `active=false` is the durable delete fence. A claim serialized after
    // this transaction may still terminalize queue state, but cannot send.
    sqlx::query(
        "update webhook_subscription
         set active = false,
             delete_requested_at = coalesce(delete_requested_at, now()),
             updated_at = now()
         where id = $1
           and ($2::uuid is null or workspace_id = $2)",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&mut *transaction)
    .await?;

    // Global lock order for deletion and delivery deferral is subscription ->
    // queue job -> ingest attempt. Preserve it before touching attempts so a
    // concurrent duplicate-worker deferral cannot form a job/attempt cycle.
    let linked_job_ids = sqlx::query_scalar::<_, Uuid>(
        "select job.id
         from ingest_job job
         join webhook_delivery_attempt delivery on delivery.job_id = job.id
         where delivery.subscription_id = $1
           and job.job_kind = 'webhook_delivery'::ingest_job_kind
         order by job.id
         for update of job",
    )
    .bind(id)
    .fetch_all(&mut *transaction)
    .await?;

    // Lock current delivery rows after the subscription and queue jobs. A
    // lease age is deliberately not used as proof that HTTP cannot still
    // happen: only owner terminalization/acknowledgement permits hard delete.
    let in_flight_deliveries = sqlx::query_scalar::<_, Uuid>(
        "select id
         from webhook_delivery_attempt
         where subscription_id = $1
           and delivery_state = 'delivering'
         order by id
         for update",
    )
    .bind(id)
    .fetch_all(&mut *transaction)
    .await?;
    let preserve_in_flight = !in_flight_deliveries.is_empty();

    sqlx::query(
        "update ingest_attempt attempt
         set attempt_state = 'canceled',
             failure_class = 'webhook_delivery',
             failure_code = 'subscription_deleted',
             failure_message = 'Webhook delivery was canceled because its subscription was deleted',
             finished_at = now()
         where attempt_state in ('leased', 'running')
           and attempt.job_id = any($2)
           and (
               not $3::bool
               or not exists (
                   select 1
                   from webhook_delivery_attempt delivery
                   where delivery.subscription_id = $1
                     and delivery.job_id = attempt.job_id
                     and delivery.delivery_state = 'delivering'
               )
           )",
    )
    .bind(id)
    .bind(&linked_job_ids)
    .bind(preserve_in_flight)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "update ingest_job job
         set queue_state = 'canceled',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where job.job_kind = 'webhook_delivery'::ingest_job_kind
           and job.queue_state in ('queued', 'leased', 'paused', 'failed')
           and job.id = any($2)
           and (
               not $3::bool
               or not exists (
                   select 1
                   from webhook_delivery_attempt delivery
                   where delivery.subscription_id = $1
                     and delivery.job_id = job.id
                     and delivery.delivery_state = 'delivering'
               )
           )",
    )
    .bind(id)
    .bind(&linked_job_ids)
    .bind(preserve_in_flight)
    .execute(&mut *transaction)
    .await?;
    if !in_flight_deliveries.is_empty() {
        let count = u64::try_from(in_flight_deliveries.len()).unwrap_or(u64::MAX);
        transaction.commit().await?;
        return Ok(DeleteWebhookSubscriptionOutcome::Draining { in_flight_deliveries: count });
    }

    let result = sqlx::query(
        "delete from webhook_subscription
         where id = $1
           and ($2::uuid is null or workspace_id = $2)",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    if result.rows_affected() == 1 {
        Ok(DeleteWebhookSubscriptionOutcome::Deleted)
    } else {
        Ok(DeleteWebhookSubscriptionOutcome::NotFound)
    }
}

#[cfg(test)]
mod debug_redaction_tests;
