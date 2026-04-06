use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::IamMe;
use crate::diagnostics::OperatorWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminSection {
    Access,
    Mcp,
    Operations,
    Ai,
    Pricing,
    Settings,
}

impl AdminSection {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Access => "Access",
            Self::Mcp => "MCP",
            Self::Operations => "Operations",
            Self::Ai => "AI",
            Self::Pricing => "Pricing",
            Self::Settings => "Settings",
        }
    }

    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::Access => "/admin/access",
            Self::Mcp => "/admin/mcp",
            Self::Operations => "/admin/operations",
            Self::Ai => "/admin/ai",
            Self::Pricing => "/admin/pricing",
            Self::Settings => "/admin/settings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityGate {
    pub section: AdminSection,
    pub allowed: bool,
    pub reason: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminCapabilityState {
    pub admin_enabled: bool,
    pub can_manage_tokens: bool,
    pub can_read_audit: bool,
    pub can_read_operations: bool,
    pub can_manage_ai: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminViewerSummary {
    pub principal_id: Uuid,
    pub display_name: String,
    pub access_label: String,
    pub is_admin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSectionSummary {
    pub section: AdminSection,
    pub title: String,
    pub summary: String,
    pub item_count: Option<i32>,
    pub gate: CapabilityGate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminToken {
    pub principal_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub issued_by_principal_id: Option<Uuid>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminProvider {
    pub id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub credential_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminModel {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub allowed_binding_purposes: Vec<String>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminCredential {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key_summary: String,
    pub credential_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminModelPreset {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub model_catalog_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminBindingValidation {
    pub id: Uuid,
    pub binding_id: Uuid,
    pub validation_state: String,
    pub checked_at: DateTime<Utc>,
    pub failure_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLibraryBinding {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_purpose: String,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
    pub binding_state: String,
    pub latest_validation: Option<AdminBindingValidation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPrice {
    pub id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub price_variant_key: String,
    pub request_input_tokens_min: Option<i32>,
    pub request_input_tokens_max: Option<i32>,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub workspace_id: Option<Uuid>,
    pub set_in_workspace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminOpsSnapshot {
    pub library_id: Uuid,
    pub queue_depth: i32,
    pub running_attempts: i32,
    pub readable_document_count: i32,
    pub failed_document_count: i32,
    pub degraded_state: String,
    pub latest_knowledge_generation_id: Option<Uuid>,
    pub knowledge_generation_state: Option<String>,
    pub last_recomputed_at: DateTime<Utc>,
    pub warning_count: i32,
    pub knowledge_generation_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditEvent {
    pub id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub result_kind: String,
    pub created_at: DateTime<Utc>,
    pub redacted_message: Option<String>,
    pub subject_summary: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminConsoleData {
    pub viewer: IamMe,
    pub capabilities: AdminCapabilityState,
    pub tokens: Vec<AdminToken>,
    pub providers: Vec<AdminProvider>,
    pub models: Vec<AdminModel>,
    pub credentials: Vec<AdminCredential>,
    pub presets: Vec<AdminModelPreset>,
    pub bindings: Vec<AdminLibraryBinding>,
    pub prices: Vec<AdminPrice>,
    pub ops: Option<AdminOpsSnapshot>,
    pub audit_events: Vec<AdminAuditEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSurface {
    pub viewer: AdminViewerSummary,
    pub capabilities: AdminCapabilityState,
    pub sections: Vec<AdminSectionSummary>,
    pub warnings: Vec<OperatorWarning>,
}
