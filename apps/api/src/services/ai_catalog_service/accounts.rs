use super::provider_validation::sync_provider_model_catalog;
use super::*;

impl AiCatalogService {
    pub async fn list_accounts_exact(
        &self,
        state: &AppState,
        scope: AiScopeRef,
    ) -> Result<Vec<AiAccount>, ApiError> {
        let rows = ai_repository::list_accounts_exact(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_account_row).collect())
    }

    pub async fn list_visible_accounts(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<AiAccount>, ApiError> {
        let rows = ai_repository::list_visible_accounts(
            &state.persistence.postgres,
            workspace_id,
            library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_account_row).collect())
    }

    pub async fn get_account(
        &self,
        state: &AppState,
        account_id: Uuid,
    ) -> Result<AiAccount, ApiError> {
        let row = ai_repository::get_account_by_id(&state.persistence.postgres, account_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("provider_credential", account_id))?;
        Ok(map_account_row(row))
    }

    pub async fn create_account(
        &self,
        state: &AppState,
        command: CreateAiAccountCommand,
    ) -> Result<AiAccount, ApiError> {
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
        let base_url =
            provider_credential_base_url_for_create(provider, command.base_url.as_deref())?;
        validate_provider_access(state, provider, &models, api_key.as_deref(), base_url.as_deref())
            .await?;
        let row = ai_repository::create_account(
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
        let account = map_account_row(row);
        sync_provider_model_catalog_after_account_save(state, provider, &account).await;
        Ok(account)
    }

    pub async fn update_account(
        &self,
        state: &AppState,
        command: UpdateAiAccountCommand,
    ) -> Result<AiAccount, ApiError> {
        let label = normalize_non_empty(&command.label, "label")?;
        let existing = self.get_account(state, command.account_id).await?;
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, Some(existing.provider_catalog_id)).await?;
        let provider =
            providers.iter().find(|entry| entry.id == existing.provider_catalog_id).ok_or_else(
                || ApiError::resource_not_found("provider_catalog", existing.provider_catalog_id),
            )?;
        let api_key = normalize_optional(command.api_key.as_deref());
        let base_url = provider_credential_base_url_for_update(
            provider,
            existing.base_url.as_deref(),
            command.base_url.as_deref(),
        )?;
        let effective_api_key = api_key.as_deref().or(existing.api_key.as_deref());
        validate_provider_access(state, provider, &models, effective_api_key, base_url.as_deref())
            .await?;
        let row = ai_repository::update_account(
            &state.persistence.postgres,
            command.account_id,
            &label,
            api_key.as_deref(),
            base_url.as_deref(),
            &command.credential_state,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("provider_credential", command.account_id))?;
        let account = map_account_row(row);
        sync_provider_model_catalog_after_account_save(state, provider, &account).await;
        Ok(account)
    }

    pub async fn delete_account(&self, state: &AppState, account_id: Uuid) -> Result<(), ApiError> {
        let affected = ai_repository::delete_account(&state.persistence.postgres, account_id)
            .await
            .map_err(map_ai_delete_error)?;
        if affected == 0 {
            return Err(ApiError::resource_not_found("provider_credential", account_id));
        }
        Ok(())
    }
}

pub(super) fn map_account_row(row: ai_repository::AiAccountRow) -> AiAccount {
    AiAccount {
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

async fn sync_provider_model_catalog_after_account_save(
    state: &AppState,
    provider: &ProviderCatalogEntry,
    account: &AiAccount,
) {
    if account.credential_state != "active" {
        return;
    }

    match sync_provider_model_catalog(
        state,
        provider,
        account.api_key.as_deref(),
        account.base_url.as_deref(),
    )
    .await
    {
        Ok(model_names) => {
            tracing::info!(
                provider_kind = %provider.provider_kind,
                account_id = %account.id,
                model_count = model_names.len(),
                "synced provider model catalog after account save"
            );
        }
        Err(error) => {
            tracing::warn!(
                provider_kind = %provider.provider_kind,
                account_id = %account.id,
                error = %error,
                "failed to sync provider model catalog after account save"
            );
        }
    }
}
