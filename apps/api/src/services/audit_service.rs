use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::audit::{
        AuditEvent, AuditEventInternalView, AuditEventRedactedView, AuditEventSubject,
    },
    infra::repositories::audit_repository,
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct AppendAuditEventCommand {
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub result_kind: String,
    pub redacted_message: Option<String>,
    pub internal_message: Option<String>,
    pub subjects: Vec<AppendAuditEventSubjectCommand>,
}

#[derive(Debug, Clone)]
pub struct AppendAuditEventSubjectCommand {
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct ListAuditEventSubjectFilter {
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
    pub subject_filter: ListAuditEventSubjectFilter,
    pub surface_kind: Option<String>,
    pub result_kind: Option<String>,
    pub search: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct AuditEventPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Clone, Default)]
pub struct AuditService;

impl AuditService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Persists a new audit event together with its attached subjects.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository write fails.
    pub async fn append_event(
        &self,
        state: &AppState,
        command: AppendAuditEventCommand,
    ) -> Result<AuditEventInternalView, ApiError> {
        let event = audit_repository::append_audit_event(
            &state.persistence.postgres,
            audit_repository::NewAuditEvent {
                actor_principal_id: command.actor_principal_id,
                surface_kind: command.surface_kind,
                action_kind: command.action_kind,
                request_id: command.request_id,
                trace_id: command.trace_id,
                result_kind: command.result_kind,
                redacted_message: command.redacted_message,
                internal_message: command.internal_message,
            },
            &command
                .subjects
                .into_iter()
                .map(|subject| audit_repository::NewAuditEventSubject {
                    subject_kind: subject.subject_kind,
                    subject_id: subject.subject_id,
                    workspace_id: subject.workspace_id,
                    library_id: subject.library_id,
                    document_id: subject.document_id,
                })
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_internal_event(event))
    }

    /// Lists redacted audit events visible to the caller and optional workspace scope.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository query fails.
    pub async fn list_redacted_events(
        &self,
        state: &AppState,
        query: &ListAuditEventsQuery,
    ) -> Result<AuditEventPage<AuditEventRedactedView>, ApiError> {
        let page =
            audit_repository::list_audit_events(&state.persistence.postgres, &map_list_query(query))
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(AuditEventPage {
            items: page.items.into_iter().map(map_redacted_event).collect(),
            total: page.total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    /// Lists internal audit events visible to the caller and optional workspace scope.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository query fails.
    pub async fn list_internal_events(
        &self,
        state: &AppState,
        query: &ListAuditEventsQuery,
    ) -> Result<AuditEventPage<AuditEventInternalView>, ApiError> {
        let page =
            audit_repository::list_audit_events(&state.persistence.postgres, &map_list_query(query))
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(AuditEventPage {
            items: page.items.into_iter().map(map_internal_event).collect(),
            total: page.total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    /// Lists all subjects attached to a previously recorded audit event.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository query fails.
    pub async fn list_event_subjects(
        &self,
        state: &AppState,
        audit_event_id: Uuid,
    ) -> Result<Vec<AuditEventSubject>, ApiError> {
        let rows = audit_repository::list_audit_event_subjects(
            &state.persistence.postgres,
            audit_event_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_event_subject).collect())
    }

    /// Lists audit events with the public event shape.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository query fails.
    pub async fn list_events(
        &self,
        state: &AppState,
        query: &ListAuditEventsQuery,
    ) -> Result<AuditEventPage<AuditEvent>, ApiError> {
        let page =
            audit_repository::list_audit_events(&state.persistence.postgres, &map_list_query(query))
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(AuditEventPage {
            items: page.items.into_iter().map(map_event).collect(),
            total: page.total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    #[must_use]
    pub fn query_session_subject(
        &self,
        session_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "query_session".to_string(),
            subject_id: session_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }
    }

    #[must_use]
    pub fn query_execution_subject(
        &self,
        execution_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "query_execution".to_string(),
            subject_id: execution_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }
    }

    #[must_use]
    pub fn knowledge_document_subject(
        &self,
        document_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "knowledge_document".to_string(),
            subject_id: document_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: Some(document_id),
        }
    }

    #[must_use]
    pub fn knowledge_bundle_subject(
        &self,
        bundle_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "knowledge_bundle".to_string(),
            subject_id: bundle_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }
    }

    #[must_use]
    pub fn async_operation_subject(
        &self,
        operation_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "async_operation".to_string(),
            subject_id: operation_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }
    }

    #[must_use]
    pub fn runtime_execution_subject(
        &self,
        runtime_execution_id: Uuid,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "runtime_execution".to_string(),
            subject_id: runtime_execution_id,
            workspace_id,
            library_id,
            document_id: None,
        }
    }
}

const fn map_subject_filter(
    filter: &ListAuditEventSubjectFilter,
) -> audit_repository::AuditEventSubjectFilter {
    audit_repository::AuditEventSubjectFilter {
        knowledge_document_id: filter.knowledge_document_id,
        knowledge_revision_id: filter.knowledge_revision_id,
        context_bundle_id: filter.context_bundle_id,
        query_session_id: filter.query_session_id,
        query_execution_id: filter.query_execution_id,
        runtime_execution_id: filter.runtime_execution_id,
        async_operation_id: filter.async_operation_id,
    }
}

fn map_list_query(query: &ListAuditEventsQuery) -> audit_repository::ListAuditEventsQuery {
    audit_repository::ListAuditEventsQuery {
        actor_principal_id: query.actor_principal_id,
        workspace_id: query.workspace_id,
        library_id: query.library_id,
        subject_filter: map_subject_filter(&query.subject_filter),
        surface_kind: query.surface_kind.clone(),
        result_kind: query.result_kind.clone(),
        search: query.search.clone(),
        limit: query.limit,
        offset: query.offset,
    }
}

fn map_event(row: audit_repository::AuditEventRow) -> AuditEvent {
    AuditEvent {
        id: row.id,
        actor_principal_id: row.actor_principal_id,
        surface_kind: row.surface_kind,
        action_kind: row.action_kind,
        result_kind: row.result_kind,
        request_id: row.request_id,
        trace_id: row.trace_id,
        created_at: row.created_at,
        redacted_message: row.redacted_message,
    }
}

fn map_redacted_event(row: audit_repository::AuditEventRow) -> AuditEventRedactedView {
    AuditEventRedactedView {
        id: row.id,
        actor_principal_id: row.actor_principal_id,
        surface_kind: row.surface_kind,
        action_kind: row.action_kind,
        result_kind: row.result_kind,
        request_id: row.request_id,
        trace_id: row.trace_id,
        created_at: row.created_at,
        redacted_message: row.redacted_message,
    }
}

fn map_internal_event(row: audit_repository::AuditEventRow) -> AuditEventInternalView {
    AuditEventInternalView {
        id: row.id,
        actor_principal_id: row.actor_principal_id,
        surface_kind: row.surface_kind,
        action_kind: row.action_kind,
        result_kind: row.result_kind,
        request_id: row.request_id,
        trace_id: row.trace_id,
        created_at: row.created_at,
        redacted_message: row.redacted_message,
        internal_message: row.internal_message,
    }
}

fn map_event_subject(row: audit_repository::AuditEventSubjectRow) -> AuditEventSubject {
    let query_session_id = (row.subject_kind == "query_session").then_some(row.subject_id);
    let query_execution_id = (row.subject_kind == "query_execution").then_some(row.subject_id);
    let runtime_execution_id = (row.subject_kind == "runtime_execution").then_some(row.subject_id);
    let context_bundle_id = (row.subject_kind == "knowledge_bundle").then_some(row.subject_id);
    let async_operation_id = (row.subject_kind == "async_operation").then_some(row.subject_id);
    AuditEventSubject {
        audit_event_id: row.audit_event_id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        document_id: row.document_id,
        query_session_id,
        query_execution_id,
        runtime_execution_id,
        context_bundle_id,
        async_operation_id,
    }
}
