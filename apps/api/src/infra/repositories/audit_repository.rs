use chrono::{DateTime, Utc};
use sqlx::FromRow;
use sqlx::PgPool;
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AuditEventRow {
    pub id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub result_kind: String,
    pub created_at: DateTime<Utc>,
    pub redacted_message: Option<String>,
    pub internal_message: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AuditEventSubjectRow {
    pub audit_event_id: Uuid,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewAuditEvent {
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub result_kind: String,
    pub redacted_message: Option<String>,
    pub internal_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewAuditEventSubject {
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditEventSubjectFilter {
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct ListAuditEventsQuery {
    pub actor_principal_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub subject_filter: AuditEventSubjectFilter,
    pub surface_kind: Option<String>,
    pub result_kind: Option<String>,
    pub search: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct AuditEventPage {
    pub items: Vec<AuditEventRow>,
    pub total: i64,
}

pub async fn append_audit_event(
    postgres: &PgPool,
    event: NewAuditEvent,
    subjects: &[NewAuditEventSubject],
) -> Result<AuditEventRow, sqlx::Error> {
    let audit_event = sqlx::query_as::<_, AuditEventRow>(
        "insert into audit_event (
            id,
            actor_principal_id,
            surface_kind,
            action_kind,
            request_id,
            trace_id,
            result_kind,
            created_at,
            redacted_message,
            internal_message
        )
        values ($1, $2, $3::surface_kind, $4, $5, $6, $7::audit_result_kind, now(), $8, $9)
        returning
            id,
            actor_principal_id,
            surface_kind::text as surface_kind,
            action_kind,
            request_id,
            trace_id,
            result_kind::text as result_kind,
            created_at,
            redacted_message,
            internal_message",
    )
    .bind(Uuid::now_v7())
    .bind(event.actor_principal_id)
    .bind(event.surface_kind)
    .bind(event.action_kind)
    .bind(event.request_id)
    .bind(event.trace_id)
    .bind(event.result_kind)
    .bind(event.redacted_message)
    .bind(event.internal_message)
    .fetch_one(postgres)
    .await?;

    if !subjects.is_empty() {
        let mut builder = QueryBuilder::<Postgres>::new(
            "insert into audit_event_subject (
                audit_event_id,
                subject_kind,
                subject_id,
                workspace_id,
                library_id,
                document_id
            ) ",
        );
        builder.push_values(subjects.iter(), |mut row, subject| {
            row.push_bind(audit_event.id)
                .push_bind(&subject.subject_kind)
                .push_bind(subject.subject_id)
                .push_bind(subject.workspace_id)
                .push_bind(subject.library_id)
                .push_bind(subject.document_id);
        });
        builder.build().execute(postgres).await?;
    }

    Ok(audit_event)
}

pub async fn list_audit_events(
    postgres: &PgPool,
    query: &ListAuditEventsQuery,
) -> Result<AuditEventPage, sqlx::Error> {
    let mut count_builder = QueryBuilder::<Postgres>::new(
        "select count(*)::bigint
         from audit_event ae
         where 1 = 1",
    );
    push_list_audit_event_filters(&mut count_builder, query);
    let total = count_builder.build_query_scalar::<i64>().fetch_one(postgres).await?;

    let mut builder = QueryBuilder::<Postgres>::new(
        "select
            ae.id,
            ae.actor_principal_id,
            ae.surface_kind::text as surface_kind,
            ae.action_kind,
            ae.request_id,
            ae.trace_id,
            ae.result_kind::text as result_kind,
            ae.created_at,
            ae.redacted_message,
            ae.internal_message
         from audit_event ae
         where 1 = 1",
    );
    push_list_audit_event_filters(&mut builder, query);
    builder.push(" order by ae.created_at desc, ae.id desc limit ");
    builder.push_bind(query.limit);
    builder.push(" offset ");
    builder.push_bind(query.offset);

    let items = builder.build_query_as::<AuditEventRow>().fetch_all(postgres).await?;
    Ok(AuditEventPage { items, total })
}

fn push_list_audit_event_filters(
    builder: &mut QueryBuilder<'_, Postgres>,
    query: &ListAuditEventsQuery,
) {
    if let Some(actor_principal_id) = query.actor_principal_id {
        builder.push(" and ae.actor_principal_id = ");
        builder.push_bind(actor_principal_id);
    }

    if let Some(workspace_id) = query.workspace_id {
        builder.push(
            " and exists (
                select 1
                from audit_event_subject aes
                where aes.audit_event_id = ae.id
                  and aes.workspace_id = ",
        );
        builder.push_bind(workspace_id);
        builder.push(")");
    }

    if let Some(library_id) = query.library_id {
        builder.push(
            " and exists (
                select 1
                from audit_event_subject aes
                where aes.audit_event_id = ae.id
                  and aes.library_id = ",
        );
        builder.push_bind(library_id);
        builder.push(")");
    }

    if let Some(knowledge_document_id) = query.subject_filter.knowledge_document_id {
        builder.push(
            " and exists (
                select 1
                from audit_event_subject aes
                where aes.audit_event_id = ae.id
                  and (
                        aes.document_id = ",
        );
        builder.push_bind(knowledge_document_id);
        builder.push(
            "        or (
                            aes.subject_kind = 'knowledge_document'
                        and aes.subject_id = ",
        );
        builder.push_bind(knowledge_document_id);
        builder.push(
            "           )
                  )
            )",
        );
    }

    if let Some(knowledge_revision_id) = query.subject_filter.knowledge_revision_id {
        push_exact_subject_filter(builder, "knowledge_revision", knowledge_revision_id);
    }

    if let Some(context_bundle_id) = query.subject_filter.context_bundle_id {
        push_exact_subject_filter(builder, "knowledge_bundle", context_bundle_id);
    }

    if let Some(query_session_id) = query.subject_filter.query_session_id {
        push_exact_subject_filter(builder, "query_session", query_session_id);
    }

    if let Some(query_execution_id) = query.subject_filter.query_execution_id {
        push_exact_subject_filter(builder, "query_execution", query_execution_id);
    }

    if let Some(runtime_execution_id) = query.subject_filter.runtime_execution_id {
        push_exact_subject_filter(builder, "runtime_execution", runtime_execution_id);
    }

    if let Some(async_operation_id) = query.subject_filter.async_operation_id {
        push_exact_subject_filter(builder, "async_operation", async_operation_id);
    }

    if let Some(surface_kind) = query.surface_kind.as_ref() {
        builder.push(" and ae.surface_kind::text = ");
        builder.push_bind(surface_kind.clone());
    }

    if let Some(result_kind) = query.result_kind.as_ref() {
        builder.push(" and ae.result_kind::text = ");
        builder.push_bind(result_kind.clone());
    }

    if let Some(search) = query.search.as_ref() {
        let pattern = format!("%{search}%");
        builder.push(
            " and (
                ae.action_kind ilike ",
        );
        builder.push_bind(pattern.clone());
        builder.push(" or coalesce(ae.redacted_message, '') ilike ");
        builder.push_bind(pattern.clone());
        builder.push(" or coalesce(ae.internal_message, '') ilike ");
        builder.push_bind(pattern.clone());
        builder.push(" or coalesce(ae.actor_principal_id::text, '') ilike ");
        builder.push_bind(pattern.clone());
        builder.push(
            " or exists (
                    select 1
                    from audit_event_subject aes
                    where aes.audit_event_id = ae.id
                      and (
                            aes.subject_kind ilike ",
        );
        builder.push_bind(pattern.clone());
        builder.push("       or aes.subject_id::text ilike ");
        builder.push_bind(pattern.clone());
        builder.push("       or coalesce(aes.workspace_id::text, '') ilike ");
        builder.push_bind(pattern.clone());
        builder.push("       or coalesce(aes.library_id::text, '') ilike ");
        builder.push_bind(pattern.clone());
        builder.push("       or coalesce(aes.document_id::text, '') ilike ");
        builder.push_bind(pattern);
        builder.push(
            "      )
                )
            )",
        );
    }
}

fn push_exact_subject_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    subject_kind: &'static str,
    subject_id: Uuid,
) {
    builder.push(
        " and exists (
            select 1
            from audit_event_subject aes
            where aes.audit_event_id = ae.id
              and aes.subject_kind = ",
    );
    builder.push_bind(subject_kind);
    builder.push(" and aes.subject_id = ");
    builder.push_bind(subject_id);
    builder.push(")");
}

pub async fn list_audit_event_subjects(
    postgres: &PgPool,
    audit_event_id: Uuid,
) -> Result<Vec<AuditEventSubjectRow>, sqlx::Error> {
    sqlx::query_as::<_, AuditEventSubjectRow>(
        "select
            audit_event_id,
            subject_kind,
            subject_id,
            workspace_id,
            library_id,
            document_id
         from audit_event_subject
         where audit_event_id = $1
         order by subject_kind asc, subject_id asc",
    )
    .bind(audit_event_id)
    .fetch_all(postgres)
    .await
}

pub async fn append_bootstrap_claim_event(
    postgres: &PgPool,
    actor_principal_id: Uuid,
    request_id: &str,
    redacted_message: &str,
    internal_message: &str,
) -> Result<(), sqlx::Error> {
    append_audit_event(
        postgres,
        NewAuditEvent {
            actor_principal_id: Some(actor_principal_id),
            surface_kind: "bootstrap".to_string(),
            action_kind: "iam.bootstrap.claim".to_string(),
            request_id: Some(request_id.to_string()),
            trace_id: None,
            result_kind: "succeeded".to_string(),
            redacted_message: Some(redacted_message.to_string()),
            internal_message: Some(internal_message.to_string()),
        },
        &[NewAuditEventSubject {
            subject_kind: "principal".to_string(),
            subject_id: actor_principal_id,
            workspace_id: None,
            library_id: None,
            document_id: None,
        }],
    )
    .await?;
    Ok(())
}
