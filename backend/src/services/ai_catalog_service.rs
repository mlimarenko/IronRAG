#![allow(
    clippy::iter_without_into_iter,
    clippy::missing_errors_doc,
    clippy::result_large_err,
    clippy::too_many_lines
)]

use rust_decimal::Decimal;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::{
        AiBindingPurpose, BindingValidation, LibraryModelBinding, ModelCatalogEntry, ModelPreset,
        PriceCatalogEntry, ProviderCatalogEntry, ProviderCredential,
    },
    infra::repositories::{ai_repository, catalog_repository},
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct CreateProviderCredentialCommand {
    pub workspace_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: String,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateProviderCredentialCommand {
    pub credential_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub credential_state: String,
}

#[derive(Debug, Clone)]
pub struct CreateModelPresetCommand {
    pub workspace_id: Uuid,
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
pub struct CreateLibraryBindingCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_purpose: AiBindingPurpose,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
    pub updated_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateLibraryBindingCommand {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapAiBindingSelection {
    pub binding_purpose: AiBindingPurpose,
    pub provider_kind: Option<String>,
    pub model_catalog_id: Option<Uuid>,
    pub configured: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapAiCredentialSource {
    Missing,
    Env,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapAiProviderDescriptor {
    pub provider_catalog_id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub credential_source: BootstrapAiCredentialSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapAiCredentialInput {
    pub provider_kind: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapAiBindingInput {
    pub binding_purpose: AiBindingPurpose,
    pub provider_kind: String,
    pub model_catalog_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct BootstrapAiSetupDescriptor {
    pub providers: Vec<BootstrapAiProviderDescriptor>,
    pub models: Vec<ModelCatalogEntry>,
    pub binding_selections: Vec<BootstrapAiBindingSelection>,
}

#[derive(Debug, Clone)]
pub struct ApplyBootstrapAiSetupCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub credentials: Vec<BootstrapAiCredentialInput>,
    pub binding_selections: Vec<BootstrapAiBindingInput>,
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
    pub api_key: String,
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

const CANONICAL_RUNTIME_BINDING_PURPOSES: [AiBindingPurpose; 4] = [
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::EmbedChunk,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Vision,
];

impl AiCatalogService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn list_provider_catalog(
        &self,
        state: &AppState,
    ) -> Result<Vec<ProviderCatalogEntry>, ApiError> {
        let rows = ai_repository::list_provider_catalog(&state.persistence.postgres)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_provider_row).collect())
    }

    pub async fn list_model_catalog(
        &self,
        state: &AppState,
        provider_catalog_id: Option<Uuid>,
    ) -> Result<Vec<ModelCatalogEntry>, ApiError> {
        let rows =
            ai_repository::list_model_catalog(&state.persistence.postgres, provider_catalog_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        rows.into_iter().map(map_model_row).collect()
    }

    pub async fn list_price_catalog(
        &self,
        state: &AppState,
        model_catalog_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
    ) -> Result<Vec<PriceCatalogEntry>, ApiError> {
        let rows = ai_repository::list_price_catalog(
            &state.persistence.postgres,
            model_catalog_id,
            workspace_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_price_row).collect())
    }

    pub async fn get_price_catalog_entry(
        &self,
        state: &AppState,
        price_id: Uuid,
    ) -> Result<PriceCatalogEntry, ApiError> {
        let row = ai_repository::get_price_catalog_by_id(&state.persistence.postgres, price_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("price_catalog_entry", price_id))?;
        Ok(map_price_row(row))
    }

    pub async fn create_workspace_price_override(
        &self,
        state: &AppState,
        command: CreateWorkspacePriceOverrideCommand,
    ) -> Result<PriceCatalogEntry, ApiError> {
        let billing_unit = normalize_non_empty(&command.billing_unit, "billingUnit")?;
        let currency_code = normalize_currency_code(&command.currency_code)?;
        let row = ai_repository::create_workspace_price_override(
            &state.persistence.postgres,
            command.workspace_id,
            command.model_catalog_id,
            &billing_unit,
            command.unit_price,
            &currency_code,
            command.effective_from,
            command.effective_to,
        )
        .await
        .map_err(map_ai_write_error)?;
        Ok(map_price_row(row))
    }

    pub async fn update_workspace_price_override(
        &self,
        state: &AppState,
        command: UpdateWorkspacePriceOverrideCommand,
    ) -> Result<PriceCatalogEntry, ApiError> {
        let billing_unit = normalize_non_empty(&command.billing_unit, "billingUnit")?;
        let currency_code = normalize_currency_code(&command.currency_code)?;
        let row = ai_repository::update_workspace_price_override(
            &state.persistence.postgres,
            command.price_id,
            command.model_catalog_id,
            &billing_unit,
            command.unit_price,
            &currency_code,
            command.effective_from,
            command.effective_to,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("price_catalog_entry", command.price_id))?;
        Ok(map_price_row(row))
    }

    pub async fn list_provider_credentials(
        &self,
        state: &AppState,
        workspace_id: Uuid,
    ) -> Result<Vec<ProviderCredential>, ApiError> {
        let rows =
            ai_repository::list_provider_credentials(&state.persistence.postgres, workspace_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_provider_credential_row).collect())
    }

    pub async fn get_provider_credential(
        &self,
        state: &AppState,
        credential_id: Uuid,
    ) -> Result<ProviderCredential, ApiError> {
        let row = ai_repository::get_provider_credential_by_id(
            &state.persistence.postgres,
            credential_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("provider_credential", credential_id))?;
        Ok(map_provider_credential_row(row))
    }

    pub async fn create_provider_credential(
        &self,
        state: &AppState,
        command: CreateProviderCredentialCommand,
    ) -> Result<ProviderCredential, ApiError> {
        let label = normalize_non_empty(&command.label, "label")?;
        let api_key = normalize_non_empty(&command.api_key, "apiKey")?;
        let row = ai_repository::create_provider_credential(
            &state.persistence.postgres,
            command.workspace_id,
            command.provider_catalog_id,
            &label,
            &api_key,
            command.created_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?;
        Ok(map_provider_credential_row(row))
    }

    pub async fn update_provider_credential(
        &self,
        state: &AppState,
        command: UpdateProviderCredentialCommand,
    ) -> Result<ProviderCredential, ApiError> {
        let label = normalize_non_empty(&command.label, "label")?;
        let api_key = normalize_optional(command.api_key.as_deref());
        let row = ai_repository::update_provider_credential(
            &state.persistence.postgres,
            command.credential_id,
            &label,
            api_key.as_deref(),
            &command.credential_state,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| {
            ApiError::resource_not_found("provider_credential", command.credential_id)
        })?;
        Ok(map_provider_credential_row(row))
    }

    pub async fn list_model_presets(
        &self,
        state: &AppState,
        workspace_id: Uuid,
    ) -> Result<Vec<ModelPreset>, ApiError> {
        let rows = ai_repository::list_model_presets(&state.persistence.postgres, workspace_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_model_preset_row).collect())
    }

    pub async fn get_model_preset(
        &self,
        state: &AppState,
        preset_id: Uuid,
    ) -> Result<ModelPreset, ApiError> {
        let row = ai_repository::get_model_preset_by_id(&state.persistence.postgres, preset_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("model_preset", preset_id))?;
        Ok(map_model_preset_row(row))
    }

    pub async fn create_model_preset(
        &self,
        state: &AppState,
        command: CreateModelPresetCommand,
    ) -> Result<ModelPreset, ApiError> {
        let preset_name = normalize_non_empty(&command.preset_name, "presetName")?;
        let row = ai_repository::create_model_preset(
            &state.persistence.postgres,
            command.workspace_id,
            command.model_catalog_id,
            &preset_name,
            normalize_optional(command.system_prompt.as_deref()).as_deref(),
            command.temperature,
            command.top_p,
            command.max_output_tokens_override,
            command.extra_parameters_json,
            command.created_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?;
        Ok(map_model_preset_row(row))
    }

    pub async fn update_model_preset(
        &self,
        state: &AppState,
        command: UpdateModelPresetCommand,
    ) -> Result<ModelPreset, ApiError> {
        let preset_name = normalize_non_empty(&command.preset_name, "presetName")?;
        let row = ai_repository::update_model_preset(
            &state.persistence.postgres,
            command.preset_id,
            &preset_name,
            normalize_optional(command.system_prompt.as_deref()).as_deref(),
            command.temperature,
            command.top_p,
            command.max_output_tokens_override,
            command.extra_parameters_json,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("model_preset", command.preset_id))?;
        Ok(map_model_preset_row(row))
    }

    pub async fn list_library_bindings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<LibraryModelBinding>, ApiError> {
        let rows = ai_repository::list_library_bindings(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        rows.into_iter().map(map_library_binding_row).collect()
    }

    pub async fn get_library_binding(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<LibraryModelBinding, ApiError> {
        let row = ai_repository::get_library_binding_by_id(&state.persistence.postgres, binding_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("library_binding", binding_id))?;
        map_library_binding_row(row)
    }

    pub async fn create_library_binding(
        &self,
        state: &AppState,
        command: CreateLibraryBindingCommand,
    ) -> Result<LibraryModelBinding, ApiError> {
        self.validate_binding_target(
            state,
            command.binding_purpose,
            command.provider_credential_id,
            command.model_preset_id,
        )
        .await?;
        let row = ai_repository::create_library_binding(
            &state.persistence.postgres,
            command.workspace_id,
            command.library_id,
            binding_purpose_key(command.binding_purpose),
            command.provider_credential_id,
            command.model_preset_id,
            command.updated_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?;
        map_library_binding_row(row)
    }

    pub async fn update_library_binding(
        &self,
        state: &AppState,
        command: UpdateLibraryBindingCommand,
    ) -> Result<LibraryModelBinding, ApiError> {
        let existing = ai_repository::get_library_binding_by_id(
            &state.persistence.postgres,
            command.binding_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("library_binding", command.binding_id))?;
        self.validate_binding_target(
            state,
            parse_binding_purpose(&existing.binding_purpose)?,
            command.provider_credential_id,
            command.model_preset_id,
        )
        .await?;
        let row = ai_repository::update_library_binding(
            &state.persistence.postgres,
            command.binding_id,
            command.provider_credential_id,
            command.model_preset_id,
            &command.binding_state,
            command.updated_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("library_binding", command.binding_id))?;
        map_library_binding_row(row)
    }

    pub async fn describe_bootstrap_ai_setup(
        &self,
        state: &AppState,
    ) -> Result<BootstrapAiSetupDescriptor, ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let configured_ai = state.ui_bootstrap_ai_setup.as_ref();
        let provider_descriptors = providers
            .iter()
            .map(|provider| BootstrapAiProviderDescriptor {
                provider_catalog_id: provider.id,
                provider_kind: provider.provider_kind.clone(),
                display_name: provider.display_name.clone(),
                api_style: provider.api_style.clone(),
                lifecycle_state: provider.lifecycle_state.clone(),
                credential_source: if bootstrap_provider_secret(
                    configured_ai,
                    &provider.provider_kind,
                )
                .is_some()
                {
                    BootstrapAiCredentialSource::Env
                } else {
                    BootstrapAiCredentialSource::Missing
                },
            })
            .collect::<Vec<_>>();
        let binding_selections = CANONICAL_RUNTIME_BINDING_PURPOSES
            .iter()
            .map(|purpose| {
                resolve_bootstrap_binding_suggestion(
                    *purpose,
                    configured_ai,
                    &provider_descriptors,
                    &providers,
                    &models,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(BootstrapAiSetupDescriptor {
            providers: provider_descriptors,
            models,
            binding_selections,
        })
    }

    pub async fn apply_configured_bootstrap_ai_setup(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        updated_by_principal_id: Option<Uuid>,
    ) -> Result<bool, ApiError> {
        let Some(configured_ai) = state.ui_bootstrap_ai_setup.as_ref() else {
            return Ok(false);
        };
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let binding_selections =
            resolve_configured_bootstrap_binding_inputs(configured_ai, &providers, &models)?;
        if binding_selections.is_empty()
            || !bootstrap_binding_inputs_cover_canonical_purposes(&binding_selections)
        {
            return Ok(false);
        }
        self.apply_bootstrap_ai_setup(
            state,
            ApplyBootstrapAiSetupCommand {
                workspace_id,
                library_id,
                credentials: configured_ai
                    .provider_secrets
                    .iter()
                    .map(|secret| BootstrapAiCredentialInput {
                        provider_kind: secret.provider_kind.clone(),
                        api_key: Some(secret.api_key.clone()),
                    })
                    .collect(),
                binding_selections,
                updated_by_principal_id,
            },
        )
        .await?;
        Ok(true)
    }

    pub async fn validate_bootstrap_ai_setup_inputs(
        &self,
        state: &AppState,
        credentials: &[BootstrapAiCredentialInput],
        binding_selections: &[BootstrapAiBindingInput],
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let binding_inputs =
            normalize_bootstrap_binding_inputs(binding_selections, &providers, &models)?;
        validate_bootstrap_binding_inputs_complete(&binding_inputs)?;
        let provider_secrets =
            bootstrap_provider_secret_map(state.ui_bootstrap_ai_setup.as_ref(), credentials);
        for provider_kind in binding_inputs
            .iter()
            .map(|selection| selection.provider_kind.as_str())
            .collect::<std::collections::BTreeSet<_>>()
        {
            if !provider_secrets.contains_key(provider_kind) {
                return Err(ApiError::BadRequest(format!(
                    "bootstrap ai setup requires an API key for provider {provider_kind}",
                )));
            }
        }
        Ok(())
    }

    pub async fn apply_bootstrap_ai_setup(
        &self,
        state: &AppState,
        command: ApplyBootstrapAiSetupCommand,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let binding_inputs =
            normalize_bootstrap_binding_inputs(&command.binding_selections, &providers, &models)?;
        validate_bootstrap_binding_inputs_complete(&binding_inputs)?;

        let existing_credentials =
            self.list_provider_credentials(state, command.workspace_id).await?;
        let provider_secrets = bootstrap_provider_secret_map(
            state.ui_bootstrap_ai_setup.as_ref(),
            &command.credentials,
        );
        let mut credentials_by_provider = std::collections::HashMap::new();
        for provider_kind in binding_inputs
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
                command.workspace_id,
                provider,
                provider_secrets.get(provider_kind).cloned(),
                &existing_credentials,
                command.updated_by_principal_id,
            )
            .await?;
            credentials_by_provider.insert(provider.provider_kind.clone(), credential);
        }

        let mut presets = self.list_model_presets(state, command.workspace_id).await?;
        let mut preset_ids_by_purpose = Vec::new();
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
            let preset_name = canonical_runtime_preset_name(
                &provider.display_name,
                selection.binding_purpose,
                &model.model_name,
            );
            let preset_id = ensure_bootstrap_model_preset(
                self,
                state,
                command.workspace_id,
                selection.model_catalog_id,
                &preset_name,
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

        let bindings = self.list_library_bindings(state, command.library_id).await?;
        let mut bindings = bindings;
        for selection in &binding_inputs {
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
            ensure_bootstrap_library_binding(
                self,
                state,
                command.workspace_id,
                command.library_id,
                selection.binding_purpose,
                provider_credential_id,
                model_preset_id,
                &mut bindings,
                command.updated_by_principal_id,
            )
            .await?;
        }

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
        let binding = ai_repository::get_active_library_binding_by_purpose(
            &state.persistence.postgres,
            library_id,
            binding_purpose_key(binding_purpose),
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        if let Some(binding) = binding {
            if let Ok(resolved) = self.resolve_runtime_binding_by_row(state, binding.clone()).await
            {
                return Ok(Some(resolved));
            }
            self.ensure_library_runtime_profile(state, binding.workspace_id, library_id, None)
                .await?;
        } else if let Some(library) =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .map_err(|_| ApiError::Internal)?
        {
            self.ensure_library_runtime_profile(state, library.workspace_id, library_id, None)
                .await?;
        } else {
            return Ok(None);
        }

        let Some(binding) = ai_repository::get_active_library_binding_by_purpose(
            &state.persistence.postgres,
            library_id,
            binding_purpose_key(binding_purpose),
        )
        .await
        .map_err(|_| ApiError::Internal)?
        else {
            return Ok(None);
        };

        self.resolve_runtime_binding_by_row(state, binding).await.map(Some)
    }

    pub async fn ensure_workspace_runtime_profiles(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        updated_by_principal_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let libraries = crate::infra::repositories::catalog_repository::list_libraries(
            &state.persistence.postgres,
            Some(workspace_id),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        for library in libraries {
            self.ensure_library_runtime_profile(
                state,
                workspace_id,
                library.id,
                updated_by_principal_id,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn ensure_library_runtime_profile(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        updated_by_principal_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let provider_by_id = providers
            .into_iter()
            .map(|provider| (provider.id, provider))
            .collect::<std::collections::HashMap<_, _>>();
        let models = self.list_model_catalog(state, None).await?;
        let model_by_id = models
            .iter()
            .map(|model| (model.id, model))
            .collect::<std::collections::HashMap<_, _>>();
        let presets = self.list_model_presets(state, workspace_id).await?;
        let mut presets_by_model = presets.into_iter().fold(
            std::collections::HashMap::<Uuid, Vec<ModelPreset>>::new(),
            |mut acc, preset| {
                acc.entry(preset.model_catalog_id).or_default().push(preset);
                acc
            },
        );
        let credentials = self.list_provider_credentials(state, workspace_id).await?;
        let mut active_credentials = credentials
            .iter()
            .filter(|credential| credential.credential_state == "active")
            .cloned()
            .collect::<Vec<_>>();
        active_credentials.sort_by(|left, right| {
            let left_provider = provider_by_id
                .get(&left.provider_catalog_id)
                .map(|provider| provider.display_name.as_str())
                .unwrap_or("");
            let right_provider = provider_by_id
                .get(&right.provider_catalog_id)
                .map(|provider| provider.display_name.as_str())
                .unwrap_or("");
            left_provider
                .cmp(right_provider)
                .then_with(|| left.label.cmp(&right.label))
                .then_with(|| left.id.cmp(&right.id))
        });
        if active_credentials.is_empty() {
            return Ok(());
        }

        let mut bindings = self.list_library_bindings(state, library_id).await?;

        for purpose in CANONICAL_RUNTIME_BINDING_PURPOSES {
            let Some((credential, model)) =
                select_canonical_runtime_target(purpose, &active_credentials, &models)
            else {
                continue;
            };
            let Some(provider) = provider_by_id.get(&credential.provider_catalog_id) else {
                continue;
            };
            let preset_name =
                canonical_runtime_preset_name(&provider.display_name, purpose, &model.model_name);
            let preset_id = match select_runtime_preset(
                presets_by_model.get(&model.id).map(Vec::as_slice).unwrap_or(&[]),
                &preset_name,
            ) {
                Some(existing) => existing.id,
                None => {
                    let created = self
                        .create_model_preset(
                            state,
                            CreateModelPresetCommand {
                                workspace_id,
                                model_catalog_id: model.id,
                                preset_name: preset_name.clone(),
                                system_prompt: None,
                                temperature: None,
                                top_p: None,
                                max_output_tokens_override: None,
                                extra_parameters_json: serde_json::json!({}),
                                created_by_principal_id: updated_by_principal_id,
                            },
                        )
                        .await?;
                    presets_by_model.entry(model.id).or_default().push(created.clone());
                    created.id
                }
            };

            let existing_index =
                bindings.iter().position(|binding| binding.binding_purpose == purpose);
            match existing_index {
                Some(index)
                    if library_binding_is_runtime_ready(
                        &bindings[index],
                        &credentials,
                        &model_by_id,
                        &presets_by_model,
                    ) =>
                {
                    continue;
                }
                Some(index) => {
                    let updated = self
                        .update_library_binding(
                            state,
                            UpdateLibraryBindingCommand {
                                binding_id: bindings[index].id,
                                provider_credential_id: credential.id,
                                model_preset_id: preset_id,
                                binding_state: "active".to_string(),
                                updated_by_principal_id,
                            },
                        )
                        .await?;
                    bindings[index] = updated;
                }
                None => {
                    let created = self
                        .create_library_binding(
                            state,
                            CreateLibraryBindingCommand {
                                workspace_id,
                                library_id,
                                binding_purpose: purpose,
                                provider_credential_id: credential.id,
                                model_preset_id: preset_id,
                                updated_by_principal_id,
                            },
                        )
                        .await?;
                    bindings.push(created);
                }
            }
        }

        Ok(())
    }

    pub async fn resolve_runtime_binding_by_id(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<ResolvedRuntimeBinding, ApiError> {
        let binding =
            ai_repository::get_library_binding_by_id(&state.persistence.postgres, binding_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("library_binding", binding_id))?;
        self.resolve_runtime_binding_by_row(state, binding).await
    }

    async fn resolve_runtime_binding_by_row(
        &self,
        state: &AppState,
        binding: ai_repository::AiLibraryModelBindingRow,
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
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .find(|entry| entry.id == provider.id)
            .ok_or_else(|| ApiError::resource_not_found("provider_catalog", provider.id))?;

        Ok(ResolvedRuntimeBinding {
            binding_id: binding.id,
            workspace_id: binding.workspace_id,
            library_id: binding.library_id,
            binding_purpose,
            provider_catalog_id: provider.id,
            provider_kind: provider.provider_kind,
            provider_base_url: provider_row.default_base_url,
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

    async fn validate_binding_target(
        &self,
        state: &AppState,
        binding_purpose: AiBindingPurpose,
        provider_credential_id: Uuid,
        model_preset_id: Uuid,
    ) -> Result<(), ApiError> {
        let provider_credential =
            self.get_provider_credential(state, provider_credential_id).await?;
        let model_preset = self.get_model_preset(state, model_preset_id).await?;
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

fn select_runtime_preset<'a>(
    presets: &'a [ModelPreset],
    canonical_name: &str,
) -> Option<&'a ModelPreset> {
    if let Some(existing) = presets.iter().find(|preset| preset.preset_name == canonical_name) {
        return Some(existing);
    }
    match presets {
        [only] => Some(only),
        _ => None,
    }
}

fn select_canonical_runtime_target<'a>(
    purpose: AiBindingPurpose,
    credentials: &'a [ProviderCredential],
    models: &'a [ModelCatalogEntry],
) -> Option<(&'a ProviderCredential, &'a ModelCatalogEntry)> {
    credentials.iter().find_map(|credential| {
        models
            .iter()
            .find(|model| {
                model.provider_catalog_id == credential.provider_catalog_id
                    && model.allowed_binding_purposes.contains(&purpose)
            })
            .map(|model| (credential, model))
    })
}

fn library_binding_is_runtime_ready(
    binding: &LibraryModelBinding,
    credentials: &[ProviderCredential],
    model_by_id: &std::collections::HashMap<Uuid, &ModelCatalogEntry>,
    presets_by_model: &std::collections::HashMap<Uuid, Vec<ModelPreset>>,
) -> bool {
    if binding.binding_state != "active" {
        return false;
    }

    let Some(credential) =
        credentials.iter().find(|credential| credential.id == binding.provider_credential_id)
    else {
        return false;
    };
    if credential.credential_state != "active" {
        return false;
    }

    let preset = presets_by_model
        .values()
        .flat_map(|presets| presets.iter())
        .find(|preset| preset.id == binding.model_preset_id);
    let Some(preset) = preset else {
        return false;
    };
    let Some(model) = model_by_id.get(&preset.model_catalog_id) else {
        return false;
    };
    credential.provider_catalog_id == model.provider_catalog_id
        && model.allowed_binding_purposes.contains(&binding.binding_purpose)
}

fn normalize_non_empty(value: &str, field_name: &'static str) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest(format!("{field_name} must not be empty")));
    }
    Ok(normalized.to_string())
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToString::to_string)
}

fn normalize_currency_code(value: &str) -> Result<String, ApiError> {
    let normalized = normalize_non_empty(value, "currencyCode")?;
    Ok(normalized.to_ascii_uppercase())
}

fn map_ai_write_error(error: sqlx::Error) -> ApiError {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            ApiError::Conflict("AI catalog resource already exists".to_string())
        }
        sqlx::Error::Database(database_error) if database_error.is_foreign_key_violation() => {
            ApiError::NotFound("referenced AI catalog resource was not found".to_string())
        }
        sqlx::Error::Database(database_error) if database_error.is_check_violation() => {
            ApiError::BadRequest("AI catalog payload violated schema constraints".to_string())
        }
        _ => ApiError::Internal,
    }
}

fn map_provider_row(row: ai_repository::AiProviderCatalogRow) -> ProviderCatalogEntry {
    ProviderCatalogEntry {
        id: row.id,
        provider_kind: row.provider_kind,
        display_name: row.display_name,
        api_style: row.api_style,
        lifecycle_state: row.lifecycle_state,
    }
}

fn map_model_row(row: ai_repository::AiModelCatalogRow) -> Result<ModelCatalogEntry, ApiError> {
    Ok(ModelCatalogEntry {
        id: row.id,
        provider_catalog_id: row.provider_catalog_id,
        model_name: row.model_name,
        capability_kind: row.capability_kind,
        modality_kind: row.modality_kind,
        allowed_binding_purposes: parse_allowed_binding_purposes(&row.metadata_json)?,
        context_window: row.context_window,
        max_output_tokens: row.max_output_tokens,
    })
}

fn map_price_row(row: ai_repository::AiPriceCatalogRow) -> PriceCatalogEntry {
    PriceCatalogEntry {
        id: row.id,
        model_catalog_id: row.model_catalog_id,
        billing_unit: row.billing_unit,
        price_variant_key: row.price_variant_key,
        request_input_tokens_min: row.request_input_tokens_min,
        request_input_tokens_max: row.request_input_tokens_max,
        unit_price: row.unit_price,
        currency_code: row.currency_code,
        effective_from: row.effective_from,
        effective_to: row.effective_to,
        catalog_scope: row.catalog_scope,
        workspace_id: row.workspace_id,
    }
}

fn map_provider_credential_row(row: ai_repository::AiProviderCredentialRow) -> ProviderCredential {
    ProviderCredential {
        id: row.id,
        workspace_id: row.workspace_id,
        provider_catalog_id: row.provider_catalog_id,
        label: row.label,
        api_key: row.api_key,
        credential_state: row.credential_state,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_model_preset_row(row: ai_repository::AiModelPresetRow) -> ModelPreset {
    ModelPreset {
        id: row.id,
        workspace_id: row.workspace_id,
        model_catalog_id: row.model_catalog_id,
        preset_name: row.preset_name,
        system_prompt: row.system_prompt,
        temperature: row.temperature,
        top_p: row.top_p,
        max_output_tokens_override: row.max_output_tokens_override,
        extra_parameters_json: row.extra_parameters_json,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_library_binding_row(
    row: ai_repository::AiLibraryModelBindingRow,
) -> Result<LibraryModelBinding, ApiError> {
    Ok(LibraryModelBinding {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        binding_purpose: parse_binding_purpose(&row.binding_purpose)?,
        provider_credential_id: row.provider_credential_id,
        model_preset_id: row.model_preset_id,
        binding_state: row.binding_state,
    })
}

fn map_binding_validation_row(row: ai_repository::AiBindingValidationRow) -> BindingValidation {
    BindingValidation {
        id: row.id,
        binding_id: row.binding_id,
        validation_state: row.validation_state,
        checked_at: row.checked_at,
        failure_code: row.failure_code,
        message: row.message,
    }
}

fn parse_binding_purpose(value: &str) -> Result<AiBindingPurpose, ApiError> {
    match value {
        "extract_text" => Ok(AiBindingPurpose::ExtractText),
        "extract_graph" => Ok(AiBindingPurpose::ExtractGraph),
        "embed_chunk" => Ok(AiBindingPurpose::EmbedChunk),
        "query_retrieve" => Ok(AiBindingPurpose::QueryRetrieve),
        "query_answer" => Ok(AiBindingPurpose::QueryAnswer),
        "vision" => Ok(AiBindingPurpose::Vision),
        _ => Err(ApiError::Internal),
    }
}

pub(crate) fn binding_purpose_key(value: AiBindingPurpose) -> &'static str {
    match value {
        AiBindingPurpose::ExtractText => "extract_text",
        AiBindingPurpose::ExtractGraph => "extract_graph",
        AiBindingPurpose::EmbedChunk => "embed_chunk",
        AiBindingPurpose::QueryRetrieve => "query_retrieve",
        AiBindingPurpose::QueryAnswer => "query_answer",
        AiBindingPurpose::Vision => "vision",
    }
}

pub(crate) fn canonical_runtime_preset_name(
    provider_display_name: &str,
    purpose: AiBindingPurpose,
    model_name: &str,
) -> String {
    let purpose_label = match purpose {
        AiBindingPurpose::ExtractText => "Extract Text",
        AiBindingPurpose::ExtractGraph => "Extract Graph",
        AiBindingPurpose::EmbedChunk => "Embed Chunk",
        AiBindingPurpose::QueryRetrieve => "Query Retrieve",
        AiBindingPurpose::QueryAnswer => "Query Answer",
        AiBindingPurpose::Vision => "Vision",
    };
    format!("{provider_display_name} {purpose_label} · {model_name}")
}

fn provider_id_for_kind(providers: &[ProviderCatalogEntry], provider_kind: &str) -> Option<Uuid> {
    providers
        .iter()
        .find(|provider| provider.provider_kind == provider_kind)
        .map(|provider| provider.id)
}

fn bootstrap_provider_secret(
    configured_ai: Option<&crate::app::config::UiBootstrapAiSetup>,
    provider_kind: &str,
) -> Option<String> {
    configured_ai
        .and_then(|config| {
            config.provider_secrets.iter().find(|secret| secret.provider_kind == provider_kind)
        })
        .map(|secret| secret.api_key.clone())
}

fn bootstrap_provider_secret_map(
    configured_ai: Option<&crate::app::config::UiBootstrapAiSetup>,
    credential_inputs: &[BootstrapAiCredentialInput],
) -> std::collections::HashMap<String, String> {
    let mut secrets = configured_ai
        .map(|config| {
            config
                .provider_secrets
                .iter()
                .map(|secret| (secret.provider_kind.clone(), secret.api_key.clone()))
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();
    for credential in credential_inputs {
        if let Some(api_key) = normalize_optional(credential.api_key.as_deref()) {
            secrets.insert(credential.provider_kind.trim().to_ascii_lowercase(), api_key);
        }
    }
    secrets
}

fn configured_binding_default_for_purpose<'a>(
    configured_ai: Option<&'a crate::app::config::UiBootstrapAiSetup>,
    purpose: AiBindingPurpose,
) -> Option<&'a crate::app::config::UiBootstrapAiBindingDefault> {
    configured_ai.and_then(|config| {
        config
            .binding_defaults
            .iter()
            .find(|binding| binding.binding_purpose == binding_purpose_key(purpose))
    })
}

fn resolve_bootstrap_binding_suggestion(
    purpose: AiBindingPurpose,
    configured_ai: Option<&crate::app::config::UiBootstrapAiSetup>,
    provider_descriptors: &[BootstrapAiProviderDescriptor],
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<BootstrapAiBindingSelection, ApiError> {
    if let Some(configured_default) = configured_binding_default_for_purpose(configured_ai, purpose)
    {
        if let Some(model) =
            select_configured_bootstrap_model(configured_default, purpose, providers, models)?
        {
            let provider_kind = providers
                .iter()
                .find(|provider| provider.id == model.provider_catalog_id)
                .map(|provider| provider.provider_kind.clone());
            return Ok(BootstrapAiBindingSelection {
                binding_purpose: purpose,
                provider_kind,
                model_catalog_id: Some(model.id),
                configured: true,
            });
        }
    }

    let suggested_model = select_bootstrap_suggested_model(purpose, provider_descriptors, models);
    Ok(BootstrapAiBindingSelection {
        binding_purpose: purpose,
        provider_kind: suggested_model.and_then(|model| {
            providers
                .iter()
                .find(|provider| provider.id == model.provider_catalog_id)
                .map(|provider| provider.provider_kind.clone())
        }),
        model_catalog_id: suggested_model.map(|model| model.id),
        configured: false,
    })
}

fn select_configured_bootstrap_model<'a>(
    binding_default: &crate::app::config::UiBootstrapAiBindingDefault,
    purpose: AiBindingPurpose,
    providers: &[ProviderCatalogEntry],
    models: &'a [ModelCatalogEntry],
) -> Result<Option<&'a ModelCatalogEntry>, ApiError> {
    let provider_catalog_id = binding_default
        .provider_kind
        .as_deref()
        .map(|provider_kind| {
            provider_id_for_kind(providers, provider_kind).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "configured bootstrap provider `{provider_kind}` is not available"
                ))
            })
        })
        .transpose()?;
    let model_name = binding_default.model_name.as_deref();

    match (provider_catalog_id, model_name) {
        (Some(provider_catalog_id), Some(model_name)) => Ok(models.iter().find(|model| {
            model.provider_catalog_id == provider_catalog_id
                && model.model_name == model_name
                && model.allowed_binding_purposes.contains(&purpose)
        })),
        (Some(provider_catalog_id), None) => {
            Ok(select_bootstrap_suggested_model_for_provider(provider_catalog_id, purpose, models))
        }
        (None, Some(model_name)) => Ok(models.iter().find(|model| {
            model.model_name == model_name && model.allowed_binding_purposes.contains(&purpose)
        })),
        (None, None) => Ok(None),
    }
}

fn select_bootstrap_suggested_model<'a>(
    purpose: AiBindingPurpose,
    providers: &[BootstrapAiProviderDescriptor],
    models: &'a [ModelCatalogEntry],
) -> Option<&'a ModelCatalogEntry> {
    models.iter().filter(|model| model.allowed_binding_purposes.contains(&purpose)).min_by(
        |left, right| {
            bootstrap_provider_priority(left.provider_catalog_id, providers)
                .cmp(&bootstrap_provider_priority(right.provider_catalog_id, providers))
                .then_with(|| {
                    bootstrap_provider_name(left.provider_catalog_id, providers)
                        .cmp(&bootstrap_provider_name(right.provider_catalog_id, providers))
                })
                .then_with(|| left.model_name.cmp(&right.model_name))
                .then_with(|| left.id.cmp(&right.id))
        },
    )
}

fn select_bootstrap_suggested_model_for_provider<'a>(
    provider_catalog_id: Uuid,
    purpose: AiBindingPurpose,
    models: &'a [ModelCatalogEntry],
) -> Option<&'a ModelCatalogEntry> {
    models
        .iter()
        .filter(|model| {
            model.provider_catalog_id == provider_catalog_id
                && model.allowed_binding_purposes.contains(&purpose)
        })
        .min_by(|left, right| {
            left.model_name.cmp(&right.model_name).then_with(|| left.id.cmp(&right.id))
        })
}

fn bootstrap_provider_priority(
    provider_catalog_id: Uuid,
    providers: &[BootstrapAiProviderDescriptor],
) -> u8 {
    providers
        .iter()
        .find(|provider| provider.provider_catalog_id == provider_catalog_id)
        .map(|provider| match provider.credential_source {
            BootstrapAiCredentialSource::Env => 0,
            BootstrapAiCredentialSource::Missing => 1,
        })
        .unwrap_or(2)
}

fn bootstrap_provider_name<'a>(
    provider_catalog_id: Uuid,
    providers: &'a [BootstrapAiProviderDescriptor],
) -> &'a str {
    providers
        .iter()
        .find(|provider| provider.provider_catalog_id == provider_catalog_id)
        .map(|provider| provider.display_name.as_str())
        .unwrap_or("")
}

fn resolve_configured_bootstrap_binding_inputs(
    configured_ai: &crate::app::config::UiBootstrapAiSetup,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiBindingInput>, ApiError> {
    let env_provider_kinds = configured_ai
        .provider_secrets
        .iter()
        .map(|secret| secret.provider_kind.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut selections = Vec::new();
    for purpose in CANONICAL_RUNTIME_BINDING_PURPOSES {
        if let Some(binding_default) =
            configured_binding_default_for_purpose(Some(configured_ai), purpose)
        {
            if let Some(model) =
                select_configured_bootstrap_model(binding_default, purpose, providers, models)?
            {
                let provider_kind = providers
                    .iter()
                    .find(|provider| provider.id == model.provider_catalog_id)
                    .map(|provider| provider.provider_kind.clone())
                    .ok_or_else(|| {
                        ApiError::resource_not_found("provider_catalog", model.provider_catalog_id)
                    })?;
                if env_provider_kinds.contains(provider_kind.as_str()) {
                    selections.push(BootstrapAiBindingInput {
                        binding_purpose: purpose,
                        provider_kind,
                        model_catalog_id: model.id,
                    });
                    continue;
                }
            }
        }

        let fallback = providers
            .iter()
            .filter(|provider| env_provider_kinds.contains(provider.provider_kind.as_str()))
            .filter_map(|provider| {
                select_bootstrap_suggested_model_for_provider(provider.id, purpose, models).map(
                    |model| BootstrapAiBindingInput {
                        binding_purpose: purpose,
                        provider_kind: provider.provider_kind.clone(),
                        model_catalog_id: model.id,
                    },
                )
            })
            .min_by(|left, right| left.provider_kind.cmp(&right.provider_kind));
        if let Some(fallback) = fallback {
            selections.push(fallback);
        }
    }
    Ok(selections)
}

fn bootstrap_binding_inputs_cover_canonical_purposes(inputs: &[BootstrapAiBindingInput]) -> bool {
    CANONICAL_RUNTIME_BINDING_PURPOSES
        .iter()
        .all(|purpose| inputs.iter().any(|selection| selection.binding_purpose == *purpose))
}

fn validate_bootstrap_binding_inputs_complete(
    inputs: &[BootstrapAiBindingInput],
) -> Result<(), ApiError> {
    if !bootstrap_binding_inputs_cover_canonical_purposes(inputs) {
        return Err(ApiError::BadRequest(
            "bootstrap binding selections must cover extract_graph, embed_chunk, query_answer, and vision"
                .to_string(),
        ));
    }
    Ok(())
}

fn normalize_bootstrap_binding_inputs(
    inputs: &[BootstrapAiBindingInput],
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiBindingInput>, ApiError> {
    let mut normalized = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for input in inputs {
        let provider_kind = input.provider_kind.trim().to_ascii_lowercase();
        if provider_kind.is_empty() {
            return Err(ApiError::BadRequest(
                "bootstrap providerKind must not be empty".to_string(),
            ));
        }
        if !seen.insert(binding_purpose_key(input.binding_purpose).to_string()) {
            return Err(ApiError::BadRequest(
                "bootstrap binding purposes must be unique".to_string(),
            ));
        }
        let provider_catalog_id =
            provider_id_for_kind(providers, &provider_kind).ok_or_else(|| {
                ApiError::resource_not_found("provider_catalog", provider_kind.clone())
            })?;
        let model = models
            .iter()
            .find(|model| model.id == input.model_catalog_id)
            .ok_or_else(|| ApiError::resource_not_found("model_catalog", input.model_catalog_id))?;
        validate_model_binding_purpose(input.binding_purpose, model)?;
        if model.provider_catalog_id != provider_catalog_id {
            return Err(ApiError::BadRequest(
                "bootstrap model selection must belong to the selected provider".to_string(),
            ));
        }
        normalized.push(BootstrapAiBindingInput {
            binding_purpose: input.binding_purpose,
            provider_kind,
            model_catalog_id: input.model_catalog_id,
        });
    }
    Ok(normalized)
}

async fn ensure_bootstrap_provider_credential(
    service: &AiCatalogService,
    state: &AppState,
    workspace_id: Uuid,
    provider: &ProviderCatalogEntry,
    api_key: Option<String>,
    existing_credentials: &[ProviderCredential],
    updated_by_principal_id: Option<Uuid>,
) -> Result<ProviderCredential, ApiError> {
    let canonical_label = format!("Bootstrap {}", provider.display_name);
    let provider_credentials =
        bootstrap_provider_credentials_for_provider(existing_credentials, provider.id);
    let canonical_credential =
        bootstrap_resolve_provider_credential(&canonical_label, &provider_credentials);
    if let Some(api_key) = api_key {
        if let Some(existing) = canonical_credential {
            return match service
                .update_provider_credential(
                    state,
                    UpdateProviderCredentialCommand {
                        credential_id: existing.id,
                        label: canonical_label.clone(),
                        api_key: Some(api_key),
                        credential_state: "active".to_string(),
                    },
                )
                .await
            {
                Ok(updated) => Ok(updated),
                Err(ApiError::Conflict(_)) => {
                    bootstrap_reload_provider_credential(
                        service,
                        state,
                        workspace_id,
                        provider,
                        &canonical_label,
                    )
                    .await
                }
                Err(error) => Err(error),
            };
        }
        return match service
            .create_provider_credential(
                state,
                CreateProviderCredentialCommand {
                    workspace_id,
                    provider_catalog_id: provider.id,
                    label: canonical_label.clone(),
                    api_key,
                    created_by_principal_id: updated_by_principal_id,
                },
            )
            .await
        {
            Ok(created) => Ok(created),
            Err(ApiError::Conflict(_)) => {
                bootstrap_reload_provider_credential(
                    service,
                    state,
                    workspace_id,
                    provider,
                    &canonical_label,
                )
                .await
            }
            Err(error) => Err(error),
        };
    }

    canonical_credential.ok_or_else(|| {
        ApiError::BadRequest(format!(
            "bootstrap ai setup requires an API key for provider {}",
            provider.provider_kind
        ))
    })
}

fn bootstrap_provider_credentials_for_provider(
    credentials: &[ProviderCredential],
    provider_catalog_id: Uuid,
) -> Vec<ProviderCredential> {
    credentials
        .iter()
        .filter(|credential| credential.provider_catalog_id == provider_catalog_id)
        .cloned()
        .collect()
}

fn bootstrap_resolve_provider_credential(
    canonical_label: &str,
    credentials: &[ProviderCredential],
) -> Option<ProviderCredential> {
    credentials
        .iter()
        .find(|credential| credential.label == canonical_label)
        .cloned()
        .or_else(|| (credentials.len() == 1).then(|| credentials[0].clone()))
        .or_else(|| {
            credentials.iter().find(|credential| credential.credential_state == "active").cloned()
        })
}

async fn bootstrap_reload_provider_credential(
    service: &AiCatalogService,
    state: &AppState,
    workspace_id: Uuid,
    provider: &ProviderCatalogEntry,
    canonical_label: &str,
) -> Result<ProviderCredential, ApiError> {
    let reloaded = service.list_provider_credentials(state, workspace_id).await?;
    bootstrap_resolve_provider_credential(
        canonical_label,
        &bootstrap_provider_credentials_for_provider(&reloaded, provider.id),
    )
    .ok_or_else(|| ApiError::Conflict("AI catalog resource already exists".to_string()))
}

fn bootstrap_find_runtime_preset(
    presets: &[ModelPreset],
    model_catalog_id: Uuid,
    canonical_name: &str,
) -> Option<ModelPreset> {
    let matching = presets
        .iter()
        .filter(|preset| preset.model_catalog_id == model_catalog_id)
        .cloned()
        .collect::<Vec<_>>();
    select_runtime_preset(&matching, canonical_name).cloned()
}

async fn ensure_bootstrap_model_preset(
    service: &AiCatalogService,
    state: &AppState,
    workspace_id: Uuid,
    model_catalog_id: Uuid,
    preset_name: &str,
    presets: &mut Vec<ModelPreset>,
    created_by_principal_id: Option<Uuid>,
) -> Result<ModelPreset, ApiError> {
    if let Some(existing) = bootstrap_find_runtime_preset(presets, model_catalog_id, preset_name) {
        return Ok(existing);
    }

    match service
        .create_model_preset(
            state,
            CreateModelPresetCommand {
                workspace_id,
                model_catalog_id,
                preset_name: preset_name.to_string(),
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens_override: None,
                extra_parameters_json: serde_json::json!({}),
                created_by_principal_id,
            },
        )
        .await
    {
        Ok(created) => {
            presets.push(created.clone());
            Ok(created)
        }
        Err(ApiError::Conflict(_)) => {
            *presets = service.list_model_presets(state, workspace_id).await?;
            bootstrap_find_runtime_preset(presets, model_catalog_id, preset_name)
                .ok_or_else(|| ApiError::Conflict("AI catalog resource already exists".to_string()))
        }
        Err(error) => Err(error),
    }
}

fn bootstrap_find_library_binding(
    bindings: &[LibraryModelBinding],
    purpose: AiBindingPurpose,
) -> Option<LibraryModelBinding> {
    bindings.iter().find(|binding| binding.binding_purpose == purpose).cloned()
}

async fn ensure_bootstrap_library_binding(
    service: &AiCatalogService,
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    binding_purpose: AiBindingPurpose,
    provider_credential_id: Uuid,
    model_preset_id: Uuid,
    bindings: &mut Vec<LibraryModelBinding>,
    updated_by_principal_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let existing = bootstrap_find_library_binding(bindings, binding_purpose);
    let operation = if let Some(existing) = existing {
        service
            .update_library_binding(
                state,
                UpdateLibraryBindingCommand {
                    binding_id: existing.id,
                    provider_credential_id,
                    model_preset_id,
                    binding_state: "active".to_string(),
                    updated_by_principal_id,
                },
            )
            .await
    } else {
        service
            .create_library_binding(
                state,
                CreateLibraryBindingCommand {
                    workspace_id,
                    library_id,
                    binding_purpose,
                    provider_credential_id,
                    model_preset_id,
                    updated_by_principal_id,
                },
            )
            .await
    };

    match operation {
        Ok(binding) => {
            if let Some(index) =
                bindings.iter().position(|entry| entry.binding_purpose == binding_purpose)
            {
                bindings[index] = binding;
            } else {
                bindings.push(binding);
            }
            Ok(())
        }
        Err(ApiError::Conflict(_)) => {
            *bindings = service.list_library_bindings(state, library_id).await?;
            let existing =
                bootstrap_find_library_binding(bindings, binding_purpose).ok_or_else(|| {
                    ApiError::Conflict("AI catalog resource already exists".to_string())
                })?;
            let updated = service
                .update_library_binding(
                    state,
                    UpdateLibraryBindingCommand {
                        binding_id: existing.id,
                        provider_credential_id,
                        model_preset_id,
                        binding_state: "active".to_string(),
                        updated_by_principal_id,
                    },
                )
                .await?;
            if let Some(index) =
                bindings.iter().position(|entry| entry.binding_purpose == binding_purpose)
            {
                bindings[index] = updated;
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn parse_allowed_binding_purposes(
    metadata_json: &Value,
) -> Result<Vec<AiBindingPurpose>, ApiError> {
    let roles = metadata_json
        .get("defaultRoles")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::Internal)?;
    if roles.is_empty() {
        return Err(ApiError::Internal);
    }

    let mut allowed = Vec::with_capacity(roles.len());
    for role in roles {
        let role = role.as_str().ok_or_else(|| ApiError::Internal)?;
        let purpose = parse_binding_purpose(role)?;
        if !allowed.contains(&purpose) {
            allowed.push(purpose);
        }
    }
    Ok(allowed)
}

fn validate_model_binding_purpose(
    binding_purpose: AiBindingPurpose,
    model: &ModelCatalogEntry,
) -> Result<(), ApiError> {
    if model.allowed_binding_purposes.contains(&binding_purpose) {
        return Ok(());
    }

    let allowed = model
        .allowed_binding_purposes
        .iter()
        .map(|purpose| binding_purpose_key(*purpose))
        .collect::<Vec<_>>()
        .join(", ");
    Err(ApiError::BadRequest(format!(
        "binding purpose {} is incompatible with model {}; allowed purposes: {}",
        binding_purpose_key(binding_purpose),
        model.model_name,
        allowed,
    )))
}

#[cfg(test)]
mod tests {
    use super::{
        BootstrapAiBindingInput, BootstrapAiCredentialSource, BootstrapAiProviderDescriptor,
        bootstrap_binding_inputs_cover_canonical_purposes, parse_allowed_binding_purposes,
        resolve_bootstrap_binding_suggestion, validate_bootstrap_binding_inputs_complete,
        validate_model_binding_purpose,
    };
    use crate::app::config::UiBootstrapAiBindingDefault;
    use crate::domains::ai::{AiBindingPurpose, ModelCatalogEntry};
    use crate::interfaces::http::router_support::ApiError;
    use uuid::Uuid;

    fn sample_model(allowed_binding_purposes: Vec<AiBindingPurpose>) -> ModelCatalogEntry {
        ModelCatalogEntry {
            id: Uuid::nil(),
            provider_catalog_id: Uuid::nil(),
            model_name: "sample-model".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes,
            context_window: None,
            max_output_tokens: None,
        }
    }

    fn sample_provider(
        provider_kind: &str,
        credential_source: BootstrapAiCredentialSource,
    ) -> BootstrapAiProviderDescriptor {
        BootstrapAiProviderDescriptor {
            provider_catalog_id: Uuid::now_v7(),
            provider_kind: provider_kind.to_string(),
            display_name: provider_kind.to_string(),
            api_style: "openai_compatible".to_string(),
            lifecycle_state: "active".to_string(),
            credential_source,
        }
    }

    #[test]
    fn parses_allowed_binding_purposes_from_default_roles() {
        let metadata = serde_json::json!({
            "defaultRoles": ["extract_graph", "query_answer"]
        });
        let purposes =
            parse_allowed_binding_purposes(&metadata).expect("defaultRoles should parse");
        assert_eq!(purposes, vec![AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer]);
    }

    #[test]
    fn rejects_incompatible_binding_purpose() {
        let model = sample_model(vec![AiBindingPurpose::EmbedChunk]);
        let error = validate_model_binding_purpose(AiBindingPurpose::ExtractGraph, &model)
            .expect_err("incompatible purpose should fail");
        assert!(matches!(error, ApiError::BadRequest(_)));
        assert!(format!("{error:?}").contains("incompatible"));
    }

    #[test]
    fn bootstrap_binding_inputs_must_cover_all_canonical_purposes() {
        let inputs = vec![
            BootstrapAiBindingInput {
                binding_purpose: AiBindingPurpose::ExtractGraph,
                provider_kind: "openai".to_string(),
                model_catalog_id: Uuid::now_v7(),
            },
            BootstrapAiBindingInput {
                binding_purpose: AiBindingPurpose::EmbedChunk,
                provider_kind: "openai".to_string(),
                model_catalog_id: Uuid::now_v7(),
            },
            BootstrapAiBindingInput {
                binding_purpose: AiBindingPurpose::QueryAnswer,
                provider_kind: "openai".to_string(),
                model_catalog_id: Uuid::now_v7(),
            },
        ];

        assert!(!bootstrap_binding_inputs_cover_canonical_purposes(&inputs));
        assert!(matches!(
            validate_bootstrap_binding_inputs_complete(&inputs),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn bootstrap_binding_suggestion_marks_env_configured_defaults() {
        let provider = sample_provider("openai", BootstrapAiCredentialSource::Env);
        let model = ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: provider.provider_catalog_id,
            model_name: "gpt-5.4".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::QueryAnswer],
            context_window: None,
            max_output_tokens: None,
        };
        let configured = crate::app::config::UiBootstrapAiSetup {
            provider_secrets: vec![],
            binding_defaults: vec![UiBootstrapAiBindingDefault {
                binding_purpose: "query_answer".to_string(),
                provider_kind: Some("openai".to_string()),
                model_name: Some("gpt-5.4".to_string()),
            }],
        };

        let selection = resolve_bootstrap_binding_suggestion(
            AiBindingPurpose::QueryAnswer,
            Some(&configured),
            std::slice::from_ref(&provider),
            &[crate::domains::ai::ProviderCatalogEntry {
                id: provider.provider_catalog_id,
                provider_kind: provider.provider_kind.clone(),
                display_name: provider.display_name.clone(),
                api_style: provider.api_style.clone(),
                lifecycle_state: provider.lifecycle_state.clone(),
            }],
            &[model],
        )
        .expect("configured suggestion should resolve");

        assert_eq!(selection.provider_kind.as_deref(), Some("openai"));
        assert!(selection.configured);
    }

    #[test]
    fn bootstrap_binding_suggestion_keeps_openai_available_for_extract_graph() {
        let openai_provider = sample_provider("openai", BootstrapAiCredentialSource::Env);
        let deepseek_provider = sample_provider("deepseek", BootstrapAiCredentialSource::Missing);
        let qwen_provider = sample_provider("qwen", BootstrapAiCredentialSource::Missing);
        let providers = vec![
            crate::domains::ai::ProviderCatalogEntry {
                id: openai_provider.provider_catalog_id,
                provider_kind: openai_provider.provider_kind.clone(),
                display_name: openai_provider.display_name.clone(),
                api_style: openai_provider.api_style.clone(),
                lifecycle_state: openai_provider.lifecycle_state.clone(),
            },
            crate::domains::ai::ProviderCatalogEntry {
                id: deepseek_provider.provider_catalog_id,
                provider_kind: deepseek_provider.provider_kind.clone(),
                display_name: deepseek_provider.display_name.clone(),
                api_style: deepseek_provider.api_style.clone(),
                lifecycle_state: deepseek_provider.lifecycle_state.clone(),
            },
            crate::domains::ai::ProviderCatalogEntry {
                id: qwen_provider.provider_catalog_id,
                provider_kind: qwen_provider.provider_kind.clone(),
                display_name: qwen_provider.display_name.clone(),
                api_style: qwen_provider.api_style.clone(),
                lifecycle_state: qwen_provider.lifecycle_state.clone(),
            },
        ];
        let models = vec![
            ModelCatalogEntry {
                id: Uuid::now_v7(),
                provider_catalog_id: openai_provider.provider_catalog_id,
                model_name: "gpt-5.4".to_string(),
                capability_kind: "chat".to_string(),
                modality_kind: "multimodal".to_string(),
                allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
                context_window: None,
                max_output_tokens: None,
            },
            ModelCatalogEntry {
                id: Uuid::now_v7(),
                provider_catalog_id: deepseek_provider.provider_catalog_id,
                model_name: "deepseek-chat".to_string(),
                capability_kind: "chat".to_string(),
                modality_kind: "text".to_string(),
                allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
                context_window: None,
                max_output_tokens: None,
            },
            ModelCatalogEntry {
                id: Uuid::now_v7(),
                provider_catalog_id: qwen_provider.provider_catalog_id,
                model_name: "qwen-flash".to_string(),
                capability_kind: "chat".to_string(),
                modality_kind: "text".to_string(),
                allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
                context_window: None,
                max_output_tokens: None,
            },
        ];
        let configured = crate::app::config::UiBootstrapAiSetup {
            provider_secrets: vec![],
            binding_defaults: vec![UiBootstrapAiBindingDefault {
                binding_purpose: "extract_graph".to_string(),
                provider_kind: Some("openai".to_string()),
                model_name: Some("gpt-5.4".to_string()),
            }],
        };

        let selection = resolve_bootstrap_binding_suggestion(
            AiBindingPurpose::ExtractGraph,
            Some(&configured),
            &[openai_provider, deepseek_provider, qwen_provider],
            &providers,
            &models,
        )
        .expect("configured extract_graph suggestion should resolve");

        assert_eq!(selection.provider_kind.as_deref(), Some("openai"));
        assert!(selection.configured);
    }
}
