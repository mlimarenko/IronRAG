use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::audit::{
        AuditAssistantCallSummary, AuditAssistantModel, AuditEventInternalView,
        AuditEventRedactedView, AuditEventSubject,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_AUDIT_REVIEW, POLICY_USAGE_READ, authorize_mcp_audit_review,
            load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::iam::audit::{AuditEventPage, ListAuditEventSubjectFilter, ListAuditEventsQuery},
};

const DEFAULT_AUDIT_LIMIT: u32 = 50;
const MAX_AUDIT_LIMIT: u32 = 1000;

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
    pub runtime_execution_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub surface_kind: Option<String>,
    pub result_kind: Option<String>,
    pub search: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub internal: Option<bool>,
    pub include_assistant: Option<bool>,
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
    pub runtime_execution_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditAssistantModelResponse {
    pub provider_kind: String,
    pub model_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditAssistantCallResponse {
    pub query_execution_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub models: Vec<AuditAssistantModelResponse>,
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub provider_call_count: i64,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_call: Option<AuditAssistantCallResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventPageResponse {
    pub items: Vec<AuditEventResponse>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/audit/events", get(list_audit_events))
}

#[tracing::instrument(
    level = "info",
    name = "http.list_audit_events",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id, item_count)
)]
async fn list_audit_events(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AuditEventsQuery>,
) -> Result<Json<AuditEventPageResponse>, ApiError> {
    let span = tracing::Span::current();
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
        runtime_execution_id: query.runtime_execution_id,
        async_operation_id: query.async_operation_id,
    };
    let list_query = ListAuditEventsQuery {
        actor_principal_id: query.actor_principal_id,
        workspace_id: workspace_filter,
        library_id: library_filter,
        subject_filter,
        surface_kind: query.surface_kind.filter(|value| !value.trim().is_empty()),
        result_kind: query.result_kind.filter(|value| !value.trim().is_empty()),
        search: query.search.filter(|value| !value.trim().is_empty()),
        limit: i64::from(query.limit.unwrap_or(DEFAULT_AUDIT_LIMIT).clamp(1, MAX_AUDIT_LIMIT)),
        offset: i64::from(query.offset.unwrap_or_default()),
    };

    let mut response_items = Vec::new();
    let total = if internal {
        let events =
            state.canonical_services.audit.list_internal_events(&state, &list_query).await?;
        let total = events.total;
        push_internal_response_items(
            &state,
            &auth,
            workspace_filter,
            library_filter,
            &mut response_items,
            events,
        )
        .await?;
        total
    } else {
        let events =
            state.canonical_services.audit.list_redacted_events(&state, &list_query).await?;
        let total = events.total;
        push_redacted_response_items(
            &state,
            &auth,
            workspace_filter,
            library_filter,
            &mut response_items,
            events,
        )
        .await?;
        total
    };

    if query.include_assistant.unwrap_or(false) {
        attach_assistant_call_summaries(&state, &auth, &mut response_items).await?;
    }
    span.record("item_count", response_items.len());
    Ok(Json(AuditEventPageResponse {
        items: response_items,
        total,
        limit: list_query.limit,
        offset: list_query.offset,
    }))
}

async fn push_internal_response_items(
    state: &AppState,
    auth: &AuthContext,
    workspace_filter: Option<Uuid>,
    library_filter: Option<Uuid>,
    response_items: &mut Vec<AuditEventResponse>,
    page: AuditEventPage<AuditEventInternalView>,
) -> Result<(), ApiError> {
    for event in page.items {
        let subjects = visible_subjects(
            state,
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

    Ok(())
}

async fn push_redacted_response_items(
    state: &AppState,
    auth: &AuthContext,
    workspace_filter: Option<Uuid>,
    library_filter: Option<Uuid>,
    response_items: &mut Vec<AuditEventResponse>,
    page: AuditEventPage<AuditEventRedactedView>,
) -> Result<(), ApiError> {
    for event in page.items {
        let subjects = visible_subjects(
            state,
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

    Ok(())
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

async fn attach_assistant_call_summaries(
    state: &AppState,
    auth: &AuthContext,
    items: &mut [AuditEventResponse],
) -> Result<(), ApiError> {
    let query_execution_ids = items
        .iter()
        .filter(|event| event.action_kind == "query.execution.run")
        .filter_map(query_execution_id_from_event)
        .collect::<Vec<_>>();
    if query_execution_ids.is_empty() {
        return Ok(());
    }

    let summaries = state
        .canonical_services
        .audit
        .list_assistant_call_summaries(state, &query_execution_ids)
        .await?;

    for event in items.iter_mut().filter(|event| event.action_kind == "query.execution.run") {
        let Some(query_execution_id) = query_execution_id_from_event(event) else {
            continue;
        };
        let Some((workspace_id, library_id)) = audit_scope_from_event(event) else {
            continue;
        };
        if !auth.has_library_permission(workspace_id, library_id, POLICY_USAGE_READ) {
            continue;
        }
        if let Some(summary) = summaries.get(&query_execution_id) {
            event.assistant_call = Some(map_assistant_call(summary));
        }
    }

    Ok(())
}

fn query_execution_id_from_event(event: &AuditEventResponse) -> Option<Uuid> {
    event.subjects.iter().find_map(|subject| subject.query_execution_id)
}

fn audit_scope_from_event(event: &AuditEventResponse) -> Option<(Uuid, Uuid)> {
    event.subjects.iter().find_map(|subject| Some((subject.workspace_id?, subject.library_id?)))
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
        assistant_call: None,
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
        assistant_call: None,
    }
}

fn map_assistant_call(summary: &AuditAssistantCallSummary) -> AuditAssistantCallResponse {
    AuditAssistantCallResponse {
        query_execution_id: summary.query_execution_id,
        conversation_id: summary.conversation_id,
        runtime_execution_id: summary.runtime_execution_id,
        models: summary.models.iter().map(map_assistant_model).collect(),
        total_cost: summary.total_cost,
        currency_code: summary.currency_code.clone(),
        provider_call_count: summary.provider_call_count,
    }
}

fn map_assistant_model(model: &AuditAssistantModel) -> AuditAssistantModelResponse {
    AuditAssistantModelResponse {
        provider_kind: model.provider_kind.clone(),
        model_name: model.model_name.clone(),
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
        runtime_execution_id: subject.runtime_execution_id,
        context_bundle_id: subject.context_bundle_id,
        async_operation_id: subject.async_operation_id,
    }
}
