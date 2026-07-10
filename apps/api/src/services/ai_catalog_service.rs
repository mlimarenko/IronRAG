#![allow(
    clippy::iter_without_into_iter,
    clippy::missing_errors_doc,
    clippy::result_large_err,
    clippy::too_many_lines
)]

use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

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
        ProviderModelDiscoveryMode, ProviderProfile, ProviderRuntimeProfile,
    },
    infra::repositories::{ai_repository, catalog_repository},
    integrations::llm::ChatRequestSeed,
    interfaces::http::router_support::ApiError,
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
#[cfg(test)]
use catalog::parse_allowed_binding_purposes;
use catalog::validate_model_binding_purpose;
#[cfg(test)]
use provider_validation::{
    canonicalize_provider_base_url, discovered_provider_model_signature_for_capability,
    is_loopback_base_url,
};
use provider_validation::{
    is_provider_credential_validation_error, provider_credential_base_url_for_create,
    provider_credential_base_url_for_update, runtime_provider_base_url, validate_provider_access,
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct UpdateAiAccountCommand {
    pub account_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub credential_state: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapAiCredentialInput {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
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

#[derive(Debug, Clone)]
pub struct ApplyBootstrapAiSetupCommand {
    pub credentials: Vec<BootstrapAiCredentialInput>,
    pub binding_inputs: Vec<BootstrapAiBindingInput>,
    pub updated_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct ApplyBootstrapProviderBindingBundleCommand {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub updated_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
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
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

impl ResolvedRuntimeBinding {
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

#[derive(Clone, Default)]
pub struct AiCatalogService;

const CANONICAL_REQUIRED_RUNTIME_BINDING_PURPOSES: [AiBindingPurpose; 6] = [
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::EmbedChunk,
    AiBindingPurpose::QueryRetrieve,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Agent,
];

fn provider_credential_policy(provider: &ProviderCatalogEntry) -> &ProviderCredentialPolicy {
    &provider.credential_policy
}

fn provider_runtime_profile_json(profile: &ProviderProfile) -> serde_json::Value {
    serde_json::json!({
        "runtime": profile.runtime,
        "baseUrl": profile.base_url,
        "credentials": profile.credentials,
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

fn provider_capability_for_binding(
    provider: &ProviderCatalogEntry,
    binding_purpose: AiBindingPurpose,
) -> ProviderCapabilityState {
    match binding_purpose {
        AiBindingPurpose::ExtractText
        | AiBindingPurpose::ExtractGraph
        | AiBindingPurpose::QueryCompile
        | AiBindingPurpose::QueryAnswer
        | AiBindingPurpose::Agent => provider.capabilities.chat,
        AiBindingPurpose::EmbedChunk | AiBindingPurpose::QueryRetrieve => {
            provider.capabilities.embeddings
        }
        AiBindingPurpose::Vision => provider.capabilities.vision,
    }
}

fn vector_index_counterpart_purpose(binding_purpose: AiBindingPurpose) -> Option<AiBindingPurpose> {
    match binding_purpose {
        AiBindingPurpose::EmbedChunk => Some(AiBindingPurpose::QueryRetrieve),
        AiBindingPurpose::QueryRetrieve => Some(AiBindingPurpose::EmbedChunk),
        _ => None,
    }
}

fn invalidate_vector_dimension_cache_for_binding(binding_purpose: AiBindingPurpose) {
    if vector_index_counterpart_purpose(binding_purpose).is_some() {
        crate::services::query::vector_dimensions::invalidate_vector_index_dimension_cache();
    }
}

fn validate_provider_capability_for_binding(
    provider: &ProviderCatalogEntry,
    binding_purpose: AiBindingPurpose,
) -> Result<(), ApiError> {
    let capability = provider_capability_for_binding(provider, binding_purpose);
    if capability.is_supported() {
        return Ok(());
    }
    Err(ApiError::BadRequest(format!(
        "provider {} does not declare supported capability for binding purpose {}: {:?}",
        provider.provider_kind,
        binding_purpose.as_str(),
        capability
    )))
}

fn bootstrap_env_credential_needs_sync(
    stored_api_key: Option<&str>,
    _credential_state: &str,
    configured_api_key: &str,
) -> bool {
    stored_api_key != Some(configured_api_key)
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
            "active",
        )
        .await?;
        let row = ai_repository::create_binding(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
            binding_purpose_key(command.binding_purpose),
            command.account_id,
            command.model_catalog_id,
            command.system_prompt.as_deref(),
            command.temperature,
            command.top_p,
            command.max_output_tokens_override,
            command.extra_parameters_json.clone(),
            command.updated_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?;
        self.sync_vector_counterpart_binding(
            state,
            scope,
            command.binding_purpose,
            command.account_id,
            command.model_catalog_id,
            command.system_prompt.as_deref(),
            command.temperature,
            command.top_p,
            command.max_output_tokens_override,
            &command.extra_parameters_json,
            command.updated_by_principal_id,
        )
        .await?;
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
        self.validate_binding_target_for_scope(
            state,
            scope,
            binding_purpose,
            command.account_id,
            command.model_catalog_id,
            &command.binding_state,
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
        if command.binding_state == "active" {
            self.sync_vector_counterpart_binding(
                state,
                scope,
                binding_purpose,
                command.account_id,
                command.model_catalog_id,
                command.system_prompt.as_deref(),
                command.temperature,
                command.top_p,
                command.max_output_tokens_override,
                &command.extra_parameters_json,
                command.updated_by_principal_id,
            )
            .await?;
        } else {
            self.deactivate_vector_counterpart_binding(
                state,
                scope,
                binding_purpose,
                command.updated_by_principal_id,
            )
            .await?;
        }
        invalidate_vector_dimension_cache_for_binding(binding_purpose);
        map_binding_row(row)
    }

    #[allow(clippy::too_many_arguments)]
    async fn sync_vector_counterpart_binding(
        &self,
        state: &AppState,
        scope: AiScopeRef,
        binding_purpose: AiBindingPurpose,
        account_id: Uuid,
        model_catalog_id: Uuid,
        system_prompt: Option<&str>,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_output_tokens_override: Option<i32>,
        extra_parameters_json: &serde_json::Value,
        updated_by_principal_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let Some(counterpart_purpose) = vector_index_counterpart_purpose(binding_purpose) else {
            return Ok(());
        };
        self.validate_binding_target_for_scope(
            state,
            scope,
            counterpart_purpose,
            account_id,
            model_catalog_id,
            "active",
        )
        .await?;
        let counterpart_key = binding_purpose_key(counterpart_purpose);
        let existing = ai_repository::list_bindings_exact(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .into_iter()
        .find(|row| row.binding_purpose == counterpart_key);

        if let Some(existing) = existing {
            if existing.account_id == account_id
                && existing.model_catalog_id == model_catalog_id
                && existing.system_prompt.as_deref() == system_prompt
                && existing.temperature == temperature
                && existing.top_p == top_p
                && existing.max_output_tokens_override == max_output_tokens_override
                && &existing.extra_parameters_json == extra_parameters_json
                && existing.binding_state == "active"
            {
                return Ok(());
            }
            ai_repository::update_binding(
                &state.persistence.postgres,
                existing.id,
                account_id,
                model_catalog_id,
                system_prompt,
                temperature,
                top_p,
                max_output_tokens_override,
                extra_parameters_json.clone(),
                "active",
                updated_by_principal_id,
            )
            .await
            .map_err(map_ai_write_error)?;
        } else {
            ai_repository::create_binding(
                &state.persistence.postgres,
                scope_kind_key(scope.scope_kind),
                scope.workspace_id,
                scope.library_id,
                counterpart_key,
                account_id,
                model_catalog_id,
                system_prompt,
                temperature,
                top_p,
                max_output_tokens_override,
                extra_parameters_json.clone(),
                updated_by_principal_id,
            )
            .await
            .map_err(map_ai_write_error)?;
        }
        Ok(())
    }

    async fn exact_vector_counterpart_binding(
        &self,
        state: &AppState,
        scope: AiScopeRef,
        binding_purpose: AiBindingPurpose,
    ) -> Result<Option<ai_repository::AiBindingRow>, ApiError> {
        let Some(counterpart_purpose) = vector_index_counterpart_purpose(binding_purpose) else {
            return Ok(None);
        };
        let counterpart_key = binding_purpose_key(counterpart_purpose);
        Ok(ai_repository::list_bindings_exact(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .into_iter()
        .find(|row| row.binding_purpose == counterpart_key))
    }

    async fn deactivate_vector_counterpart_binding(
        &self,
        state: &AppState,
        scope: AiScopeRef,
        binding_purpose: AiBindingPurpose,
        updated_by_principal_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let Some(existing) =
            self.exact_vector_counterpart_binding(state, scope, binding_purpose).await?
        else {
            return Ok(());
        };
        if existing.binding_state != "active" {
            return Ok(());
        }
        ai_repository::update_binding(
            &state.persistence.postgres,
            existing.id,
            existing.account_id,
            existing.model_catalog_id,
            existing.system_prompt.as_deref(),
            existing.temperature,
            existing.top_p,
            existing.max_output_tokens_override,
            existing.extra_parameters_json.clone(),
            "inactive",
            updated_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?;
        Ok(())
    }

    pub async fn delete_binding(&self, state: &AppState, binding_id: Uuid) -> Result<(), ApiError> {
        let existing = ai_repository::get_binding_by_id(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("binding_assignment", binding_id))?;
        let scope = scope_ref_from_binding_row(&existing)?;
        let binding_purpose = parse_binding_purpose(&existing.binding_purpose)?;
        let counterpart =
            self.exact_vector_counterpart_binding(state, scope, binding_purpose).await?;
        let deleted = ai_repository::delete_binding(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !deleted {
            return Err(ApiError::resource_not_found("binding_assignment", binding_id));
        }
        if let Some(counterpart) = counterpart {
            let _ = ai_repository::delete_binding(&state.persistence.postgres, counterpart.id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
        let configured_ai = state.ui_bootstrap_ai_setup.as_ref();
        let mut binding_bundles = Vec::new();
        for provider in &providers {
            if let Some(bundle) = resolve_bootstrap_provider_binding_bundle(
                provider,
                &providers,
                &models,
                bootstrap_credential_source(configured_ai, &provider.provider_kind),
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
        command: ApplyBootstrapProviderBindingBundleCommand,
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
                    api_key: command.api_key,
                    base_url: command.base_url,
                }],
                binding_inputs,
                updated_by_principal_id: command.updated_by_principal_id,
            },
        )
        .await
    }

    /// Idempotently ensure every env-keyed provider has a canonical
    /// "Bootstrap <DisplayName>" account.
    ///
    /// Runs independent of binding selection and creates one account
    /// per env-keyed provider. Existing canonical rows in any scope are
    /// synchronized when the configured key changes so restored or copied
    /// bindings cannot retain a stale env-managed credential. Operator-managed
    /// state and base URL remain unchanged. Returns the number of created or
    /// updated rows.
    pub async fn ensure_env_ai_accounts(&self, state: &AppState) -> Result<usize, ApiError> {
        let Some(configured_ai) = state.ui_bootstrap_ai_setup.as_ref() else {
            return Ok(0);
        };
        if configured_ai.provider_secrets.is_empty() {
            return Ok(0);
        }
        let providers = self.list_provider_catalog(state).await?;
        let mut changed = 0usize;
        let mut created = 0usize;
        let mut updated = 0usize;
        let mut skipped = 0usize;
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
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .into_iter()
            .map(accounts::map_account_row)
            .collect::<Vec<_>>();
            let has_instance_account =
                existing_accounts.iter().any(|account| account.scope_kind == AiScopeKind::Instance);
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
                match result {
                    Ok(_) => {
                        changed += 1;
                        created += 1;
                    }
                    Err(error) if is_provider_credential_validation_error(&error) => {
                        skipped += 1;
                        tracing::warn!(
                            stage = "bootstrap",
                            provider_kind = %secret.provider_kind,
                            error = %error,
                            "skipped env-keyed provider account",
                        );
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            for account in existing_accounts {
                if !bootstrap_env_credential_needs_sync(
                    account.api_key.as_deref(),
                    &account.credential_state,
                    &secret.api_key,
                ) {
                    continue;
                }
                let result = self
                    .update_account(
                        state,
                        UpdateAiAccountCommand {
                            account_id: account.id,
                            label: label.clone(),
                            api_key: Some(secret.api_key.clone()),
                            base_url: account.base_url,
                            credential_state: account.credential_state,
                        },
                    )
                    .await;
                match result {
                    Ok(_) => {
                        changed += 1;
                        updated += 1;
                    }
                    Err(error) if is_provider_credential_validation_error(&error) => {
                        skipped += 1;
                        tracing::warn!(
                            stage = "bootstrap",
                            provider_kind = %secret.provider_kind,
                            account_scope = %scope_kind_key(account.scope_kind),
                            error = %error,
                            "skipped env-keyed provider account synchronization",
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if created > 0 {
            tracing::info!(stage = "bootstrap", created, "ensured env-keyed provider accounts",);
        }
        if updated > 0 {
            tracing::info!(
                stage = "bootstrap",
                updated,
                "synchronized env-keyed provider accounts",
            );
        }
        if skipped > 0 {
            tracing::warn!(
                stage = "bootstrap",
                skipped,
                "some env-keyed provider accounts could not be validated",
            );
        }
        Ok(changed)
    }

    pub async fn apply_configured_bootstrap_ai_setup(
        &self,
        state: &AppState,
        _workspace_id: Uuid,
        _library_id: Uuid,
        updated_by_principal_id: Option<Uuid>,
    ) -> Result<bool, ApiError> {
        let Some(configured_ai) = state.ui_bootstrap_ai_setup.as_ref() else {
            return Ok(false);
        };
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let binding_inputs =
            resolve_configured_bootstrap_binding_inputs(configured_ai, &providers, &models)?;
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
        let provider_credentials = bootstrap_provider_credential_map(
            state.ui_bootstrap_ai_setup.as_ref(),
            &command.credentials,
        );
        let mut accounts_by_provider = std::collections::HashMap::new();
        for provider_kind in binding_inputs
            .iter()
            .map(|selection| selection.provider_kind.as_str())
            .collect::<std::collections::BTreeSet<_>>()
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

    pub async fn resolve_active_runtime_binding(
        &self,
        state: &AppState,
        library_id: Uuid,
        binding_purpose: AiBindingPurpose,
    ) -> Result<Option<ResolvedRuntimeBinding>, ApiError> {
        // Fan out the two independent PG lookups. The library row and the
        // effective binding both key off `library_id` only — neither feeds
        // the other — so running them sequentially used to pay 2× round-trips
        // for no reason.
        let (library_result, binding_result) = tokio::join!(
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id),
            ai_repository::get_effective_binding_by_purpose(
                &state.persistence.postgres,
                library_id,
                binding_purpose_key(binding_purpose),
            ),
        );
        let Some(library) =
            library_result.map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };
        let Some(binding) =
            binding_result.map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };

        self.resolve_runtime_binding_by_row(state, binding, library.workspace_id, library.id)
            .await
            .map(Some)
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
        let account = account_result?;
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
            api_key: account.api_key,
            model_catalog_id: model.id,
            model_name: model.model_name,
            system_prompt: binding.system_prompt,
            temperature: binding.temperature,
            top_p: binding.top_p,
            max_output_tokens_override: binding.max_output_tokens_override,
            extra_parameters_json: merge_provider_runtime_profile(
                binding.extra_parameters_json,
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
        binding_state: &str,
    ) -> Result<(), ApiError> {
        let account = self.get_account(state, account_id).await?;
        let account_scope = scope_ref_from_account(&account)?;
        if !scope_can_use_resource(scope, account_scope) {
            return Err(ApiError::BadRequest(
                "binding cannot use a provider account from an unrelated scope".to_string(),
            ));
        }

        let provider = self.get_provider_catalog(state, account.provider_catalog_id).await?;
        let model = self.get_model_catalog(state, model_catalog_id).await?;

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
        if binding_state == "active"
            && let Some(counterpart_purpose) = vector_index_counterpart_purpose(binding_purpose)
        {
            validate_model_binding_purpose(counterpart_purpose, &model)?;
            validate_provider_capability_for_binding(&provider, counterpart_purpose)?;
        }
        Ok(())
    }
}
