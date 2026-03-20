use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::{
        BindingValidation, LibraryModelBinding, ModelCatalogEntry, ModelPreset, PriceCatalogEntry,
        ProviderCatalogEntry, ProviderCredential,
    },
    infra::repositories::ai_repository,
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct CreateProviderCredentialCommand {
    pub workspace_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub secret_ref: String,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateProviderCredentialCommand {
    pub credential_id: Uuid,
    pub label: String,
    pub secret_ref: String,
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
    pub binding_purpose: String,
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
pub struct CreateBindingValidationCommand {
    pub binding_id: Uuid,
    pub validation_state: String,
    pub failure_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Default)]
pub struct AiCatalogService;

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
        Ok(rows.into_iter().map(map_model_row).collect())
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
        let secret_ref = normalize_non_empty(&command.secret_ref, "secretRef")?;
        let row = ai_repository::create_provider_credential(
            &state.persistence.postgres,
            command.workspace_id,
            command.provider_catalog_id,
            &label,
            &secret_ref,
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
        let secret_ref = normalize_non_empty(&command.secret_ref, "secretRef")?;
        let row = ai_repository::update_provider_credential(
            &state.persistence.postgres,
            command.credential_id,
            &label,
            &secret_ref,
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
        Ok(rows.into_iter().map(map_library_binding_row).collect())
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
        Ok(map_library_binding_row(row))
    }

    pub async fn create_library_binding(
        &self,
        state: &AppState,
        command: CreateLibraryBindingCommand,
    ) -> Result<LibraryModelBinding, ApiError> {
        let row = ai_repository::create_library_binding(
            &state.persistence.postgres,
            command.workspace_id,
            command.library_id,
            &command.binding_purpose,
            command.provider_credential_id,
            command.model_preset_id,
            command.updated_by_principal_id,
        )
        .await
        .map_err(map_ai_write_error)?;
        Ok(map_library_binding_row(row))
    }

    pub async fn update_library_binding(
        &self,
        state: &AppState,
        command: UpdateLibraryBindingCommand,
    ) -> Result<LibraryModelBinding, ApiError> {
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
        Ok(map_library_binding_row(row))
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

fn map_model_row(row: ai_repository::AiModelCatalogRow) -> ModelCatalogEntry {
    ModelCatalogEntry {
        id: row.id,
        provider_catalog_id: row.provider_catalog_id,
        model_name: row.model_name,
        capability_kind: row.capability_kind,
        modality_kind: row.modality_kind,
        context_window: row.context_window,
        max_output_tokens: row.max_output_tokens,
    }
}

fn map_price_row(row: ai_repository::AiPriceCatalogRow) -> PriceCatalogEntry {
    PriceCatalogEntry {
        id: row.id,
        model_catalog_id: row.model_catalog_id,
        billing_unit: row.billing_unit,
        unit_price: row.unit_price,
        currency_code: row.currency_code,
        effective_from: row.effective_from,
    }
}

fn map_provider_credential_row(row: ai_repository::AiProviderCredentialRow) -> ProviderCredential {
    ProviderCredential {
        id: row.id,
        workspace_id: row.workspace_id,
        provider_catalog_id: row.provider_catalog_id,
        label: row.label,
        secret_ref: row.secret_ref,
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

fn map_library_binding_row(row: ai_repository::AiLibraryModelBindingRow) -> LibraryModelBinding {
    LibraryModelBinding {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        binding_purpose: row.binding_purpose,
        provider_credential_id: row.provider_credential_id,
        model_preset_id: row.model_preset_id,
        binding_state: row.binding_state,
    }
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
