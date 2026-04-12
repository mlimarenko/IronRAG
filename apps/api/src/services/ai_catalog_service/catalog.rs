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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
}

fn normalize_currency_code(value: &str) -> Result<String, ApiError> {
    let normalized = normalize_non_empty(value, "currencyCode")?;
    Ok(normalized.to_ascii_uppercase())
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

pub(super) fn parse_allowed_binding_purposes(
    metadata_json: &serde_json::Value,
) -> Result<Vec<AiBindingPurpose>, ApiError> {
    let roles = metadata_json
        .get("defaultRoles")
        .and_then(serde_json::Value::as_array)
        .ok_or(ApiError::Internal)?;
    if roles.is_empty() {
        return Err(ApiError::Internal);
    }

    let mut allowed = Vec::with_capacity(roles.len());
    for role in roles {
        let role = role.as_str().ok_or(ApiError::Internal)?;
        let purpose = parse_binding_purpose(role)?;
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
