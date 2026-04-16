use super::provider_validation::sync_provider_model_catalog;
use super::*;

impl AiCatalogService {
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        let credential = map_provider_credential_row(row);
        sync_provider_model_catalog_after_credential_save(state, provider, &credential).await;
        Ok(credential)
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
        let credential = map_provider_credential_row(row);
        sync_provider_model_catalog_after_credential_save(state, provider, &credential).await;
        Ok(credential)
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

async fn sync_provider_model_catalog_after_credential_save(
    state: &AppState,
    provider: &ProviderCatalogEntry,
    credential: &ProviderCredential,
) {
    if credential.credential_state != "active" {
        return;
    }

    match sync_provider_model_catalog(
        state,
        provider,
        credential.api_key.as_deref(),
        credential.base_url.as_deref(),
    )
    .await
    {
        Ok(model_names) => {
            tracing::info!(
                provider_kind = %provider.provider_kind,
                credential_id = %credential.id,
                model_count = model_names.len(),
                "synced provider model catalog after credential save"
            );
        }
        Err(error) => {
            tracing::warn!(
                provider_kind = %provider.provider_kind,
                credential_id = %credential.id,
                error = %error,
                "failed to sync provider model catalog after credential save"
            );
        }
    }
}
