use super::*;

impl AiCatalogService {
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_model_preset_row).collect())
    }

    pub async fn get_model_preset(
        &self,
        state: &AppState,
        preset_id: Uuid,
    ) -> Result<ModelPreset, ApiError> {
        let row = ai_repository::get_model_preset_by_id(&state.persistence.postgres, preset_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
