use super::provider_validation::sync_provider_model_catalog;
use super::*;
use std::collections::{BTreeSet, HashMap};

impl AiCatalogService {
    pub async fn list_provider_catalog(
        &self,
        state: &AppState,
    ) -> Result<Vec<ProviderCatalogEntry>, ApiError> {
        let rows = ai_repository::list_provider_catalog(&state.persistence.postgres)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_provider_row).collect()
    }

    pub async fn list_model_catalog(
        &self,
        state: &AppState,
        provider_catalog_id: Option<Uuid>,
    ) -> Result<Vec<ModelCatalogEntry>, ApiError> {
        let rows =
            ai_repository::list_model_catalog(&state.persistence.postgres, provider_catalog_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_model_row).collect()
    }

    pub async fn get_provider_catalog(
        &self,
        state: &AppState,
        provider_catalog_id: Uuid,
    ) -> Result<ProviderCatalogEntry, ApiError> {
        let row = ai_repository::get_provider_catalog_by_id(
            &state.persistence.postgres,
            provider_catalog_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("provider_catalog", provider_catalog_id))?;
        map_provider_row(row)
    }

    pub async fn get_model_catalog(
        &self,
        state: &AppState,
        model_catalog_id: Uuid,
    ) -> Result<ModelCatalogEntry, ApiError> {
        let row =
            ai_repository::get_model_catalog_by_id(&state.persistence.postgres, model_catalog_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("model_catalog", model_catalog_id))?;
        map_model_row(row)
    }

    pub async fn create_provider_catalog(
        &self,
        state: &AppState,
        command: CreateProviderCatalogCommand,
    ) -> Result<ProviderCatalogEntry, ApiError> {
        let provider_kind = normalize_non_empty(&command.provider_kind, "providerKind")?;
        let display_name = normalize_non_empty(&command.display_name, "displayName")?;
        let api_style =
            normalize_allowed_value(&command.api_style, "apiStyle", &["openai_compatible"])?;
        let lifecycle_state =
            normalize_lifecycle_state(&command.lifecycle_state, "lifecycleState")?;
        let default_base_url = normalize_optional(command.default_base_url.as_deref());
        parse_provider_profile(&provider_kind, &command.capability_flags_json)?;
        let row = ai_repository::create_provider_catalog(
            &state.persistence.postgres,
            &provider_kind,
            &display_name,
            &api_style,
            &lifecycle_state,
            default_base_url.as_deref(),
            command.capability_flags_json,
        )
        .await
        .map_err(map_ai_write_error)?;
        map_provider_row(row)
    }

    pub async fn update_provider_catalog(
        &self,
        state: &AppState,
        command: UpdateProviderCatalogCommand,
    ) -> Result<ProviderCatalogEntry, ApiError> {
        let provider_kind = normalize_non_empty(&command.provider_kind, "providerKind")?;
        let display_name = normalize_non_empty(&command.display_name, "displayName")?;
        let api_style =
            normalize_allowed_value(&command.api_style, "apiStyle", &["openai_compatible"])?;
        let lifecycle_state =
            normalize_lifecycle_state(&command.lifecycle_state, "lifecycleState")?;
        let default_base_url = normalize_optional(command.default_base_url.as_deref());
        if let Some(profile_json) = &command.capability_flags_json {
            parse_provider_profile(&provider_kind, profile_json)?;
        }
        let row = ai_repository::update_provider_catalog(
            &state.persistence.postgres,
            command.provider_id,
            &provider_kind,
            &display_name,
            &api_style,
            &lifecycle_state,
            default_base_url.as_deref(),
            command.capability_flags_json,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("provider_catalog", command.provider_id))?;
        map_provider_row(row)
    }

    /// Real hard delete; 409 (via `map_ai_delete_error`) when a model still
    /// references this provider. `update_provider_catalog` with
    /// `lifecycleState: "disabled"` remains the reversible pause — the two
    /// are distinct operations, not two paths to the same result (see the AI
    /// section of `memory/2026-07-17-rest-api-query-refactor-plan.md`).
    pub async fn delete_provider_catalog(
        &self,
        state: &AppState,
        provider_catalog_id: Uuid,
    ) -> Result<(), ApiError> {
        let deleted = ai_repository::delete_provider_catalog(
            &state.persistence.postgres,
            provider_catalog_id,
        )
        .await
        .map_err(map_ai_delete_error)?;
        if !deleted {
            return Err(ApiError::resource_not_found("provider_catalog", provider_catalog_id));
        }
        Ok(())
    }

    pub async fn create_model_catalog(
        &self,
        state: &AppState,
        command: CreateModelCatalogCommand,
    ) -> Result<ModelCatalogEntry, ApiError> {
        let model_name = normalize_non_empty(&command.model_name, "modelName")?;
        let capability_kind = normalize_allowed_value(
            &command.capability_kind,
            "capabilityKind",
            &["chat", "embedding"],
        )?;
        let modality_kind = normalize_allowed_value(
            &command.modality_kind,
            "modalityKind",
            &["text", "multimodal"],
        )?;
        let lifecycle_state =
            normalize_lifecycle_state(&command.lifecycle_state, "lifecycleState")?;
        let provider = self.get_provider_catalog(state, command.provider_catalog_id).await?;
        validate_model_catalog_binding_contract(
            &provider,
            &model_name,
            &capability_kind,
            &modality_kind,
            &command.allowed_binding_purposes,
        )?;
        let metadata_json = metadata_with_binding_purposes(
            command.metadata_json,
            &command.allowed_binding_purposes,
        )?;
        let row = ai_repository::create_model_catalog(
            &state.persistence.postgres,
            command.provider_catalog_id,
            &model_name,
            &capability_kind,
            &modality_kind,
            command.context_window,
            command.max_output_tokens,
            &lifecycle_state,
            metadata_json,
        )
        .await
        .map_err(map_ai_write_error)?;
        map_model_row(row)
    }

    pub async fn update_model_catalog(
        &self,
        state: &AppState,
        command: UpdateModelCatalogCommand,
    ) -> Result<ModelCatalogEntry, ApiError> {
        let model_name = normalize_non_empty(&command.model_name, "modelName")?;
        let capability_kind = normalize_allowed_value(
            &command.capability_kind,
            "capabilityKind",
            &["chat", "embedding"],
        )?;
        let modality_kind = normalize_allowed_value(
            &command.modality_kind,
            "modalityKind",
            &["text", "multimodal"],
        )?;
        let lifecycle_state =
            normalize_lifecycle_state(&command.lifecycle_state, "lifecycleState")?;
        let provider = self.get_provider_catalog(state, command.provider_catalog_id).await?;
        validate_model_catalog_binding_contract(
            &provider,
            &model_name,
            &capability_kind,
            &modality_kind,
            &command.allowed_binding_purposes,
        )?;
        let existing =
            ai_repository::get_model_catalog_by_id(&state.persistence.postgres, command.model_id)
                .await
                .map_err(map_ai_write_error)?
                .ok_or_else(|| ApiError::resource_not_found("model_catalog", command.model_id))?;
        let metadata_json = metadata_with_binding_purposes(
            command.metadata_json.unwrap_or(existing.metadata_json),
            &command.allowed_binding_purposes,
        )?;
        let row = ai_repository::update_model_catalog(
            &state.persistence.postgres,
            command.model_id,
            command.provider_catalog_id,
            &model_name,
            &capability_kind,
            &modality_kind,
            command.context_window,
            command.max_output_tokens,
            &lifecycle_state,
            metadata_json,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("model_catalog", command.model_id))?;
        map_model_row(row)
    }

    /// Real hard delete; 409 (via `map_ai_delete_error`) when a price
    /// override or binding still references this model.
    /// `update_model_catalog` with `lifecycleState: "disabled"` remains the
    /// reversible pause — the two are distinct operations, not two paths to
    /// the same result (same reasoning as `delete_provider_catalog` above).
    pub async fn delete_model_catalog(
        &self,
        state: &AppState,
        model_catalog_id: Uuid,
    ) -> Result<(), ApiError> {
        let deleted =
            ai_repository::delete_model_catalog(&state.persistence.postgres, model_catalog_id)
                .await
                .map_err(map_ai_delete_error)?;
        if !deleted {
            return Err(ApiError::resource_not_found("model_catalog", model_catalog_id));
        }
        Ok(())
    }

    /// Item-read of a single model with its real resolved availability
    /// state, computed the same way `list_resolved_model_catalog` computes
    /// it for the list endpoint (instance-scoped account visibility — no
    /// workspace/library context is available at this call site). Used by
    /// the create/update/get handlers so none of them fall back to the
    /// `ModelAvailabilityState::Unknown` stub the plan flagged as a defect.
    pub async fn get_resolved_model_catalog(
        &self,
        state: &AppState,
        model_catalog_id: Uuid,
    ) -> Result<ResolvedModelCatalogEntry, ApiError> {
        let model = self.get_model_catalog(state, model_catalog_id).await?;
        let resolved = self
            .list_resolved_model_catalog(state, Some(model.provider_catalog_id), None, None, None)
            .await?;
        resolved
            .into_iter()
            .find(|entry| entry.model.id == model_catalog_id)
            .ok_or_else(|| ApiError::resource_not_found("model_catalog", model_catalog_id))
    }

    pub async fn list_resolved_model_catalog(
        &self,
        state: &AppState,
        provider_catalog_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
        account_id: Option<Uuid>,
    ) -> Result<Vec<ResolvedModelCatalogEntry>, ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let provider_by_id =
            providers.iter().map(|provider| (provider.id, provider)).collect::<HashMap<_, _>>();
        let (scoped_accounts, explicit_account) =
            self.resolved_catalog_accounts(state, workspace_id, library_id, account_id).await?;
        let available_account_ids_by_provider =
            available_account_ids_by_provider(&scoped_accounts, provider_catalog_id);
        let (explicitly_available_account_ids, explicitly_checked_providers) = self
            .refresh_explicitly_requested_provider_models(
                state,
                provider_catalog_id,
                explicit_account.as_ref(),
                &provider_by_id,
            )
            .await;

        let models = self.list_model_catalog(state, provider_catalog_id).await?;
        Ok(models
            .into_iter()
            .map(|model| {
                resolve_model_catalog_entry(
                    model,
                    &provider_by_id,
                    &available_account_ids_by_provider,
                    &explicitly_available_account_ids,
                    &explicitly_checked_providers,
                )
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_price_row).collect())
    }

    pub async fn get_price_catalog_entry(
        &self,
        state: &AppState,
        price_id: Uuid,
    ) -> Result<PriceCatalogEntry, ApiError> {
        let row = ai_repository::get_price_catalog_by_id(&state.persistence.postgres, price_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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

    pub async fn delete_workspace_price_override(
        &self,
        state: &AppState,
        price_id: Uuid,
    ) -> Result<(), ApiError> {
        let affected =
            ai_repository::delete_workspace_price_override(&state.persistence.postgres, price_id)
                .await
                .map_err(map_ai_delete_error)?;
        if affected == 0 {
            return Err(ApiError::resource_not_found("price_catalog_entry", price_id));
        }
        Ok(())
    }

    async fn resolved_catalog_accounts(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
        account_id: Option<Uuid>,
    ) -> Result<(Vec<AiAccountSummary>, Option<AiAccount>), ApiError> {
        let summaries =
            self.list_visible_account_summaries(state, workspace_id, library_id).await?;
        let Some(target_account_id) = account_id else {
            return Ok((summaries, None));
        };
        let summary =
            summaries.into_iter().find(|account| account.id == target_account_id).ok_or_else(
                || ApiError::resource_not_found("provider_credential", target_account_id),
            )?;
        // Visibility is established using a secret-free projection. Only the
        // explicitly requested account is decrypted for model discovery.
        let account = self.get_account(state, target_account_id).await?;
        Ok((vec![summary], Some(account)))
    }

    async fn refresh_explicitly_requested_provider_models(
        &self,
        state: &AppState,
        provider_catalog_id: Option<Uuid>,
        account: Option<&AiAccount>,
        provider_by_id: &HashMap<Uuid, &ProviderCatalogEntry>,
    ) -> (HashMap<(Uuid, String), BTreeSet<Uuid>>, BTreeSet<Uuid>) {
        let Some(account) = account.filter(|account| account.credential_state == "active") else {
            return (HashMap::new(), BTreeSet::new());
        };
        let Some(provider) = provider_by_id.get(&account.provider_catalog_id) else {
            return (HashMap::new(), BTreeSet::new());
        };
        if !should_refresh_provider_models(provider, provider_catalog_id) {
            return (HashMap::new(), BTreeSet::new());
        }

        match sync_provider_model_catalog(
            state,
            provider,
            account.api_key.as_deref(),
            account.base_url.as_deref(),
        )
        .await
        {
            Ok(model_names) => (
                explicitly_available_accounts_by_model(provider.id, account.id, model_names),
                BTreeSet::from([provider.id]),
            ),
            Err(error) => {
                tracing::warn!(
                    provider_kind = %provider.provider_kind,
                    account_id = %account.id,
                    error = %error,
                    "failed to refresh provider models for account-specific request"
                );
                (HashMap::new(), BTreeSet::new())
            }
        }
    }
}

fn available_account_ids_by_provider(
    accounts: &[AiAccountSummary],
    provider_catalog_id: Option<Uuid>,
) -> HashMap<Uuid, BTreeSet<Uuid>> {
    accounts
        .iter()
        .filter(|account| account.credential_state == "active")
        .filter(|account| {
            provider_catalog_id.is_none_or(|value| value == account.provider_catalog_id)
        })
        .fold(HashMap::new(), |mut account_ids_by_provider, account| {
            account_ids_by_provider
                .entry(account.provider_catalog_id)
                .or_default()
                .insert(account.id);
            account_ids_by_provider
        })
}

fn should_refresh_provider_models(
    provider: &ProviderCatalogEntry,
    provider_catalog_id: Option<Uuid>,
) -> bool {
    provider_catalog_id.is_none_or(|value| value == provider.id)
        && provider_credential_policy(provider).validation_mode
            == ProviderCredentialValidationMode::ModelList
}

fn explicitly_available_accounts_by_model(
    provider_id: Uuid,
    account_id: Uuid,
    model_names: Vec<String>,
) -> HashMap<(Uuid, String), BTreeSet<Uuid>> {
    model_names.into_iter().fold(HashMap::new(), |mut account_ids_by_model, model_name| {
        account_ids_by_model.entry((provider_id, model_name)).or_default().insert(account_id);
        account_ids_by_model
    })
}

fn resolve_model_catalog_entry(
    model: ModelCatalogEntry,
    provider_by_id: &HashMap<Uuid, &ProviderCatalogEntry>,
    available_account_ids_by_provider: &HashMap<Uuid, BTreeSet<Uuid>>,
    explicitly_available_account_ids: &HashMap<(Uuid, String), BTreeSet<Uuid>>,
    explicitly_checked_providers: &BTreeSet<Uuid>,
) -> ResolvedModelCatalogEntry {
    let available_account_ids = model_available_account_ids(
        &model,
        available_account_ids_by_provider,
        explicitly_available_account_ids,
        explicitly_checked_providers,
    );
    let availability_state = model_availability_state(
        &model,
        provider_by_id.get(&model.provider_catalog_id).copied(),
        explicitly_checked_providers,
        &available_account_ids,
    );
    ResolvedModelCatalogEntry { model, availability_state, available_account_ids }
}

fn model_available_account_ids(
    model: &ModelCatalogEntry,
    available_account_ids_by_provider: &HashMap<Uuid, BTreeSet<Uuid>>,
    explicitly_available_account_ids: &HashMap<(Uuid, String), BTreeSet<Uuid>>,
    explicitly_checked_providers: &BTreeSet<Uuid>,
) -> Vec<Uuid> {
    let account_ids = if explicitly_checked_providers.contains(&model.provider_catalog_id) {
        explicitly_available_account_ids.get(&(model.provider_catalog_id, model.model_name.clone()))
    } else {
        available_account_ids_by_provider.get(&model.provider_catalog_id)
    };
    account_ids.map(|ids| ids.iter().copied().collect()).unwrap_or_default()
}

fn model_availability_state(
    model: &ModelCatalogEntry,
    provider: Option<&ProviderCatalogEntry>,
    explicitly_checked_providers: &BTreeSet<Uuid>,
    available_account_ids: &[Uuid],
) -> ModelAvailabilityState {
    if model.lifecycle_state == "disabled"
        || provider.is_some_and(|entry| entry.lifecycle_state == "disabled")
    {
        return ModelAvailabilityState::Unavailable;
    }

    match provider.map(|entry| entry.model_discovery.mode) {
        Some(ProviderModelDiscoveryMode::Credential)
            if explicitly_checked_providers.contains(&model.provider_catalog_id) =>
        {
            if available_account_ids.is_empty() {
                ModelAvailabilityState::Unavailable
            } else {
                ModelAvailabilityState::Available
            }
        }
        Some(ProviderModelDiscoveryMode::Credential) | None => ModelAvailabilityState::Unknown,
        Some(_) => ModelAvailabilityState::Available,
    }
}

fn normalize_currency_code(value: &str) -> Result<String, ApiError> {
    let normalized = normalize_non_empty(value, "currencyCode")?;
    Ok(normalized.to_ascii_uppercase())
}

fn normalize_allowed_value(
    value: &str,
    field_name: &'static str,
    allowed: &[&str],
) -> Result<String, ApiError> {
    let normalized = normalize_non_empty(value, field_name)?;
    if allowed.iter().any(|entry| *entry == normalized) {
        return Ok(normalized);
    }
    Err(ApiError::BadRequest(format!("{field_name} has unsupported value")))
}

fn normalize_lifecycle_state(value: &str, field_name: &'static str) -> Result<String, ApiError> {
    normalize_allowed_value(value, field_name, &["active", "preview", "deprecated", "disabled"])
}

pub(super) fn metadata_with_binding_purposes(
    metadata_json: serde_json::Value,
    purposes: &[AiBindingPurpose],
) -> Result<serde_json::Value, ApiError> {
    if purposes.is_empty() {
        return Err(ApiError::BadRequest("allowedBindingPurposes must not be empty".to_string()));
    }
    let mut object = match metadata_json {
        serde_json::Value::Null => serde_json::Map::new(),
        serde_json::Value::Object(object) => object,
        _ => {
            return Err(ApiError::BadRequest("metadataJson must be an object".to_string()));
        }
    };
    if let Some(value) = object.get("requestPolicy") {
        parse_request_policy(value, "metadataJson.requestPolicy")?;
    }
    object.insert(
        "defaultRoles".to_string(),
        serde_json::Value::Array(
            purposes
                .iter()
                .map(|purpose| serde_json::Value::String(binding_purpose_key(*purpose).to_string()))
                .collect(),
        ),
    );
    Ok(serde_json::Value::Object(object))
}

pub(super) fn map_provider_row(
    row: ai_repository::AiProviderCatalogRow,
) -> Result<ProviderCatalogEntry, ApiError> {
    let profile = parse_provider_profile(&row.provider_kind, &row.capability_flags_json)?;
    let policy = profile.credentials.clone();
    Ok(ProviderCatalogEntry {
        id: row.id,
        provider_kind: row.provider_kind,
        display_name: row.display_name,
        api_style: row.api_style,
        lifecycle_state: row.lifecycle_state,
        default_base_url: row.default_base_url,
        capability_flags_json: row.capability_flags_json,
        api_key_required: policy.api_key_required,
        base_url_required: policy.base_url_required,
        credential_policy: policy,
        base_url_policy: profile.base_url.clone(),
        model_discovery: profile.model_discovery.clone(),
        capabilities: profile.capabilities.clone(),
        runtime: profile.runtime.clone(),
        ui_hints: profile.ui_hints.clone(),
        profile,
    })
}

pub(super) fn parse_provider_profile(
    provider_kind: &str,
    capability_flags_json: &serde_json::Value,
) -> Result<ProviderProfile, ApiError> {
    let profile = serde_json::from_value::<ProviderProfile>(capability_flags_json.clone())
        .map_err(|error| {
            ApiError::BadRequest(format!(
                "provider {provider_kind} has invalid provider profile metadata: {error}"
            ))
        })?;
    super::bootstrap::validate_bootstrap_provider_metadata(provider_kind, capability_flags_json)?;
    validate_request_policy(
        &profile.request_policy,
        &format!("provider {provider_kind} capabilityFlagsJson.requestPolicy"),
    )?;
    Ok(profile)
}

pub(super) fn map_model_row(
    row: ai_repository::AiModelCatalogRow,
) -> Result<ModelCatalogEntry, ApiError> {
    let role_context = format!("model {:?} metadataJson.defaultRoles", row.model_name);
    let allowed_binding_purposes =
        parse_allowed_binding_purposes(&row.metadata_json, &role_context)?;
    if allowed_binding_purposes.is_empty() && row.lifecycle_state != "disabled" {
        return Err(ApiError::BadRequest(format!(
            "{role_context} must not be empty unless the model lifecycleState is disabled"
        )));
    }

    Ok(ModelCatalogEntry {
        id: row.id,
        provider_catalog_id: row.provider_catalog_id,
        model_name: row.model_name,
        capability_kind: row.capability_kind,
        modality_kind: row.modality_kind,
        lifecycle_state: row.lifecycle_state,
        metadata_json: row.metadata_json.clone(),
        allowed_binding_purposes,
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

pub(super) fn parse_allowed_binding_purposes(
    metadata_json: &serde_json::Value,
    role_context: &str,
) -> Result<Vec<AiBindingPurpose>, ApiError> {
    let Some(raw_roles) = metadata_json.get("defaultRoles") else {
        return Ok(Vec::new());
    };
    let roles = raw_roles
        .as_array()
        .ok_or_else(|| ApiError::BadRequest(format!("{role_context} must be an array")))?;

    let mut allowed = Vec::with_capacity(roles.len());
    for (index, role) in roles.iter().enumerate() {
        let role_str = role.as_str().ok_or_else(|| {
            ApiError::BadRequest(format!("{role_context}[{index}] must be a string"))
        })?;
        let purpose = parse_binding_purpose(role_str).map_err(|_| {
            ApiError::BadRequest(format!(
                "{role_context}[{index}] contains unsupported binding purpose {role_str:?}"
            ))
        })?;
        if !allowed.contains(&purpose) {
            allowed.push(purpose);
        }
    }
    Ok(allowed)
}

pub(super) fn validate_model_binding_purpose(
    binding_purpose: AiBindingPurpose,
    model: &ModelCatalogEntry,
) -> Result<(), ApiError> {
    if !model.allowed_binding_purposes.contains(&binding_purpose) {
        let allowed = model
            .allowed_binding_purposes
            .iter()
            .map(|purpose| binding_purpose_key(*purpose))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ApiError::BadRequest(format!(
            "binding purpose {} is incompatible with model {}; allowed purposes: {}",
            binding_purpose_key(binding_purpose),
            model.model_name,
            allowed,
        )));
    }

    let required_capability_kind =
        if binding_purpose == AiBindingPurpose::EmbedChunk { "embedding" } else { "chat" };
    if model.capability_kind != required_capability_kind {
        return Err(ApiError::BadRequest(format!(
            "binding purpose {} requires a {} model; model {} declares capability {}",
            binding_purpose_key(binding_purpose),
            required_capability_kind,
            model.model_name,
            model.capability_kind,
        )));
    }

    if binding_purpose == AiBindingPurpose::ExtractText && model.modality_kind != "multimodal" {
        return Err(ApiError::BadRequest(format!(
            "binding purpose {} requires a multimodal model; model {} declares modality {}",
            binding_purpose_key(binding_purpose),
            model.model_name,
            model.modality_kind,
        )));
    }

    Ok(())
}

fn validate_model_catalog_binding_contract(
    provider: &ProviderCatalogEntry,
    model_name: &str,
    capability_kind: &str,
    modality_kind: &str,
    allowed_binding_purposes: &[AiBindingPurpose],
) -> Result<(), ApiError> {
    let model = ModelCatalogEntry {
        id: Uuid::nil(),
        provider_catalog_id: provider.id,
        model_name: model_name.to_string(),
        capability_kind: capability_kind.to_string(),
        modality_kind: modality_kind.to_string(),
        lifecycle_state: "active".to_string(),
        metadata_json: serde_json::Value::Null,
        allowed_binding_purposes: allowed_binding_purposes.to_vec(),
        context_window: None,
        max_output_tokens: None,
    };
    for binding_purpose in allowed_binding_purposes {
        validate_model_binding_purpose(*binding_purpose, &model)?;
        validate_provider_capability_for_binding(provider, *binding_purpose)?;
    }
    Ok(())
}
