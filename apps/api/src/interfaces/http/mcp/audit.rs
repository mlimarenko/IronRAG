use crate::{
    app::state::AppState,
    interfaces::http::auth::AuthContext,
    mcp_types::{McpMutationReceipt, McpSearchDocumentsResponse},
    services::iam::audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
};

/// One MCP audit event, minus the actor and request identity that
/// [`record_mcp_audit`] binds from the live [`AuthContext`]. Keeps the
/// descriptive fields together so the append call takes a cohesive value
/// instead of a long positional argument list.
pub(super) struct McpAuditEvent {
    pub(super) action_kind: &'static str,
    pub(super) result_kind: &'static str,
    pub(super) redacted_message: Option<String>,
    pub(super) internal_message: Option<String>,
    pub(super) subjects: Vec<AppendAuditEventSubjectCommand>,
}

pub(super) async fn record_mcp_audit(
    state: &AppState,
    auth: &AuthContext,
    request_id: &str,
    event: McpAuditEvent,
) {
    if let Err(error) = state
        .application_services
        .audit
        .append_event(
            state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "mcp".to_string(),
                action_kind: event.action_kind.to_string(),
                request_id: Some(request_id.to_string()),
                trace_id: None,
                result_kind: event.result_kind.to_string(),
                redacted_message: event.redacted_message,
                internal_message: event.internal_message,
                subjects: event.subjects,
            },
        )
        .await
    {
        tracing::warn!(stage = "audit", error = %error, "audit append failed");
    }
}

/// `receipt.operation_id` already *is* the shared async-operation id
/// (see [`McpMutationReceipt`]) — no DB round-trip needed to derive it,
/// unlike the pre-convergence version of this function.
pub(super) fn build_mcp_mutation_subjects(
    state: &AppState,
    receipts: &[McpMutationReceipt],
) -> Vec<AppendAuditEventSubjectCommand> {
    let mut subjects = Vec::new();
    for receipt in receipts {
        if let Some(document_id) = receipt.document_id {
            subjects.push(state.application_services.audit.knowledge_document_subject(
                document_id,
                receipt.workspace_id,
                receipt.library_id,
            ));
        }
        subjects.push(state.application_services.audit.async_operation_subject(
            receipt.operation_id,
            receipt.workspace_id,
            receipt.library_id,
        ));
    }
    sort_and_dedup_subjects(&mut subjects);
    subjects
}

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
        subjects.push(state.application_services.audit.knowledge_document_subject(
            hit.document_id,
            hit.workspace_id,
            hit.library_id,
        ));
    }
    sort_and_dedup_subjects(&mut subjects);
    subjects
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
