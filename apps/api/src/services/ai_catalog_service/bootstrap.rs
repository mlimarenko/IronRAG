pub(super) use super::bootstrap_persistence::{
    ensure_bootstrap_binding, ensure_bootstrap_provider_account,
};
use super::provider_validation::fetch_provider_model_names;
use super::*;
use serde::Deserialize;

/// Default `temperature` for generative (non-embedding) bootstrap bindings.
///
/// Every generative purpose in this product emits either a schema-validated
/// structured object (`query_compile`, `extract_graph`) or text bound to
/// retrieved evidence (`query_answer`, `extract_text`, `agent`). None of them
/// is open-ended writing, so sampling variance buys nothing and costs
/// reproducibility: the same question compiles to a different typed query and
/// then reads as a different answer on a second ask. Operators can still raise
/// this per binding when they want variety.
const DEFAULT_BOOTSTRAP_TEMPERATURE: f64 = 0.0;
/// Default `top_p` for generative (non-embedding) bootstrap bindings.
///
/// Paired with a zero temperature: restricting the nucleus as well would drop
/// tokens the greedy choice may still need, without adding determinism.
const DEFAULT_BOOTSTRAP_TOP_P: f64 = 1.0;

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

pub(super) fn bootstrap_credential_source(
    configured_ai: Option<&crate::app::config::UiBootstrapAiSetup>,
    provider_kind: &str,
) -> BootstrapAiCredentialSource {
    if bootstrap_provider_secret(configured_ai, provider_kind).is_some() {
        BootstrapAiCredentialSource::Env
    } else {
        BootstrapAiCredentialSource::Missing
    }
}

pub(super) fn bootstrap_provider_credential_map(
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

pub(super) fn bootstrap_bundle_is_self_contained(
    bundle: &BootstrapAiProviderBindingBundle,
) -> bool {
    bundle
        .bindings
        .iter()
        .all(|binding| binding.owner_provider_catalog_id == bundle.provider_catalog_id)
}

fn configured_binding_default_for_purpose(
    configured_ai: Option<&crate::app::config::UiBootstrapAiSetup>,
    purpose: AiBindingPurpose,
) -> Option<&crate::app::config::UiBootstrapAiBindingDefault> {
    configured_ai.and_then(|config| {
        config.binding_defaults.iter().find(|binding| binding.binding_purpose == purpose)
    })
}

fn bootstrap_model_supports_purpose(model: &ModelCatalogEntry, purpose: AiBindingPurpose) -> bool {
    validate_model_binding_purpose(purpose, model).is_ok()
}

fn select_configured_bootstrap_model<'a>(
    binding_default: &crate::app::config::UiBootstrapAiBindingDefault,
    purpose: AiBindingPurpose,
    providers: &[ProviderCatalogEntry],
    models: &'a [ModelCatalogEntry],
) -> Result<Option<&'a ModelCatalogEntry>, ApiError> {
    let configured_provider = binding_default
        .provider_kind
        .as_deref()
        .map(|provider_kind| {
            providers.iter().find(|provider| provider.provider_kind == provider_kind).ok_or_else(
                || {
                    ApiError::BadRequest(format!(
                        "configured bootstrap provider `{provider_kind}` is not available"
                    ))
                },
            )
        })
        .transpose()?;
    let model_name = binding_default.model_name.as_deref();

    match (configured_provider, model_name) {
        (Some(provider), Some(model_name)) => Ok(models.iter().find(|model| {
            model.provider_catalog_id == provider.id
                && model.model_name == model_name
                && bootstrap_model_supports_purpose(model, purpose)
        })),
        (Some(provider), None) => {
            Ok(select_bootstrap_suggested_model_for_provider(provider, purpose, models))
        }
        (None, Some(model_name)) => Ok(models.iter().find(|model| {
            model.model_name == model_name && bootstrap_model_supports_purpose(model, purpose)
        })),
        (None, None) => Ok(None),
    }
}

fn select_bootstrap_suggested_model_for_provider<'a>(
    provider: &ProviderCatalogEntry,
    purpose: AiBindingPurpose,
    models: &'a [ModelCatalogEntry],
) -> Option<&'a ModelCatalogEntry> {
    if let Some(preferred_model_name) =
        bootstrap_binding_profile_for_provider_purpose(provider, purpose)
            .map(|profile| profile.model_name)
    {
        return models.iter().find(|model| {
            model.provider_catalog_id == provider.id
                && model.model_name == preferred_model_name
                && validate_model_binding_purpose(purpose, model).is_ok()
        });
    }

    // The MCP/UI agent has its own execution contract and tuning surface.
    // Requiring an explicit provider preset prevents a generic agent-capable
    // model (or the query-answer model) from becoming an implicit default.
    if purpose == AiBindingPurpose::Agent {
        return None;
    }

    models
        .iter()
        .filter(|model| {
            model.provider_catalog_id == provider.id
                && validate_model_binding_purpose(purpose, model).is_ok()
        })
        .min_by(|left, right| {
            left.model_name.cmp(&right.model_name).then_with(|| left.id.cmp(&right.id))
        })
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BootstrapProviderBindingProfile {
    pub(super) purpose: AiBindingPurpose,
    pub(super) model_name: String,
    pub(super) system_prompt: Option<String>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) max_output_tokens_override: Option<i32>,
    pub(super) extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapProviderMetadata {
    #[serde(default)]
    bootstrap_presets: Vec<BootstrapProviderBindingMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapProviderBindingMetadata {
    purpose: String,
    model_name: String,
    system_prompt: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_output_tokens_override: Option<i32>,
    #[serde(default)]
    extra_parameters_json: serde_json::Map<String, serde_json::Value>,
}

fn bootstrap_provider_ui_hints(
    provider: &ProviderCatalogEntry,
) -> Result<serde_json::Value, ApiError> {
    Ok(provider.ui_hints.clone())
}

fn parse_bootstrap_provider_binding_profiles(
    capability_flags_json: &serde_json::Value,
) -> Result<Vec<BootstrapProviderBindingProfile>, String> {
    let metadata =
        serde_json::from_value::<BootstrapProviderMetadata>(capability_flags_json.clone())
            .map_err(|error| format!("bootstrapPresets has invalid schema: {error}"))?;
    let mut seen_purposes = std::collections::BTreeSet::new();
    let mut profiles = Vec::with_capacity(metadata.bootstrap_presets.len());

    for (index, preset) in metadata.bootstrap_presets.into_iter().enumerate() {
        let purpose = parse_binding_purpose(&preset.purpose).map_err(|_| {
            format!(
                "bootstrapPresets[{index}].purpose contains unsupported binding purpose {:?}",
                preset.purpose
            )
        })?;
        let purpose_key = binding_purpose_key(purpose);
        if !seen_purposes.insert(purpose_key) {
            return Err(format!(
                "bootstrapPresets contains duplicate binding purpose {purpose_key:?}"
            ));
        }
        let model_name = preset.model_name.trim();
        if model_name.is_empty() {
            return Err(format!("bootstrapPresets[{index}].modelName must not be blank"));
        }
        let is_embedding = purpose == AiBindingPurpose::EmbedChunk;
        profiles.push(BootstrapProviderBindingProfile {
            purpose,
            model_name: model_name.to_string(),
            system_prompt: normalize_optional(preset.system_prompt.as_deref()),
            temperature: preset
                .temperature
                .or_else(|| (!is_embedding).then_some(DEFAULT_BOOTSTRAP_TEMPERATURE)),
            top_p: preset.top_p.or_else(|| (!is_embedding).then_some(DEFAULT_BOOTSTRAP_TOP_P)),
            max_output_tokens_override: preset.max_output_tokens_override,
            extra_parameters_json: serde_json::Value::Object(preset.extra_parameters_json),
        });
    }

    Ok(profiles)
}

pub(super) fn validate_bootstrap_provider_metadata(
    provider_kind: &str,
    capability_flags_json: &serde_json::Value,
) -> Result<(), ApiError> {
    parse_bootstrap_provider_binding_profiles(capability_flags_json).map(|_| ()).map_err(|error| {
        ApiError::BadRequest(format!(
            "provider {provider_kind} has invalid bootstrap preset metadata: {error}"
        ))
    })
}

fn bootstrap_provider_binding_profile(
    provider: &ProviderCatalogEntry,
) -> Result<Vec<BootstrapProviderBindingProfile>, ApiError> {
    parse_bootstrap_provider_binding_profiles(&provider.capability_flags_json)
        .map_err(|error| ApiError::internal_with_log(error, "invalid provider capability flags"))
}

fn bootstrap_binding_descriptors_from_profile(
    provider: &ProviderCatalogEntry,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
    profile: Vec<BootstrapProviderBindingProfile>,
) -> Vec<BootstrapAiBindingDescriptor> {
    profile
        .into_iter()
        .filter_map(|binding_profile| {
            let model = models.iter().find(|model| {
                model.provider_catalog_id == provider.id
                    && model.model_name == binding_profile.model_name
                    && bootstrap_model_supports_purpose(model, binding_profile.purpose)
            })?;
            let model_owner = providers
                .iter()
                .find(|entry| entry.id == model.provider_catalog_id)
                .unwrap_or(provider);
            Some(BootstrapAiBindingDescriptor {
                binding_purpose: binding_profile.purpose,
                owner_provider_catalog_id: model_owner.id,
                owner_provider_kind: model_owner.provider_kind.clone(),
                model_catalog_id: model.id,
                model_name: model.model_name.clone(),
                system_prompt: binding_profile.system_prompt,
                temperature: binding_profile.temperature,
                top_p: binding_profile.top_p,
                max_output_tokens_override: binding_profile.max_output_tokens_override,
                extra_parameters_json: binding_profile.extra_parameters_json,
            })
        })
        .collect()
}

pub(super) fn bootstrap_binding_profile_for_provider_purpose(
    provider: &ProviderCatalogEntry,
    purpose: AiBindingPurpose,
) -> Option<BootstrapProviderBindingProfile> {
    bootstrap_provider_binding_profile(provider)
        .ok()
        .and_then(|profiles| profiles.into_iter().find(|profile| profile.purpose == purpose))
}

#[cfg(test)]
pub(super) fn resolve_bootstrap_provider_binding_descriptors(
    provider: &ProviderCatalogEntry,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiBindingDescriptor>, ApiError> {
    Ok(bootstrap_binding_descriptors_from_profile(
        provider,
        providers,
        models,
        bootstrap_provider_binding_profile(provider)?,
    ))
}

pub(super) fn resolve_bootstrap_provider_binding_bundle(
    provider: &ProviderCatalogEntry,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
    credential_source: BootstrapAiCredentialSource,
) -> Result<Option<BootstrapAiProviderBindingBundle>, ApiError> {
    let profile = bootstrap_provider_binding_profile(provider)?;
    if !REQUIRED_BINDING_PURPOSES
        .iter()
        .all(|purpose| profile.iter().any(|binding| binding.purpose == *purpose))
    {
        return Ok(None);
    }

    let bindings = bootstrap_binding_descriptors_from_profile(provider, providers, models, profile);
    if !REQUIRED_BINDING_PURPOSES
        .iter()
        .all(|purpose| bindings.iter().any(|binding| binding.binding_purpose == *purpose))
    {
        return Ok(None);
    }

    Ok(Some(BootstrapAiProviderBindingBundle {
        provider_catalog_id: provider.id,
        provider_kind: provider.provider_kind.clone(),
        display_name: provider.display_name.clone(),
        credential_source,
        default_base_url: provider.default_base_url.clone(),
        api_key_required: provider.api_key_required,
        base_url_required: provider.base_url_required,
        credential_policy: provider.credential_policy.clone(),
        base_url_policy: provider.base_url_policy.clone(),
        model_discovery: provider.model_discovery.clone(),
        capabilities: provider.capabilities.clone(),
        runtime: provider.runtime.clone(),
        ui_hints: bootstrap_provider_ui_hints(provider)?,
        bindings,
    }))
}

pub(super) fn resolve_bootstrap_provider_bundle(
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
    provider_kind: &str,
) -> Result<BootstrapAiProviderBindingBundle, ApiError> {
    let normalized_provider_kind = provider_kind.trim().to_ascii_lowercase();
    let provider =
        providers.iter().find(|entry| entry.provider_kind == normalized_provider_kind).ok_or_else(
            || ApiError::resource_not_found("provider_catalog", normalized_provider_kind.clone()),
        )?;
    resolve_bootstrap_provider_binding_bundle(
        provider,
        providers,
        models,
        BootstrapAiCredentialSource::Missing,
    )?
    .ok_or_else(|| {
        ApiError::BadRequest(format!(
            "provider {normalized_provider_kind} does not expose a complete bootstrap binding bundle",
        ))
    })
}

fn build_bootstrap_binding_input(
    provider: &ProviderCatalogEntry,
    model: &ModelCatalogEntry,
    purpose: AiBindingPurpose,
) -> BootstrapAiBindingInput {
    let binding_profile = bootstrap_binding_profile_for_provider_purpose(provider, purpose)
        .filter(|profile| profile.model_name == model.model_name);
    BootstrapAiBindingInput {
        binding_purpose: purpose,
        provider_kind: provider.provider_kind.clone(),
        model_catalog_id: model.id,
        system_prompt: binding_profile.as_ref().and_then(|profile| profile.system_prompt.clone()),
        temperature: binding_profile.as_ref().and_then(|profile| profile.temperature),
        top_p: binding_profile.as_ref().and_then(|profile| profile.top_p),
        max_output_tokens_override: binding_profile
            .as_ref()
            .and_then(|profile| profile.max_output_tokens_override),
        extra_parameters_json: binding_profile
            .as_ref()
            .map_or_else(|| json!({}), |profile| profile.extra_parameters_json.clone()),
    }
}

pub(super) fn resolve_configured_bootstrap_binding_inputs(
    configured_ai: &crate::app::config::UiBootstrapAiSetup,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiBindingInput>, ApiError> {
    let env_provider_kinds = configured_ai
        .provider_secrets
        .iter()
        .map(|secret| secret.provider_kind.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut selections = Vec::new();
    for purpose in REQUIRED_BINDING_PURPOSES {
        if let Some(binding_default) =
            configured_binding_default_for_purpose(Some(configured_ai), purpose)
            && let Some(model) =
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
                selections.push(build_bootstrap_binding_input(provider, model, purpose));
                continue;
            }
        }

        let bundled_selection = providers
            .iter()
            .filter(|provider| env_provider_kinds.contains(provider.provider_kind.as_str()))
            .filter_map(|provider| {
                resolve_bootstrap_provider_binding_bundle(
                    provider,
                    providers,
                    models,
                    BootstrapAiCredentialSource::Env,
                )
                .ok()
                .flatten()
                .and_then(|bundle| {
                    bundle
                        .bindings
                        .into_iter()
                        .find(|binding| binding.binding_purpose == purpose)
                        .map(|binding| BootstrapAiBindingInput {
                            binding_purpose: binding.binding_purpose,
                            provider_kind: binding.owner_provider_kind,
                            model_catalog_id: binding.model_catalog_id,
                            system_prompt: binding.system_prompt,
                            temperature: binding.temperature,
                            top_p: binding.top_p,
                            max_output_tokens_override: binding.max_output_tokens_override,
                            extra_parameters_json: binding.extra_parameters_json,
                        })
                })
            })
            .min_by(|left, right| left.provider_kind.cmp(&right.provider_kind));
        if let Some(selection) = bundled_selection {
            selections.push(selection);
            continue;
        }

        let suggested_selection = providers
            .iter()
            .filter(|provider| env_provider_kinds.contains(provider.provider_kind.as_str()))
            .filter_map(|provider| {
                select_bootstrap_suggested_model_for_provider(provider, purpose, models)
                    .map(|model| build_bootstrap_binding_input(provider, model, purpose))
            })
            .min_by(|left, right| left.provider_kind.cmp(&right.provider_kind));
        if let Some(selection) = suggested_selection {
            selections.push(selection);
        }
    }
    Ok(selections)
}

pub(super) fn bootstrap_binding_inputs_cover_required_purposes(
    inputs: &[BootstrapAiBindingInput],
) -> bool {
    REQUIRED_BINDING_PURPOSES
        .iter()
        .all(|purpose| inputs.iter().any(|selection| selection.binding_purpose == *purpose))
}

pub(super) fn validate_bootstrap_binding_inputs_cover_required_purposes(
    inputs: &[BootstrapAiBindingInput],
) -> Result<(), ApiError> {
    if !bootstrap_binding_inputs_cover_required_purposes(inputs) {
        return Err(ApiError::BadRequest(
            "bootstrap binding bundle must cover extract_text, extract_graph, embed_chunk, query_compile, query_answer, and agent"
                .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn normalize_bootstrap_binding_inputs(
    inputs: &[BootstrapAiBindingInput],
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiBindingInput>, ApiError> {
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
        normalized.push(BootstrapAiBindingInput {
            binding_purpose: input.binding_purpose,
            provider_kind,
            model_catalog_id: input.model_catalog_id,
            system_prompt: normalize_optional(input.system_prompt.as_deref()),
            temperature: input.temperature,
            top_p: input.top_p,
            max_output_tokens_override: input.max_output_tokens_override,
            extra_parameters_json: input.extra_parameters_json.clone(),
        });
    }
    Ok(normalized)
}

pub(super) fn missing_bootstrap_model_list_models(
    provider: &ProviderCatalogEntry,
    binding_inputs: &[BootstrapAiBindingInput],
    models: &[ModelCatalogEntry],
    discovered_model_names: &[String],
) -> Result<Vec<String>, ApiError> {
    let discovered = discovered_model_names
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let mut selected = std::collections::BTreeSet::new();
    for input in binding_inputs.iter().filter(|input| input.provider_kind == provider.provider_kind)
    {
        let model = models
            .iter()
            .find(|model| model.id == input.model_catalog_id)
            .ok_or_else(|| ApiError::resource_not_found("model_catalog", input.model_catalog_id))?;
        if model.provider_catalog_id != provider.id {
            return Err(ApiError::BadRequest(
                "bootstrap model selection must belong to the selected provider".to_string(),
            ));
        }
        selected.insert(model.model_name.as_str());
    }

    Ok(selected
        .into_iter()
        .filter(|model_name| !discovered.contains(model_name))
        .map(ToString::to_string)
        .collect())
}

pub(super) async fn validate_bootstrap_model_list_binding_inputs(
    provider: &ProviderCatalogEntry,
    account: &AiAccount,
    binding_inputs: &[BootstrapAiBindingInput],
    models: &[ModelCatalogEntry],
) -> Result<(), ApiError> {
    if provider.credential_policy.validation_mode != ProviderCredentialValidationMode::ModelList {
        return Ok(());
    }
    let Some(base_url) = runtime_provider_base_url(provider, account.base_url.as_deref())? else {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires a baseUrl",
            provider.provider_kind
        )));
    };
    let discovered_model_names =
        fetch_provider_model_names(provider, account.api_key.as_deref(), &base_url).await?;
    let missing_model_names = missing_bootstrap_model_list_models(
        provider,
        binding_inputs,
        models,
        &discovered_model_names,
    )?;
    if missing_model_names.is_empty() {
        return Ok(());
    }

    Err(ApiError::BadRequest(format!(
        "bootstrap provider {} selected binding model(s) not returned by provider model discovery: {}",
        provider.provider_kind,
        missing_model_names.join(", ")
    )))
}
