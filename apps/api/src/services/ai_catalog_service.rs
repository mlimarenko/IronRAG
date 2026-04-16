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
        AiBindingAssignment, AiBindingPurpose, AiScopeKind, BindingValidation,
        ModelAvailabilityState, ModelCatalogEntry, ModelPreset, PriceCatalogEntry,
        ProviderCatalogEntry, ProviderCredential, ResolvedModelCatalogEntry,
    },
    infra::repositories::{ai_repository, catalog_repository},
    interfaces::http::router_support::ApiError,
};

mod bootstrap;
mod catalog;
mod credentials;
mod presets;
mod provider_validation;
mod shared;
#[cfg(test)]
mod tests;

use bootstrap::{
    BOOTSTRAPPED_PROVIDER_KINDS, bootstrap_bundle_has_required_fallback_credentials,
    bootstrap_credential_source, bootstrap_preset_inputs_cover_canonical_purposes,
    bootstrap_preset_profile_for_purpose, bootstrap_provider_credential_map,
    ensure_bootstrap_binding_assignment, ensure_bootstrap_model_preset,
    ensure_bootstrap_provider_credential, normalize_bootstrap_preset_inputs,
    resolve_bootstrap_provider_bundle, resolve_bootstrap_provider_preset_bundle,
    resolve_configured_bootstrap_preset_inputs, validate_bootstrap_preset_inputs_complete,
};
#[cfg(test)]
use catalog::parse_allowed_binding_purposes;
use catalog::validate_model_binding_purpose;
#[cfg(test)]
use provider_validation::{
    canonicalize_provider_base_url, discovered_provider_model_signature, is_loopback_base_url,
};
use provider_validation::{
    normalize_provider_base_url_input, resolve_provider_base_url, validate_provider_access,
};
use shared::{
    binding_purpose_key, canonical_runtime_preset_name, map_ai_write_error,
    map_binding_assignment_row, map_binding_validation_row, normalize_non_empty,
    normalize_optional, normalize_scope_ref, parse_binding_purpose, parse_scope_kind,
    scope_can_use_resource, scope_kind_key, scope_ref_from_binding_row,
    scope_ref_from_model_preset, scope_ref_from_provider_credential, select_runtime_preset,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiScopeRef {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct CreateProviderCredentialCommand {
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
pub struct UpdateProviderCredentialCommand {
    pub credential_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub credential_state: String,
}

#[derive(Debug, Clone)]
pub struct CreateModelPresetCommand {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub model_catalog_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateModelPresetCommand {
    pub preset_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct CreateBindingAssignmentCommand {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: AiBindingPurpose,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
    pub updated_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateBindingAssignmentCommand {
    pub binding_id: Uuid,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
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
pub struct BootstrapAiPresetDescriptor {
    pub binding_purpose: AiBindingPurpose,
    pub owner_provider_catalog_id: Uuid,
    pub owner_provider_kind: String,
    pub model_catalog_id: Uuid,
    pub model_name: String,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BootstrapAiProviderPresetBundle {
    pub provider_catalog_id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub credential_source: BootstrapAiCredentialSource,
    pub default_base_url: Option<String>,
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub presets: Vec<BootstrapAiPresetDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapAiCredentialInput {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BootstrapAiPresetInput {
    pub binding_purpose: AiBindingPurpose,
    pub provider_kind: String,
    pub model_catalog_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct BootstrapAiSetupDescriptor {
    pub preset_bundles: Vec<BootstrapAiProviderPresetBundle>,
}

#[derive(Debug, Clone)]
pub struct ApplyBootstrapAiSetupCommand {
    pub credentials: Vec<BootstrapAiCredentialInput>,
    pub preset_inputs: Vec<BootstrapAiPresetInput>,
    pub updated_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct ApplyBootstrapProviderPresetBundleCommand {
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
    pub credential_id: Uuid,
    pub api_key: Option<String>,
    pub model_catalog_id: Uuid,
    pub model_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Clone, Default)]
pub struct AiCatalogService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderCredentialValidationMode {
    ChatRoundTrip,
    ModelList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderCredentialPolicy {
    api_key_required: bool,
    base_url_required: bool,
    validation_mode: ProviderCredentialValidationMode,
}

const CANONICAL_RUNTIME_BINDING_PURPOSES: [AiBindingPurpose; 4] = [
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::EmbedChunk,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Vision,
];

fn provider_credential_policy(provider_kind: &str) -> ProviderCredentialPolicy {
    match provider_kind {
        "ollama" => ProviderCredentialPolicy {
            api_key_required: false,
            base_url_required: true,
            validation_mode: ProviderCredentialValidationMode::ModelList,
        },
        _ => ProviderCredentialPolicy {
            api_key_required: true,
            base_url_required: false,
            validation_mode: ProviderCredentialValidationMode::ChatRoundTrip,
        },
    }
}

impl AiCatalogService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn list_binding_assignments(
        &self,
        state: &AppState,
        scope: AiScopeRef,
    ) -> Result<Vec<AiBindingAssignment>, ApiError> {
        let rows = ai_repository::list_binding_assignments_exact(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_binding_assignment_row).collect()
    }

    pub async fn get_binding_assignment(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<AiBindingAssignment, ApiError> {
        let row =
            ai_repository::get_binding_assignment_by_id(&state.persistence.postgres, binding_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("binding_assignment", binding_id))?;
        map_binding_assignment_row(row)
    }

    pub async fn create_binding_assignment(
        &self,
        state: &AppState,
        command: CreateBindingAssignmentCommand,
    ) -> Result<AiBindingAssignment, ApiError> {
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
            command.provider_credential_id,
            command.model_preset_id,
        )
        .await?;
        let row = ai_repository::create_binding_assignment(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
            binding_purpose_key(command.binding_purpose),
            command.provider_credential_id,
            command.model_preset_id,
            command.updated_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?;
        map_binding_assignment_row(row)
    }

    pub async fn update_binding_assignment(
        &self,
        state: &AppState,
        command: UpdateBindingAssignmentCommand,
    ) -> Result<AiBindingAssignment, ApiError> {
        let existing = ai_repository::get_binding_assignment_by_id(
            &state.persistence.postgres,
            command.binding_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("binding_assignment", command.binding_id))?;
        let scope = scope_ref_from_binding_row(&existing)?;
        self.validate_binding_target_for_scope(
            state,
            scope,
            parse_binding_purpose(&existing.binding_purpose)?,
            command.provider_credential_id,
            command.model_preset_id,
        )
        .await?;
        let row = ai_repository::update_binding_assignment(
            &state.persistence.postgres,
            command.binding_id,
            command.provider_credential_id,
            command.model_preset_id,
            &command.binding_state,
            command.updated_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("binding_assignment", command.binding_id))?;
        map_binding_assignment_row(row)
    }

    pub async fn delete_binding_assignment(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<(), ApiError> {
        let deleted =
            ai_repository::delete_binding_assignment(&state.persistence.postgres, binding_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !deleted {
            return Err(ApiError::resource_not_found("binding_assignment", binding_id));
        }
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
        let mut preset_bundles = Vec::new();
        for provider in &providers {
            if let Some(bundle) = resolve_bootstrap_provider_preset_bundle(
                provider,
                &providers,
                &models,
                bootstrap_credential_source(configured_ai, &provider.provider_kind),
            )? {
                if !bootstrap_bundle_has_required_fallback_credentials(
                    &bundle,
                    &providers,
                    configured_ai,
                ) {
                    continue;
                }
                preset_bundles.push(bundle);
            }
        }
        preset_bundles.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.provider_kind.cmp(&right.provider_kind))
        });

        Ok(BootstrapAiSetupDescriptor { preset_bundles })
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

    pub async fn apply_bootstrap_provider_preset_bundle(
        &self,
        state: &AppState,
        command: ApplyBootstrapProviderPresetBundleCommand,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let bundle =
            resolve_bootstrap_provider_bundle(&providers, &models, &command.provider_kind)?;
        let preset_inputs = bundle
            .presets
            .into_iter()
            .map(|preset| BootstrapAiPresetInput {
                binding_purpose: preset.binding_purpose,
                provider_kind: preset.owner_provider_kind,
                model_catalog_id: preset.model_catalog_id,
                preset_name: preset.preset_name,
                system_prompt: preset.system_prompt,
                temperature: preset.temperature,
                top_p: preset.top_p,
                max_output_tokens_override: preset.max_output_tokens_override,
                extra_parameters_json: json!({}),
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
                preset_inputs,
                updated_by_principal_id: command.updated_by_principal_id,
            },
        )
        .await
    }

    /// Pre-creates model presets for every provider that ships a
    /// bootstrap profile (OpenAI, DeepSeek, Qwen, Ollama), regardless
    /// of whether the operator has configured a credential or API key.
    /// Presets are cheap — they are just named configurations over a
    /// model catalog entry. Having them ready means that when the
    /// operator later adds a credential, they can immediately assign
    /// bindings to existing presets instead of creating them from
    /// scratch.
    ///
    /// Runs idempotently at startup. Each preset is looked up by its
    /// canonical name before creation, so re-running does not duplicate.
    pub async fn seed_all_provider_presets(&self, state: &AppState) -> Result<usize, ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let instance_scope =
            AiScopeRef { scope_kind: AiScopeKind::Instance, workspace_id: None, library_id: None };
        let mut presets = self.list_model_presets_exact(state, instance_scope).await?;
        let mut created_count = 0usize;

        for provider_kind in BOOTSTRAPPED_PROVIDER_KINDS {
            let Some(bundle) =
                providers.iter().find(|p| p.provider_kind == *provider_kind).and_then(|provider| {
                    resolve_bootstrap_provider_preset_bundle(
                        provider,
                        &providers,
                        &models,
                        BootstrapAiCredentialSource::Missing,
                    )
                    .ok()
                    .flatten()
                })
            else {
                continue;
            };

            for descriptor in &bundle.presets {
                let input = BootstrapAiPresetInput {
                    binding_purpose: descriptor.binding_purpose,
                    provider_kind: descriptor.owner_provider_kind.clone(),
                    model_catalog_id: descriptor.model_catalog_id,
                    preset_name: descriptor.preset_name.clone(),
                    system_prompt: descriptor.system_prompt.clone(),
                    temperature: descriptor.temperature,
                    top_p: descriptor.top_p,
                    max_output_tokens_override: descriptor.max_output_tokens_override,
                    extra_parameters_json: serde_json::json!({}),
                };
                let _ =
                    ensure_bootstrap_model_preset(self, state, &input, &mut presets, None).await?;
                created_count += 1;
            }
        }

        if created_count > 0 {
            tracing::info!(
                stage = "bootstrap",
                presets = created_count,
                "seeded model presets for all providers",
            );
        }
        Ok(created_count)
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
        let preset_inputs =
            resolve_configured_bootstrap_preset_inputs(configured_ai, &providers, &models)?;
        if preset_inputs.is_empty()
            || !bootstrap_preset_inputs_cover_canonical_purposes(&preset_inputs)
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
                preset_inputs,
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
        let preset_inputs =
            normalize_bootstrap_preset_inputs(&command.preset_inputs, &providers, &models)?;
        validate_bootstrap_preset_inputs_complete(&preset_inputs)?;

        for input in &preset_inputs {
            tracing::info!(stage = "bootstrap", provider_kind = %input.provider_kind, "AI provider selected for bootstrap");
        }

        let instance_scope =
            AiScopeRef { scope_kind: AiScopeKind::Instance, workspace_id: None, library_id: None };
        let existing_credentials =
            self.list_provider_credentials_exact(state, instance_scope).await?;
        let provider_credentials = bootstrap_provider_credential_map(
            state.ui_bootstrap_ai_setup.as_ref(),
            &command.credentials,
        );
        let mut credentials_by_provider = std::collections::HashMap::new();
        for provider_kind in preset_inputs
            .iter()
            .map(|selection| selection.provider_kind.as_str())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let provider =
                providers.iter().find(|entry| entry.provider_kind == provider_kind).ok_or_else(
                    || ApiError::resource_not_found("provider_catalog", provider_kind.to_string()),
                )?;
            let credential = ensure_bootstrap_provider_credential(
                self,
                state,
                provider,
                provider_credentials.get(provider_kind).cloned(),
                &existing_credentials,
                command.updated_by_principal_id,
            )
            .await?;
            // Clone required: the HashMap outlives the `providers` borrow used later.
            credentials_by_provider.insert(provider.provider_kind.clone(), credential);
        }

        let mut presets = self.list_model_presets_exact(state, instance_scope).await?;
        let mut preset_ids_by_purpose = Vec::new();
        for selection in &preset_inputs {
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
            let preset_id = ensure_bootstrap_model_preset(
                self,
                state,
                selection,
                &mut presets,
                command.updated_by_principal_id,
            )
            .await?
            .id;
            preset_ids_by_purpose.push((
                selection.binding_purpose,
                provider.provider_kind.clone(),
                preset_id,
            ));
        }

        let mut bindings = self.list_binding_assignments(state, instance_scope).await?;
        for selection in &preset_inputs {
            let (_, provider_kind, model_preset_id) = preset_ids_by_purpose
                .iter()
                .find(|(purpose, _, _)| *purpose == selection.binding_purpose)
                .cloned()
                .ok_or_else(|| {
                    ApiError::BadRequest("bootstrap binding preset was not created".to_string())
                })?;
            let provider_credential_id = credentials_by_provider
                .get(&provider_kind)
                .map(|credential| credential.id)
                .ok_or_else(|| {
                    ApiError::BadRequest("bootstrap credential was not created".to_string())
                })?;
            ensure_bootstrap_binding_assignment(
                self,
                state,
                selection.binding_purpose,
                provider_credential_id,
                model_preset_id,
                &mut bindings,
                command.updated_by_principal_id,
            )
            .await?;
        }

        tracing::info!(
            stage = "bootstrap",
            presets_count = preset_inputs.len(),
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
        let Some(library) =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };
        let Some(binding) = ai_repository::get_effective_binding_assignment_by_purpose(
            &state.persistence.postgres,
            library_id,
            binding_purpose_key(binding_purpose),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        let binding =
            ai_repository::get_binding_assignment_by_id(&state.persistence.postgres, binding_id)
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
        binding: ai_repository::AiBindingAssignmentRow,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> Result<ResolvedRuntimeBinding, ApiError> {
        let provider_credential =
            self.get_provider_credential(state, binding.provider_credential_id).await?;
        let model_preset = self.get_model_preset(state, binding.model_preset_id).await?;
        let binding_purpose = parse_binding_purpose(&binding.binding_purpose)?;
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let provider = providers
            .into_iter()
            .find(|entry| entry.id == provider_credential.provider_catalog_id)
            .ok_or_else(|| {
                ApiError::resource_not_found(
                    "provider_catalog",
                    provider_credential.provider_catalog_id,
                )
            })?;
        let model =
            models.into_iter().find(|entry| entry.id == model_preset.model_catalog_id).ok_or_else(
                || ApiError::resource_not_found("model_catalog", model_preset.model_catalog_id),
            )?;
        if model.provider_catalog_id != provider.id {
            return Err(ApiError::BadRequest(
                "binding links a provider credential to a model from another provider".to_string(),
            ));
        }
        if provider_credential.credential_state != "active" {
            return Err(ApiError::BadRequest("provider credential is not active".to_string()));
        }
        validate_model_binding_purpose(binding_purpose, &model)?;

        let provider_row = ai_repository::list_provider_catalog(&state.persistence.postgres)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .into_iter()
            .find(|entry| entry.id == provider.id)
            .ok_or_else(|| ApiError::resource_not_found("provider_catalog", provider.id))?;

        Ok(ResolvedRuntimeBinding {
            binding_id: binding.id,
            workspace_id,
            library_id,
            binding_purpose,
            provider_catalog_id: provider.id,
            provider_kind: provider.provider_kind,
            provider_base_url: provider_credential
                .base_url
                .clone()
                .or(provider_row.default_base_url),
            provider_api_style: provider.api_style,
            credential_id: provider_credential.id,
            api_key: provider_credential.api_key,
            model_catalog_id: model.id,
            model_name: model.model_name,
            system_prompt: model_preset.system_prompt,
            temperature: model_preset.temperature,
            top_p: model_preset.top_p,
            max_output_tokens_override: model_preset.max_output_tokens_override,
            extra_parameters_json: model_preset.extra_parameters_json,
        })
    }

    async fn validate_binding_target_for_scope(
        &self,
        state: &AppState,
        scope: AiScopeRef,
        binding_purpose: AiBindingPurpose,
        provider_credential_id: Uuid,
        model_preset_id: Uuid,
    ) -> Result<(), ApiError> {
        let provider_credential =
            self.get_provider_credential(state, provider_credential_id).await?;
        let model_preset = self.get_model_preset(state, model_preset_id).await?;
        let credential_scope = scope_ref_from_provider_credential(&provider_credential)?;
        let preset_scope = scope_ref_from_model_preset(&model_preset)?;
        if !scope_can_use_resource(scope, credential_scope) {
            return Err(ApiError::BadRequest(
                "binding cannot use a provider credential from an unrelated scope".to_string(),
            ));
        }
        if !scope_can_use_resource(scope, preset_scope) {
            return Err(ApiError::BadRequest(
                "binding cannot use a model preset from an unrelated scope".to_string(),
            ));
        }

        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let provider = providers
            .into_iter()
            .find(|entry| entry.id == provider_credential.provider_catalog_id)
            .ok_or_else(|| {
                ApiError::resource_not_found(
                    "provider_catalog",
                    provider_credential.provider_catalog_id,
                )
            })?;
        let model =
            models.into_iter().find(|entry| entry.id == model_preset.model_catalog_id).ok_or_else(
                || ApiError::resource_not_found("model_catalog", model_preset.model_catalog_id),
            )?;

        if model.provider_catalog_id != provider.id {
            return Err(ApiError::BadRequest(
                "binding links a provider credential to a model from another provider".to_string(),
            ));
        }
        if provider_credential.credential_state != "active" {
            return Err(ApiError::BadRequest("provider credential is not active".to_string()));
        }
        validate_model_binding_purpose(binding_purpose, &model)
    }
}
