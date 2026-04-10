#![allow(
    clippy::iter_without_into_iter,
    clippy::missing_errors_doc,
    clippy::result_large_err,
    clippy::too_many_lines
)]

use reqwest::{Client, Url};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::{
        AiBindingAssignment, AiBindingPurpose, AiScopeKind, BindingValidation,
        ModelAvailabilityState, ModelCatalogEntry, ModelPreset, PriceCatalogEntry,
        ProviderCatalogEntry, ProviderCredential, ResolvedModelCatalogEntry,
    },
    infra::repositories::{ai_repository, catalog_repository},
    integrations::llm::ChatRequest,
    interfaces::http::router_support::ApiError,
    shared::provider_base_url::provider_base_url_candidates,
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

    pub async fn list_resolved_model_catalog(
        &self,
        state: &AppState,
        provider_catalog_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
        credential_id: Option<Uuid>,
    ) -> Result<Vec<ResolvedModelCatalogEntry>, ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let provider_by_id =
            providers.iter().map(|provider| (provider.id, provider)).collect::<HashMap<_, _>>();
        let visible_credentials =
            self.list_visible_provider_credentials(state, workspace_id, library_id).await?;
        let discovery_credentials = match credential_id {
            Some(credential_id) => vec![
                visible_credentials
                    .iter()
                    .find(|credential| credential.id == credential_id)
                    .cloned()
                    .ok_or_else(|| {
                        ApiError::resource_not_found("provider_credential", credential_id)
                    })?,
            ],
            None => visible_credentials.clone(),
        };

        let mut availability_by_model = HashMap::<(Uuid, String), BTreeSet<Uuid>>::new();
        let mut checked_ollama_providers = BTreeSet::<Uuid>::new();

        for credential in discovery_credentials
            .iter()
            .filter(|credential| credential.credential_state == "active")
        {
            let Some(provider) = provider_by_id.get(&credential.provider_catalog_id) else {
                continue;
            };
            if provider.provider_kind != "ollama" {
                continue;
            }
            if provider_catalog_id.is_some_and(|value| value != provider.id) {
                continue;
            }
            let Some(base_url) =
                credential.base_url.as_deref().or(provider.default_base_url.as_deref())
            else {
                continue;
            };
            let model_names =
                match fetch_provider_model_names(provider, credential.api_key.as_deref(), base_url)
                    .await
                {
                    Ok(model_names) => model_names,
                    Err(error) => {
                        tracing::warn!(
                            provider_kind = %provider.provider_kind,
                            credential_id = %credential.id,
                            error = %error,
                            "failed to discover provider models"
                        );
                        continue;
                    }
                };
            checked_ollama_providers.insert(provider.id);
            for model_name in model_names {
                ensure_discovered_ollama_model_catalog_entry(
                    state,
                    provider.id,
                    model_name.as_str(),
                )
                .await?;
                availability_by_model
                    .entry((provider.id, model_name))
                    .or_default()
                    .insert(credential.id);
            }
        }

        let models = self.list_model_catalog(state, provider_catalog_id).await?;
        Ok(models
            .into_iter()
            .map(|model| {
                let available_credential_ids = availability_by_model
                    .get(&(model.provider_catalog_id, model.model_name.clone()))
                    .map(|credential_ids| credential_ids.iter().copied().collect::<Vec<_>>())
                    .unwrap_or_default();
                let availability_state = match provider_by_id
                    .get(&model.provider_catalog_id)
                    .map(|provider| provider.provider_kind.as_str())
                {
                    Some("ollama")
                        if checked_ollama_providers.contains(&model.provider_catalog_id) =>
                    {
                        if available_credential_ids.is_empty() {
                            ModelAvailabilityState::Unavailable
                        } else {
                            ModelAvailabilityState::Available
                        }
                    }
                    Some("ollama") => ModelAvailabilityState::Unknown,
                    _ => ModelAvailabilityState::Available,
                };
                ResolvedModelCatalogEntry { model, availability_state, available_credential_ids }
            })
            .collect())
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

    pub async fn list_provider_credentials_exact(
        &self,
        state: &AppState,
        scope: AiScopeRef,
    ) -> Result<Vec<ProviderCredential>, ApiError> {
        let rows = ai_repository::list_provider_credentials_exact(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_provider_credential_row).collect())
    }

    pub async fn list_visible_provider_credentials(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<ProviderCredential>, ApiError> {
        let rows = ai_repository::list_visible_provider_credentials(
            &state.persistence.postgres,
            workspace_id,
            library_id,
        )
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
        let scope = normalize_scope_ref(
            state,
            command.scope_kind,
            command.workspace_id,
            command.library_id,
        )
        .await?;
        let label = normalize_non_empty(&command.label, "label")?;
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, Some(command.provider_catalog_id)).await?;
        let provider =
            providers.iter().find(|entry| entry.id == command.provider_catalog_id).ok_or_else(
                || ApiError::resource_not_found("provider_catalog", command.provider_catalog_id),
            )?;
        let api_key = normalize_optional(command.api_key.as_deref());
        let base_url = resolve_provider_base_url(provider, command.base_url.as_deref())?;
        validate_provider_access(state, provider, &models, api_key.as_deref(), base_url.as_deref())
            .await?;
        let row = ai_repository::create_provider_credential(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
            command.provider_catalog_id,
            &label,
            api_key.as_deref(),
            base_url.as_deref(),
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
        let existing = self.get_provider_credential(state, command.credential_id).await?;
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, Some(existing.provider_catalog_id)).await?;
        let provider =
            providers.iter().find(|entry| entry.id == existing.provider_catalog_id).ok_or_else(
                || ApiError::resource_not_found("provider_catalog", existing.provider_catalog_id),
            )?;
        let api_key = normalize_optional(command.api_key.as_deref());
        let base_url = normalize_provider_base_url_input(provider, command.base_url.as_deref())?;
        let effective_api_key = api_key.as_deref().or(existing.api_key.as_deref());
        let effective_base_url = base_url
            .as_deref()
            .or(existing.base_url.as_deref())
            .or(provider.default_base_url.as_deref());
        validate_provider_access(state, provider, &models, effective_api_key, effective_base_url)
            .await?;
        let row = ai_repository::update_provider_credential(
            &state.persistence.postgres,
            command.credential_id,
            &label,
            api_key.as_deref(),
            base_url.as_deref(),
            &command.credential_state,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| {
            ApiError::resource_not_found("provider_credential", command.credential_id)
        })?;
        Ok(map_provider_credential_row(row))
    }

    pub async fn list_model_presets_exact(
        &self,
        state: &AppState,
        scope: AiScopeRef,
    ) -> Result<Vec<ModelPreset>, ApiError> {
        let rows = ai_repository::list_model_presets_exact(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_model_preset_row).collect())
    }

    pub async fn list_visible_model_presets(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<ModelPreset>, ApiError> {
        let rows = ai_repository::list_visible_model_presets(
            &state.persistence.postgres,
            workspace_id,
            library_id,
        )
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
        let scope = normalize_scope_ref(
            state,
            command.scope_kind,
            command.workspace_id,
            command.library_id,
        )
        .await?;
        let preset_name = normalize_non_empty(&command.preset_name, "presetName")?;
        let row = ai_repository::create_model_preset(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
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
        .map_err(|_| ApiError::Internal)?;
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
                .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?
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
                .map_err(|_| ApiError::Internal)?;
        if !deleted {
            return Err(ApiError::resource_not_found("binding_assignment", binding_id));
        }
        Ok(())
    }

    pub async fn describe_bootstrap_ai_setup(
        &self,
        state: &AppState,
    ) -> Result<BootstrapAiSetupDescriptor, ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let configured_ai = state.ui_bootstrap_ai_setup.as_ref();
        let mut preset_bundles = Vec::new();
        for provider in &providers {
            if let Some(bundle) = resolve_bootstrap_provider_preset_bundle(
                provider,
                &models,
                bootstrap_credential_source(configured_ai, &provider.provider_kind),
            )? {
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
                provider_kind: bundle.provider_kind.clone(),
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
                .map_err(|_| ApiError::Internal)?
        else {
            return Ok(None);
        };
        let Some(binding) = ai_repository::get_effective_binding_assignment_by_purpose(
            &state.persistence.postgres,
            library_id,
            binding_purpose_key(binding_purpose),
        )
        .await
        .map_err(|_| ApiError::Internal)?
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
                .map_err(|_| ApiError::Internal)?
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
            .map_err(|_| ApiError::Internal)?
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

fn normalize_provider_base_url_input(
    provider: &ProviderCatalogEntry,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    normalize_optional(value)
        .map(|candidate| canonicalize_provider_base_url(provider, &candidate))
        .transpose()
}

fn resolve_provider_base_url(
    provider: &ProviderCatalogEntry,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    if let Some(base_url) = normalize_provider_base_url_input(provider, value)? {
        return Ok(Some(base_url));
    }
    if provider.base_url_required {
        return provider
            .default_base_url
            .as_deref()
            .map(|candidate| canonicalize_provider_base_url(provider, candidate))
            .transpose()
            .and_then(|base_url| {
                base_url.ok_or_else(|| {
                    ApiError::BadRequest(format!(
                        "provider {} requires a baseUrl",
                        provider.provider_kind
                    ))
                })
            })
            .map(Some);
    }
    Ok(None)
}

fn canonicalize_provider_base_url(
    provider: &ProviderCatalogEntry,
    value: &str,
) -> Result<String, ApiError> {
    let mut url = Url::parse(value).map_err(|_| {
        ApiError::BadRequest(format!(
            "baseUrl must be a valid absolute URL for provider {}",
            provider.provider_kind
        ))
    })?;
    if matches!(url.scheme(), "http" | "https") {
        if provider.provider_kind == "ollama" {
            let mut path_segments = url
                .path_segments()
                .map(|segments| segments.filter(|segment| !segment.is_empty()).collect::<Vec<_>>())
                .unwrap_or_default();
            match path_segments.last().copied() {
                Some("v1") => {}
                Some("api") => {
                    path_segments.pop();
                    path_segments.push("v1");
                }
                _ => path_segments.push("v1"),
            }
            url.set_path(&format!("/{}", path_segments.join("/")));
        }
        if url.path() != "/" {
            let trimmed_path = url.path().trim_end_matches('/').to_string();
            url.set_path(&trimmed_path);
        }
        return Ok(url.to_string().trim_end_matches('/').to_string());
    }
    Err(ApiError::BadRequest(format!(
        "baseUrl must use http or https for provider {}",
        provider.provider_kind
    )))
}

fn discovered_ollama_model_signature(
    model_name: &str,
) -> (&'static str, &'static str, Vec<AiBindingPurpose>) {
    let normalized = model_name.trim().to_ascii_lowercase();
    if normalized.contains("embedding") {
        return ("embedding", "text", vec![AiBindingPurpose::EmbedChunk]);
    }
    if normalized.contains("vl")
        || normalized.contains("vision")
        || normalized.contains("llava")
        || normalized.contains("minicpm-v")
    {
        return ("chat", "multimodal", vec![AiBindingPurpose::Vision]);
    }
    ("chat", "text", vec![AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer])
}

async fn ensure_discovered_ollama_model_catalog_entry(
    state: &AppState,
    provider_catalog_id: Uuid,
    model_name: &str,
) -> Result<(), ApiError> {
    let (capability_kind, modality_kind, allowed_binding_purposes) =
        discovered_ollama_model_signature(model_name);
    let metadata_json = json!({
        "defaultRoles": allowed_binding_purposes
            .iter()
            .map(|purpose| purpose.as_str())
            .collect::<Vec<_>>(),
        "seedSource": "provider_discovery",
    });
    ai_repository::upsert_model_catalog(
        &state.persistence.postgres,
        provider_catalog_id,
        model_name,
        capability_kind,
        modality_kind,
        metadata_json,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(())
}

async fn fetch_provider_model_names(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
) -> Result<Vec<String>, ApiError> {
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| Client::new());

    let candidate_urls = provider_base_url_candidates(&provider.provider_kind, base_url);
    let mut last_error = None;
    for candidate_url in candidate_urls {
        let request = client.get(format!("{candidate_url}/models"));
        let request = if let Some(token) = normalize_optional(api_key) {
            request.bearer_auth(token)
        } else {
            request
        };
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    last_error = Some(ApiError::BadRequest(format!(
                        "provider credential validation failed for {}: status={} body={body}",
                        provider.display_name, status
                    )));
                    continue;
                }

                let body = response.json::<Value>().await.map_err(|error| {
                    ApiError::BadRequest(format!(
                        "provider credential validation failed for {}: invalid model list response: {error}",
                        provider.display_name
                    ))
                })?;
                let mut model_names = body
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(ToString::to_string)
                    })
                    .collect::<Vec<_>>();
                model_names.sort();
                model_names.dedup();
                return Ok(model_names);
            }
            Err(error) => {
                last_error = Some(ApiError::BadRequest(format!(
                    "provider credential validation failed for {}: {error}",
                    provider.display_name
                )));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        ApiError::BadRequest(format!(
            "provider credential validation failed for {}",
            provider.display_name
        ))
    }))
}

fn is_loopback_base_url(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .and_then(|url| {
            url.host().map(|host| match host {
                url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
                url::Host::Ipv4(host) => host.is_loopback(),
                url::Host::Ipv6(host) => host.is_loopback(),
            })
        })
        .unwrap_or(false)
}

fn ollama_loopback_runtime_error(provider: &ProviderCatalogEntry) -> ApiError {
    ApiError::BadRequest(format!(
        "provider credential validation failed for {}: RustRAG cannot reach an Ollama server bound only to host localhost from inside Docker; expose Ollama on 0.0.0.0:11434 or run Ollama in Docker, then use a host-reachable URL such as http://host.docker.internal:11434",
        provider.display_name
    ))
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
    let policy = provider_credential_policy(&row.provider_kind);
    ProviderCatalogEntry {
        id: row.id,
        provider_kind: row.provider_kind,
        display_name: row.display_name,
        api_style: row.api_style,
        lifecycle_state: row.lifecycle_state,
        default_base_url: row.default_base_url,
        api_key_required: policy.api_key_required,
        base_url_required: policy.base_url_required,
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
        scope_kind: parse_scope_kind(&row.scope_kind).unwrap_or(AiScopeKind::Workspace),
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        provider_catalog_id: row.provider_catalog_id,
        label: row.label,
        api_key: row.api_key,
        base_url: row.base_url,
        credential_state: row.credential_state,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_model_preset_row(row: ai_repository::AiModelPresetRow) -> ModelPreset {
    ModelPreset {
        id: row.id,
        scope_kind: parse_scope_kind(&row.scope_kind).unwrap_or(AiScopeKind::Workspace),
        workspace_id: row.workspace_id,
        library_id: row.library_id,
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

fn map_binding_assignment_row(
    row: ai_repository::AiBindingAssignmentRow,
) -> Result<AiBindingAssignment, ApiError> {
    Ok(AiBindingAssignment {
        id: row.id,
        scope_kind: parse_scope_kind(&row.scope_kind)?,
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

fn parse_scope_kind(value: &str) -> Result<AiScopeKind, ApiError> {
    match value {
        "instance" => Ok(AiScopeKind::Instance),
        "workspace" => Ok(AiScopeKind::Workspace),
        "library" => Ok(AiScopeKind::Library),
        _ => Err(ApiError::Internal),
    }
}

fn scope_kind_key(value: AiScopeKind) -> &'static str {
    value.as_str()
}

async fn normalize_scope_ref(
    state: &AppState,
    scope_kind: AiScopeKind,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<AiScopeRef, ApiError> {
    match scope_kind {
        AiScopeKind::Instance => {
            if workspace_id.is_some() || library_id.is_some() {
                return Err(ApiError::BadRequest(
                    "instance scope must not include workspaceId or libraryId".to_string(),
                ));
            }
            Ok(AiScopeRef { scope_kind, workspace_id: None, library_id: None })
        }
        AiScopeKind::Workspace => {
            let workspace_id = workspace_id.ok_or_else(|| {
                ApiError::BadRequest("workspace scope requires workspaceId".to_string())
            })?;
            if library_id.is_some() {
                return Err(ApiError::BadRequest(
                    "workspace scope must not include libraryId".to_string(),
                ));
            }
            let exists =
                catalog_repository::get_workspace_by_id(&state.persistence.postgres, workspace_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .is_some();
            if !exists {
                return Err(ApiError::resource_not_found("workspace", workspace_id));
            }
            Ok(AiScopeRef { scope_kind, workspace_id: Some(workspace_id), library_id: None })
        }
        AiScopeKind::Library => {
            let library_id = library_id.ok_or_else(|| {
                ApiError::BadRequest("library scope requires libraryId".to_string())
            })?;
            let library =
                catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
            if let Some(expected_workspace_id) = workspace_id {
                if expected_workspace_id != library.workspace_id {
                    return Err(ApiError::BadRequest(
                        "libraryId does not belong to workspaceId".to_string(),
                    ));
                }
            }
            Ok(AiScopeRef {
                scope_kind,
                workspace_id: Some(library.workspace_id),
                library_id: Some(library.id),
            })
        }
    }
}

fn scope_ref_from_binding_row(
    row: &ai_repository::AiBindingAssignmentRow,
) -> Result<AiScopeRef, ApiError> {
    Ok(AiScopeRef {
        scope_kind: parse_scope_kind(&row.scope_kind)?,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
    })
}

fn scope_ref_from_provider_credential(
    credential: &ProviderCredential,
) -> Result<AiScopeRef, ApiError> {
    Ok(AiScopeRef {
        scope_kind: credential.scope_kind,
        workspace_id: credential.workspace_id,
        library_id: credential.library_id,
    })
}

fn scope_ref_from_model_preset(preset: &ModelPreset) -> Result<AiScopeRef, ApiError> {
    Ok(AiScopeRef {
        scope_kind: preset.scope_kind,
        workspace_id: preset.workspace_id,
        library_id: preset.library_id,
    })
}

fn scope_can_use_resource(owner_scope: AiScopeRef, resource_scope: AiScopeRef) -> bool {
    match owner_scope.scope_kind {
        AiScopeKind::Instance => resource_scope.scope_kind == AiScopeKind::Instance,
        AiScopeKind::Workspace => {
            resource_scope.scope_kind == AiScopeKind::Instance
                || (resource_scope.scope_kind == AiScopeKind::Workspace
                    && resource_scope.workspace_id == owner_scope.workspace_id)
        }
        AiScopeKind::Library => {
            resource_scope.scope_kind == AiScopeKind::Instance
                || (resource_scope.scope_kind == AiScopeKind::Workspace
                    && resource_scope.workspace_id == owner_scope.workspace_id)
                || (resource_scope.scope_kind == AiScopeKind::Library
                    && resource_scope.library_id == owner_scope.library_id)
        }
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

fn bootstrap_credential_source(
    configured_ai: Option<&crate::app::config::UiBootstrapAiSetup>,
    provider_kind: &str,
) -> BootstrapAiCredentialSource {
    if bootstrap_provider_secret(configured_ai, provider_kind).is_some() {
        BootstrapAiCredentialSource::Env
    } else {
        BootstrapAiCredentialSource::Missing
    }
}

fn bootstrap_provider_credential_map(
    configured_ai: Option<&crate::app::config::UiBootstrapAiSetup>,
    credential_inputs: &[BootstrapAiCredentialInput],
) -> std::collections::HashMap<String, BootstrapAiCredentialInput> {
    let mut credentials = configured_ai
        .map(|config| {
            config
                .provider_secrets
                .iter()
                .map(|secret| {
                    (
                        secret.provider_kind.clone(),
                        BootstrapAiCredentialInput {
                            provider_kind: secret.provider_kind.clone(),
                            api_key: Some(secret.api_key.clone()),
                            base_url: None,
                        },
                    )
                })
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();
    for credential in credential_inputs {
        let provider_kind = credential.provider_kind.trim().to_ascii_lowercase();
        let api_key = normalize_optional(credential.api_key.as_deref());
        let base_url = normalize_optional(credential.base_url.as_deref());
        if api_key.is_some() || base_url.is_some() {
            credentials.insert(
                provider_kind.clone(),
                BootstrapAiCredentialInput { provider_kind, api_key, base_url },
            );
        }
    }
    credentials
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

fn select_provider_validation_model<'a>(
    provider: &ProviderCatalogEntry,
    models: &'a [ModelCatalogEntry],
) -> Option<&'a ModelCatalogEntry> {
    for purpose in
        [AiBindingPurpose::QueryAnswer, AiBindingPurpose::ExtractGraph, AiBindingPurpose::Vision]
    {
        if let Some(profile) =
            bootstrap_preset_profile_for_purpose(&provider.provider_kind, purpose)
        {
            if let Some(model) = models.iter().find(|entry| {
                entry.provider_catalog_id == provider.id && entry.model_name == profile.model_name
            }) {
                return Some(model);
            }
        }
    }

    models
        .iter()
        .filter(|model| model.provider_catalog_id == provider.id && model.capability_kind == "chat")
        .min_by(|left, right| {
            left.model_name.cmp(&right.model_name).then_with(|| left.id.cmp(&right.id))
        })
}

#[derive(Clone, Copy)]
struct BootstrapProviderPresetProfile {
    purpose: AiBindingPurpose,
    model_name: &'static str,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_output_tokens_override: Option<i32>,
}

const OPENAI_BOOTSTRAP_PRESET_PROFILE: [BootstrapProviderPresetProfile; 4] = [
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::ExtractGraph,
        model_name: "gpt-5.4-nano",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::EmbedChunk,
        model_name: "text-embedding-3-large",
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::QueryAnswer,
        model_name: "gpt-5.4-mini",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::Vision,
        model_name: "gpt-5.4-mini",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
];

const QWEN_BOOTSTRAP_PRESET_PROFILE: [BootstrapProviderPresetProfile; 4] = [
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::ExtractGraph,
        model_name: "qwen-flash",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::EmbedChunk,
        model_name: "text-embedding-v4",
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::QueryAnswer,
        model_name: "qwen3-max",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::Vision,
        model_name: "qwen-vl-max",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
];

const OLLAMA_BOOTSTRAP_PRESET_PROFILE: [BootstrapProviderPresetProfile; 4] = [
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::ExtractGraph,
        model_name: "qwen3:0.6b",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::EmbedChunk,
        model_name: "qwen3-embedding:0.6b",
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::QueryAnswer,
        model_name: "qwen3:0.6b",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
    BootstrapProviderPresetProfile {
        purpose: AiBindingPurpose::Vision,
        model_name: "qwen3-vl:2b",
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens_override: None,
    },
];

fn bootstrap_provider_preset_profile(
    provider_kind: &str,
) -> Option<&'static [BootstrapProviderPresetProfile]> {
    match provider_kind {
        "openai" => Some(&OPENAI_BOOTSTRAP_PRESET_PROFILE),
        "ollama" => Some(&OLLAMA_BOOTSTRAP_PRESET_PROFILE),
        "qwen" => Some(&QWEN_BOOTSTRAP_PRESET_PROFILE),
        _ => None,
    }
}

fn bootstrap_preset_profile_for_purpose(
    provider_kind: &str,
    purpose: AiBindingPurpose,
) -> Option<BootstrapProviderPresetProfile> {
    bootstrap_provider_preset_profile(provider_kind)
        .and_then(|profiles| profiles.iter().find(|profile| profile.purpose == purpose).copied())
}

fn resolve_bootstrap_provider_preset_bundle(
    provider: &ProviderCatalogEntry,
    models: &[ModelCatalogEntry],
    credential_source: BootstrapAiCredentialSource,
) -> Result<Option<BootstrapAiProviderPresetBundle>, ApiError> {
    let Some(profile) = bootstrap_provider_preset_profile(&provider.provider_kind) else {
        return Ok(None);
    };

    let mut presets = Vec::with_capacity(profile.len());
    for preset_profile in profile {
        let Some(model) = models.iter().find(|model| {
            model.provider_catalog_id == provider.id
                && model.model_name == preset_profile.model_name
        }) else {
            return Ok(None);
        };
        if !model.allowed_binding_purposes.contains(&preset_profile.purpose) {
            return Ok(None);
        }
        presets.push(BootstrapAiPresetDescriptor {
            binding_purpose: preset_profile.purpose,
            model_catalog_id: model.id,
            model_name: model.model_name.clone(),
            preset_name: canonical_runtime_preset_name(
                &provider.display_name,
                preset_profile.purpose,
                &model.model_name,
            ),
            system_prompt: None,
            temperature: preset_profile.temperature,
            top_p: preset_profile.top_p,
            max_output_tokens_override: preset_profile.max_output_tokens_override,
        });
    }

    Ok(Some(BootstrapAiProviderPresetBundle {
        provider_catalog_id: provider.id,
        provider_kind: provider.provider_kind.clone(),
        display_name: provider.display_name.clone(),
        credential_source,
        default_base_url: provider.default_base_url.clone(),
        api_key_required: provider.api_key_required,
        base_url_required: provider.base_url_required,
        presets,
    }))
}

fn resolve_bootstrap_provider_bundle(
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
    provider_kind: &str,
) -> Result<BootstrapAiProviderPresetBundle, ApiError> {
    let normalized_provider_kind = provider_kind.trim().to_ascii_lowercase();
    let provider =
        providers.iter().find(|entry| entry.provider_kind == normalized_provider_kind).ok_or_else(
            || ApiError::resource_not_found("provider_catalog", normalized_provider_kind.clone()),
        )?;
    resolve_bootstrap_provider_preset_bundle(
        provider,
        models,
        BootstrapAiCredentialSource::Missing,
    )?
    .ok_or_else(|| {
        ApiError::BadRequest(format!(
            "provider {normalized_provider_kind} does not expose a complete bootstrap preset bundle",
        ))
    })
}

fn build_bootstrap_preset_input(
    provider: &ProviderCatalogEntry,
    model: &ModelCatalogEntry,
    purpose: AiBindingPurpose,
) -> BootstrapAiPresetInput {
    let preset_profile = bootstrap_preset_profile_for_purpose(&provider.provider_kind, purpose)
        .filter(|profile| profile.model_name == model.model_name);
    BootstrapAiPresetInput {
        binding_purpose: purpose,
        provider_kind: provider.provider_kind.clone(),
        model_catalog_id: model.id,
        preset_name: canonical_runtime_preset_name(
            &provider.display_name,
            purpose,
            &model.model_name,
        ),
        system_prompt: None,
        temperature: preset_profile.and_then(|profile| profile.temperature),
        top_p: preset_profile.and_then(|profile| profile.top_p),
        max_output_tokens_override: preset_profile
            .and_then(|profile| profile.max_output_tokens_override),
        extra_parameters_json: json!({}),
    }
}

fn resolve_configured_bootstrap_preset_inputs(
    configured_ai: &crate::app::config::UiBootstrapAiSetup,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiPresetInput>, ApiError> {
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
                    let provider = providers
                        .iter()
                        .find(|entry| entry.provider_kind == provider_kind)
                        .ok_or_else(|| {
                            ApiError::resource_not_found("provider_catalog", provider_kind.clone())
                        })?;
                    selections.push(build_bootstrap_preset_input(provider, model, purpose));
                    continue;
                }
            }
        }

        let bundled_fallback = providers
            .iter()
            .filter(|provider| env_provider_kinds.contains(provider.provider_kind.as_str()))
            .filter_map(|provider| {
                resolve_bootstrap_provider_preset_bundle(
                    provider,
                    models,
                    BootstrapAiCredentialSource::Env,
                )
                .ok()
                .flatten()
                .and_then(|bundle| {
                    bundle.presets.into_iter().find(|preset| preset.binding_purpose == purpose).map(
                        |preset| BootstrapAiPresetInput {
                            binding_purpose: preset.binding_purpose,
                            provider_kind: provider.provider_kind.clone(),
                            model_catalog_id: preset.model_catalog_id,
                            preset_name: preset.preset_name,
                            system_prompt: preset.system_prompt,
                            temperature: preset.temperature,
                            top_p: preset.top_p,
                            max_output_tokens_override: preset.max_output_tokens_override,
                            extra_parameters_json: json!({}),
                        },
                    )
                })
            })
            .min_by(|left, right| left.provider_kind.cmp(&right.provider_kind));
        if let Some(fallback) = bundled_fallback {
            selections.push(fallback);
            continue;
        }

        let fallback = providers
            .iter()
            .filter(|provider| env_provider_kinds.contains(provider.provider_kind.as_str()))
            .filter_map(|provider| {
                select_bootstrap_suggested_model_for_provider(provider.id, purpose, models)
                    .map(|model| build_bootstrap_preset_input(provider, model, purpose))
            })
            .min_by(|left, right| left.provider_kind.cmp(&right.provider_kind));
        if let Some(fallback) = fallback {
            selections.push(fallback);
        }
    }
    Ok(selections)
}

fn bootstrap_preset_inputs_cover_canonical_purposes(inputs: &[BootstrapAiPresetInput]) -> bool {
    CANONICAL_RUNTIME_BINDING_PURPOSES
        .iter()
        .all(|purpose| inputs.iter().any(|selection| selection.binding_purpose == *purpose))
}

fn validate_bootstrap_preset_inputs_complete(
    inputs: &[BootstrapAiPresetInput],
) -> Result<(), ApiError> {
    if !bootstrap_preset_inputs_cover_canonical_purposes(inputs) {
        return Err(ApiError::BadRequest(
            "bootstrap preset bundle must cover extract_graph, embed_chunk, query_answer, and vision"
                .to_string(),
        ));
    }
    Ok(())
}

fn normalize_bootstrap_preset_inputs(
    inputs: &[BootstrapAiPresetInput],
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiPresetInput>, ApiError> {
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
        let preset_name = normalize_non_empty(&input.preset_name, "presetName")?;
        normalized.push(BootstrapAiPresetInput {
            binding_purpose: input.binding_purpose,
            provider_kind,
            model_catalog_id: input.model_catalog_id,
            preset_name,
            system_prompt: normalize_optional(input.system_prompt.as_deref()),
            temperature: input.temperature,
            top_p: input.top_p,
            max_output_tokens_override: input.max_output_tokens_override,
            extra_parameters_json: input.extra_parameters_json.clone(),
        });
    }
    Ok(normalized)
}

async fn validate_provider_access(
    state: &AppState,
    provider: &ProviderCatalogEntry,
    models: &[ModelCatalogEntry],
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<(), ApiError> {
    let policy = provider_credential_policy(&provider.provider_kind);
    let normalized_api_key = normalize_optional(api_key);
    let normalized_base_url = match base_url {
        Some(url) => Some(canonicalize_provider_base_url(provider, url)?),
        None => resolve_provider_base_url(provider, None)?,
    };

    if policy.api_key_required && normalized_api_key.is_none() {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires an apiKey",
            provider.provider_kind
        )));
    }
    if policy.base_url_required && normalized_base_url.is_none() {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires a baseUrl",
            provider.provider_kind
        )));
    }

    match policy.validation_mode {
        ProviderCredentialValidationMode::ChatRoundTrip => {
            let model = select_provider_validation_model(provider, models).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "provider {} does not expose a chat model for credential validation",
                    provider.provider_kind
                ))
            })?;

            state
                .llm_gateway
                .generate(ChatRequest {
                    provider_kind: provider.provider_kind.clone(),
                    model_name: model.model_name.clone(),
                    prompt: "Reply with OK.".to_string(),
                    api_key_override: normalized_api_key.clone(),
                    base_url_override: normalized_base_url.clone(),
                    system_prompt: Some(
                        "Validate the supplied provider credentials by replying with the single token OK.".to_string(),
                    ),
                    temperature: Some(0.0),
                    top_p: Some(1.0),
                    max_output_tokens_override: Some(16),
                    response_format: None,
                    extra_parameters_json: json!({}),
                })
                .await
                .map(|_| ())
                .map_err(|error| {
                    tracing::warn!(stage = "bootstrap", provider_kind = %provider.provider_kind, error = %error, "provider credential validation failed");
                    ApiError::BadRequest(format!(
                        "provider credential validation failed for {}: {error}",
                        provider.display_name
                    ))
                })
        }
        ProviderCredentialValidationMode::ModelList => {
            validate_provider_model_listing(
                provider,
                normalized_api_key.as_deref(),
                normalized_base_url.as_deref(),
            )
            .await
        }
    }
}

async fn validate_provider_model_listing(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<(), ApiError> {
    let Some(base_url) = base_url else {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires a baseUrl",
            provider.provider_kind
        )));
    };
    let ollama_loopback_base_url =
        provider.provider_kind == "ollama" && is_loopback_base_url(base_url);
    match fetch_provider_model_names(provider, api_key, base_url).await {
        Ok(_) => Ok(()),
        Err(error) if ollama_loopback_base_url => {
            let message = error.to_string();
            if message.contains("Connection refused")
                || message.contains("error trying to connect")
                || message.contains("timed out")
            {
                Err(ollama_loopback_runtime_error(provider))
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

async fn ensure_bootstrap_provider_credential(
    service: &AiCatalogService,
    state: &AppState,
    provider: &ProviderCatalogEntry,
    credential_input: Option<BootstrapAiCredentialInput>,
    existing_credentials: &[ProviderCredential],
    updated_by_principal_id: Option<Uuid>,
) -> Result<ProviderCredential, ApiError> {
    let canonical_label = format!("Bootstrap {}", provider.display_name);
    let provider_credentials =
        bootstrap_provider_credentials_for_provider(existing_credentials, provider.id);
    let canonical_credential =
        bootstrap_resolve_provider_credential(&canonical_label, &provider_credentials);
    let api_key =
        credential_input.as_ref().and_then(|input| normalize_optional(input.api_key.as_deref()));
    let base_url = resolve_provider_base_url(
        provider,
        credential_input.as_ref().and_then(|input| input.base_url.as_deref()),
    )?;
    if api_key.is_some() || base_url.is_some() {
        if let Some(existing) = canonical_credential {
            return match service
                .update_provider_credential(
                    state,
                    UpdateProviderCredentialCommand {
                        credential_id: existing.id,
                        label: canonical_label.clone(),
                        api_key,
                        base_url,
                        credential_state: "active".to_string(),
                    },
                )
                .await
            {
                Ok(updated) => Ok(updated),
                Err(ApiError::Conflict(_)) => {
                    bootstrap_reload_provider_credential(service, state, provider, &canonical_label)
                        .await
                }
                Err(error) => Err(error),
            };
        }
        return match service
            .create_provider_credential(
                state,
                CreateProviderCredentialCommand {
                    scope_kind: AiScopeKind::Instance,
                    workspace_id: None,
                    library_id: None,
                    provider_catalog_id: provider.id,
                    label: canonical_label.clone(),
                    api_key,
                    base_url,
                    created_by_principal_id: updated_by_principal_id,
                },
            )
            .await
        {
            Ok(created) => Ok(created),
            Err(ApiError::Conflict(_)) => {
                bootstrap_reload_provider_credential(service, state, provider, &canonical_label)
                    .await
            }
            Err(error) => Err(error),
        };
    }

    canonical_credential.ok_or_else(|| {
        let required_field = if provider.api_key_required { "apiKey" } else { "baseUrl" };
        ApiError::BadRequest(format!(
            "bootstrap ai setup requires {required_field} for provider {}",
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
    provider: &ProviderCatalogEntry,
    canonical_label: &str,
) -> Result<ProviderCredential, ApiError> {
    let reloaded = service
        .list_provider_credentials_exact(
            state,
            AiScopeRef { scope_kind: AiScopeKind::Instance, workspace_id: None, library_id: None },
        )
        .await?;
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
    preset_input: &BootstrapAiPresetInput,
    presets: &mut Vec<ModelPreset>,
    created_by_principal_id: Option<Uuid>,
) -> Result<ModelPreset, ApiError> {
    if let Some(existing) = bootstrap_find_runtime_preset(
        presets,
        preset_input.model_catalog_id,
        &preset_input.preset_name,
    ) {
        let needs_update = existing.system_prompt != preset_input.system_prompt
            || existing.temperature != preset_input.temperature
            || existing.top_p != preset_input.top_p
            || existing.max_output_tokens_override != preset_input.max_output_tokens_override
            || existing.extra_parameters_json != preset_input.extra_parameters_json;
        if !needs_update {
            return Ok(existing);
        }

        let updated = service
            .update_model_preset(
                state,
                UpdateModelPresetCommand {
                    preset_id: existing.id,
                    preset_name: preset_input.preset_name.clone(),
                    system_prompt: preset_input.system_prompt.clone(),
                    temperature: preset_input.temperature,
                    top_p: preset_input.top_p,
                    max_output_tokens_override: preset_input.max_output_tokens_override,
                    extra_parameters_json: preset_input.extra_parameters_json.clone(),
                },
            )
            .await?;
        if let Some(index) = presets.iter().position(|preset| preset.id == updated.id) {
            presets[index] = updated.clone();
        }
        return Ok(updated);
    }

    match service
        .create_model_preset(
            state,
            CreateModelPresetCommand {
                scope_kind: AiScopeKind::Instance,
                workspace_id: None,
                library_id: None,
                model_catalog_id: preset_input.model_catalog_id,
                preset_name: preset_input.preset_name.clone(),
                system_prompt: preset_input.system_prompt.clone(),
                temperature: preset_input.temperature,
                top_p: preset_input.top_p,
                max_output_tokens_override: preset_input.max_output_tokens_override,
                extra_parameters_json: preset_input.extra_parameters_json.clone(),
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
            *presets = service
                .list_model_presets_exact(
                    state,
                    AiScopeRef {
                        scope_kind: AiScopeKind::Instance,
                        workspace_id: None,
                        library_id: None,
                    },
                )
                .await?;
            bootstrap_find_runtime_preset(
                presets,
                preset_input.model_catalog_id,
                &preset_input.preset_name,
            )
            .ok_or_else(|| ApiError::Conflict("AI catalog resource already exists".to_string()))
        }
        Err(error) => Err(error),
    }
}

fn bootstrap_find_binding_assignment(
    bindings: &[AiBindingAssignment],
    purpose: AiBindingPurpose,
) -> Option<AiBindingAssignment> {
    bindings.iter().find(|binding| binding.binding_purpose == purpose).cloned()
}

async fn ensure_bootstrap_binding_assignment(
    service: &AiCatalogService,
    state: &AppState,
    binding_purpose: AiBindingPurpose,
    provider_credential_id: Uuid,
    model_preset_id: Uuid,
    bindings: &mut Vec<AiBindingAssignment>,
    updated_by_principal_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let existing = bootstrap_find_binding_assignment(bindings, binding_purpose);
    let operation = if let Some(existing) = existing {
        service
            .update_binding_assignment(
                state,
                UpdateBindingAssignmentCommand {
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
            .create_binding_assignment(
                state,
                CreateBindingAssignmentCommand {
                    scope_kind: AiScopeKind::Instance,
                    workspace_id: None,
                    library_id: None,
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
            *bindings = service
                .list_binding_assignments(
                    state,
                    AiScopeRef {
                        scope_kind: AiScopeKind::Instance,
                        workspace_id: None,
                        library_id: None,
                    },
                )
                .await?;
            let existing = bootstrap_find_binding_assignment(bindings, binding_purpose)
                .ok_or_else(|| {
                    ApiError::Conflict("AI catalog resource already exists".to_string())
                })?;
            let updated = service
                .update_binding_assignment(
                    state,
                    UpdateBindingAssignmentCommand {
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
        BootstrapAiCredentialSource, BootstrapAiPresetInput,
        bootstrap_preset_inputs_cover_canonical_purposes, canonicalize_provider_base_url,
        is_loopback_base_url, parse_allowed_binding_purposes, provider_credential_policy,
        resolve_bootstrap_provider_preset_bundle, resolve_configured_bootstrap_preset_inputs,
        validate_bootstrap_preset_inputs_complete, validate_model_binding_purpose,
    };
    use crate::app::config::UiBootstrapAiBindingDefault;
    use crate::domains::ai::{AiBindingPurpose, ModelCatalogEntry, ProviderCatalogEntry};
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

    fn sample_provider(provider_kind: &str) -> ProviderCatalogEntry {
        let policy = provider_credential_policy(provider_kind);
        ProviderCatalogEntry {
            id: Uuid::now_v7(),
            provider_kind: provider_kind.to_string(),
            display_name: provider_kind.to_string(),
            api_style: "openai_compatible".to_string(),
            lifecycle_state: "active".to_string(),
            default_base_url: Some("https://example.com/v1".to_string()),
            api_key_required: policy.api_key_required,
            base_url_required: policy.base_url_required,
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
    fn bootstrap_preset_inputs_must_cover_all_canonical_purposes() {
        let inputs = vec![
            BootstrapAiPresetInput {
                binding_purpose: AiBindingPurpose::ExtractGraph,
                provider_kind: "openai".to_string(),
                model_catalog_id: Uuid::now_v7(),
                preset_name: "OpenAI Extract Graph · gpt-5.4-nano".to_string(),
                system_prompt: None,
                temperature: Some(0.3),
                top_p: Some(0.9),
                max_output_tokens_override: None,
                extra_parameters_json: serde_json::json!({}),
            },
            BootstrapAiPresetInput {
                binding_purpose: AiBindingPurpose::EmbedChunk,
                provider_kind: "openai".to_string(),
                model_catalog_id: Uuid::now_v7(),
                preset_name: "OpenAI Embed Chunk · text-embedding-3-large".to_string(),
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens_override: None,
                extra_parameters_json: serde_json::json!({}),
            },
            BootstrapAiPresetInput {
                binding_purpose: AiBindingPurpose::QueryAnswer,
                provider_kind: "openai".to_string(),
                model_catalog_id: Uuid::now_v7(),
                preset_name: "OpenAI Query Answer · gpt-5.4-mini".to_string(),
                system_prompt: None,
                temperature: Some(0.3),
                top_p: Some(0.9),
                max_output_tokens_override: None,
                extra_parameters_json: serde_json::json!({}),
            },
        ];

        assert!(!bootstrap_preset_inputs_cover_canonical_purposes(&inputs));
        assert!(matches!(
            validate_bootstrap_preset_inputs_complete(&inputs),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn bootstrap_bundle_uses_expected_openai_models() {
        let provider = sample_provider("openai");
        let extract_graph_model_id = Uuid::now_v7();
        let query_answer_model_id = Uuid::now_v7();
        let embed_model_id = Uuid::now_v7();
        let vision_model_id = Uuid::now_v7();
        let models = vec![
            ModelCatalogEntry {
                id: extract_graph_model_id,
                provider_catalog_id: provider.id,
                model_name: "gpt-5.4-nano".to_string(),
                capability_kind: "chat".to_string(),
                modality_kind: "multimodal".to_string(),
                allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
                context_window: None,
                max_output_tokens: None,
            },
            ModelCatalogEntry {
                id: query_answer_model_id,
                provider_catalog_id: provider.id,
                model_name: "gpt-5.4-mini".to_string(),
                capability_kind: "chat".to_string(),
                modality_kind: "multimodal".to_string(),
                allowed_binding_purposes: vec![
                    AiBindingPurpose::QueryAnswer,
                    AiBindingPurpose::Vision,
                ],
                context_window: None,
                max_output_tokens: None,
            },
            ModelCatalogEntry {
                id: embed_model_id,
                provider_catalog_id: provider.id,
                model_name: "text-embedding-3-large".to_string(),
                capability_kind: "embedding".to_string(),
                modality_kind: "text".to_string(),
                allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
                context_window: None,
                max_output_tokens: None,
            },
            ModelCatalogEntry {
                id: vision_model_id,
                provider_catalog_id: provider.id,
                model_name: "gpt-5.4-mini".to_string(),
                capability_kind: "chat".to_string(),
                modality_kind: "multimodal".to_string(),
                allowed_binding_purposes: vec![
                    AiBindingPurpose::QueryAnswer,
                    AiBindingPurpose::Vision,
                ],
                context_window: None,
                max_output_tokens: None,
            },
        ];

        let bundle = resolve_bootstrap_provider_preset_bundle(
            &provider,
            &models,
            BootstrapAiCredentialSource::Missing,
        )
        .expect("openai bundle should resolve")
        .expect("openai bundle should be available");

        assert_eq!(bundle.provider_kind, "openai");
        assert_eq!(bundle.presets.len(), 4);
        assert_eq!(
            bundle
                .presets
                .iter()
                .find(|preset| preset.binding_purpose == AiBindingPurpose::ExtractGraph)
                .map(|preset| preset.model_name.as_str()),
            Some("gpt-5.4-nano")
        );
        assert_eq!(
            bundle
                .presets
                .iter()
                .find(|preset| preset.binding_purpose == AiBindingPurpose::QueryAnswer)
                .and_then(|preset| preset.temperature),
            Some(0.3)
        );
    }

    #[test]
    fn bootstrap_bundle_uses_expected_ollama_models() {
        let provider = sample_provider("ollama");
        let graph_model_id = Uuid::now_v7();
        let embed_model_id = Uuid::now_v7();
        let vision_model_id = Uuid::now_v7();
        let models = vec![
            ModelCatalogEntry {
                id: graph_model_id,
                provider_catalog_id: provider.id,
                model_name: "qwen3:0.6b".to_string(),
                capability_kind: "chat".to_string(),
                modality_kind: "text".to_string(),
                allowed_binding_purposes: vec![
                    AiBindingPurpose::ExtractGraph,
                    AiBindingPurpose::QueryAnswer,
                ],
                context_window: None,
                max_output_tokens: None,
            },
            ModelCatalogEntry {
                id: embed_model_id,
                provider_catalog_id: provider.id,
                model_name: "qwen3-embedding:0.6b".to_string(),
                capability_kind: "embedding".to_string(),
                modality_kind: "text".to_string(),
                allowed_binding_purposes: vec![AiBindingPurpose::EmbedChunk],
                context_window: None,
                max_output_tokens: None,
            },
            ModelCatalogEntry {
                id: vision_model_id,
                provider_catalog_id: provider.id,
                model_name: "qwen3-vl:2b".to_string(),
                capability_kind: "chat".to_string(),
                modality_kind: "multimodal".to_string(),
                allowed_binding_purposes: vec![AiBindingPurpose::Vision],
                context_window: None,
                max_output_tokens: None,
            },
        ];

        let bundle = resolve_bootstrap_provider_preset_bundle(
            &provider,
            &models,
            BootstrapAiCredentialSource::Missing,
        )
        .expect("ollama bundle should resolve")
        .expect("ollama bundle should be available");

        assert_eq!(bundle.provider_kind, "ollama");
        assert_eq!(bundle.default_base_url.as_deref(), Some("https://example.com/v1"));
        assert!(!bundle.api_key_required);
        assert!(bundle.base_url_required);
        assert_eq!(
            bundle
                .presets
                .iter()
                .find(|preset| preset.binding_purpose == AiBindingPurpose::ExtractGraph)
                .map(|preset| preset.model_name.as_str()),
            Some("qwen3:0.6b")
        );
        assert_eq!(
            bundle
                .presets
                .iter()
                .find(|preset| preset.binding_purpose == AiBindingPurpose::Vision)
                .map(|preset| preset.model_name.as_str()),
            Some("qwen3-vl:2b")
        );
    }

    #[test]
    fn canonicalizes_ollama_root_urls_to_v1() {
        let provider = sample_provider("ollama");

        assert_eq!(
            canonicalize_provider_base_url(&provider, "http://localhost:11434")
                .expect("root ollama url should normalize"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            canonicalize_provider_base_url(&provider, "http://localhost:11434/api")
                .expect("/api ollama url should normalize"),
            "http://localhost:11434/v1"
        );
    }

    #[test]
    fn detects_loopback_base_urls() {
        assert!(is_loopback_base_url("http://localhost:11434/v1"));
        assert!(is_loopback_base_url("http://127.0.0.1:11434/v1"));
        assert!(!is_loopback_base_url("http://host.docker.internal:11434/v1"));
    }

    #[test]
    fn configured_bootstrap_presets_inherit_provider_bundle_tuning_when_models_match() {
        let provider = sample_provider("openai");
        let model = ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: provider.id,
            model_name: "gpt-5.4-nano".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "multimodal".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
            context_window: None,
            max_output_tokens: None,
        };
        let configured = crate::app::config::UiBootstrapAiSetup {
            provider_secrets: vec![crate::app::config::UiBootstrapAiProviderSecret {
                provider_kind: "openai".to_string(),
                api_key: "test-openai-key".to_string(),
            }],
            binding_defaults: vec![UiBootstrapAiBindingDefault {
                binding_purpose: "extract_graph".to_string(),
                provider_kind: Some("openai".to_string()),
                model_name: Some("gpt-5.4-nano".to_string()),
            }],
        };

        let preset_inputs = resolve_configured_bootstrap_preset_inputs(
            &configured,
            std::slice::from_ref(&provider),
            &[model],
        )
        .expect("configured preset inputs should resolve");

        assert_eq!(preset_inputs.len(), 1);
        assert_eq!(preset_inputs[0].provider_kind, "openai");
        assert_eq!(preset_inputs[0].binding_purpose, AiBindingPurpose::ExtractGraph);
        assert_eq!(preset_inputs[0].temperature, Some(0.3));
        assert_eq!(preset_inputs[0].top_p, Some(0.9));
    }

    #[test]
    fn bootstrap_bundle_omits_incomplete_provider_profiles() {
        let deepseek_provider = sample_provider("deepseek");
        let models = vec![ModelCatalogEntry {
            id: Uuid::now_v7(),
            provider_catalog_id: deepseek_provider.id,
            model_name: "deepseek-chat".to_string(),
            capability_kind: "chat".to_string(),
            modality_kind: "text".to_string(),
            allowed_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
            context_window: None,
            max_output_tokens: None,
        }];

        let bundle = resolve_bootstrap_provider_preset_bundle(
            &deepseek_provider,
            &models,
            BootstrapAiCredentialSource::Missing,
        )
        .expect("deepseek resolution should not error");

        assert!(bundle.is_none());
    }
}
