use std::collections::{HashMap, HashSet};

use serde_json::json;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::{
    app::state::AppState,
    domains::ai::{
        AiAccount, AiBinding, AiBindingPurpose, AiScopeKind, BindingValidation,
        DiscoveredProviderModel, ModelAvailabilityState, ModelCatalogEntry, ModelPricing,
        ProviderCatalogEntry, ResolvedModelCatalogEntry,
    },
    domains::provider_profiles::{
        ProviderBaseUrlPolicy, ProviderCapabilities, ProviderCapabilityState,
        ProviderCredentialPolicy, ProviderCredentialValidationMode, ProviderModelCapabilityKind,
        ProviderModelDiscovery, ProviderModelDiscoveryMode, ProviderModelSelection,
        ProviderProfile, ProviderRequestPolicy, ProviderRuntimeProfile, SelectedReasoningMode,
        TYPESAFE_SYSTEM_ONE_RUNTIME_KIND,
    },
    infra::repositories::{ai_repository, catalog_repository},
    integrations::llm::{ChatRequestSeed, embedding_request_parameters},
    interfaces::http::router_support::ApiError,
    shared::secret_encryption::SecretPurpose,
};

mod accounts;
mod binding_eligibility;
mod binding_operations;
mod bootstrap;
mod bootstrap_operations;
mod bootstrap_persistence;
mod catalog;
mod model_discovery;
mod provider_access;
mod provider_validation;
mod runtime;
mod runtime_resolution;
mod shared;
#[cfg(test)]
mod test_binding;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use test_binding::systemone_decision_binding_base;

pub use model_discovery::AvailableProviderModel;
pub use runtime::{
    EmbeddingDimensions, EmbeddingDimensionsValidationError, ResolvedRuntimeBinding,
};

use binding_eligibility::{
    BindingTarget, validate_binding_target, validate_model_purposes_for_provider,
};
#[cfg(test)]
use binding_operations::{binding_affects_vector_index_dimension, validate_binding_request_policy};
use binding_operations::{parse_request_policy, validate_request_policy};
#[cfg(test)]
use bootstrap_operations::bootstrap_env_credential_needs_sync;
use runtime::{merge_binding_request_context, resolve_effective_embedding_dimensions};
use runtime_resolution::deduplicate_binding_purposes;
pub(crate) use runtime_resolution::embedding_profile_key_from_effective_row;

#[cfg(test)]
use bootstrap::resolve_bootstrap_provider_binding_descriptors;
use bootstrap::{
    bootstrap_binding_inputs_cover_required_purposes,
    bootstrap_binding_profile_for_provider_purpose, bootstrap_bundle_is_self_contained,
    bootstrap_credential_source, bootstrap_provider_credential_map, ensure_bootstrap_binding,
    ensure_bootstrap_provider_account, normalize_bootstrap_binding_inputs,
    resolve_bootstrap_provider_binding_bundle, resolve_bootstrap_provider_bundle,
    resolve_configured_bootstrap_binding_inputs,
    validate_bootstrap_binding_inputs_cover_required_purposes,
    validate_bootstrap_model_list_binding_inputs,
};
use catalog::validate_model_binding_purpose;
#[cfg(test)]
use catalog::{map_model_row, validate_model_metadata};
#[cfg(test)]
use provider_access::is_loopback_base_url;
#[cfg(test)]
use provider_validation::normalize_provider_base_url;
use provider_validation::{
    is_provider_credential_validation_error, provider_credential_base_url_for_create,
    provider_credential_base_url_for_update, runtime_provider_base_url, validate_provider_access,
    validate_provider_base_url_key_reuse,
};
use shared::{
    binding_purpose_key, map_ai_delete_error, map_ai_write_error, map_binding_row,
    map_binding_validation_row, normalize_non_empty, normalize_optional, normalize_scope_ref,
    parse_binding_purpose, parse_scope_kind, scope_can_use_resource, scope_kind_key,
    scope_ref_from_account, scope_ref_from_binding_row,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiScopeRef {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
}

/// AI account metadata safe for list/availability paths.
///
/// This type can report whether a credential exists but can never carry the
/// persisted envelope or decrypted provider key.
#[derive(Debug, Clone)]
pub struct AiAccountSummary {
    pub id: Uuid,
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub base_url: Option<String>,
    pub credential_state: String,
    pub has_api_key: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct CreateAiAccountCommand {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
}

impl Drop for CreateAiAccountCommand {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for CreateAiAccountCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreateAiAccountCommand")
            .field("scope_kind", &self.scope_kind)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("provider_catalog_id", &self.provider_catalog_id)
            .field("label", &self.label)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("created_by_principal_id", &self.created_by_principal_id)
            .finish()
    }
}

#[derive(Clone)]
pub struct UpdateAiAccountCommand {
    pub account_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub credential_state: String,
    pub expected_updated_at: chrono::DateTime<chrono::Utc>,
}

impl Drop for UpdateAiAccountCommand {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for UpdateAiAccountCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateAiAccountCommand")
            .field("account_id", &self.account_id)
            .field("label", &self.label)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("credential_state", &self.credential_state)
            .field("expected_updated_at", &self.expected_updated_at)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct CreateProviderCatalogCommand {
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct UpdateProviderCatalogCommand {
    pub provider_id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct CreateModelCatalogCommand {
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub lifecycle_state: String,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub metadata_json: serde_json::Value,
    pub pricing: ModelPricing,
}

#[derive(Debug, Clone)]
pub struct UpdateModelCatalogCommand {
    pub model_id: Uuid,
    pub lifecycle_state: String,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub metadata_json: Option<serde_json::Value>,
    pub pricing: ModelPricing,
}

#[derive(Debug, Clone)]
pub struct CreateAiBindingCommand {
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
    pub extra_parameters_json: serde_json::Value,
    /// `active` (default when omitted) or `disabled`; validated by the same
    /// purpose-eligibility gate as an update.
    pub binding_state: Option<String>,
    pub updated_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateAiBindingCommand {
    pub binding_id: Uuid,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
    pub binding_state: String,
    pub updated_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct CreateBindingValidationCommand {
    pub binding_id: Uuid,
    pub validation_state: String,
    pub failure_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapAiCredentialSource {
    Missing,
    Env,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BootstrapAiBindingDescriptor {
    pub binding_purpose: AiBindingPurpose,
    pub owner_provider_catalog_id: Uuid,
    pub owner_provider_kind: String,
    pub model_catalog_id: Uuid,
    pub model_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BootstrapAiProviderBindingBundle {
    pub provider_catalog_id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub credential_source: BootstrapAiCredentialSource,
    pub default_base_url: Option<String>,
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub credential_policy: ProviderCredentialPolicy,
    pub base_url_policy: ProviderBaseUrlPolicy,
    pub model_discovery: ProviderModelDiscovery,
    pub capabilities: ProviderCapabilities,
    pub runtime: ProviderRuntimeProfile,
    pub ui_hints: serde_json::Value,
    pub bindings: Vec<BootstrapAiBindingDescriptor>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BootstrapAiCredentialInput {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

impl std::fmt::Debug for BootstrapAiCredentialInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapAiCredentialInput")
            .field("provider_kind", &self.provider_kind)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Drop for BootstrapAiCredentialInput {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BootstrapAiBindingInput {
    pub binding_purpose: AiBindingPurpose,
    pub provider_kind: String,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct BootstrapAiSetupDescriptor {
    pub binding_bundles: Vec<BootstrapAiProviderBindingBundle>,
}

#[derive(Clone)]
pub struct ApplyBootstrapAiSetupCommand {
    pub credentials: Vec<BootstrapAiCredentialInput>,
    pub binding_inputs: Vec<BootstrapAiBindingInput>,
    pub updated_by_principal_id: Option<Uuid>,
}

#[derive(Clone)]
pub struct ApplyBootstrapProviderBindingBundleCommand {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub updated_by_principal_id: Option<Uuid>,
}

impl Drop for ApplyBootstrapProviderBindingBundleCommand {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for ApplyBootstrapProviderBindingBundleCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApplyBootstrapProviderBindingBundleCommand")
            .field("provider_kind", &self.provider_kind)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("updated_by_principal_id", &self.updated_by_principal_id)
            .finish()
    }
}
#[derive(Clone, Default)]
pub struct AiCatalogService;

const REQUIRED_BINDING_PURPOSES: [AiBindingPurpose; 6] = [
    AiBindingPurpose::ExtractText,
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::EmbedChunk,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Agent,
];

const fn provider_credential_policy(provider: &ProviderCatalogEntry) -> &ProviderCredentialPolicy {
    &provider.credential_policy
}
impl AiCatalogService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
