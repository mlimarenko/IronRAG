use uuid::Uuid;

use crate::{
    app::state::AppState,
    services::iam::audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
};

/// One REST-surface audit event, grouped so the append helper takes a single
/// cohesive value instead of a long positional list. The surface is always
/// `rest`; the trace id is read from the current span when the row is written.
pub(crate) struct RestAuditEvent {
    pub(crate) request_id: Option<String>,
    pub(crate) action_kind: String,
    pub(crate) result_kind: String,
    pub(crate) redacted_message: Option<String>,
    pub(crate) internal_message: Option<String>,
    pub(crate) subjects: Vec<AppendAuditEventSubjectCommand>,
}

/// Appends one REST audit event, logging (and swallowing) append failures so a
/// failed audit write never masks the handler's own outcome. Every REST surface
/// records through this single path.
pub(crate) async fn record_rest_audit_event(
    state: &AppState,
    actor_principal_id: Uuid,
    event: RestAuditEvent,
) {
    let trace_id = crate::observability::current_trace_id();
    if let Err(error) = state
        .application_services
        .audit
        .append_event(
            state,
            AppendAuditEventCommand {
                actor_principal_id: Some(actor_principal_id),
                surface_kind: "rest".to_string(),
                action_kind: event.action_kind,
                request_id: event.request_id,
                trace_id,
                result_kind: event.result_kind,
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
