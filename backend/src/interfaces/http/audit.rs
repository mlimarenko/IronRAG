use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::audit::{AuditEventInternalView, AuditEventRedactedView, AuditEventSubject},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_AUDIT_REVIEW, authorize_mcp_audit_review, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::audit_service::ListAuditEventSubjectFilter,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventsQuery {
    pub actor_principal_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub internal: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventSubjectResponse {
    pub audit_event_id: Uuid,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventResponse {
    pub id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub result_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub redacted_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal_message: Option<String>,
    pub subjects: Vec<AuditEventSubjectResponse>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/audit/events", get(list_audit_events))
}

async fn list_audit_events(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AuditEventsQuery>,
) -> Result<Json<Vec<AuditEventResponse>>, ApiError> {
    let internal = query.internal.unwrap_or(false);
    if internal && !auth.is_system_admin {
        return Err(ApiError::forbidden(
            "internal audit view requires system administrator access",
        ));
    }

    let mut workspace_filter = if auth.is_system_admin {
        query.workspace_id
    } else {
        authorize_mcp_audit_review(&auth, query.workspace_id)?
    };

    let library_filter = if let Some(library_id) = query.library_id {
        let library =
            load_library_and_authorize(&auth, &state, library_id, POLICY_MCP_AUDIT_REVIEW).await?;
        if let Some(workspace_id) = workspace_filter
            && workspace_id != library.workspace_id
        {
            return Err(ApiError::BadRequest(
                "libraryId does not belong to workspaceId".to_string(),
            ));
        }
        workspace_filter = Some(library.workspace_id);
        Some(library.id)
    } else {
        None
    };
    let subject_filter = ListAuditEventSubjectFilter {
        knowledge_document_id: query.knowledge_document_id,
        knowledge_revision_id: query.knowledge_revision_id,
        context_bundle_id: query.context_bundle_id,
        query_session_id: query.query_session_id,
        query_execution_id: query.query_execution_id,
        async_operation_id: query.async_operation_id,
    };

    let mut response_items = Vec::new();
    if internal {
        let events = state
            .canonical_services
            .audit
            .list_internal_events(
                &state,
                query.actor_principal_id,
                workspace_filter,
                library_filter,
                &subject_filter,
            )
            .await?;
        for event in events {
            let subjects = visible_subjects(
                &state,
                event.id,
                auth.is_system_admin,
                workspace_filter,
                library_filter,
            )
            .await?;
            if auth.is_system_admin || !subjects.is_empty() {
                response_items.push(map_internal_event(event, subjects));
            }
        }
    } else {
        let events = state
            .canonical_services
            .audit
            .list_redacted_events(
                &state,
                query.actor_principal_id,
                workspace_filter,
                library_filter,
                &subject_filter,
            )
            .await?;
        for event in events {
            let subjects = visible_subjects(
                &state,
                event.id,
                auth.is_system_admin,
                workspace_filter,
                library_filter,
            )
            .await?;
            if auth.is_system_admin || !subjects.is_empty() {
                response_items.push(map_redacted_event(event, subjects));
            }
        }
    }

    Ok(Json(response_items))
}

async fn visible_subjects(
    state: &AppState,
    audit_event_id: Uuid,
    is_system_admin: bool,
    workspace_filter: Option<Uuid>,
    library_filter: Option<Uuid>,
) -> Result<Vec<AuditEventSubjectResponse>, ApiError> {
    let subjects =
        state.canonical_services.audit.list_event_subjects(state, audit_event_id).await?;

    Ok(subjects
        .into_iter()
        .filter(|subject| {
            if is_system_admin {
                return true;
            }
            if let Some(library_id) = library_filter {
                return subject.library_id == Some(library_id);
            }
            if let Some(workspace_id) = workspace_filter {
                return subject.workspace_id == Some(workspace_id);
            }
            false
        })
        .map(map_subject)
        .collect())
}

fn map_internal_event(
    event: AuditEventInternalView,
    subjects: Vec<AuditEventSubjectResponse>,
) -> AuditEventResponse {
    AuditEventResponse {
        id: event.id,
        actor_principal_id: event.actor_principal_id,
        surface_kind: event.surface_kind,
        action_kind: event.action_kind,
        result_kind: event.result_kind,
        request_id: event.request_id,
        trace_id: event.trace_id,
        created_at: event.created_at,
        redacted_message: event.redacted_message,
        internal_message: event.internal_message,
        subjects,
    }
}

fn map_redacted_event(
    event: AuditEventRedactedView,
    subjects: Vec<AuditEventSubjectResponse>,
) -> AuditEventResponse {
    AuditEventResponse {
        id: event.id,
        actor_principal_id: event.actor_principal_id,
        surface_kind: event.surface_kind,
        action_kind: event.action_kind,
        result_kind: event.result_kind,
        request_id: event.request_id,
        trace_id: event.trace_id,
        created_at: event.created_at,
        redacted_message: event.redacted_message,
        internal_message: None,
        subjects,
    }
}

fn map_subject(subject: AuditEventSubject) -> AuditEventSubjectResponse {
    let knowledge_document_id = match subject.subject_kind.as_str() {
        "knowledge_document" => Some(subject.subject_id),
        "knowledge_revision" => subject.document_id,
        _ => None,
    };
    let knowledge_revision_id =
        (subject.subject_kind == "knowledge_revision").then_some(subject.subject_id);

    AuditEventSubjectResponse {
        audit_event_id: subject.audit_event_id,
        subject_kind: subject.subject_kind,
        subject_id: subject.subject_id,
        workspace_id: subject.workspace_id,
        library_id: subject.library_id,
        document_id: knowledge_document_id.or(subject.document_id),
        knowledge_document_id,
        knowledge_revision_id,
        query_session_id: subject.query_session_id,
        query_execution_id: subject.query_execution_id,
        context_bundle_id: subject.context_bundle_id,
        async_operation_id: subject.async_operation_id,
    }
}
