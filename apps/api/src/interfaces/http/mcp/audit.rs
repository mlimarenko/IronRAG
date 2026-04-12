use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    mcp_types::{
        McpAuditActionKind, McpAuditScope, McpMutationReceipt, McpSearchDocumentsResponse,
    },
    services::iam::audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
};

pub(super) async fn record_canonical_mcp_audit(
    state: &AppState,
    auth: &AuthContext,
    request_id: &str,
    action_kind: &str,
    result_kind: &str,
    redacted_message: Option<String>,
    internal_message: Option<String>,
    subjects: Vec<AppendAuditEventSubjectCommand>,
) {
    if let Err(error) = state
        .canonical_services
        .audit
        .append_event(
            state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "mcp".to_string(),
                action_kind: action_kind.to_string(),
                request_id: Some(request_id.to_string()),
                trace_id: None,
                result_kind: result_kind.to_string(),
                redacted_message,
                internal_message,
                subjects,
            },
        )
        .await
    {
        tracing::warn!(stage = "audit", error = %error, "audit append failed");
    }
}

#[allow(clippy::unused_async)]
pub(super) async fn record_success_audit(
    _auth: &AuthContext,
    _state: &AppState,
    _request_id: &str,
    _action_kind: McpAuditActionKind,
    _scope: McpAuditScope,
    _metadata_json: serde_json::Value,
) {
    // Canonical MCP audit now persists through `audit_event` only.
}

#[allow(clippy::unused_async)]
pub(super) async fn record_error_audit(
    _auth: &AuthContext,
    _state: &AppState,
    _request_id: &str,
    _action_kind: McpAuditActionKind,
    _scope: McpAuditScope,
    _error: &ApiError,
    _metadata_json: serde_json::Value,
) {
    // Canonical MCP audit now persists through `audit_event` only.
}

pub(super) async fn build_mcp_mutation_subjects(
    state: &AppState,
    receipts: &[McpMutationReceipt],
) -> Vec<AppendAuditEventSubjectCommand> {
    let mut subjects = Vec::new();
    for receipt in receipts {
        if let Some(document_id) = receipt.document_id {
            subjects.push(state.canonical_services.audit.knowledge_document_subject(
                document_id,
                receipt.workspace_id,
                receipt.library_id,
            ));
        }
        if let Ok(admission) =
            state.canonical_services.content.get_mutation_admission(state, receipt.receipt_id).await
            && let Some(async_operation_id) = admission.async_operation_id
        {
            subjects.push(state.canonical_services.audit.async_operation_subject(
                async_operation_id,
                receipt.workspace_id,
                receipt.library_id,
            ));
        }
    }
    sort_and_dedup_subjects(&mut subjects);
    subjects
}

#[allow(clippy::unused_async)]
pub(super) async fn build_mcp_web_ingest_subjects(
    _state: &AppState,
    receipts: &[crate::domains::ingest::WebIngestRunReceipt],
) -> Vec<AppendAuditEventSubjectCommand> {
    let mut subjects = Vec::new();
    for receipt in receipts {
        subjects.push(AppendAuditEventSubjectCommand {
            subject_kind: "content_web_ingest_run".to_string(),
            subject_id: receipt.run_id,
            workspace_id: None,
            library_id: Some(receipt.library_id),
            document_id: None,
        });
    }
    sort_and_dedup_subjects(&mut subjects);
    subjects
}

pub(super) fn build_mcp_search_subjects(
    state: &AppState,
    payload: &McpSearchDocumentsResponse,
) -> Vec<AppendAuditEventSubjectCommand> {
    let mut subjects = Vec::new();
    for hit in &payload.hits {
        subjects.push(state.canonical_services.audit.knowledge_document_subject(
            hit.document_id,
            hit.workspace_id,
            hit.library_id,
        ));
    }
    sort_and_dedup_subjects(&mut subjects);
    subjects
}

pub(super) fn search_scope_from_request(
    auth: &AuthContext,
    library_ids: Option<&[Uuid]>,
) -> McpAuditScope {
    McpAuditScope {
        workspace_id: auth.workspace_id,
        library_id: library_ids.and_then(single_scope_id),
        document_id: None,
    }
}

pub(super) fn search_scope_from_response(
    auth: &AuthContext,
    payload: &McpSearchDocumentsResponse,
) -> McpAuditScope {
    McpAuditScope {
        workspace_id: auth
            .workspace_id
            .or_else(|| payload.hits.first().map(|hit| hit.workspace_id)),
        library_id: single_scope_id(&payload.library_ids),
        document_id: None,
    }
}

pub(super) fn mutation_scope_from_receipts(
    receipts: &[McpMutationReceipt],
) -> Option<McpAuditScope> {
    receipts.first().map(|receipt| McpAuditScope {
        workspace_id: Some(receipt.workspace_id),
        library_id: Some(receipt.library_id),
        document_id: (receipts.len() == 1).then_some(receipt.document_id).flatten(),
    })
}

fn sort_and_dedup_subjects(subjects: &mut Vec<AppendAuditEventSubjectCommand>) {
    subjects.sort_by(|left, right| {
        left.subject_kind
            .cmp(&right.subject_kind)
            .then_with(|| left.subject_id.cmp(&right.subject_id))
    });
    subjects.dedup_by(|left, right| {
        left.subject_kind == right.subject_kind && left.subject_id == right.subject_id
    });
}

fn single_scope_id(values: &[Uuid]) -> Option<Uuid> {
    (values.len() == 1).then_some(values[0])
}
