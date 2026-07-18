use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::{
    app::state::AppState,
    domains::ai::{
        AiAccount, AiBinding, AiBindingPurpose, AiScopeKind, BindingValidation,
        ModelAvailabilityState, ModelCatalogEntry, PriceCatalogEntry, ProviderCatalogEntry,
        ResolvedModelCatalogEntry,
    },
    domains::provider_profiles::{
        ProviderBaseUrlPolicy, ProviderCapabilities, ProviderCapabilityState,
        ProviderCredentialPolicy, ProviderCredentialValidationMode, ProviderModelDiscovery,
        ProviderModelDiscoveryMode, ProviderModelSelection, ProviderProfile, ProviderRequestPolicy,
        ProviderRuntimeProfile,
    },
    infra::repositories::{ai_repository, catalog_repository},
    integrations::llm::{ChatRequestSeed, embedding_request_parameters},
    interfaces::http::router_support::ApiError,
    shared::secret_encryption::SecretPurpose,
};

mod accounts;
mod bootstrap;
mod catalog;
mod provider_validation;
mod shared;
#[cfg(test)]
mod tests;

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
use catalog::{map_model_row, metadata_with_binding_purposes, parse_allowed_binding_purposes};
#[cfg(test)]
use provider_validation::{
    canonicalize_provider_base_url, discovered_provider_model_signature_for_capability,
    is_loopback_base_url,
};
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

fn bootstrap_ai_setup_for_operation(
    state: &AppState,
) -> Option<crate::app::config::UiBootstrapAiSetup> {
    state.ui_bootstrap_ai_setup.clone().or_else(|| state.settings.resolved_ui_bootstrap_ai_setup())
}

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
    pub capability_kind: String,
    pub modality_kind: String,
    pub lifecycle_state: String,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct UpdateModelCatalogCommand {
    pub model_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub lifecycle_state: String,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub metadata_json: Option<serde_json::Value>,
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
pub struct CreateWorkspacePriceOverrideCommand {
    pub workspace_id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: chrono::DateTime<chrono::Utc>,
    pub effective_to: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpdateWorkspacePriceOverrideCommand {
    pub price_id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: chrono::DateTime<chrono::Utc>,
    pub effective_to: Option<chrono::DateTime<chrono::Utc>>,
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

#[derive(Clone)]
pub struct ResolvedRuntimeBinding {
    pub binding_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_purpose: AiBindingPurpose,
    pub provider_catalog_id: Uuid,
    pub provider_kind: String,
    pub provider_base_url: Option<String>,
    pub provider_api_style: String,
    pub account_id: Uuid,
    pub api_key: Option<String>,
    pub model_catalog_id: Uuid,
    pub model_name: String,
    /// Validated output dimension of the effective embedding profile.
    ///
    /// Binding request parameters take precedence over model-catalog metadata.
    /// `None` means no dimension was declared. Runtime resolution may use only
    /// exact-profile persisted vector metadata; it never issues a hidden
    /// provider probe.
    pub effective_embedding_dimensions: Option<EmbeddingDimensions>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

impl Drop for ResolvedRuntimeBinding {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for ResolvedRuntimeBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedRuntimeBinding")
            .field("binding_id", &self.binding_id)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("binding_purpose", &self.binding_purpose)
            .field("provider_catalog_id", &self.provider_catalog_id)
            .field("provider_kind", &self.provider_kind)
            .field("provider_base_url", &self.provider_base_url.as_ref().map(|_| "<redacted>"))
            .field("provider_api_style", &self.provider_api_style)
            .field("account_id", &self.account_id)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model_catalog_id", &self.model_catalog_id)
            .field("model_name", &self.model_name)
            .field("effective_embedding_dimensions", &self.effective_embedding_dimensions)
            .finish_non_exhaustive()
    }
}

/// Non-zero embedding dimension supported by the Postgres vector plane.
///
/// The private representation prevents an invalid zero or a value that cannot
/// be indexed by the selected pgvector `vector`/`halfvec` storage strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EmbeddingDimensions(std::num::NonZeroU32);

const MAX_INDEXED_EMBEDDING_DIMENSIONS: u64 = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why an embedding dimension cannot be represented by [`EmbeddingDimensions`].
pub enum EmbeddingDimensionsValidationError {
    /// Zero cannot describe a vector space.
    Zero,
    /// The dimension is greater than the largest supported pgvector storage lane.
    ExceedsStorageLimit,
}

impl std::fmt::Display for EmbeddingDimensionsValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zero => formatter.write_str("must be greater than zero"),
            Self::ExceedsStorageLimit => {
                formatter.write_str("exceeds the vector storage dimension limit")
            }
        }
    }
}

impl std::error::Error for EmbeddingDimensionsValidationError {}

impl TryFrom<u64> for EmbeddingDimensions {
    type Error = EmbeddingDimensionsValidationError;

    fn try_from(dimensions: u64) -> Result<Self, Self::Error> {
        if dimensions == 0 {
            return Err(EmbeddingDimensionsValidationError::Zero);
        }
        if dimensions > MAX_INDEXED_EMBEDDING_DIMENSIONS {
            return Err(EmbeddingDimensionsValidationError::ExceedsStorageLimit);
        }
        let dimensions = u32::try_from(dimensions)
            .map_err(|_| EmbeddingDimensionsValidationError::ExceedsStorageLimit)?;
        let dimensions = std::num::NonZeroU32::new(dimensions)
            .ok_or(EmbeddingDimensionsValidationError::Zero)?;
        Ok(Self(dimensions))
    }
}

impl EmbeddingDimensions {
    /// Returns the validated dimension as an unsigned integer suitable for
    /// persistence and provider-boundary validation.
    #[must_use]
    pub fn get(self) -> u64 {
        u64::from(self.0.get())
    }
}

impl ResolvedRuntimeBinding {
    /// Stable, secret-free identity of the effective request profile that
    /// produces embedding vectors.
    ///
    /// Scope and binding-row identifiers are deliberately excluded: an
    /// inherited instance/workspace binding and its library resolution must
    /// address the same vector lane. Conversely, every value that can select
    /// a different embedding execution path is included. Credential/account
    /// and catalog-row UUIDs are deliberately excluded: rotating or reseeding
    /// those records does not change a vector space when endpoint, runtime,
    /// model identity, and semantic request parameters are unchanged. Binding
    /// purpose is routing metadata and is not part of the provider request.
    #[must_use]
    pub fn embedding_execution_profile_key(&self) -> String {
        const PROFILE_DOMAIN: &[u8] = b"ironrag.embedding-execution-profile.v1";
        const KEY_PREFIX: &str = "embedding-profile:v1:";

        let mut hasher = Sha256::new();
        update_embedding_profile_field(&mut hasher, b"domain", PROFILE_DOMAIN);
        update_embedding_profile_field(
            &mut hasher,
            b"provider_kind",
            self.provider_kind.as_bytes(),
        );
        let normalized_base_url =
            normalized_embedding_profile_base_url(self.provider_base_url.as_deref());
        update_embedding_profile_optional_field(
            &mut hasher,
            b"provider_base_url",
            normalized_base_url.as_deref().map(str::as_bytes),
        );
        update_embedding_profile_field(
            &mut hasher,
            b"provider_api_style",
            self.provider_api_style.as_bytes(),
        );
        update_embedding_profile_optional_field(
            &mut hasher,
            b"provider_runtime_kind",
            embedding_profile_runtime_string(&self.extra_parameters_json, "kind")
                .as_deref()
                .map(str::as_bytes),
        );
        update_embedding_profile_optional_field(
            &mut hasher,
            b"provider_embeddings_path",
            embedding_profile_runtime_string(&self.extra_parameters_json, "embeddingsPath")
                .as_deref()
                .map(str::as_bytes),
        );
        update_embedding_profile_optional_field(
            &mut hasher,
            b"provider_private_network_routing",
            embedding_profile_private_network_routing(&self.extra_parameters_json)
                .as_deref()
                .map(str::as_bytes),
        );
        update_embedding_profile_field(&mut hasher, b"model_name", self.model_name.as_bytes());
        update_embedding_profile_json_field(
            &mut hasher,
            b"embedding_request_parameters",
            &self.effective_embedding_request_parameters(),
        );
        format!("{KEY_PREFIX}{}", hex::encode(hasher.finalize()))
    }

    /// Canonical semantic request profile used only for vector-space identity.
    ///
    /// Catalog dimensions are inserted into this projection without mutating
    /// `extra_parameters_json`, so they fence stored vectors but are not sent to
    /// providers that do not accept an explicit dimensions request parameter.
    /// Explicit binding dimensions are reinserted from the validated typed
    /// field, keeping hashing and dimension resolution on one source of truth.
    fn effective_embedding_request_parameters(&self) -> serde_json::Value {
        let mut parameters = embedding_request_parameters(&self.extra_parameters_json);
        let Some(parameters) = parameters.as_object_mut() else {
            return serde_json::json!({});
        };
        parameters.remove("dimensions");
        if let Some(dimensions) = self.effective_embedding_dimensions {
            parameters.insert(
                "dimensions".to_string(),
                serde_json::Value::Number(serde_json::Number::from(dimensions.get())),
            );
        }
        serde_json::Value::Object(std::mem::take(parameters))
    }

    #[must_use]
    pub fn chat_request_seed(&self) -> ChatRequestSeed {
        ChatRequestSeed {
            provider_kind: self.provider_kind.clone(),
            model_name: self.model_name.clone(),
            api_key_override: self.api_key.clone(),
            base_url_override: self.provider_base_url.clone(),
            system_prompt: self.system_prompt.clone(),
            temperature: self.temperature,
            top_p: self.top_p,
            max_output_tokens_override: self.max_output_tokens_override,
            extra_parameters_json: self.extra_parameters_json.clone(),
        }
    }
}

fn normalized_embedding_profile_base_url(value: Option<&str>) -> Option<String> {
    value.map(|value| {
        let trimmed = value.trim().trim_end_matches('/');
        reqwest::Url::parse(trimmed).map_or_else(
            |_| trimmed.to_string(),
            |url| url.to_string().trim_end_matches('/').to_string(),
        )
    })
}

fn embedding_profile_runtime_string(
    extra_parameters_json: &serde_json::Value,
    field: &str,
) -> Option<String> {
    extra_parameters_json
        .get("_providerProfile")?
        .get("runtime")?
        .get(field)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if field == "embeddingsPath" {
                format!("/{}", value.trim_matches('/'))
            } else {
                value.to_string()
            }
        })
}

fn embedding_profile_private_network_routing(
    extra_parameters_json: &serde_json::Value,
) -> Option<String> {
    extra_parameters_json
        .get("_providerProfile")?
        .get("baseUrl")?
        .get("allowPrivateNetwork")?
        .as_bool()
        .map(|value| value.to_string())
}

fn update_embedding_profile_field(hasher: &mut Sha256, name: &[u8], value: &[u8]) {
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn update_embedding_profile_optional_field(hasher: &mut Sha256, name: &[u8], value: Option<&[u8]>) {
    if let Some(value) = value {
        hasher.update([1]);
        update_embedding_profile_field(hasher, name, value);
    } else {
        hasher.update([0]);
        update_embedding_profile_field(hasher, name, &[]);
    }
}

fn update_embedding_profile_json_field(
    hasher: &mut Sha256,
    name: &[u8],
    value: &serde_json::Value,
) {
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name);
    update_embedding_profile_json(hasher, value);
}

fn update_embedding_profile_json(hasher: &mut Sha256, value: &serde_json::Value) {
    use serde_json::Value;

    match value {
        Value::Null => hasher.update(b"n"),
        Value::Bool(value) => hasher.update(if *value { b"b1" } else { b"b0" }),
        Value::Number(value) => {
            update_embedding_profile_field(hasher, b"number", value.to_string().as_bytes());
        }
        Value::String(value) => {
            update_embedding_profile_field(hasher, b"string", value.as_bytes());
        }
        Value::Array(values) => {
            hasher.update(b"[");
            hasher.update((values.len() as u64).to_le_bytes());
            for value in values {
                update_embedding_profile_json(hasher, value);
            }
            hasher.update(b"]");
        }
        Value::Object(values) => {
            hasher.update(b"{");
            hasher.update((values.len() as u64).to_le_bytes());
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                update_embedding_profile_field(hasher, b"key", key.as_bytes());
                if let Some(value) = values.get(key) {
                    update_embedding_profile_json(hasher, value);
                }
            }
            hasher.update(b"}");
        }
    }
}

#[derive(Clone, Default)]
pub struct AiCatalogService;

const CANONICAL_REQUIRED_RUNTIME_BINDING_PURPOSES: [AiBindingPurpose; 5] = [
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::EmbedChunk,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Agent,
];

const fn provider_credential_policy(provider: &ProviderCatalogEntry) -> &ProviderCredentialPolicy {
    &provider.credential_policy
}

fn provider_runtime_profile_json(profile: &ProviderProfile) -> serde_json::Value {
    serde_json::json!({
        "runtime": profile.runtime,
        "baseUrl": profile.base_url,
        "credentials": profile.credentials,
        "requestPolicy": profile.request_policy,
    })
}

fn merge_provider_runtime_profile(
    extra_parameters_json: serde_json::Value,
    profile: &ProviderProfile,
) -> serde_json::Value {
    let mut object =
        extra_parameters_json.as_object().cloned().unwrap_or_else(serde_json::Map::new);
    object.insert("_providerProfile".to_string(), provider_runtime_profile_json(profile));
    serde_json::Value::Object(object)
}

fn merge_model_request_policy(
    extra_parameters_json: serde_json::Value,
    model_metadata_json: &serde_json::Value,
) -> serde_json::Value {
    let mut object =
        extra_parameters_json.as_object().cloned().unwrap_or_else(serde_json::Map::new);
    if !object.contains_key("_providerRequestPolicy")
        && let Some(policy) = model_metadata_json.get("requestPolicy")
    {
        object.insert("_providerRequestPolicy".to_string(), policy.clone());
    }
    serde_json::Value::Object(object)
}

fn parse_request_policy(
    value: &serde_json::Value,
    field_name: &str,
) -> Result<ProviderRequestPolicy, ApiError> {
    let policy = serde_json::from_value::<ProviderRequestPolicy>(value.clone())
        .map_err(|_| ApiError::BadRequest(format!("{field_name} is invalid")))?;
    validate_request_policy(&policy, field_name)?;
    Ok(policy)
}

fn validate_request_policy(
    policy: &ProviderRequestPolicy,
    field_name: &str,
) -> Result<(), ApiError> {
    if policy.is_valid() {
        return Ok(());
    }
    Err(ApiError::BadRequest(format!("{field_name}.defaultToolMaxOutputTokens must be positive")))
}

fn validate_binding_request_policy(
    extra_parameters_json: &serde_json::Value,
) -> Result<(), ApiError> {
    let Some(value) = extra_parameters_json.get("_providerRequestPolicy") else {
        return Ok(());
    };
    parse_request_policy(value, "extraParametersJson._providerRequestPolicy").map(|_| ())
}

fn parse_embedding_dimensions(
    source: &serde_json::Value,
    field_name: &str,
) -> Result<Option<EmbeddingDimensions>, ApiError> {
    let Some(value) = source.get("dimensions") else {
        return Ok(None);
    };
    let dimensions = value.as_u64().ok_or_else(|| {
        ApiError::BadRequest(format!("{field_name}.dimensions must be a positive integer"))
    })?;
    EmbeddingDimensions::try_from(dimensions)
        .map(Some)
        .map_err(|error| ApiError::BadRequest(format!("{field_name}.dimensions {error}")))
}

fn resolve_effective_embedding_dimensions(
    binding_purpose: AiBindingPurpose,
    binding_extra_parameters_json: &serde_json::Value,
    model_metadata_json: &serde_json::Value,
) -> Result<Option<EmbeddingDimensions>, ApiError> {
    if binding_purpose != AiBindingPurpose::EmbedChunk {
        return Ok(None);
    }
    if let Some(dimensions) =
        parse_embedding_dimensions(binding_extra_parameters_json, "extraParametersJson")?
    {
        return Ok(Some(dimensions));
    }
    parse_embedding_dimensions(model_metadata_json, "model.metadataJson")
}

fn require_supported_provider_capability(
    provider: &ProviderCatalogEntry,
    binding_purpose: AiBindingPurpose,
    capability_name: &'static str,
    capability: ProviderCapabilityState,
) -> Result<(), ApiError> {
    if capability.is_supported() {
        return Ok(());
    }
    Err(ApiError::BadRequest(format!(
        "provider {} does not declare supported {} capability for binding purpose {}: {:?}",
        provider.provider_kind,
        capability_name,
        binding_purpose.as_str(),
        capability,
    )))
}

fn binding_affects_vector_index_dimension(binding_purpose: AiBindingPurpose) -> bool {
    binding_purpose == AiBindingPurpose::EmbedChunk
}

fn invalidate_vector_dimension_cache_for_binding(binding_purpose: AiBindingPurpose) {
    if binding_affects_vector_index_dimension(binding_purpose) {
        crate::services::query::vector_dimensions::invalidate_vector_index_dimension_cache();
    }
}

fn validate_provider_capability_for_binding(
    provider: &ProviderCatalogEntry,
    binding_purpose: AiBindingPurpose,
) -> Result<(), ApiError> {
    match binding_purpose {
        AiBindingPurpose::ExtractText => {
            require_supported_provider_capability(
                provider,
                binding_purpose,
                "chat",
                provider.capabilities.chat,
            )?;
            require_supported_provider_capability(
                provider,
                binding_purpose,
                "vision",
                provider.capabilities.vision,
            )
        }
        AiBindingPurpose::ExtractGraph
        | AiBindingPurpose::QueryCompile
        | AiBindingPurpose::QueryAnswer => require_supported_provider_capability(
            provider,
            binding_purpose,
            "chat",
            provider.capabilities.chat,
        ),
        AiBindingPurpose::Agent => {
            require_supported_provider_capability(
                provider,
                binding_purpose,
                "chat",
                provider.capabilities.chat,
            )?;
            require_supported_provider_capability(
                provider,
                binding_purpose,
                "tools",
                provider.capabilities.tools,
            )
        }
        AiBindingPurpose::EmbedChunk => require_supported_provider_capability(
            provider,
            binding_purpose,
            "embeddings",
            provider.capabilities.embeddings,
        ),
    }
}

fn deduplicate_binding_purposes(binding_purposes: &[AiBindingPurpose]) -> Vec<AiBindingPurpose> {
    let mut seen = HashSet::with_capacity(binding_purposes.len());
    binding_purposes.iter().copied().filter(|purpose| seen.insert(*purpose)).collect()
}

fn validate_binding_account_scope(scope: AiScopeRef, account: &AiAccount) -> Result<(), ApiError> {
    let account_scope = scope_ref_from_account(account)?;
    if scope_can_use_resource(scope, account_scope) {
        return Ok(());
    }
    Err(ApiError::BadRequest(
        "binding cannot use a provider account from an unrelated scope".to_string(),
    ))
}

fn validate_binding_target_components(
    binding_purpose: AiBindingPurpose,
    extra_parameters_json: &serde_json::Value,
    account: &AiAccount,
    model: &ModelCatalogEntry,
    provider: &ProviderCatalogEntry,
) -> Result<(), ApiError> {
    if model.provider_catalog_id != provider.id {
        return Err(ApiError::BadRequest(
            "binding links a provider account to a model from another provider".to_string(),
        ));
    }
    if account.credential_state != "active" {
        return Err(ApiError::BadRequest("provider account is not active".to_string()));
    }
    resolve_effective_embedding_dimensions(
        binding_purpose,
        extra_parameters_json,
        &model.metadata_json,
    )?;
    validate_model_binding_purpose(binding_purpose, model)?;
    validate_provider_capability_for_binding(provider, binding_purpose)
}

fn validate_runtime_binding_components(
    binding_purpose: AiBindingPurpose,
    model: &ModelCatalogEntry,
    provider: &ProviderCatalogEntry,
    account_credential_state: &str,
    account_base_url: Option<&str>,
) -> Result<Option<String>, ApiError> {
    if model.provider_catalog_id != provider.id {
        return Err(ApiError::BadRequest(
            "binding links a provider account to a model from another provider".to_string(),
        ));
    }
    if account_credential_state != "active" {
        return Err(ApiError::BadRequest("provider account is not active".to_string()));
    }
    validate_model_binding_purpose(binding_purpose, model)?;
    validate_provider_capability_for_binding(provider, binding_purpose)?;
    runtime_provider_base_url(provider, account_base_url)
}

fn map_effective_provider_selection_row(
    mut row: ai_repository::EffectiveProviderSelectionRow,
) -> Result<(AiBindingPurpose, ProviderModelSelection), ApiError> {
    let binding_purpose = parse_binding_purpose(&row.binding_purpose)?;
    let model = catalog::map_model_row(ai_repository::AiModelCatalogRow {
        id: row.model_catalog_id,
        provider_catalog_id: row.model_provider_catalog_id,
        model_name: std::mem::take(&mut row.model_name),
        capability_kind: std::mem::take(&mut row.model_capability_kind),
        modality_kind: std::mem::take(&mut row.model_modality_kind),
        context_window: row.model_context_window,
        max_output_tokens: row.model_max_output_tokens,
        lifecycle_state: std::mem::take(&mut row.model_lifecycle_state),
        metadata_json: std::mem::take(&mut row.model_metadata_json),
    })?;
    let provider = catalog::map_provider_row(ai_repository::AiProviderCatalogRow {
        id: row.provider_catalog_id,
        provider_kind: std::mem::take(&mut row.provider_kind),
        display_name: std::mem::take(&mut row.provider_display_name),
        api_style: std::mem::take(&mut row.provider_api_style),
        lifecycle_state: std::mem::take(&mut row.provider_lifecycle_state),
        default_base_url: row.provider_default_base_url.take(),
        capability_flags_json: std::mem::take(&mut row.provider_capability_flags_json),
    })?;
    validate_runtime_binding_components(
        binding_purpose,
        &model,
        &provider,
        &row.account_credential_state,
        row.account_base_url.as_deref(),
    )?;

    Ok((
        binding_purpose,
        ProviderModelSelection {
            provider_kind: provider.provider_kind,
            model_name: model.model_name,
        },
    ))
}

fn map_effective_runtime_binding_row(
    state: &AppState,
    mut row: ai_repository::EffectiveRuntimeBindingRow,
    requested_purpose: AiBindingPurpose,
) -> Result<ResolvedRuntimeBinding, ApiError> {
    let stored_purpose = parse_binding_purpose(&row.binding_purpose)?;
    let model = catalog::map_model_row(ai_repository::AiModelCatalogRow {
        id: row.model_catalog_id,
        provider_catalog_id: row.model_provider_catalog_id,
        model_name: std::mem::take(&mut row.model_name),
        capability_kind: std::mem::take(&mut row.model_capability_kind),
        modality_kind: std::mem::take(&mut row.model_modality_kind),
        context_window: row.model_context_window,
        max_output_tokens: row.model_max_output_tokens,
        lifecycle_state: std::mem::take(&mut row.model_lifecycle_state),
        metadata_json: std::mem::take(&mut row.model_metadata_json),
    })?;
    let provider = catalog::map_provider_row(ai_repository::AiProviderCatalogRow {
        id: row.provider_catalog_id,
        provider_kind: std::mem::take(&mut row.provider_kind),
        display_name: std::mem::take(&mut row.provider_display_name),
        api_style: std::mem::take(&mut row.provider_api_style),
        lifecycle_state: std::mem::take(&mut row.provider_lifecycle_state),
        default_base_url: row.provider_default_base_url.take(),
        capability_flags_json: std::mem::take(&mut row.provider_capability_flags_json),
    })?;

    let account_base_url = row.account_base_url.take();
    let provider_base_url = validate_runtime_binding_components(
        stored_purpose,
        &model,
        &provider,
        &row.account_credential_state,
        account_base_url.as_deref(),
    )?;
    if requested_purpose != stored_purpose {
        return Err(ApiError::Internal);
    }
    let effective_embedding_dimensions = resolve_effective_embedding_dimensions(
        stored_purpose,
        &row.extra_parameters_json,
        &model.metadata_json,
    )?;
    // A legacy row can still contain plaintext. Move it immediately into a
    // zeroizing owner before decrypting so both legacy and encrypted storage
    // representations are scrubbed when this mapper returns.
    let stored_api_key = row.account_api_key.take().map(Zeroizing::new);
    let api_key = stored_api_key
        .as_ref()
        .map(|stored| {
            state.credential_cipher.decrypt(
                SecretPurpose::AiAccountApiKey,
                row.account_id,
                stored.as_str(),
            )
        })
        .transpose()
        .map_err(ApiError::from_secret_encryption)?
        .map(|secret| secret.expose_secret().to_owned());

    Ok(ResolvedRuntimeBinding {
        binding_id: row.binding_id,
        workspace_id: row.resolved_workspace_id,
        library_id: row.resolved_library_id,
        binding_purpose: requested_purpose,
        provider_catalog_id: provider.id,
        provider_kind: provider.provider_kind.clone(),
        provider_base_url,
        provider_api_style: provider.api_style.clone(),
        account_id: row.account_id,
        api_key,
        model_catalog_id: model.id,
        model_name: model.model_name,
        effective_embedding_dimensions,
        system_prompt: row.system_prompt.take(),
        temperature: row.temperature,
        top_p: row.top_p,
        max_output_tokens_override: row.max_output_tokens_override,
        extra_parameters_json: merge_provider_runtime_profile(
            merge_model_request_policy(
                std::mem::take(&mut row.extra_parameters_json),
                &model.metadata_json,
            ),
            &provider.profile,
        ),
    })
}

fn bootstrap_env_credential_needs_sync(
    cipher: &crate::shared::secret_encryption::CredentialCipher,
    account_id: Uuid,
    stored_api_key: Option<&str>,
    configured_api_key: &str,
) -> Result<bool, ApiError> {
    let Some(stored_api_key) = stored_api_key else {
        return Ok(true);
    };
    let stored_api_key = cipher
        .decrypt(SecretPurpose::AiAccountApiKey, account_id, stored_api_key)
        .map_err(ApiError::from_secret_encryption)?;
    let stored_digest = Sha256::digest(stored_api_key.expose_secret().as_bytes());
    let configured_digest = Sha256::digest(configured_api_key.as_bytes());
    Ok(!bool::from(stored_digest.ct_eq(&configured_digest)))
}

#[derive(Debug, Default)]
struct EnvAccountSyncCounts {
    changed: usize,
    created: usize,
    updated: usize,
    skipped: usize,
}

fn record_env_account_sync_result(
    result: Result<AiAccount, ApiError>,
    provider_kind: &str,
    account_scope: Option<&str>,
    is_creation: bool,
    counts: &mut EnvAccountSyncCounts,
) -> Result<bool, ApiError> {
    match result {
        Ok(_) => {
            counts.changed += 1;
            if is_creation {
                counts.created += 1;
            } else {
                counts.updated += 1;
            }
            Ok(false)
        }
        Err(error) if is_provider_credential_validation_error(&error) => {
            counts.skipped += 1;
            tracing::warn!(
                stage = "bootstrap",
                provider_kind,
                account_scope,
                error = %error,
                "skipped env-keyed provider account synchronization",
            );
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn log_env_account_sync_counts(counts: &EnvAccountSyncCounts) {
    if counts.created > 0 {
        tracing::info!(
            stage = "bootstrap",
            created = counts.created,
            "ensured env-keyed provider accounts",
        );
    }
    if counts.updated > 0 {
        tracing::info!(
            stage = "bootstrap",
            updated = counts.updated,
            "synchronized env-keyed provider accounts",
        );
    }
    if counts.skipped > 0 {
        tracing::warn!(
            stage = "bootstrap",
            skipped = counts.skipped,
            "some env-keyed provider accounts could not be validated",
        );
    }
}

impl AiCatalogService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn list_bindings(
        &self,
        state: &AppState,
        scope: AiScopeRef,
    ) -> Result<Vec<AiBinding>, ApiError> {
        let rows = ai_repository::list_bindings_exact(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_binding_row).collect()
    }

    pub async fn get_binding(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<AiBinding, ApiError> {
        let row = ai_repository::get_binding_by_id(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("binding_assignment", binding_id))?;
        map_binding_row(row)
    }

    pub async fn create_binding(
        &self,
        state: &AppState,
        command: CreateAiBindingCommand,
    ) -> Result<AiBinding, ApiError> {
        validate_binding_request_policy(&command.extra_parameters_json)?;
        let scope = normalize_scope_ref(
            state,
            command.scope_kind,
            command.workspace_id,
            command.library_id,
        )
        .await?;
        self.validate_binding_target_for_scope(
            state,
            scope,
            command.binding_purpose,
            command.account_id,
            command.model_catalog_id,
            &command.extra_parameters_json,
        )
        .await?;
        let repository_input = ai_repository::CreateAiBindingInput {
            scope_kind: scope_kind_key(scope.scope_kind),
            workspace_id: scope.workspace_id,
            library_id: scope.library_id,
            binding_purpose: binding_purpose_key(command.binding_purpose),
            account_id: command.account_id,
            model_catalog_id: command.model_catalog_id,
            system_prompt: command.system_prompt.as_deref(),
            temperature: command.temperature,
            top_p: command.top_p,
            max_output_tokens_override: command.max_output_tokens_override,
            extra_parameters_json: command.extra_parameters_json.clone(),
            updated_by_principal_id: command.updated_by_principal_id,
        };
        let row = ai_repository::create_binding(&state.persistence.postgres, repository_input)
            .await
            .map_err(map_ai_write_error)?;
        invalidate_vector_dimension_cache_for_binding(command.binding_purpose);
        map_binding_row(row)
    }

    pub async fn update_binding(
        &self,
        state: &AppState,
        command: UpdateAiBindingCommand,
    ) -> Result<AiBinding, ApiError> {
        let existing =
            ai_repository::get_binding_by_id(&state.persistence.postgres, command.binding_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                    ApiError::resource_not_found("binding_assignment", command.binding_id)
                })?;
        let scope = scope_ref_from_binding_row(&existing)?;
        let binding_purpose = parse_binding_purpose(&existing.binding_purpose)?;
        validate_binding_request_policy(&command.extra_parameters_json)?;
        self.validate_binding_target_for_scope(
            state,
            scope,
            binding_purpose,
            command.account_id,
            command.model_catalog_id,
            &command.extra_parameters_json,
        )
        .await?;
        let row = ai_repository::update_binding(
            &state.persistence.postgres,
            command.binding_id,
            command.account_id,
            command.model_catalog_id,
            command.system_prompt.as_deref(),
            command.temperature,
            command.top_p,
            command.max_output_tokens_override,
            command.extra_parameters_json.clone(),
            &command.binding_state,
            command.updated_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("binding_assignment", command.binding_id))?;
        invalidate_vector_dimension_cache_for_binding(binding_purpose);
        map_binding_row(row)
    }

    pub async fn delete_binding(&self, state: &AppState, binding_id: Uuid) -> Result<(), ApiError> {
        let existing = ai_repository::get_binding_by_id(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("binding_assignment", binding_id))?;
        let binding_purpose = parse_binding_purpose(&existing.binding_purpose)?;
        let deleted = ai_repository::delete_binding(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !deleted {
            return Err(ApiError::resource_not_found("binding_assignment", binding_id));
        }
        invalidate_vector_dimension_cache_for_binding(binding_purpose);
        Ok(())
    }

    pub async fn describe_bootstrap_ai_setup(
        &self,
        state: &AppState,
    ) -> Result<BootstrapAiSetupDescriptor, ApiError> {
        let (providers, models) = tokio::try_join!(
            self.list_provider_catalog(state),
            self.list_model_catalog(state, None),
        )?;
        // Bootstrap provider credentials are read only while this operation
        // needs them; AppState never retains another plaintext copy.
        let configured_ai = bootstrap_ai_setup_for_operation(state);
        let mut binding_bundles = Vec::new();
        for provider in &providers {
            if let Some(bundle) = resolve_bootstrap_provider_binding_bundle(
                provider,
                &providers,
                &models,
                bootstrap_credential_source(configured_ai.as_ref(), &provider.provider_kind),
            )? {
                if !bootstrap_bundle_is_self_contained(&bundle) {
                    continue;
                }
                binding_bundles.push(bundle);
            }
        }
        binding_bundles.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.provider_kind.cmp(&right.provider_kind))
        });

        Ok(BootstrapAiSetupDescriptor { binding_bundles })
    }

    pub async fn ensure_bootstrap_provider_bundle_available(
        &self,
        state: &AppState,
        provider_kind: &str,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        resolve_bootstrap_provider_bundle(&providers, &models, provider_kind)?;
        Ok(())
    }

    pub async fn apply_bootstrap_provider_binding_bundle(
        &self,
        state: &AppState,
        mut command: ApplyBootstrapProviderBindingBundleCommand,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let bundle =
            resolve_bootstrap_provider_bundle(&providers, &models, &command.provider_kind)?;
        let binding_inputs = bundle
            .bindings
            .into_iter()
            .map(|binding| BootstrapAiBindingInput {
                binding_purpose: binding.binding_purpose,
                provider_kind: binding.owner_provider_kind,
                model_catalog_id: binding.model_catalog_id,
                system_prompt: binding.system_prompt,
                temperature: binding.temperature,
                top_p: binding.top_p,
                max_output_tokens_override: binding.max_output_tokens_override,
                extra_parameters_json: binding.extra_parameters_json,
            })
            .collect();
        self.apply_bootstrap_ai_setup(
            state,
            ApplyBootstrapAiSetupCommand {
                credentials: vec![BootstrapAiCredentialInput {
                    provider_kind: bundle.provider_kind,
                    api_key: command.api_key.take(),
                    base_url: command.base_url.take(),
                }],
                binding_inputs,
                updated_by_principal_id: command.updated_by_principal_id,
            },
        )
        .await
    }

    /// Idempotently ensure every env-keyed provider has a canonical
    /// `Bootstrap <DisplayName>` account.
    ///
    /// Runs independent of binding selection and creates one account
    /// per env-keyed provider. Existing canonical rows in any scope are
    /// synchronized when the configured key changes so restored or copied
    /// bindings cannot retain a stale env-managed credential. Operator-managed
    /// state and base URL remain unchanged. Returns the number of created or
    /// updated rows.
    pub async fn ensure_env_ai_accounts(&self, state: &AppState) -> Result<usize, ApiError> {
        if !state.settings.credential_encryption_write_enabled {
            tracing::info!(
                stage = "bootstrap",
                "deferred env-keyed provider account synchronization while encrypted writes are disabled",
            );
            return Ok(0);
        }
        let Some(configured_ai) = bootstrap_ai_setup_for_operation(state) else {
            return Ok(0);
        };
        if configured_ai.provider_secrets.is_empty() {
            return Ok(0);
        }
        let providers = self.list_provider_catalog(state).await?;
        let mut counts = EnvAccountSyncCounts::default();
        for secret in &configured_ai.provider_secrets {
            let Some(provider) = providers.iter().find(|p| p.provider_kind == secret.provider_kind)
            else {
                continue;
            };
            let label = format!("Bootstrap {}", provider.display_name);
            let existing_accounts = ai_repository::list_accounts_by_provider_and_label(
                &state.persistence.postgres,
                provider.id,
                &label,
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
            let has_instance_account =
                existing_accounts.iter().any(|account| account.scope_kind == "instance");
            if !has_instance_account {
                let result = self
                    .create_account(
                        state,
                        CreateAiAccountCommand {
                            scope_kind: AiScopeKind::Instance,
                            workspace_id: None,
                            library_id: None,
                            provider_catalog_id: provider.id,
                            label: label.clone(),
                            api_key: Some(secret.api_key.clone()),
                            base_url: None,
                            created_by_principal_id: None,
                        },
                    )
                    .await;
                let should_skip_provider = record_env_account_sync_result(
                    result,
                    &secret.provider_kind,
                    None,
                    true,
                    &mut counts,
                )?;
                if should_skip_provider {
                    continue;
                }
            }
            for mut account in existing_accounts {
                if !bootstrap_env_credential_needs_sync(
                    &state.credential_cipher,
                    account.id,
                    account.api_key.as_deref(),
                    &secret.api_key,
                )? {
                    continue;
                }
                let result = self
                    .update_account(
                        state,
                        UpdateAiAccountCommand {
                            account_id: account.id,
                            label: label.clone(),
                            api_key: Some(secret.api_key.clone()),
                            base_url: account.base_url.take(),
                            credential_state: std::mem::take(&mut account.credential_state),
                        },
                    )
                    .await;
                record_env_account_sync_result(
                    result,
                    &secret.provider_kind,
                    Some(&account.scope_kind),
                    false,
                    &mut counts,
                )?;
            }
        }
        log_env_account_sync_counts(&counts);
        Ok(counts.changed)
    }

    pub async fn apply_configured_bootstrap_ai_setup(
        &self,
        state: &AppState,
        _workspace_id: Uuid,
        _library_id: Uuid,
        updated_by_principal_id: Option<Uuid>,
    ) -> Result<bool, ApiError> {
        let Some(configured_ai) = bootstrap_ai_setup_for_operation(state) else {
            return Ok(false);
        };
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let binding_inputs =
            resolve_configured_bootstrap_binding_inputs(&configured_ai, &providers, &models)?;
        if binding_inputs.is_empty()
            || !bootstrap_binding_inputs_cover_required_purposes(&binding_inputs)
        {
            return Ok(false);
        }
        self.apply_bootstrap_ai_setup(
            state,
            ApplyBootstrapAiSetupCommand {
                credentials: configured_ai
                    .provider_secrets
                    .iter()
                    .map(|secret| BootstrapAiCredentialInput {
                        provider_kind: secret.provider_kind.clone(),
                        api_key: Some(secret.api_key.clone()),
                        base_url: None,
                    })
                    .collect(),
                binding_inputs,
                updated_by_principal_id,
            },
        )
        .await?;
        Ok(true)
    }

    pub async fn apply_bootstrap_ai_setup(
        &self,
        state: &AppState,
        command: ApplyBootstrapAiSetupCommand,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let binding_inputs =
            normalize_bootstrap_binding_inputs(&command.binding_inputs, &providers, &models)?;
        validate_bootstrap_binding_inputs_cover_required_purposes(&binding_inputs)?;

        for input in &binding_inputs {
            tracing::info!(stage = "bootstrap", provider_kind = %input.provider_kind, "AI provider selected for bootstrap");
        }

        let instance_scope =
            AiScopeRef { scope_kind: AiScopeKind::Instance, workspace_id: None, library_id: None };
        let existing_accounts = self.list_accounts_exact(state, instance_scope).await?;
        let configured_ai = bootstrap_ai_setup_for_operation(state);
        let provider_credentials =
            bootstrap_provider_credential_map(configured_ai.as_ref(), &command.credentials);
        let mut accounts_by_provider = std::collections::HashMap::new();
        for provider_kind in binding_inputs.iter().map(|selection| selection.provider_kind.as_str())
        {
            let provider =
                providers.iter().find(|entry| entry.provider_kind == provider_kind).ok_or_else(
                    || ApiError::resource_not_found("provider_catalog", provider_kind.to_string()),
                )?;
            let account = ensure_bootstrap_provider_account(
                self,
                state,
                provider,
                provider_credentials.get(provider_kind).cloned(),
                &existing_accounts,
                command.updated_by_principal_id,
            )
            .await?;
            // Clone required: the HashMap outlives the `providers` borrow used later.
            accounts_by_provider.insert(provider.provider_kind.clone(), account);
        }

        for (provider_kind, account) in &accounts_by_provider {
            let provider =
                providers.iter().find(|entry| entry.provider_kind == *provider_kind).ok_or_else(
                    || ApiError::resource_not_found("provider_catalog", provider_kind.clone()),
                )?;
            validate_bootstrap_model_list_binding_inputs(
                provider,
                account,
                &binding_inputs,
                &models,
            )
            .await?;
        }

        let mut bindings = self.list_bindings(state, instance_scope).await?;
        for selection in &binding_inputs {
            let provider = providers
                .iter()
                .find(|entry| entry.provider_kind == selection.provider_kind)
                .ok_or_else(|| {
                    ApiError::resource_not_found(
                        "provider_catalog",
                        selection.provider_kind.clone(),
                    )
                })?;
            let model =
                models.iter().find(|entry| entry.id == selection.model_catalog_id).ok_or_else(
                    || ApiError::resource_not_found("model_catalog", selection.model_catalog_id),
                )?;
            validate_model_binding_purpose(selection.binding_purpose, model)?;
            validate_provider_capability_for_binding(provider, selection.binding_purpose)?;
            if model.provider_catalog_id != provider.id {
                return Err(ApiError::BadRequest(
                    "bootstrap model selection must belong to the selected provider".to_string(),
                ));
            }
            let account_id = accounts_by_provider
                .get(&selection.provider_kind)
                .map(|account| account.id)
                .ok_or_else(|| {
                    ApiError::BadRequest("bootstrap account was not created".to_string())
                })?;
            ensure_bootstrap_binding(
                self,
                state,
                selection,
                account_id,
                &mut bindings,
                command.updated_by_principal_id,
            )
            .await?;
        }

        tracing::info!(
            stage = "bootstrap",
            bindings_count = binding_inputs.len(),
            "bootstrap bundle applied"
        );

        Ok(())
    }

    pub async fn validate_binding(
        &self,
        state: &AppState,
        command: CreateBindingValidationCommand,
    ) -> Result<BindingValidation, ApiError> {
        let row = ai_repository::create_binding_validation(
            &state.persistence.postgres,
            command.binding_id,
            &command.validation_state,
            normalize_optional(command.failure_code.as_deref()).as_deref(),
            normalize_optional(command.message.as_deref()).as_deref(),
        )
        .await
        .map_err(map_ai_write_error)?;
        Ok(map_binding_validation_row(row))
    }

    pub async fn get_binding_validation(
        &self,
        state: &AppState,
        validation_id: Uuid,
    ) -> Result<BindingValidation, ApiError> {
        let row =
            ai_repository::get_binding_validation_by_id(&state.persistence.postgres, validation_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("binding_validation", validation_id))?;
        Ok(map_binding_validation_row(row))
    }

    pub async fn list_binding_validations(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<Vec<BindingValidation>, ApiError> {
        let rows = ai_repository::list_binding_validations(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_binding_validation_row).collect())
    }

    pub async fn resolve_active_runtime_binding(
        &self,
        state: &AppState,
        library_id: Uuid,
        binding_purpose: AiBindingPurpose,
    ) -> Result<Option<ResolvedRuntimeBinding>, ApiError> {
        let mut rows = ai_repository::list_effective_runtime_bindings(
            &state.persistence.postgres,
            library_id,
            &[binding_purpose.as_str().to_string()],
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let Some(row) = rows.pop() else {
            return Ok(None);
        };
        if !rows.is_empty() {
            return Err(ApiError::Internal);
        }
        let binding = map_effective_runtime_binding_row(state, row, binding_purpose)?;
        Ok(Some(binding))
    }

    /// Resolves a typed provider/model profile with one secret-free statement.
    ///
    /// Duplicate purposes are ignored after their first appearance. Missing
    /// selections are absent from the returned map, allowing callers to
    /// enforce their own required/optional policy. Account credentials and
    /// binding prompts are not selected. An empty input performs no I/O.
    pub async fn resolve_effective_provider_selections(
        &self,
        state: &AppState,
        library_id: Uuid,
        binding_purposes: &[AiBindingPurpose],
    ) -> Result<HashMap<AiBindingPurpose, ProviderModelSelection>, ApiError> {
        let binding_purposes = deduplicate_binding_purposes(binding_purposes);
        if binding_purposes.is_empty() {
            return Ok(HashMap::new());
        }
        let binding_purpose_keys =
            binding_purposes.iter().map(|purpose| purpose.as_str().to_string()).collect::<Vec<_>>();
        let rows = ai_repository::list_effective_provider_selections(
            &state.persistence.postgres,
            library_id,
            &binding_purpose_keys,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

        let requested = binding_purposes.iter().copied().collect::<HashSet<_>>();
        let mut selections = HashMap::with_capacity(rows.len());
        for row in rows {
            let (binding_purpose, selection) = map_effective_provider_selection_row(row)?;
            if !requested.contains(&binding_purpose)
                || selections.insert(binding_purpose, selection).is_some()
            {
                return Err(ApiError::Internal);
            }
        }
        Ok(selections)
    }

    pub async fn resolve_runtime_binding_by_id(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<ResolvedRuntimeBinding, ApiError> {
        let binding = ai_repository::get_binding_by_id(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("library_binding", binding_id))?;
        let workspace_id = binding.workspace_id.unwrap_or_else(Uuid::nil);
        let library_id = binding.library_id.unwrap_or_else(Uuid::nil);
        self.resolve_runtime_binding_by_row(state, binding, workspace_id, library_id).await
    }

    async fn resolve_runtime_binding_by_row(
        &self,
        state: &AppState,
        binding: ai_repository::AiBindingRow,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> Result<ResolvedRuntimeBinding, ApiError> {
        // The account and the model are both independent of each other —
        // the binding row already carries both foreign keys inline — so we
        // resolve them in parallel. The provider depends on the resolved
        // account, so it is fetched afterward.
        let (account_result, model_result) = tokio::join!(
            self.get_account(state, binding.account_id),
            self.get_model_catalog(state, binding.model_catalog_id),
        );
        let mut account = account_result?;
        let model = model_result?;
        let binding_purpose = parse_binding_purpose(&binding.binding_purpose)?;
        let provider = self.get_provider_catalog(state, account.provider_catalog_id).await?;
        if model.provider_catalog_id != provider.id {
            return Err(ApiError::BadRequest(
                "binding links a provider account to a model from another provider".to_string(),
            ));
        }
        if account.credential_state != "active" {
            return Err(ApiError::BadRequest("provider account is not active".to_string()));
        }
        validate_model_binding_purpose(binding_purpose, &model)?;
        validate_provider_capability_for_binding(&provider, binding_purpose)?;
        let effective_embedding_dimensions = resolve_effective_embedding_dimensions(
            binding_purpose,
            &binding.extra_parameters_json,
            &model.metadata_json,
        )?;

        Ok(ResolvedRuntimeBinding {
            binding_id: binding.id,
            workspace_id,
            library_id,
            binding_purpose,
            provider_catalog_id: provider.id,
            provider_kind: provider.provider_kind.clone(),
            provider_base_url: runtime_provider_base_url(&provider, account.base_url.as_deref())?,
            provider_api_style: provider.api_style.clone(),
            account_id: account.id,
            api_key: account.api_key.take(),
            model_catalog_id: model.id,
            model_name: model.model_name,
            effective_embedding_dimensions,
            system_prompt: binding.system_prompt,
            temperature: binding.temperature,
            top_p: binding.top_p,
            max_output_tokens_override: binding.max_output_tokens_override,
            extra_parameters_json: merge_provider_runtime_profile(
                merge_model_request_policy(binding.extra_parameters_json, &model.metadata_json),
                &provider.profile,
            ),
        })
    }

    async fn validate_binding_target_for_scope(
        &self,
        state: &AppState,
        scope: AiScopeRef,
        binding_purpose: AiBindingPurpose,
        account_id: Uuid,
        model_catalog_id: Uuid,
        extra_parameters_json: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let account = self.get_account(state, account_id).await?;
        validate_binding_account_scope(scope, &account)?;

        let (provider, model) = tokio::try_join!(
            self.get_provider_catalog(state, account.provider_catalog_id),
            self.get_model_catalog(state, model_catalog_id),
        )?;
        validate_binding_target_components(
            binding_purpose,
            extra_parameters_json,
            &account,
            &model,
            &provider,
        )
    }
}
