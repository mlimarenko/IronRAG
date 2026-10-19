use super::*;

pub(super) fn deduplicate_binding_purposes(
    binding_purposes: &[AiBindingPurpose],
) -> Vec<AiBindingPurpose> {
    let mut seen = HashSet::with_capacity(binding_purposes.len());
    binding_purposes.iter().copied().filter(|purpose| seen.insert(*purpose)).collect()
}
/// Runs the canonical purpose-eligibility gate for a resolved read-path row
/// and then resolves the provider's runtime base URL. A row whose model or
/// provider drifted out of eligibility after the binding was written (a
/// purpose removed from the model, or the provider's decision capability
/// changed) is refused here rather than served.
fn validate_runtime_binding_components(
    binding_purpose: AiBindingPurpose,
    model: &ModelCatalogEntry,
    provider: &ProviderCatalogEntry,
    account_credential_state: &str,
    account_base_url: Option<&str>,
) -> Result<Option<String>, ApiError> {
    validate_binding_target(BindingTarget {
        purpose: binding_purpose,
        provider,
        model,
        account_credential_state,
    })?;
    runtime_provider_base_url(provider, account_base_url)
}

fn map_effective_model_row(
    row: &mut ai_repository::EffectiveModelRow,
) -> Result<ModelCatalogEntry, ApiError> {
    catalog::map_model_row(ai_repository::AiModelCatalogRow {
        id: row.model_catalog_id,
        provider_catalog_id: row.model_provider_catalog_id,
        model_name: std::mem::take(&mut row.model_name),
        context_window: row.model_context_window,
        max_output_tokens: row.model_max_output_tokens,
        lifecycle_state: std::mem::take(&mut row.model_lifecycle_state),
        metadata_json: std::mem::take(&mut row.model_metadata_json),
        allowed_binding_purposes: std::mem::take(&mut row.model_allowed_binding_purposes),
        currency_code: std::mem::take(&mut row.model_currency_code),
        input_price_per_1m: row.model_input_price_per_1m,
        cached_input_price_per_1m: row.model_cached_input_price_per_1m,
        cache_write_input_price_per_1m: row.model_cache_write_input_price_per_1m,
        output_price_per_1m: row.model_output_price_per_1m,
        long_context_threshold_tokens: row.model_long_context_threshold_tokens,
        long_context_input_price_per_1m: row.model_long_context_input_price_per_1m,
        long_context_cached_input_price_per_1m: row.model_long_context_cached_input_price_per_1m,
        long_context_output_price_per_1m: row.model_long_context_output_price_per_1m,
    })
}

fn map_effective_provider_selection_row(
    mut row: ai_repository::EffectiveProviderSelectionRow,
) -> Result<(AiBindingPurpose, ProviderModelSelection), ApiError> {
    let binding_purpose = parse_binding_purpose(&row.binding_purpose)?;
    let model = map_effective_model_row(&mut row.model)?;
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

pub(crate) fn embedding_profile_key_from_effective_row(
    mut row: ai_repository::EffectiveEmbeddingProfileRow,
) -> Result<String, ApiError> {
    let binding_purpose = parse_binding_purpose(&row.binding_purpose)?;
    let model = map_effective_model_row(&mut row.model)?;
    let provider = catalog::map_provider_row(ai_repository::AiProviderCatalogRow {
        id: row.provider_catalog_id,
        provider_kind: std::mem::take(&mut row.provider_kind),
        display_name: std::mem::take(&mut row.provider_display_name),
        api_style: std::mem::take(&mut row.provider_api_style),
        lifecycle_state: std::mem::take(&mut row.provider_lifecycle_state),
        default_base_url: row.provider_default_base_url.take(),
        capability_flags_json: std::mem::take(&mut row.provider_capability_flags_json),
    })?;
    let provider_base_url = validate_runtime_binding_components(
        binding_purpose,
        &model,
        &provider,
        &row.account_credential_state,
        row.account_base_url.as_deref(),
    )?;
    let effective_embedding_dimensions = resolve_effective_embedding_dimensions(
        binding_purpose,
        &row.binding_extra_parameters_json,
        &model.metadata_json,
    )?;
    let extra_parameters_json =
        merge_binding_request_context(row.binding_extra_parameters_json, &model, &provider.profile);
    let binding = ResolvedRuntimeBinding {
        binding_id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        binding_purpose,
        provider_catalog_id: provider.id,
        provider_kind: provider.provider_kind,
        provider_base_url,
        provider_api_style: provider.api_style,
        account_id: Uuid::nil(),
        api_key: None,
        model_catalog_id: model.id,
        model_name: model.model_name,
        effective_embedding_dimensions,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json,
    };
    Ok(binding.embedding_execution_profile_key())
}

fn map_effective_runtime_binding_row(
    state: &AppState,
    mut row: ai_repository::EffectiveRuntimeBindingRow,
    requested_purpose: AiBindingPurpose,
) -> Result<ResolvedRuntimeBinding, ApiError> {
    let stored_purpose = parse_binding_purpose(&row.binding_purpose)?;
    let model = map_effective_model_row(&mut row.model)?;
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

    let extra_parameters_json = merge_binding_request_context(
        std::mem::take(&mut row.extra_parameters_json),
        &model,
        &provider.profile,
    );
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
        extra_parameters_json,
    })
}
impl AiCatalogService {
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
        let effective_embedding_dimensions = resolve_effective_embedding_dimensions(
            binding_purpose,
            &binding.extra_parameters_json,
            &model.metadata_json,
        )?;

        let extra_parameters_json =
            merge_binding_request_context(binding.extra_parameters_json, &model, &provider.profile);
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
            extra_parameters_json,
        })
    }
}
