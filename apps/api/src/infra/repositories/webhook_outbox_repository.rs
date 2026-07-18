use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Executor, FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::domains::webhook::WebhookEvent;

use super::webhook_repository::{
    MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE, WebhookSubscriptionTargetRow,
};

pub const MAX_WEBHOOK_LIFECYCLE_OUTBOX_BATCH_LIMIT: i64 = 100;
pub const MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT: i64 = 500;
pub const MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES: usize = 64;
const MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_QUERY_LIMIT: i64 =
    MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT + 1;

const AUDIT_WEBHOOK_LIFECYCLE_OUTBOX_SELECT_SQL: &str = "select
        id,
        event_type,
        occurred_at,
        workspace_id,
        library_id,
        dispatch_state,
        dispatch_attempts,
        last_error_code,
        resolution_reason_code,
        available_at,
        lease_expires_at,
        dispatched_at,
        resolved_at,
        created_at,
        updated_at
     from webhook_lifecycle_outbox";

const REQUEUE_DEAD_LETTER_WEBHOOK_LIFECYCLE_OUTBOX_SQL: &str = "update webhook_lifecycle_outbox
     set dispatch_state = 'pending',
         dispatch_attempts = 0,
         available_at = now(),
         lease_owner = null,
         lease_token = null,
         leased_at = null,
         lease_expires_at = null,
         last_error_code = null,
         last_error = null,
         dispatched_at = null,
         resolution_reason_code = null,
         resolved_at = null,
         updated_at = now()
     where id = $1
       and dispatch_state = 'dead_letter'
     returning
        id,
        event_type,
        occurred_at,
        workspace_id,
        library_id,
        dispatch_state,
        dispatch_attempts,
        last_error_code,
        resolution_reason_code,
        available_at,
        lease_expires_at,
        dispatched_at,
        resolved_at,
        created_at,
        updated_at";

/// Valid persisted states accepted by the bounded operator audit filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookLifecycleOutboxDispatchState {
    Pending,
    Dispatching,
    Dispatched,
    DeadLetter,
    Resolved,
}

impl WebhookLifecycleOutboxDispatchState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatching => "dispatching",
            Self::Dispatched => "dispatched",
            Self::DeadLetter => "dead_letter",
            Self::Resolved => "resolved",
        }
    }
}

impl std::fmt::Display for WebhookLifecycleOutboxDispatchState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for WebhookLifecycleOutboxDispatchState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "pending" => Ok(Self::Pending),
            "dispatching" => Ok(Self::Dispatching),
            "dispatched" => Ok(Self::Dispatched),
            "dead_letter" => Ok(Self::DeadLetter),
            "resolved" => Ok(Self::Resolved),
            _ => Err(format!(
                "unknown webhook lifecycle outbox state `{value}`; expected pending, dispatching, dispatched, dead-letter, or resolved"
            )),
        }
    }
}

/// Stable keyset cursor for a descending immutable `(created_at, id)` audit
/// page. State-filter membership remains a live, best-effort view: rows that
/// change into or out of a filter while paging require a fresh audit pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WebhookLifecycleOutboxAuditCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

/// Deliberately redacted operator projection. It cannot carry event payloads,
/// target URLs, credentials, custom headers, lease owners/tokens, or raw
/// failure text.
#[derive(Debug, Clone, PartialEq, Eq, FromRow, Serialize)]
pub struct WebhookLifecycleOutboxAuditRow {
    pub id: Uuid,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub dispatch_state: String,
    pub dispatch_attempts: i32,
    pub last_error_code: Option<String>,
    pub resolution_reason_code: Option<String>,
    pub available_at: DateTime<Utc>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WebhookLifecycleOutboxRow {
    pub id: Uuid,
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub payload_json: serde_json::Value,
    pub dispatch_state: String,
    pub dispatch_attempts: i32,
    pub available_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_token: Option<Uuid>,
    pub leased_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub last_error: Option<String>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub resolution_reason_code: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct WebhookLifecycleOutboxLease {
    pub lease_token: Uuid,
    pub events: Vec<WebhookLifecycleOutboxRow>,
}

/// Persists a lifecycle event through the ordinary pool executor.
///
/// Replaying identical data for a deterministic `event_id` returns the
/// existing row without resetting dispatch state. Reusing an identity for
/// different event data fails closed.
pub async fn enqueue_webhook_lifecycle_event(
    postgres: &PgPool,
    event: &WebhookEvent,
) -> Result<WebhookLifecycleOutboxRow, sqlx::Error> {
    enqueue_webhook_lifecycle_event_with_executor(postgres, event).await
}

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
    outbox_id: Uuid,
    lease_token: Uuid,
    retry_at: DateTime<Utc>,
    error_code: &str,
    error_message: &str,
    max_attempts: i32,
    retryable: bool,
) -> Result<Option<String>, sqlx::Error> {
    if error_code.is_empty()
        || error_code.len() > 64
        || !error_code.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))
        })
    {
        return Err(sqlx::Error::Protocol(
            "webhook lifecycle outbox failure code is not canonical".to_string(),
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

/// Lists a bounded redacted projection for operator audit.
///
/// The SQL projection intentionally does not load payloads, endpoint data,
/// secrets, custom headers, lease identities, or raw error text into the
/// maintenance process.
fn build_webhook_lifecycle_outbox_audit_query(
    dispatch_state: Option<WebhookLifecycleOutboxDispatchState>,
    library_id: Option<Uuid>,
    cursor: Option<WebhookLifecycleOutboxAuditCursor>,
    limit: i64,
) -> QueryBuilder<Postgres> {
    let mut query = QueryBuilder::<Postgres>::new(AUDIT_WEBHOOK_LIFECYCLE_OUTBOX_SELECT_SQL);
    query.push(" where true");
    if let Some(dispatch_state) = dispatch_state {
        query.push(" and dispatch_state = ").push_bind(dispatch_state.as_str());
    }
    if let Some(library_id) = library_id {
        query.push(" and library_id = ").push_bind(library_id);
    }
    if let Some(cursor) = cursor {
        query
            .push(" and (created_at, id) < (")
            .push_bind(cursor.created_at)
            .push(", ")
            .push_bind(cursor.id)
            .push(")");
    }
    query
        .push(" order by created_at desc, id desc limit ")
        .push_bind(limit.clamp(1, MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_QUERY_LIMIT));
    query
}

pub async fn audit_webhook_lifecycle_outbox<'e, E>(
    executor: E,
    dispatch_state: Option<WebhookLifecycleOutboxDispatchState>,
    library_id: Option<Uuid>,
    cursor: Option<WebhookLifecycleOutboxAuditCursor>,
    limit: i64,
) -> Result<Vec<WebhookLifecycleOutboxAuditRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let mut query =
        build_webhook_lifecycle_outbox_audit_query(dispatch_state, library_id, cursor, limit);
    query.build_query_as::<WebhookLifecycleOutboxAuditRow>().fetch_all(executor).await
}

/// Requeues exactly one dead-letter lifecycle event without delivering it.
///
/// The state predicate is the compare-and-set guard: missing rows and rows in
/// any state other than `dead_letter` return `None` without modification.
/// Successful repair clears retry/lease/failure state and makes the event due
/// immediately. The ordinary outbox worker performs any later delivery.
pub async fn requeue_dead_letter_webhook_lifecycle_outbox<'e, E>(
    executor: E,
    outbox_id: Uuid,
) -> Result<Option<WebhookLifecycleOutboxAuditRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, WebhookLifecycleOutboxAuditRow>(
        REQUEUE_DEAD_LETTER_WEBHOOK_LIFECYCLE_OUTBOX_SQL,
    )
    .bind(outbox_id)
    .fetch_optional(executor)
    .await
}

#[must_use]
pub fn is_canonical_webhook_lifecycle_outbox_resolution_reason_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))
        })
}

/// Permanently resolves one exact dead-letter row without pretending that it
/// was delivered.
///
/// The single statement atomically writes the typed terminal state and a
/// redacted global audit event. `audit_event_subject` intentionally has no
/// catalog foreign key, so the operator decision survives the catalog delete
/// that this terminal state unblocks. Missing rows, stale state, dispatched
/// rows, and concurrent requeues return `None` without writing an audit event.
pub async fn resolve_dead_letter_webhook_lifecycle_outbox<'e, E>(
    executor: E,
    outbox_id: Uuid,
    reason_code: &str,
) -> Result<Option<WebhookLifecycleOutboxAuditRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if !is_canonical_webhook_lifecycle_outbox_resolution_reason_code(reason_code) {
        return Err(sqlx::Error::Protocol(format!(
            "webhook lifecycle outbox resolution reason code must match ^[a-z][a-z0-9_]{{0,{}}}$",
            MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES - 1,
        )));
    }

    sqlx::query_as::<_, WebhookLifecycleOutboxAuditRow>(
        "with resolved as (
            update webhook_lifecycle_outbox
            set dispatch_state = 'resolved',
                lease_owner = null,
                lease_token = null,
                leased_at = null,
                lease_expires_at = null,
                resolution_reason_code = $2,
                resolved_at = now(),
                updated_at = now()
            where id = $1
              and dispatch_state = 'dead_letter'
            returning
                id, event_type, occurred_at, workspace_id, library_id,
                dispatch_state, dispatch_attempts, last_error_code,
                resolution_reason_code, available_at, lease_expires_at,
                dispatched_at, resolved_at, created_at, updated_at
         ), audit as (
            insert into audit_event (
                id, actor_principal_id, surface_kind, action_kind,
                request_id, trace_id, result_kind, created_at,
                redacted_message, internal_message
            )
            select
                uuidv7(), null, 'internal'::surface_kind,
                'webhook.lifecycle_outbox.dead_letter_resolved',
                null, null, 'succeeded'::audit_result_kind, resolved_at,
                'Lifecycle webhook dead-letter explicitly resolved: reason_code='
                    || resolution_reason_code,
                null
            from resolved
            returning id
         ), audit_subject as (
            insert into audit_event_subject (
                audit_event_id, subject_kind, subject_id,
                workspace_id, library_id, document_id
            )
            select
                audit.id, 'webhook_lifecycle_outbox', resolved.id,
                resolved.workspace_id, resolved.library_id, null
            from audit cross join resolved
            returning audit_event_id
         )
         select
            resolved.id, resolved.event_type, resolved.occurred_at,
            resolved.workspace_id, resolved.library_id,
            resolved.dispatch_state, resolved.dispatch_attempts,
            resolved.last_error_code, resolved.resolution_reason_code,
            resolved.available_at, resolved.lease_expires_at,
            resolved.dispatched_at, resolved.resolved_at,
            resolved.created_at, resolved.updated_at
         from resolved
         where exists (select 1 from audit_subject)",
    )
    .bind(outbox_id)
    .bind(reason_code)
    .fetch_optional(executor)
    .await
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test fixtures use descriptive panic paths for invariant violations"
)]
mod tests {
    use anyhow::{Context as _, Result};
    use sqlx::{PgPool, Row as _, postgres::PgPoolOptions};

    use super::*;
    use crate::app::config::Settings;

    #[test]
    fn operator_audit_query_never_selects_secret_bearing_columns() {
        let normalized = AUDIT_WEBHOOK_LIFECYCLE_OUTBOX_SELECT_SQL.to_ascii_lowercase();
        for forbidden in [
            "payload_json",
            "event_id",
            "target_url",
            "secret",
            "custom_headers",
            "lease_owner",
            "lease_token",
        ] {
            assert!(
                !normalized.contains(forbidden),
                "audit query loaded forbidden column {forbidden}"
            );
        }
        assert!(
            !normalized.lines().any(|line| line.trim().trim_end_matches(',') == "last_error"),
            "audit query loaded raw failure text",
        );
        assert!(normalized.contains("last_error_code"));
    }

    #[test]
    fn operator_audit_builds_sargable_keyset_predicates() {
        let cursor =
            WebhookLifecycleOutboxAuditCursor { created_at: Utc::now(), id: Uuid::now_v7() };
        let query = build_webhook_lifecycle_outbox_audit_query(
            Some(WebhookLifecycleOutboxDispatchState::DeadLetter),
            Some(Uuid::now_v7()),
            Some(cursor),
            100,
        );
        let sql = query.sql().as_str().to_ascii_lowercase();
        assert!(sql.contains("dispatch_state = $1"));
        assert!(sql.contains("library_id = $2"));
        assert!(sql.contains("(created_at, id) < ($3, $4)"));
        assert!(sql.contains("order by created_at desc, id desc limit $5"));
        assert!(!sql.contains("is null or"));
    }

    #[test]
    fn resolution_reason_codes_are_bounded_and_machine_readable() {
        assert!(is_canonical_webhook_lifecycle_outbox_resolution_reason_code("receiver_retired"));
        assert!(is_canonical_webhook_lifecycle_outbox_resolution_reason_code("a"));
        assert!(!is_canonical_webhook_lifecycle_outbox_resolution_reason_code(""));
        assert!(!is_canonical_webhook_lifecycle_outbox_resolution_reason_code("Receiver retired"));
        assert!(!is_canonical_webhook_lifecycle_outbox_resolution_reason_code(
            &"a".repeat(MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES + 1)
        ));
    }

    async fn connect_postgres() -> Result<PgPool> {
        let settings = Settings::from_env().context("load settings for outbox operator test")?;
        let postgres = PgPoolOptions::new()
            .max_connections(2)
            .connect(&settings.database_url)
            .await
            .context("connect outbox operator test postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("apply migrations for outbox operator test")?;
        Ok(postgres)
    }

    #[tokio::test]
    #[ignore = "requires local postgres service"]
    async fn audit_filter_and_exact_dead_letter_repair_are_fail_closed() -> Result<()> {
        let postgres = connect_postgres().await?;
        let mut transaction = postgres.begin().await?;
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let dead_letter_id = Uuid::now_v7();
        let pending_id = Uuid::now_v7();
        let suffix = Uuid::now_v7().simple().to_string();
        let payload_marker = format!("private-payload-{suffix}");
        let error_marker = format!("private-error-{suffix}");
        let dead_letter_event_marker = format!("revision.ready:dead-letter:{dead_letter_id}");

        sqlx::query(
            "insert into catalog_workspace (id, slug, display_name)
             values ($1, $2, 'Webhook outbox operator test')",
        )
        .bind(workspace_id)
        .bind(format!("outbox-ops-{suffix}"))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into catalog_library (id, workspace_id, slug, display_name)
             values ($1, $2, $3, 'Webhook outbox operator test library')",
        )
        .bind(library_id)
        .bind(workspace_id)
        .bind(format!("outbox-ops-library-{suffix}"))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into webhook_lifecycle_outbox (
                id, event_id, event_type, occurred_at, workspace_id, library_id,
                payload_json, dispatch_state, dispatch_attempts, last_error_code, last_error
             ) values
                ($1, $3, 'revision.ready', now(), $5, $6, $7, 'dead_letter', 8,
                 'fanout_failed', 'redacted failure'),
                ($2, $4, 'revision.ready', now(), $5, $6, $7, 'pending', 2, null, null)",
        )
        .bind(dead_letter_id)
        .bind(pending_id)
        .bind(&dead_letter_event_marker)
        .bind(format!("revision.ready:pending:{pending_id}"))
        .bind(workspace_id)
        .bind(library_id)
        .bind(serde_json::json!({ "private": &payload_marker }))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "update webhook_lifecycle_outbox
             set last_error = $2
             where id = $1",
        )
        .bind(dead_letter_id)
        .bind(&error_marker)
        .execute(&mut *transaction)
        .await?;

        let rows = audit_webhook_lifecycle_outbox(
            &mut *transaction,
            Some(WebhookLifecycleOutboxDispatchState::DeadLetter),
            Some(library_id),
            None,
            100,
        )
        .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, dead_letter_id);
        assert_eq!(rows[0].last_error_code.as_deref(), Some("fanout_failed"));
        let audit_json = serde_json::to_string(&rows)?;
        let audit_debug = format!("{rows:?}");
        for marker in [&payload_marker, &error_marker, &dead_letter_event_marker] {
            assert!(!audit_json.contains(marker), "redacted audit leaked marker {marker}");
            assert!(!audit_debug.contains(marker), "redacted audit debug leaked marker {marker}");
        }

        let untouched =
            requeue_dead_letter_webhook_lifecycle_outbox(&mut *transaction, pending_id).await?;
        assert!(untouched.is_none(), "pending rows must not be modified by dead-letter repair");

        let repaired =
            requeue_dead_letter_webhook_lifecycle_outbox(&mut *transaction, dead_letter_id)
                .await?
                .context("dead-letter row was not requeued")?;
        assert_eq!(repaired.dispatch_state, "pending");
        assert_eq!(repaired.dispatch_attempts, 0);
        assert!(repaired.lease_expires_at.is_none());

        let persisted = sqlx::query(
            "select
                dispatch_state,
                dispatch_attempts,
                available_at <= now() as due,
                lease_owner is null as lease_owner_cleared,
                lease_token is null as lease_token_cleared,
                leased_at is null as leased_at_cleared,
                lease_expires_at is null as lease_expiry_cleared,
                last_error_code is null as error_code_cleared,
                last_error is null as error_cleared,
                dispatched_at is null as dispatched_at_cleared
             from webhook_lifecycle_outbox
             where id = $1",
        )
        .bind(dead_letter_id)
        .fetch_one(&mut *transaction)
        .await?;
        assert_eq!(persisted.get::<String, _>("dispatch_state"), "pending");
        assert_eq!(persisted.get::<i32, _>("dispatch_attempts"), 0);
        for column in [
            "due",
            "lease_owner_cleared",
            "lease_token_cleared",
            "leased_at_cleared",
            "lease_expiry_cleared",
            "error_code_cleared",
            "error_cleared",
            "dispatched_at_cleared",
        ] {
            assert!(persisted.get::<bool, _>(column), "repair invariant failed: {column}");
        }

        let second_repair =
            requeue_dead_letter_webhook_lifecycle_outbox(&mut *transaction, dead_letter_id).await?;
        assert!(second_repair.is_none(), "repair must be a one-way compare-and-set");

        transaction.rollback().await?;
        postgres.close().await;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires local postgres service"]
    async fn concurrent_dead_letter_repair_has_exactly_one_winner() -> Result<()> {
        let postgres = connect_postgres().await?;
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let outbox_id = Uuid::now_v7();
        let suffix = Uuid::now_v7().simple().to_string();

        let test_result = async {
            sqlx::query(
                "insert into catalog_workspace (id, slug, display_name)
                 values ($1, $2, 'Concurrent outbox operator test')",
            )
            .bind(workspace_id)
            .bind(format!("outbox-cas-{suffix}"))
            .execute(&postgres)
            .await?;
            sqlx::query(
                "insert into catalog_library (id, workspace_id, slug, display_name)
                 values ($1, $2, $3, 'Concurrent outbox operator test library')",
            )
            .bind(library_id)
            .bind(workspace_id)
            .bind(format!("outbox-cas-library-{suffix}"))
            .execute(&postgres)
            .await?;
            sqlx::query(
                "insert into webhook_lifecycle_outbox (
                    id, event_id, event_type, occurred_at, workspace_id, library_id,
                    payload_json, dispatch_state, dispatch_attempts, last_error_code, last_error
                 ) values ($1, $2, 'revision.ready', now(), $3, $4, '{}',
                           'dead_letter', 8, 'fanout_failed', 'redacted failure')",
            )
            .bind(outbox_id)
            .bind(format!("revision.ready:concurrent-cas:{outbox_id}"))
            .bind(workspace_id)
            .bind(library_id)
            .execute(&postgres)
            .await?;

            let mut left_connection = postgres.acquire().await?;
            let mut right_connection = postgres.acquire().await?;
            let (left, right) = tokio::join!(
                requeue_dead_letter_webhook_lifecycle_outbox(&mut *left_connection, outbox_id,),
                requeue_dead_letter_webhook_lifecycle_outbox(&mut *right_connection, outbox_id,),
            );
            let winners = [left?, right?].into_iter().filter(Option::is_some).count();
            anyhow::ensure!(winners == 1, "expected exactly one CAS winner, got {winners}");
            Ok::<(), anyhow::Error>(())
        }
        .await;

        // Cleanup runs even when the assertion path returns an error.
        let _ = sqlx::query("delete from webhook_lifecycle_outbox where id = $1")
            .bind(outbox_id)
            .execute(&postgres)
            .await;
        let _ = sqlx::query("delete from catalog_library where id = $1")
            .bind(library_id)
            .execute(&postgres)
            .await;
        let _ = sqlx::query("delete from catalog_workspace where id = $1")
            .bind(workspace_id)
            .execute(&postgres)
            .await;
        postgres.close().await;
        test_result
    }

    #[tokio::test]
    #[ignore = "requires local postgres service"]
    async fn dead_letter_resolution_is_atomic_audited_and_never_masks_delivery() -> Result<()> {
        let postgres = connect_postgres().await?;
        let mut transaction = postgres.begin().await?;
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let dead_letter_id = Uuid::now_v7();
        let dispatched_id = Uuid::now_v7();
        let suffix = Uuid::now_v7().simple().to_string();

        sqlx::query(
            "insert into catalog_workspace (id, slug, display_name)
             values ($1, $2, 'Webhook outbox resolution test')",
        )
        .bind(workspace_id)
        .bind(format!("outbox-resolution-{suffix}"))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into catalog_library (id, workspace_id, slug, display_name)
             values ($1, $2, $3, 'Webhook outbox resolution test library')",
        )
        .bind(library_id)
        .bind(workspace_id)
        .bind(format!("outbox-resolution-library-{suffix}"))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into webhook_lifecycle_outbox (
                id, event_id, event_type, occurred_at, workspace_id, library_id,
                payload_json, dispatch_state, dispatch_attempts,
                last_error_code, last_error, dispatched_at
             ) values
                ($1, $3, 'revision.ready', now(), $5, $6, '{}',
                 'dead_letter', 12, 'fanout_failed', 'redacted failure', null),
                ($2, $4, 'revision.ready', now(), $5, $6, '{}',
                 'dispatched', 1, null, null, now())",
        )
        .bind(dead_letter_id)
        .bind(dispatched_id)
        .bind(format!("revision.ready:resolved:{dead_letter_id}"))
        .bind(format!("revision.ready:dispatched:{dispatched_id}"))
        .bind(workspace_id)
        .bind(library_id)
        .execute(&mut *transaction)
        .await?;

        assert!(
            resolve_dead_letter_webhook_lifecycle_outbox(
                &mut *transaction,
                dead_letter_id,
                "free form secret-like reason",
            )
            .await
            .is_err(),
            "free-form resolution text must fail before persistence",
        );
        let resolved = resolve_dead_letter_webhook_lifecycle_outbox(
            &mut *transaction,
            dead_letter_id,
            "receiver_retired",
        )
        .await?
        .context("dead-letter row was not resolved")?;
        assert_eq!(resolved.dispatch_state, "resolved");
        assert_eq!(resolved.resolution_reason_code.as_deref(), Some("receiver_retired"));
        assert!(resolved.resolved_at.is_some());
        assert!(resolved.dispatched_at.is_none(), "resolution must never claim delivery");

        assert!(
            resolve_dead_letter_webhook_lifecycle_outbox(
                &mut *transaction,
                dead_letter_id,
                "receiver_retired",
            )
            .await?
            .is_none(),
            "resolution must be one exact compare-and-set",
        );
        assert!(
            resolve_dead_letter_webhook_lifecycle_outbox(
                &mut *transaction,
                dispatched_id,
                "receiver_retired",
            )
            .await?
            .is_none(),
            "a dispatched row must never be relabeled as resolved",
        );

        let audit = sqlx::query_as::<_, (String, String, String, Uuid, Uuid)>(
            "select
                event.action_kind, event.result_kind::text,
                event.redacted_message, subject.workspace_id, subject.library_id
             from audit_event event
             join audit_event_subject subject on subject.audit_event_id = event.id
             where subject.subject_kind = 'webhook_lifecycle_outbox'
               and subject.subject_id = $1",
        )
        .bind(dead_letter_id)
        .fetch_one(&mut *transaction)
        .await?;
        assert_eq!(audit.0, "webhook.lifecycle_outbox.dead_letter_resolved");
        assert_eq!(audit.1, "succeeded");
        assert!(audit.2.contains("reason_code=receiver_retired"));
        assert_eq!((audit.3, audit.4), (workspace_id, library_id));

        sqlx::query("delete from webhook_lifecycle_outbox where id = $1")
            .bind(dead_letter_id)
            .execute(&mut *transaction)
            .await?;
        let durable_audit = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from audit_event_subject
             where subject_kind = 'webhook_lifecycle_outbox'
               and subject_id = $1",
        )
        .bind(dead_letter_id)
        .fetch_one(&mut *transaction)
        .await?;
        assert_eq!(durable_audit, 1, "resolution audit must survive outbox/catalog deletion");

        transaction.rollback().await?;
        postgres.close().await;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires local postgres service"]
    async fn lifecycle_outbox_lease_renewal_uses_db_clock_and_token_fencing() -> Result<()> {
        let postgres = connect_postgres().await?;
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let outbox_id = Uuid::now_v7();
        let suffix = Uuid::now_v7().simple().to_string();

        let test_result = async {
            sqlx::query(
                "insert into catalog_workspace (id, slug, display_name)
                 values ($1, $2, 'Webhook outbox heartbeat test')",
            )
            .bind(workspace_id)
            .bind(format!("outbox-heartbeat-{suffix}"))
            .execute(&postgres)
            .await?;
            sqlx::query(
                "insert into catalog_library (id, workspace_id, slug, display_name)
                 values ($1, $2, $3, 'Webhook outbox heartbeat test library')",
            )
            .bind(library_id)
            .bind(workspace_id)
            .bind(format!("outbox-heartbeat-library-{suffix}"))
            .execute(&postgres)
            .await?;
            sqlx::query(
                "insert into webhook_lifecycle_outbox (
                    id, event_id, event_type, occurred_at,
                    workspace_id, library_id, payload_json
                 ) values ($1, $2, 'revision.ready', now(), $3, $4, '{}')",
            )
            .bind(outbox_id)
            .bind(format!("revision.ready:heartbeat:{outbox_id}"))
            .bind(workspace_id)
            .bind(library_id)
            .execute(&postgres)
            .await?;

            let first = lease_webhook_lifecycle_outbox_batch(
                &postgres,
                "heartbeat-owner-one",
                chrono::Duration::seconds(2),
                1,
            )
            .await?;
            anyhow::ensure!(first.events.len() == 1, "first owner did not lease fixture row");
            assert!(
                renew_webhook_lifecycle_outbox_lease(
                    &postgres,
                    outbox_id,
                    first.lease_token,
                    chrono::Duration::seconds(120),
                )
                .await?
            );
            let db_clock_extended = sqlx::query_scalar::<_, bool>(
                "select lease_expires_at >= now() + interval '110 seconds'
                 from webhook_lifecycle_outbox where id = $1",
            )
            .bind(outbox_id)
            .fetch_one(&postgres)
            .await?;
            assert!(db_clock_extended, "renewal must be based on PostgreSQL now()");

            sqlx::query(
                "update webhook_lifecycle_outbox
                 set lease_expires_at = now() - interval '1 second'
                 where id = $1",
            )
            .bind(outbox_id)
            .execute(&postgres)
            .await?;
            let replacement = lease_webhook_lifecycle_outbox_batch(
                &postgres,
                "heartbeat-owner-two",
                chrono::Duration::seconds(120),
                1,
            )
            .await?;
            anyhow::ensure!(replacement.events.len() == 1, "replacement owner did not reclaim");
            assert_ne!(replacement.lease_token, first.lease_token);
            assert!(
                !renew_webhook_lifecycle_outbox_lease(
                    &postgres,
                    outbox_id,
                    first.lease_token,
                    chrono::Duration::seconds(120),
                )
                .await?,
                "stale owner must not renew the replacement token",
            );
            assert!(
                renew_webhook_lifecycle_outbox_lease(
                    &postgres,
                    outbox_id,
                    replacement.lease_token,
                    chrono::Duration::seconds(120),
                )
                .await?
            );
            assert!(
                mark_webhook_lifecycle_outbox_dispatched(
                    &postgres,
                    outbox_id,
                    replacement.lease_token,
                )
                .await?
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let _ = sqlx::query("delete from catalog_workspace where id = $1")
            .bind(workspace_id)
            .execute(&postgres)
            .await;
        postgres.close().await;
        test_result
    }
}
