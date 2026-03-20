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

#[derive(Clone, Default)]
pub struct AuditService;

impl AuditService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

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

    pub async fn list_redacted_events(
        &self,
        state: &AppState,
        actor_principal_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<AuditEventRedactedView>, ApiError> {
        let rows = audit_repository::list_audit_events(
            &state.persistence.postgres,
            actor_principal_id,
            workspace_id,
            library_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_redacted_event).collect())
    }

    pub async fn list_internal_events(
        &self,
        state: &AppState,
        actor_principal_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<AuditEventInternalView>, ApiError> {
        let rows = audit_repository::list_audit_events(
            &state.persistence.postgres,
            actor_principal_id,
            workspace_id,
            library_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_internal_event).collect())
    }

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

    pub async fn list_events(
        &self,
        state: &AppState,
        actor_principal_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<AuditEvent>, ApiError> {
        let rows = audit_repository::list_audit_events(
            &state.persistence.postgres,
            actor_principal_id,
            workspace_id,
            library_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_event).collect())
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
    AuditEventSubject {
        audit_event_id: row.audit_event_id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        document_id: row.document_id,
    }
}
