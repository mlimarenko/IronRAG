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

    pub async fn disable_provider_catalog(
        &self,
        state: &AppState,
        provider_catalog_id: Uuid,
    ) -> Result<ProviderCatalogEntry, ApiError> {
        let row = ai_repository::disable_provider_catalog(
            &state.persistence.postgres,
            provider_catalog_id,
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("provider_catalog", provider_catalog_id))?;
        map_provider_row(row)
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

    pub async fn disable_model_catalog(
        &self,
        state: &AppState,
        model_catalog_id: Uuid,
    ) -> Result<ModelCatalogEntry, ApiError> {
        let row =
            ai_repository::disable_model_catalog(&state.persistence.postgres, model_catalog_id)
                .await
                .map_err(map_ai_write_error)?
                .ok_or_else(|| ApiError::resource_not_found("model_catalog", model_catalog_id))?;
        map_model_row(row)
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
        let scoped_credentials = match credential_id {
            Some(target_credential_id) => vec![
                visible_credentials
                    .iter()
                    .find(|credential| credential.id == target_credential_id)
                    .cloned()
                    .ok_or_else(|| {
                        ApiError::resource_not_found("provider_credential", target_credential_id)
                    })?,
            ],
            None => visible_credentials,
        };

        let mut available_credential_ids_by_provider = HashMap::<Uuid, BTreeSet<Uuid>>::new();
        for credential in
            scoped_credentials.iter().filter(|credential| credential.credential_state == "active")
        {
            if provider_catalog_id.is_some_and(|value| value != credential.provider_catalog_id) {
                continue;
            }
            available_credential_ids_by_provider
                .entry(credential.provider_catalog_id)
                .or_default()
                .insert(credential.id);
        }

        let mut explicitly_available_credential_ids =
            HashMap::<(Uuid, String), BTreeSet<Uuid>>::new();
        let mut explicitly_checked_providers = BTreeSet::<Uuid>::new();
        if let Some(target_credential_id) = credential_id {
            if let Some(credential) = scoped_credentials.iter().find(|credential| {
                credential.id == target_credential_id && credential.credential_state == "active"
            }) {
                if let Some(provider) = provider_by_id.get(&credential.provider_catalog_id) {
                    let should_refresh = provider_catalog_id
                        .is_none_or(|value| value == provider.id)
                        && provider_credential_policy(provider).validation_mode
                            == ProviderCredentialValidationMode::ModelList;
                    if should_refresh {
                        match sync_provider_model_catalog(
                            state,
                            provider,
                            credential.api_key.as_deref(),
                            credential.base_url.as_deref(),
                        )
                        .await
                        {
                            Ok(model_names) => {
                                explicitly_checked_providers.insert(provider.id);
                                for model_name in model_names {
                                    explicitly_available_credential_ids
                                        .entry((provider.id, model_name))
                                        .or_default()
                                        .insert(credential.id);
                                }
                            }
                            Err(error) => {
                                tracing::warn!(
                                    provider_kind = %provider.provider_kind,
                                    credential_id = %credential.id,
                                    error = %error,
                                    "failed to refresh provider models for credential-specific request"
                                );
                            }
                        }
                    }
                }
            }
        }

        let models = self.list_model_catalog(state, provider_catalog_id).await?;
        Ok(models
            .into_iter()
            .map(|model| {
                let available_credential_ids = if explicitly_checked_providers
                    .contains(&model.provider_catalog_id)
                {
                    explicitly_available_credential_ids
                        .get(&(model.provider_catalog_id, model.model_name.clone()))
                        .map(|credential_ids| credential_ids.iter().copied().collect::<Vec<_>>())
                        .unwrap_or_default()
                } else {
                    available_credential_ids_by_provider
                        .get(&model.provider_catalog_id)
                        .map(|credential_ids| credential_ids.iter().copied().collect::<Vec<_>>())
                        .unwrap_or_default()
                };
                let availability_state = if model.lifecycle_state == "disabled"
                    || provider_by_id
                        .get(&model.provider_catalog_id)
                        .is_some_and(|provider| provider.lifecycle_state == "disabled")
                {
                    ModelAvailabilityState::Unavailable
                } else {
                    match provider_by_id
                        .get(&model.provider_catalog_id)
                        .map(|provider| provider.model_discovery.mode)
                    {
                        Some(ProviderModelDiscoveryMode::Credential)
                            if explicitly_checked_providers
                                .contains(&model.provider_catalog_id) =>
                        {
                            if available_credential_ids.is_empty() {
                                ModelAvailabilityState::Unavailable
                            } else {
                                ModelAvailabilityState::Available
                            }
                        }
                        Some(ProviderModelDiscoveryMode::Credential) => {
                            ModelAvailabilityState::Unknown
                        }
                        Some(_) => ModelAvailabilityState::Available,
                        None => ModelAvailabilityState::Unknown,
                    }
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

fn metadata_with_binding_purposes(
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

fn map_provider_row(
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

fn parse_provider_profile(
    provider_kind: &str,
    capability_flags_json: &serde_json::Value,
) -> Result<ProviderProfile, ApiError> {
    serde_json::from_value::<ProviderProfile>(capability_flags_json.clone()).map_err(|error| {
        ApiError::BadRequest(format!(
            "provider {provider_kind} has invalid provider profile metadata: {error}"
        ))
    })
}

fn map_model_row(row: ai_repository::AiModelCatalogRow) -> Result<ModelCatalogEntry, ApiError> {
    Ok(ModelCatalogEntry {
        id: row.id,
        provider_catalog_id: row.provider_catalog_id,
        model_name: row.model_name,
        capability_kind: row.capability_kind,
        modality_kind: row.modality_kind,
        lifecycle_state: row.lifecycle_state,
        metadata_json: row.metadata_json.clone(),
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

pub(super) fn parse_allowed_binding_purposes(
    metadata_json: &serde_json::Value,
) -> Result<Vec<AiBindingPurpose>, ApiError> {
    let Some(roles) = metadata_json.get("defaultRoles").and_then(serde_json::Value::as_array)
    else {
        return Ok(Vec::new());
    };

    let mut allowed = Vec::with_capacity(roles.len());
    for role in roles {
        let Some(role_str) = role.as_str() else {
            continue;
        };
        // Catalog seeds carry forward-compatible role labels (e.g. `rerank`,
        // `utility`) that aren't bound to AiBindingPurpose variants yet.
        // Skip unknown labels instead of failing — otherwise a single
        // unmapped role poisons every binding lookup that lists the
        // catalog (the error path swallows the original message and
        // surfaces as bare "internal server error").
        if let Ok(purpose) = parse_binding_purpose(role_str) {
            if !allowed.contains(&purpose) {
                allowed.push(purpose);
            }
        }
    }
    Ok(allowed)
}

pub(super) fn validate_model_binding_purpose(
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
