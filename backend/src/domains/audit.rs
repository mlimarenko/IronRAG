use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub result_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub redacted_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEventInternalView {
    pub id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub result_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub redacted_message: Option<String>,
    pub internal_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEventRedactedView {
    pub id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub result_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub redacted_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEventSubject {
    pub audit_event_id: Uuid,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}
