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
        let credential = map_provider_credential_row(row);
        self.ensure_workspace_runtime_profiles(
            state,
            credential.workspace_id,
            command.created_by_principal_id,
        )
        .await?;
        Ok(credential)
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
        let credential = map_provider_credential_row(row);
        self.ensure_workspace_runtime_profiles(state, credential.workspace_id, None).await?;
        Ok(credential)
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
            let preset_name = canonical_runtime_preset_name(&provider.display_name, purpose);
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

fn binding_purpose_key(value: AiBindingPurpose) -> &'static str {
    match value {
        AiBindingPurpose::ExtractText => "extract_text",
        AiBindingPurpose::ExtractGraph => "extract_graph",
        AiBindingPurpose::EmbedChunk => "embed_chunk",
        AiBindingPurpose::QueryRetrieve => "query_retrieve",
        AiBindingPurpose::QueryAnswer => "query_answer",
        AiBindingPurpose::Vision => "vision",
    }
}

fn canonical_runtime_preset_name(provider_display_name: &str, purpose: AiBindingPurpose) -> String {
    let purpose_label = match purpose {
        AiBindingPurpose::ExtractText => "Extract Text",
        AiBindingPurpose::ExtractGraph => "Extract Graph",
        AiBindingPurpose::EmbedChunk => "Embed Chunk",
        AiBindingPurpose::QueryRetrieve => "Query Retrieve",
        AiBindingPurpose::QueryAnswer => "Query Answer",
        AiBindingPurpose::Vision => "Vision",
    };
    format!("{provider_display_name} {purpose_label}")
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
    use super::{parse_allowed_binding_purposes, validate_model_binding_purpose};
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
}
