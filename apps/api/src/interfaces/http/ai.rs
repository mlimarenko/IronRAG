mod accounts;
mod bindings;
mod catalog;
mod support;
#[cfg(test)]
mod tests;

pub use accounts::*;
pub use bindings::*;
pub use catalog::*;
use support::*;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize as _;

use crate::{
    app::state::AppState,
    domains::ai::{
        AiAccount, AiBinding, AiBindingPurpose, AiScopeKind, BindingValidation,
        ModelAvailabilityState, ModelPricing, ProviderCatalogEntry, ResolvedModelCatalogEntry,
    },
    domains::provider_profiles::{
        ProviderBaseUrlPolicy, ProviderCapabilities, ProviderCredentialPolicy,
        ProviderModelDiscovery, ProviderRuntimeProfile,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_DISCOVERY, POLICY_PROVIDERS_ADMIN, authorize_workspace_permission,
            load_library_and_authorize,
        },
        router_support::{ApiError, RequestId, RestAuditEvent, record_rest_audit_event},
    },
    services::ai_catalog_service::{
        AiAccountSummary, AiScopeRef, AvailableProviderModel, CreateAiAccountCommand,
        CreateAiBindingCommand, CreateBindingValidationCommand, CreateModelCatalogCommand,
        CreateProviderCatalogCommand, UpdateAiAccountCommand, UpdateAiBindingCommand,
        UpdateModelCatalogCommand, UpdateProviderCatalogCommand,
    },
    services::iam::audit::AppendAuditEventSubjectCommand,
};

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ModelsQuery {
    pub provider_catalog_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AiScopeQuery {
    pub scope_kind: Option<AiScopeKind>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateProviderCatalogRequest {
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: serde_json::Value,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderCatalogRequest {
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateModelCatalogRequest {
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub lifecycle_state: String,
    #[serde(default)]
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    #[serde(default)]
    pub metadata_json: serde_json::Value,
    pub pricing: ModelPricing,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateModelCatalogRequest {
    pub lifecycle_state: String,
    #[serde(default)]
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub metadata_json: Option<serde_json::Value>,
    pub pricing: ModelPricing,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAiAccountRequest {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

impl Drop for CreateAiAccountRequest {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for CreateAiAccountRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreateAiAccountRequest")
            .field("scope_kind", &self.scope_kind)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("provider_catalog_id", &self.provider_catalog_id)
            .field("label", &self.label)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAiAccountRequest {
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub credential_state: String,
    /// Optimistic-concurrency fence from the latest account detail response.
    pub expected_updated_at: chrono::DateTime<chrono::Utc>,
}

impl Drop for UpdateAiAccountRequest {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for UpdateAiAccountRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateAiAccountRequest")
            .field("label", &self.label)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("credential_state", &self.credential_state)
            .field("expected_updated_at", &self.expected_updated_at)
            .finish()
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAiBindingRequest {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: AiBindingPurpose,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    #[serde(default)]
    pub extra_parameters_json: serde_json::Value,
    /// `active` (default) or `disabled`. An operator may store a
    /// decision-provider binding `disabled` and activate it later; omitting the
    /// field keeps the historical active default.
    #[serde(default)]
    pub binding_state: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAiBindingRequest {
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    #[serde(default)]
    pub extra_parameters_json: serde_json::Value,
    pub binding_state: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogEntryResponse {
    pub id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub credential_policy: ProviderCredentialPolicy,
    pub base_url_policy: ProviderBaseUrlPolicy,
    pub model_discovery: ProviderModelDiscovery,
    pub capabilities: ProviderCapabilities,
    pub runtime: ProviderRuntimeProfile,
    pub ui_hints: serde_json::Value,
    /// Purposes no seeded model of this provider covers, each with why. Empty
    /// means the seeded models cover all six.
    pub purpose_gaps: Vec<crate::domains::provider_profiles::ProviderPurposeGap>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntryResponse {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub lifecycle_state: String,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub availability_state: ModelAvailabilityState,
    pub available_account_ids: Vec<Uuid>,
    /// Carries `seedSource` on rows this build seeded. Its absence is what
    /// tells a client the row came from an operator, whose purpose list is a
    /// record of what has been bound rather than a statement of capability.
    pub metadata_json: serde_json::Value,
    pub pricing: ModelPricing,
}

/// One model the provider reports as reachable with an account, and the
/// catalog row that already represents it when there is one. A router's chat
/// set is seeded from one listing and may lag it, so this live listing is
/// where the operator sees what the account reaches now.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvailableProviderModelResponse {
    pub model_name: String,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub input_modalities: Vec<String>,
    /// Rates the provider published. Absent means it published none — a caller
    /// recording a price must ask for one rather than assume zero.
    pub pricing: Option<ModelPricing>,
    /// Set when this model already has a catalog row.
    pub model_catalog_id: Option<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiAccountResponse {
    pub id: Uuid,
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub base_url: Option<String>,
    pub api_key_summary: String,
    pub credential_state: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

crate::ai_binding_type! {
    #[derive(Debug, Serialize, utoipa::ToSchema)]
    #[serde(rename_all = "camelCase")]
    pub struct AiBindingResponse
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingValidationResponse {
    pub id: Uuid,
    pub binding_id: Uuid,
    pub validation_state: String,
    pub checked_at: chrono::DateTime<chrono::Utc>,
    pub failure_code: Option<String>,
    pub message: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ai/providers", get(list_providers).post(create_provider))
        .route(
            "/ai/providers/{provider_id}",
            get(get_provider).patch(update_provider).delete(delete_provider),
        )
        .route("/ai/models", get(list_models).post(create_model))
        .route("/ai/models/{model_id}", get(get_model).patch(update_model).delete(delete_model))
        .route("/ai/accounts", get(list_accounts).post(create_account))
        .route(
            "/ai/accounts/{account_id}",
            get(get_account).patch(update_account).delete(delete_account),
        )
        .route("/ai/accounts/{account_id}/available-models", get(list_available_provider_models))
        .route("/ai/bindings", get(list_bindings).post(create_binding))
        .route(
            "/ai/bindings/{binding_id}",
            get(get_binding).patch(update_binding).delete(delete_binding),
        )
        .route(
            "/ai/bindings/{binding_id}/validations",
            get(list_binding_validations).post(create_binding_validation),
        )
        .route("/ai/bindings/{binding_id}/validations/{validation_id}", get(get_binding_validation))
}

/// Builds a sync-create `201 Created` response with a `Location` header
/// pointing at the newly created resource's own item-read endpoint (see
/// router_support conventions established by the Content domain reference
/// implementation).
fn created_response<T: Serialize>(location: &str, body: T) -> axum::response::Response {
    let mut response = (StatusCode::CREATED, Json(body)).into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}
