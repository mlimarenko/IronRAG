use super::*;

const MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_QUERY_LIMIT: i64 =
    MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT + 1;

pub(super) const AUDIT_WEBHOOK_LIFECYCLE_OUTBOX_SELECT_SQL: &str = "select
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

/// Lists a bounded redacted projection for operator audit.
///
/// The SQL projection intentionally does not load payloads, endpoint data,
/// secrets, custom headers, lease identities, or raw error text into the
/// maintenance process.
pub(super) fn build_webhook_lifecycle_outbox_audit_query(
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
pub fn is_supported_webhook_lifecycle_outbox_resolution_reason_code(value: &str) -> bool {
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
    if !is_supported_webhook_lifecycle_outbox_resolution_reason_code(reason_code) {
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
