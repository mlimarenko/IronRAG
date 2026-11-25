use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::billing::CostCoverageStatus;

pub const QUERY_EXECUTION_AUDIT_ACTION: &str = "query.execution.run";
pub const QUERY_EXECUTION_AUDIT_REDACTED_MESSAGE: &str = "assistant call completed";

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AuditEventSubject {
    pub audit_event_id: Uuid,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AuditAssistantModel {
    pub provider_kind: String,
    pub model_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AuditAssistantCallSummary {
    pub query_execution_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub models: Vec<AuditAssistantModel>,
    /// Measured charged cost. `None` when no cost observation exists or when
    /// every dispatchable call is unpriced; `cost_coverage` distinguishes the
    /// two, and a genuine priced zero stays `Some(0)`.
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    /// Completed calls when `cost_coverage` is present, otherwise recorded provider-call rows.
    pub provider_call_count: i64,
    /// Calls whose liability no charge proves, including reservations that never
    /// completed.
    pub unpriced_provider_call_count: i64,
    /// Pricing completeness of the execution's calls. `None` when the rollup
    /// has no call observations.
    pub cost_coverage: Option<CostCoverageStatus>,
}
