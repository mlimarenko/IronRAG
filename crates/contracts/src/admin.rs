//! Administration and operator-facing transport contracts.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::IamMe;
use crate::diagnostics::OperatorWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Top-level sections of the administration console.
pub enum AdminSection {
    /// Identities, grants, and access tokens.
    Access,
    /// Model Context Protocol configuration.
    Mcp,
    /// Runtime health and processing state.
    Operations,
    /// Pending and active background work.
    Queue,
    /// Providers, models, credentials, and bindings.
    Ai,
    /// Model usage prices.
    Pricing,
    /// System-level settings.
    Settings,
}

impl AdminSection {
    #[must_use]
    /// Returns the display label used by administration navigation.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Access => "Access",
            Self::Mcp => "MCP",
            Self::Operations => "Operations",
            Self::Queue => "Queue",
            Self::Ai => "AI",
            Self::Pricing => "Pricing",
            Self::Settings => "Settings",
        }
    }

    #[must_use]
    /// Returns the route for this administration section.
    pub const fn path(self) -> &'static str {
        match self {
            Self::Access => "/admin/access",
            Self::Mcp => "/admin/mcp",
            Self::Operations => "/admin/operations",
            Self::Queue => "/admin/queue",
            Self::Ai => "/admin/ai",
            Self::Pricing => "/admin/pricing",
            Self::Settings => "/admin/settings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Access decision for one administration section.
pub struct CapabilityGate {
    /// Section governed by the decision.
    pub section: AdminSection,
    /// Whether the current principal may open the section.
    pub allowed: bool,
    /// Explanation shown when access is restricted.
    pub reason: Option<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the transport contract mirrors independent capability flags from the public API"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Independent administration capabilities of the current principal.
pub struct AdminCapabilityState {
    /// Whether the administration surface is available at all.
    pub admin_enabled: bool,
    /// Whether access tokens may be created or revoked.
    pub can_manage_tokens: bool,
    /// Whether audit events may be viewed.
    pub can_read_audit: bool,
    /// Whether operational state may be viewed.
    pub can_read_operations: bool,
    /// Whether AI providers, credentials, presets, and bindings may be changed.
    pub can_manage_ai: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Compact identity and access summary for the administration shell.
pub struct AdminViewerSummary {
    /// Principal represented by the current session.
    pub principal_id: Uuid,
    /// Human-readable principal name.
    pub display_name: String,
    /// Human-readable description of the principal's access level.
    pub access_label: String,
    /// Whether the principal has the system administrator role.
    pub is_admin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Navigation metadata and access state for an administration section.
pub struct AdminSectionSummary {
    /// Section represented by the summary.
    pub section: AdminSection,
    /// Title shown in navigation.
    pub title: String,
    /// Short description of the section contents.
    pub summary: String,
    /// Optional count of records represented by the section.
    pub item_count: Option<i32>,
    /// Access decision for the current principal.
    pub gate: CapabilityGate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Administrative view of an issued access token.
pub struct AdminToken {
    /// Principal authenticated by the token.
    pub principal_id: Uuid,
    /// Workspace scope when the token is not system-wide.
    pub workspace_id: Option<Uuid>,
    /// Operator-assigned token label.
    pub label: String,
    /// Non-secret prefix used to identify the token.
    pub token_prefix: String,
    /// Current token lifecycle status.
    pub status: String,
    /// Expiration time when the token is temporary.
    pub expires_at: Option<DateTime<Utc>>,
    /// Revocation time when the token has been revoked.
    pub revoked_at: Option<DateTime<Utc>>,
    /// Principal that issued the token, when recorded.
    pub issued_by_principal_id: Option<Uuid>,
    /// Most recent authenticated use of the token.
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Provider catalog entry exposed to administrators.
pub struct AdminProvider {
    /// Stable provider catalog identifier.
    pub id: Uuid,
    /// Stable provider implementation kind.
    pub provider_kind: String,
    /// Human-readable provider name.
    pub display_name: String,
    /// Protocol style expected by the provider adapter.
    pub api_style: String,
    /// Current catalog lifecycle state.
    pub lifecycle_state: String,
    /// Configured credential source, if one is available.
    pub credential_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Model catalog entry and its supported binding constraints.
pub struct AdminModel {
    /// Stable model catalog identifier.
    pub id: Uuid,
    /// Provider catalog entry that serves the model.
    pub provider_catalog_id: Uuid,
    /// Provider-facing model name.
    pub model_name: String,
    /// Primary capability category of the model.
    pub capability_kind: String,
    /// Input and output modality category.
    pub modality_kind: String,
    /// AI purposes for which the model may be selected.
    pub allowed_binding_purposes: Vec<String>,
    /// Maximum context size in tokens, when known.
    pub context_window: Option<i32>,
    /// Maximum generated output in tokens, when known.
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Redacted provider credential metadata visible to administrators.
pub struct AdminCredential {
    /// Stable credential identifier.
    pub id: Uuid,
    /// Workspace that owns the credential.
    pub workspace_id: Uuid,
    /// Provider catalog entry authenticated by the credential.
    pub provider_catalog_id: Uuid,
    /// Operator-assigned credential label.
    pub label: String,
    /// Redacted key summary suitable for identification.
    pub api_key_summary: String,
    /// Current credential lifecycle state.
    pub credential_state: String,
    /// Time at which the credential record was created.
    pub created_at: DateTime<Utc>,
    /// Time at which the credential record last changed.
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Workspace-specific generation settings for a catalog model.
pub struct AdminModelPreset {
    /// Stable preset identifier.
    pub id: Uuid,
    /// Workspace that owns the preset.
    pub workspace_id: Uuid,
    /// Catalog model configured by the preset.
    pub model_catalog_id: Uuid,
    /// Human-readable preset name.
    pub preset_name: String,
    /// Optional system prompt applied to model requests.
    pub system_prompt: Option<String>,
    /// Optional sampling temperature.
    pub temperature: Option<f64>,
    /// Optional nucleus-sampling threshold.
    pub top_p: Option<f64>,
    /// Optional output-token limit overriding the model default.
    pub max_output_tokens_override: Option<i32>,
    /// Time at which the preset was created.
    pub created_at: DateTime<Utc>,
    /// Time at which the preset last changed.
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Latest validation result for a library-to-model binding.
pub struct AdminBindingValidation {
    /// Stable validation-attempt identifier.
    pub id: Uuid,
    /// Binding checked by the validation attempt.
    pub binding_id: Uuid,
    /// Outcome state of the validation attempt.
    pub validation_state: String,
    /// Time at which validation completed.
    pub checked_at: DateTime<Utc>,
    /// Machine-readable failure category when validation failed.
    pub failure_code: Option<String>,
    /// Optional diagnostic detail about the outcome.
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Model preset and credential assigned to one library purpose.
pub struct AdminLibraryBinding {
    /// Stable binding identifier.
    pub id: Uuid,
    /// Workspace that owns the binding.
    pub workspace_id: Uuid,
    /// Library configured by the binding.
    pub library_id: Uuid,
    /// AI pipeline purpose served by the binding.
    pub binding_purpose: String,
    /// Credential used to authenticate model requests.
    pub provider_credential_id: Uuid,
    /// Model preset used for the purpose.
    pub model_preset_id: Uuid,
    /// Current binding lifecycle state.
    pub binding_state: String,
    /// Most recent validation result, if the binding has been checked.
    pub latest_validation: Option<AdminBindingValidation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Effective usage price for a catalog model.
pub struct AdminPrice {
    /// Stable price-entry identifier.
    pub id: Uuid,
    /// Catalog model to which the price applies.
    pub model_catalog_id: Uuid,
    /// Unit of model usage charged by this entry.
    pub billing_unit: String,
    /// Provider price tier or variant identifier.
    pub price_variant_key: String,
    /// Inclusive lower input-token bound for this tier.
    pub request_input_tokens_min: Option<i32>,
    /// Inclusive upper input-token bound for this tier.
    pub request_input_tokens_max: Option<i32>,
    /// Charge per billing unit.
    pub unit_price: Decimal,
    /// Currency in which the unit price is denominated.
    pub currency_code: String,
    /// Time at which the price becomes effective.
    pub effective_from: DateTime<Utc>,
    /// Time at which the price ceases to apply, if bounded.
    pub effective_to: Option<DateTime<Utc>>,
    /// Workspace scope for an override; absent for the default price.
    pub workspace_id: Option<Uuid>,
    /// Whether the price was configured within the current workspace.
    pub set_in_workspace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Point-in-time processing and knowledge-state summary for a library.
pub struct AdminOpsSnapshot {
    /// Library represented by the snapshot.
    pub library_id: Uuid,
    /// Number of queued attempts awaiting execution.
    pub queue_depth: i32,
    /// Number of attempts currently running.
    pub running_attempts: i32,
    /// Number of documents with readable text.
    pub readable_document_count: i32,
    /// Number of documents whose processing failed.
    pub failed_document_count: i32,
    /// Aggregate degradation state of the library.
    pub degraded_state: String,
    /// Most recent knowledge generation, if one exists.
    pub latest_knowledge_generation_id: Option<Uuid>,
    /// Lifecycle state of the most recent knowledge generation.
    pub knowledge_generation_state: Option<String>,
    /// Time at which the snapshot was last recomputed.
    pub last_recomputed_at: DateTime<Utc>,
    /// Number of current operator warnings.
    pub warning_count: i32,
    /// Total number of knowledge generations recorded for the library.
    pub knowledge_generation_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Redacted audit event displayed in the administration console.
pub struct AdminAuditEvent {
    /// Stable event identifier.
    pub id: Uuid,
    /// Principal that initiated the action, when attributable.
    pub actor_principal_id: Option<Uuid>,
    /// Product surface on which the action occurred.
    pub surface_kind: String,
    /// Category of action that was attempted.
    pub action_kind: String,
    /// Outcome category of the action.
    pub result_kind: String,
    /// Time at which the event was recorded.
    pub created_at: DateTime<Utc>,
    /// Optional diagnostic message with sensitive data removed.
    pub redacted_message: Option<String>,
    /// Non-sensitive summary of the affected subject.
    pub subject_summary: String,
    /// Request correlation identifier, when available.
    pub request_id: Option<String>,
    /// Distributed-trace correlation identifier, when available.
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Complete data set rendered by the administration console.
pub struct AdminConsoleData {
    /// Effective identity and grants of the current principal.
    pub viewer: IamMe,
    /// Administration capabilities of the current principal.
    pub capabilities: AdminCapabilityState,
    /// Access tokens visible to the principal.
    pub tokens: Vec<AdminToken>,
    /// Available provider catalog entries.
    pub providers: Vec<AdminProvider>,
    /// Available model catalog entries.
    pub models: Vec<AdminModel>,
    /// Provider credentials visible to the principal.
    pub credentials: Vec<AdminCredential>,
    /// Workspace model presets visible to the principal.
    pub presets: Vec<AdminModelPreset>,
    /// Library model bindings visible to the principal.
    pub bindings: Vec<AdminLibraryBinding>,
    /// Effective model usage prices.
    pub prices: Vec<AdminPrice>,
    /// Selected library's operational snapshot, when requested.
    pub ops: Option<AdminOpsSnapshot>,
    /// Recent audit events visible to the principal.
    pub audit_events: Vec<AdminAuditEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Lightweight administration shell state used before section data loads.
pub struct AdminSurface {
    /// Compact summary of the current principal.
    pub viewer: AdminViewerSummary,
    /// Administration capabilities of the current principal.
    pub capabilities: AdminCapabilityState,
    /// Navigation entries and access decisions for each section.
    pub sections: Vec<AdminSectionSummary>,
    /// Current warnings that require operator attention.
    pub warnings: Vec<OperatorWarning>,
}
