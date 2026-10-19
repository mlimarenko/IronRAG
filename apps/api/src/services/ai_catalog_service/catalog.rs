use super::*;
use std::collections::{BTreeSet, HashMap};

use crate::integrations::typesafe::{MAX_SCORE_LEVELS, MIN_SCORE_LEVELS};

struct NormalizedModelCatalogSpec {
    model_name: String,
    lifecycle_state: String,
    allowed_binding_purposes: Vec<String>,
}

fn normalize_model_catalog_spec(
    model_name: &str,
    lifecycle_state: &str,
    allowed_binding_purposes: &[AiBindingPurpose],
) -> Result<NormalizedModelCatalogSpec, ApiError> {
    let model_name = normalize_non_empty(model_name, "modelName")?;
    let lifecycle_state = normalize_lifecycle_state(lifecycle_state, "lifecycleState")?;
    validate_model_catalog_binding_contract(allowed_binding_purposes)?;
    Ok(NormalizedModelCatalogSpec {
        model_name,
        lifecycle_state,
        allowed_binding_purposes: binding_purpose_values(allowed_binding_purposes),
    })
}

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
            ai_repository::NewProviderCatalog {
                provider_kind: &provider_kind,
                display_name: &display_name,
                api_style: &api_style,
                lifecycle_state: &lifecycle_state,
                default_base_url: default_base_url.as_deref(),
                capability_flags_json: command.capability_flags_json,
            },
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
            ai_repository::UpdateProviderCatalog {
                provider_id: command.provider_id,
                provider_kind: &provider_kind,
                display_name: &display_name,
                api_style: &api_style,
                lifecycle_state: &lifecycle_state,
                default_base_url: default_base_url.as_deref(),
                capability_flags_json: command.capability_flags_json,
            },
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
        let provider = self.get_provider_catalog(state, command.provider_catalog_id).await?;
        validate_model_purposes_for_provider(&provider, &command.allowed_binding_purposes)?;
        let normalized = normalize_model_catalog_spec(
            &command.model_name,
            &command.lifecycle_state,
            &command.allowed_binding_purposes,
        )?;
        let metadata_json = validate_model_metadata(command.metadata_json)?;
        command.pricing.validate().map_err(ApiError::BadRequest)?;
        let row = ai_repository::create_model_catalog(
            &state.persistence.postgres,
            ai_repository::NewModelCatalog {
                provider_catalog_id: command.provider_catalog_id,
                model_name: &normalized.model_name,
                context_window: command.context_window,
                max_output_tokens: command.max_output_tokens,
                lifecycle_state: &normalized.lifecycle_state,
                metadata_json,
                pricing: &model_pricing_binds(&command.pricing),
                allowed_binding_purposes: &normalized.allowed_binding_purposes,
            },
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
        let existing =
            ai_repository::get_model_catalog_by_id(&state.persistence.postgres, command.model_id)
                .await
                .map_err(map_ai_write_error)?
                .ok_or_else(|| ApiError::resource_not_found("model_catalog", command.model_id))?;
        let provider = self.get_provider_catalog(state, existing.provider_catalog_id).await?;
        validate_model_purposes_for_provider(&provider, &command.allowed_binding_purposes)?;
        let normalized = normalize_model_catalog_spec(
            &existing.model_name,
            &command.lifecycle_state,
            &command.allowed_binding_purposes,
        )?;
        let metadata_json =
            validate_model_metadata(command.metadata_json.unwrap_or(existing.metadata_json))?;
        command.pricing.validate().map_err(ApiError::BadRequest)?;
        let row = ai_repository::update_model_catalog(
            &state.persistence.postgres,
            ai_repository::UpdateModelCatalog {
                model_id: command.model_id,
                context_window: command.context_window,
                max_output_tokens: command.max_output_tokens,
                lifecycle_state: &normalized.lifecycle_state,
                metadata_json,
                pricing: &model_pricing_binds(&command.pricing),
                allowed_binding_purposes: &normalized.allowed_binding_purposes,
            },
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("model_catalog", command.model_id))?;
        map_model_row(row)
    }

    /// Real hard delete; 409 (via `map_ai_delete_error`) when a binding
    /// still references this model.
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
        let scoped_accounts =
            self.resolved_catalog_accounts(state, workspace_id, library_id, account_id).await?;
        let available_account_ids_by_provider =
            available_account_ids_by_provider(&scoped_accounts, provider_catalog_id);

        let models = self.list_model_catalog(state, provider_catalog_id).await?;
        Ok(models
            .into_iter()
            .map(|model| {
                resolve_model_catalog_entry(
                    model,
                    &provider_by_id,
                    &available_account_ids_by_provider,
                )
            })
            .collect())
    }

    async fn resolved_catalog_accounts(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
        account_id: Option<Uuid>,
    ) -> Result<Vec<AiAccountSummary>, ApiError> {
        let summaries =
            self.list_visible_account_summaries(state, workspace_id, library_id).await?;
        let Some(target_account_id) = account_id else {
            return Ok(summaries);
        };
        let summary =
            summaries.into_iter().find(|account| account.id == target_account_id).ok_or_else(
                || ApiError::resource_not_found("provider_credential", target_account_id),
            )?;
        Ok(vec![summary])
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

fn resolve_model_catalog_entry(
    model: ModelCatalogEntry,
    provider_by_id: &HashMap<Uuid, &ProviderCatalogEntry>,
    available_account_ids_by_provider: &HashMap<Uuid, BTreeSet<Uuid>>,
) -> ResolvedModelCatalogEntry {
    let available_account_ids =
        model_available_account_ids(&model, available_account_ids_by_provider);
    let availability_state = model_availability_state(
        &model,
        provider_by_id.get(&model.provider_catalog_id).copied(),
        &available_account_ids,
    );
    ResolvedModelCatalogEntry { model, availability_state, available_account_ids }
}

pub(super) fn model_available_account_ids(
    model: &ModelCatalogEntry,
    available_account_ids_by_provider: &HashMap<Uuid, BTreeSet<Uuid>>,
) -> Vec<Uuid> {
    available_account_ids_by_provider
        .get(&model.provider_catalog_id)
        .map(|ids| ids.iter().copied().collect())
        .unwrap_or_default()
}

fn model_availability_state(
    model: &ModelCatalogEntry,
    provider: Option<&ProviderCatalogEntry>,
    available_account_ids: &[Uuid],
) -> ModelAvailabilityState {
    if model.lifecycle_state == "disabled"
        || provider.is_some_and(|entry| entry.lifecycle_state == "disabled")
    {
        return ModelAvailabilityState::Unavailable;
    }

    match provider.map(|entry| entry.model_discovery.mode) {
        Some(ProviderModelDiscoveryMode::Credential) | None => ModelAvailabilityState::Unknown,
        Some(_) => {
            if available_account_ids.is_empty() {
                ModelAvailabilityState::Unavailable
            } else {
                ModelAvailabilityState::Available
            }
        }
    }
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

pub(super) fn validate_model_metadata(
    metadata_json: serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let object = match metadata_json {
        serde_json::Value::Null => serde_json::Map::new(),
        serde_json::Value::Object(object) => object,
        _ => {
            return Err(ApiError::BadRequest("metadataJson must be an object".to_string()));
        }
    };
    if let Some(value) = object.get("requestPolicy") {
        parse_request_policy(value, "metadataJson.requestPolicy")?;
    }
    // The pair reasoning capability is owned by this exact model row. Validate
    // it here so an unattributed or malformed confirmation never persists; a
    // provider profile cannot carry it.
    crate::domains::provider_profiles::model_reasoning_capability(&serde_json::Value::Object(
        object.clone(),
    ))
    .map_err(ApiError::BadRequest)?;
    if object.contains_key("defaultRoles") {
        return Err(ApiError::BadRequest(
            "metadataJson.defaultRoles is reserved; use allowedBindingPurposes".to_string(),
        ));
    }
    Ok(serde_json::Value::Object(object))
}

fn binding_purpose_values(purposes: &[AiBindingPurpose]) -> Vec<String> {
    deduplicate_binding_purposes(purposes)
        .into_iter()
        .map(|purpose| binding_purpose_key(purpose).to_string())
        .collect()
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
        purpose_gaps: profile.purpose_gaps.clone(),
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
    validate_decision_capability_invariant(provider_kind, &profile)?;
    Ok(profile)
}

/// The canonical decision-capability invariant (IRG-182): `decision` is
/// supported if and only if the runtime kind is the TypeSafe System One
/// decision endpoint, and a decision provider declares no generative
/// capability and only `decision` discovery paths. Enforced on every parse —
/// create, update and read — so a hand-edited or drifted row fails closed
/// instead of silently letting a decision provider serve a generative purpose
/// or a generative provider claim the decision capability.
fn validate_decision_capability_invariant(
    provider_kind: &str,
    profile: &ProviderProfile,
) -> Result<(), ApiError> {
    let is_decision_runtime = profile.runtime.kind == TYPESAFE_SYSTEM_ONE_RUNTIME_KIND;
    let declares_decision = profile.capabilities.decision == ProviderCapabilityState::Supported;
    if is_decision_runtime != declares_decision {
        return Err(ApiError::BadRequest(format!(
            "provider {provider_kind} must declare capabilities.decision supported if and only if runtime.kind is {TYPESAFE_SYSTEM_ONE_RUNTIME_KIND}"
        )));
    }
    if !is_decision_runtime {
        return Ok(());
    }
    let levels = &profile.runtime.score_criteria;
    if !(MIN_SCORE_LEVELS..=MAX_SCORE_LEVELS).contains(&levels.len())
        || levels.iter().any(|level| level.trim().is_empty())
    {
        return Err(ApiError::BadRequest(format!(
            "provider {provider_kind} is a decision provider ({TYPESAFE_SYSTEM_ONE_RUNTIME_KIND}) and must declare {MIN_SCORE_LEVELS}..={MAX_SCORE_LEVELS} non-empty runtime.scoreCriteria levels"
        )));
    }
    let caps = &profile.capabilities;
    let generative_capability_declared =
        [caps.chat, caps.embeddings, caps.vision, caps.tools, caps.streaming]
            .into_iter()
            .any(|state| state != ProviderCapabilityState::Unsupported);
    if generative_capability_declared {
        return Err(ApiError::BadRequest(format!(
            "provider {provider_kind} is a decision provider ({TYPESAFE_SYSTEM_ONE_RUNTIME_KIND}) and must mark chat, embeddings, vision, tools and streaming unsupported"
        )));
    }
    let non_decision_discovery_path = profile
        .model_discovery
        .paths
        .iter()
        .any(|path| path.capability_kind != ProviderModelCapabilityKind::Decision);
    if non_decision_discovery_path {
        return Err(ApiError::BadRequest(format!(
            "provider {provider_kind} is a decision provider ({TYPESAFE_SYSTEM_ONE_RUNTIME_KIND}) and every modelDiscovery path must declare capabilityKind decision"
        )));
    }
    Ok(())
}

fn model_pricing_binds(pricing: &ModelPricing) -> ai_repository::ModelPricingBinds {
    ai_repository::ModelPricingBinds {
        currency_code: pricing.currency_code.trim().to_string(),
        input_price_per_1m: pricing.input_price_per_1m,
        cached_input_price_per_1m: pricing.cached_input_price_per_1m,
        cache_write_input_price_per_1m: pricing.cache_write_input_price_per_1m,
        output_price_per_1m: pricing.output_price_per_1m,
        long_context_threshold_tokens: pricing
            .long_context
            .as_ref()
            .map(|tier| tier.threshold_tokens),
        long_context_input_price_per_1m: pricing
            .long_context
            .as_ref()
            .map(|tier| tier.input_price_per_1m),
        long_context_cached_input_price_per_1m: pricing
            .long_context
            .as_ref()
            .map(|tier| tier.cached_input_price_per_1m),
        long_context_output_price_per_1m: pricing
            .long_context
            .as_ref()
            .map(|tier| tier.output_price_per_1m),
    }
}

pub(super) fn map_model_row(
    row: ai_repository::AiModelCatalogRow,
) -> Result<ModelCatalogEntry, ApiError> {
    let allowed_binding_purposes = row
        .allowed_binding_purposes
        .iter()
        .map(|purpose| {
            parse_binding_purpose(purpose).map_err(|_| {
                ApiError::BadRequest(format!(
                    "model {:?} has unsupported binding purpose {purpose:?}",
                    row.model_name
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let long_context = match (
        row.long_context_threshold_tokens,
        row.long_context_input_price_per_1m,
        row.long_context_cached_input_price_per_1m,
        row.long_context_output_price_per_1m,
    ) {
        (Some(threshold_tokens), Some(input), Some(cached_input), Some(output)) => {
            Some(crate::domains::ai::ModelLongContextPricing {
                threshold_tokens,
                input_price_per_1m: input,
                cached_input_price_per_1m: cached_input,
                output_price_per_1m: output,
            })
        }
        (None, None, None, None) => None,
        _ => {
            return Err(ApiError::internal_with_log(
                anyhow::anyhow!("model {} carries a partial long-context tier", row.id),
                "internal",
            ));
        }
    };
    Ok(ModelCatalogEntry {
        id: row.id,
        provider_catalog_id: row.provider_catalog_id,
        model_name: row.model_name,
        lifecycle_state: row.lifecycle_state,
        pricing: ModelPricing {
            currency_code: row.currency_code,
            input_price_per_1m: row.input_price_per_1m,
            cached_input_price_per_1m: row.cached_input_price_per_1m,
            cache_write_input_price_per_1m: row.cache_write_input_price_per_1m,
            output_price_per_1m: row.output_price_per_1m,
            long_context,
        },
        metadata_json: validate_model_metadata(row.metadata_json)?,
        allowed_binding_purposes,
        context_window: row.context_window,
        max_output_tokens: row.max_output_tokens,
    })
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

    Ok(())
}

fn validate_model_catalog_binding_contract(
    allowed_binding_purposes: &[AiBindingPurpose],
) -> Result<(), ApiError> {
    if allowed_binding_purposes.is_empty() {
        return Err(ApiError::BadRequest("allowedBindingPurposes must not be empty".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_model_catalog_spec_and_deduplicates_allowed_purposes() {
        let normalized = normalize_model_catalog_spec(
            "  synthetic-model  ",
            "active",
            &[
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
                AiBindingPurpose::QueryAnswer,
            ],
        )
        .expect("a complete catalog model specification must be accepted");

        assert_eq!(normalized.model_name, "synthetic-model");
        assert_eq!(normalized.lifecycle_state, "active");
        assert_eq!(normalized.allowed_binding_purposes, ["query_answer", "agent"]);
    }

    #[test]
    fn rejects_invalid_model_catalog_invariants_before_repository_write() {
        let no_purposes = normalize_model_catalog_spec("synthetic-model", "active", &[]);
        let unsupported_lifecycle = normalize_model_catalog_spec(
            "synthetic-model",
            "retired",
            &[AiBindingPurpose::QueryAnswer],
        );
        let blank_name =
            normalize_model_catalog_spec(" \t ", "active", &[AiBindingPurpose::QueryAnswer]);

        for result in [no_purposes, unsupported_lifecycle, blank_name] {
            assert!(matches!(result, Err(ApiError::BadRequest(_))));
        }
    }

    #[test]
    fn model_metadata_normalizes_null_and_rejects_non_object_or_legacy_authority() {
        assert_eq!(
            validate_model_metadata(serde_json::Value::Null)
                .expect("null metadata must normalize to an empty object"),
            serde_json::json!({})
        );

        for metadata in [
            serde_json::json!(["not-an-object"]),
            serde_json::json!({"defaultRoles": ["query_answer"]}),
        ] {
            assert!(matches!(validate_model_metadata(metadata), Err(ApiError::BadRequest(_))));
        }
    }

    /// The TypeSafe System One decision provider shape the seed publishes:
    /// every generative capability unsupported, `decision` the only supported
    /// one, and a single decision discovery path.
    fn decision_provider_profile() -> serde_json::Value {
        serde_json::json!({
            "runtime": {
                "kind": "typesafe_system_one",
                "authScheme": "bearer",
                "tokenLimitParameter": "max_tokens",
                "structuredOutput": "unsupported",
                "chatPath": "/v1/systemone",
                "embeddingsPath": null,
                "modelsPath": "/v1/models",
                "scoreCriteria": [
                    "not relevant to the query",
                    "weakly relevant to the query",
                    "moderately relevant to the query",
                    "strongly relevant to the query",
                    "directly and completely answers the query"
                ]
            },
            "credentials": {
                "apiKeyRequired": true,
                "baseUrlRequired": false,
                "baseUrlMode": "fixed",
                "validationMode": "model_list"
            },
            "baseUrl": {
                "allowOverride": false,
                "requireHttps": true,
                "allowPrivateNetwork": false,
                "trimSuffixes": []
            },
            "modelDiscovery": {
                "mode": "credential",
                "paths": [{"capabilityKind": "decision", "path": "/v1/models"}],
                "listedPricingCurrency": null
            },
            "capabilities": {
                "chat": "unsupported",
                "embeddings": "unsupported",
                "vision": "unsupported",
                "streaming": "unsupported",
                "tools": "unsupported",
                "modelDiscovery": "supported",
                "decision": "supported"
            }
        })
    }

    #[test]
    fn decision_capability_is_supported_exactly_for_the_decision_runtime() {
        let valid = decision_provider_profile();
        let parsed = parse_provider_profile("typesafe", &valid)
            .expect("the decision provider shape must parse");
        assert_eq!(parsed.capabilities.decision, ProviderCapabilityState::Supported);

        let mut generative_runtime = decision_provider_profile();
        generative_runtime["runtime"]["kind"] = serde_json::json!("openai_compatible");
        assert!(
            matches!(
                parse_provider_profile("typesafe", &generative_runtime),
                Err(ApiError::BadRequest(_))
            ),
            "a generative runtime must not declare the decision capability"
        );

        let mut undeclared = decision_provider_profile();
        undeclared["capabilities"]["decision"] = serde_json::json!("unsupported");
        assert!(
            matches!(parse_provider_profile("typesafe", &undeclared), Err(ApiError::BadRequest(_))),
            "the decision runtime must declare the decision capability"
        );
    }

    #[test]
    fn a_decision_provider_without_a_score_rubric_is_refused() {
        let mut missing = decision_provider_profile();
        missing["runtime"].as_object_mut().expect("runtime is an object").remove("scoreCriteria");
        assert!(
            matches!(parse_provider_profile("typesafe", &missing), Err(ApiError::BadRequest(_))),
            "a decision runtime must declare its score rubric"
        );

        let mut too_short = decision_provider_profile();
        too_short["runtime"]["scoreCriteria"] = serde_json::json!(["only one level"]);
        assert!(
            matches!(parse_provider_profile("typesafe", &too_short), Err(ApiError::BadRequest(_))),
            "a rubric outside 2..=10 levels must be refused"
        );

        let mut blank_level = decision_provider_profile();
        blank_level["runtime"]["scoreCriteria"] = serde_json::json!(["one", " ", "three"]);
        assert!(
            matches!(
                parse_provider_profile("typesafe", &blank_level),
                Err(ApiError::BadRequest(_))
            ),
            "a blank level must be refused"
        );
    }

    #[test]
    fn a_decision_provider_declares_only_decision_discovery_paths() {
        let mut chat_path = decision_provider_profile();
        chat_path["modelDiscovery"]["paths"][0]["capabilityKind"] = serde_json::json!("chat");
        assert!(matches!(
            parse_provider_profile("typesafe", &chat_path),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn a_decision_provider_marks_generative_capabilities_unsupported() {
        let mut chat_capable = decision_provider_profile();
        chat_capable["capabilities"]["chat"] = serde_json::json!("supported");
        assert!(matches!(
            parse_provider_profile("typesafe", &chat_capable),
            Err(ApiError::BadRequest(_))
        ));
    }
}
