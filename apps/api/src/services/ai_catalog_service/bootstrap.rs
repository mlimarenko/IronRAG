use super::provider_validation::fetch_provider_model_names_for_capabilities;
use super::*;
use serde::Deserialize;

/// Default `temperature` for generative (non-embedding) bootstrap presets.
const DEFAULT_BOOTSTRAP_TEMPERATURE: f64 = 0.3;
/// Default `top_p` for generative (non-embedding) bootstrap presets.
const DEFAULT_BOOTSTRAP_TOP_P: f64 = 0.9;

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

pub(super) fn bootstrap_bundle_is_self_contained(bundle: &BootstrapAiProviderPresetBundle) -> bool {
    bundle
        .presets
        .iter()
        .all(|preset| preset.owner_provider_catalog_id == bundle.provider_catalog_id)
}

fn configured_binding_default_for_purpose<'a>(
    configured_ai: Option<&'a crate::app::config::UiBootstrapAiSetup>,
    purpose: AiBindingPurpose,
) -> Option<&'a crate::app::config::UiBootstrapAiBindingDefault> {
    configured_ai.and_then(|config| {
        config
            .binding_defaults
            .iter()
            .find(|binding| binding.binding_purpose == binding_purpose_key(purpose))
    })
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
                && model.allowed_binding_purposes.contains(&purpose)
        })),
        (Some(provider), None) => {
            Ok(select_bootstrap_suggested_model_for_provider(provider, purpose, models))
        }
        (None, Some(model_name)) => Ok(models.iter().find(|model| {
            model.model_name == model_name && model.allowed_binding_purposes.contains(&purpose)
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
        bootstrap_preset_profile_for_provider_purpose(provider, purpose)
            .map(|profile| profile.model_name)
    {
        return models.iter().find(|model| {
            model.provider_catalog_id == provider.id
                && model.model_name == preferred_model_name
                && model.allowed_binding_purposes.contains(&purpose)
        });
    }

    models
        .iter()
        .filter(|model| {
            model.provider_catalog_id == provider.id
                && model.allowed_binding_purposes.contains(&purpose)
        })
        .min_by(|left, right| {
            left.model_name.cmp(&right.model_name).then_with(|| left.id.cmp(&right.id))
        })
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BootstrapProviderPresetProfile {
    pub(super) purpose: AiBindingPurpose,
    pub(super) model_name: String,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) max_output_tokens_override: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapProviderMetadata {
    #[serde(default)]
    bootstrap_presets: Vec<BootstrapProviderPresetMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapProviderPresetMetadata {
    purpose: String,
    model_name: String,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_output_tokens_override: Option<i32>,
}

fn bootstrap_provider_metadata(
    provider: &ProviderCatalogEntry,
) -> Result<BootstrapProviderMetadata, ApiError> {
    serde_json::from_value(provider.capability_flags_json.clone())
        .map_err(|error| ApiError::internal_with_log(error, "invalid provider capability flags"))
}

fn bootstrap_provider_ui_hints(
    provider: &ProviderCatalogEntry,
) -> Result<serde_json::Value, ApiError> {
    Ok(provider.ui_hints.clone())
}

fn bootstrap_provider_preset_profile(
    provider: &ProviderCatalogEntry,
) -> Result<Vec<BootstrapProviderPresetProfile>, ApiError> {
    let mut profiles: Vec<BootstrapProviderPresetProfile> = bootstrap_provider_metadata(provider)?
        .bootstrap_presets
        .into_iter()
        .map(|preset| {
            let purpose = parse_binding_purpose(preset.purpose.trim()).map_err(|_| {
                ApiError::internal_with_log(
                    format!("invalid bootstrap binding purpose {}", preset.purpose),
                    "invalid provider capability flags",
                )
            })?;
            let model_name = normalize_non_empty(&preset.model_name, "bootstrapPreset.modelName")?;
            let is_embedding =
                matches!(purpose, AiBindingPurpose::EmbedChunk | AiBindingPurpose::QueryRetrieve);
            Ok(BootstrapProviderPresetProfile {
                purpose,
                model_name,
                temperature: preset
                    .temperature
                    .or((!is_embedding).then_some(DEFAULT_BOOTSTRAP_TEMPERATURE)),
                top_p: preset.top_p.or((!is_embedding).then_some(DEFAULT_BOOTSTRAP_TOP_P)),
                max_output_tokens_override: preset.max_output_tokens_override,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    // Agent purpose drives the in-process MCP-agent loop and shadows the
    // QueryAnswer profile (same chat model, same provider). Provider metadata
    // declares only QueryAnswer; we synthesize the Agent twin so bootstrap
    // can seed an Agent preset+binding without each provider declaring it
    // twice.
    if !profiles.iter().any(|profile| profile.purpose == AiBindingPurpose::Agent)
        && let Some(answer) =
            profiles.iter().find(|profile| profile.purpose == AiBindingPurpose::QueryAnswer)
    {
        profiles.push(BootstrapProviderPresetProfile {
            purpose: AiBindingPurpose::Agent,
            model_name: answer.model_name.clone(),
            temperature: answer.temperature,
            top_p: answer.top_p,
            max_output_tokens_override: answer.max_output_tokens_override,
        });
    }

    Ok(profiles)
}

fn bootstrap_preset_descriptors_from_profile(
    provider: &ProviderCatalogEntry,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
    profile: Vec<BootstrapProviderPresetProfile>,
) -> Vec<BootstrapAiPresetDescriptor> {
    profile
        .into_iter()
        .filter_map(|preset_profile| {
            let model = models.iter().find(|model| {
                model.provider_catalog_id == provider.id
                    && model.model_name == preset_profile.model_name
                    && model.allowed_binding_purposes.contains(&preset_profile.purpose)
            })?;
            let model_owner = providers
                .iter()
                .find(|entry| entry.id == model.provider_catalog_id)
                .unwrap_or(provider);
            Some(BootstrapAiPresetDescriptor {
                binding_purpose: preset_profile.purpose,
                owner_provider_catalog_id: model_owner.id,
                owner_provider_kind: model_owner.provider_kind.clone(),
                model_catalog_id: model.id,
                model_name: model.model_name.clone(),
                preset_name: canonical_runtime_preset_name(
                    &model_owner.display_name,
                    preset_profile.purpose,
                    &model.model_name,
                ),
                system_prompt: None,
                temperature: preset_profile.temperature,
                top_p: preset_profile.top_p,
                max_output_tokens_override: preset_profile.max_output_tokens_override,
            })
        })
        .collect()
}

pub(super) fn bootstrap_preset_profile_for_provider_purpose(
    provider: &ProviderCatalogEntry,
    purpose: AiBindingPurpose,
) -> Option<BootstrapProviderPresetProfile> {
    bootstrap_provider_preset_profile(provider)
        .ok()
        .and_then(|profiles| profiles.into_iter().find(|profile| profile.purpose == purpose))
}

pub(super) fn resolve_bootstrap_provider_preset_descriptors(
    provider: &ProviderCatalogEntry,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiPresetDescriptor>, ApiError> {
    Ok(bootstrap_preset_descriptors_from_profile(
        provider,
        providers,
        models,
        bootstrap_provider_preset_profile(provider)?,
    ))
}

pub(super) fn resolve_bootstrap_provider_preset_bundle(
    provider: &ProviderCatalogEntry,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
    credential_source: BootstrapAiCredentialSource,
) -> Result<Option<BootstrapAiProviderPresetBundle>, ApiError> {
    let profile = bootstrap_provider_preset_profile(provider)?;
    if !CANONICAL_REQUIRED_RUNTIME_BINDING_PURPOSES
        .iter()
        .all(|purpose| profile.iter().any(|preset| preset.purpose == *purpose))
    {
        return Ok(None);
    }

    let presets = bootstrap_preset_descriptors_from_profile(provider, providers, models, profile);
    if !CANONICAL_REQUIRED_RUNTIME_BINDING_PURPOSES
        .iter()
        .all(|purpose| presets.iter().any(|preset| preset.binding_purpose == *purpose))
    {
        return Ok(None);
    }

    Ok(Some(BootstrapAiProviderPresetBundle {
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
        presets,
    }))
}

pub(super) fn resolve_bootstrap_provider_bundle(
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
    provider_kind: &str,
) -> Result<BootstrapAiProviderPresetBundle, ApiError> {
    let normalized_provider_kind = provider_kind.trim().to_ascii_lowercase();
    let provider =
        providers.iter().find(|entry| entry.provider_kind == normalized_provider_kind).ok_or_else(
            || ApiError::resource_not_found("provider_catalog", normalized_provider_kind.clone()),
        )?;
    resolve_bootstrap_provider_preset_bundle(
        provider,
        providers,
        models,
        BootstrapAiCredentialSource::Missing,
    )?
    .ok_or_else(|| {
        ApiError::BadRequest(format!(
            "provider {normalized_provider_kind} does not expose a complete bootstrap preset bundle",
        ))
    })
}

fn build_bootstrap_preset_input(
    provider: &ProviderCatalogEntry,
    model: &ModelCatalogEntry,
    purpose: AiBindingPurpose,
) -> BootstrapAiPresetInput {
    let preset_profile = bootstrap_preset_profile_for_provider_purpose(provider, purpose)
        .filter(|profile| profile.model_name == model.model_name);
    BootstrapAiPresetInput {
        binding_purpose: purpose,
        provider_kind: provider.provider_kind.clone(),
        model_catalog_id: model.id,
        preset_name: canonical_runtime_preset_name(
            &provider.display_name,
            purpose,
            &model.model_name,
        ),
        system_prompt: None,
        temperature: preset_profile.as_ref().and_then(|profile| profile.temperature),
        top_p: preset_profile.as_ref().and_then(|profile| profile.top_p),
        max_output_tokens_override: preset_profile
            .as_ref()
            .and_then(|profile| profile.max_output_tokens_override),
        extra_parameters_json: json!({}),
    }
}

pub(super) fn resolve_configured_bootstrap_preset_inputs(
    configured_ai: &crate::app::config::UiBootstrapAiSetup,
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiPresetInput>, ApiError> {
    let env_provider_kinds = configured_ai
        .provider_secrets
        .iter()
        .map(|secret| secret.provider_kind.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut selections = Vec::new();
    for purpose in CANONICAL_REQUIRED_RUNTIME_BINDING_PURPOSES
        .into_iter()
        .chain(std::iter::once(AiBindingPurpose::Vision))
    {
        if let Some(binding_default) =
            configured_binding_default_for_purpose(Some(configured_ai), purpose)
        {
            if let Some(model) =
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
                    selections.push(build_bootstrap_preset_input(provider, model, purpose));
                    continue;
                }
            }
        }

        let bundled_selection = providers
            .iter()
            .filter(|provider| env_provider_kinds.contains(provider.provider_kind.as_str()))
            .filter_map(|provider| {
                resolve_bootstrap_provider_preset_bundle(
                    provider,
                    providers,
                    models,
                    BootstrapAiCredentialSource::Env,
                )
                .ok()
                .flatten()
                .and_then(|bundle| {
                    bundle.presets.into_iter().find(|preset| preset.binding_purpose == purpose).map(
                        |preset| BootstrapAiPresetInput {
                            binding_purpose: preset.binding_purpose,
                            provider_kind: preset.owner_provider_kind,
                            model_catalog_id: preset.model_catalog_id,
                            preset_name: preset.preset_name,
                            system_prompt: preset.system_prompt,
                            temperature: preset.temperature,
                            top_p: preset.top_p,
                            max_output_tokens_override: preset.max_output_tokens_override,
                            extra_parameters_json: json!({}),
                        },
                    )
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
                    .map(|model| build_bootstrap_preset_input(provider, model, purpose))
            })
            .min_by(|left, right| left.provider_kind.cmp(&right.provider_kind));
        if let Some(selection) = suggested_selection {
            selections.push(selection);
        }
    }
    Ok(selections)
}

pub(super) fn bootstrap_preset_inputs_cover_required_purposes(
    inputs: &[BootstrapAiPresetInput],
) -> bool {
    CANONICAL_REQUIRED_RUNTIME_BINDING_PURPOSES
        .iter()
        .all(|purpose| inputs.iter().any(|selection| selection.binding_purpose == *purpose))
}

pub(super) fn validate_bootstrap_preset_inputs_cover_required_purposes(
    inputs: &[BootstrapAiPresetInput],
) -> Result<(), ApiError> {
    if !bootstrap_preset_inputs_cover_required_purposes(inputs) {
        return Err(ApiError::BadRequest(
            "bootstrap preset bundle must cover extract_graph, embed_chunk, query_retrieve, query_compile, query_answer, and agent"
                .to_string(),
        ));
    }
    validate_bootstrap_vector_index_model_catalog_ids(inputs)?;
    Ok(())
}

fn validate_bootstrap_vector_index_model_catalog_ids(
    inputs: &[BootstrapAiPresetInput],
) -> Result<(), ApiError> {
    let embed_chunk_model_id = inputs
        .iter()
        .find(|input| input.binding_purpose == AiBindingPurpose::EmbedChunk)
        .map(|input| input.model_catalog_id);
    let query_retrieve_model_id = inputs
        .iter()
        .find(|input| input.binding_purpose == AiBindingPurpose::QueryRetrieve)
        .map(|input| input.model_catalog_id);
    if let (Some(embed_chunk_model_id), Some(query_retrieve_model_id)) =
        (embed_chunk_model_id, query_retrieve_model_id)
        && embed_chunk_model_id != query_retrieve_model_id
    {
        return Err(ApiError::BadRequest(
            "bootstrap embed_chunk and query_retrieve bindings must use the same model catalog entry"
                .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn normalize_bootstrap_preset_inputs(
    inputs: &[BootstrapAiPresetInput],
    providers: &[ProviderCatalogEntry],
    models: &[ModelCatalogEntry],
) -> Result<Vec<BootstrapAiPresetInput>, ApiError> {
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
        let preset_name = normalize_non_empty(&input.preset_name, "presetName")?;
        normalized.push(BootstrapAiPresetInput {
            binding_purpose: input.binding_purpose,
            provider_kind,
            model_catalog_id: input.model_catalog_id,
            preset_name,
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
    preset_inputs: &[BootstrapAiPresetInput],
    models: &[ModelCatalogEntry],
    discovered_model_names: &[String],
) -> Result<Vec<String>, ApiError> {
    let discovered = discovered_model_names
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let mut selected = std::collections::BTreeSet::new();
    for input in preset_inputs.iter().filter(|input| input.provider_kind == provider.provider_kind)
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

fn bootstrap_model_list_capability_kinds(
    provider: &ProviderCatalogEntry,
    preset_inputs: &[BootstrapAiPresetInput],
    models: &[ModelCatalogEntry],
) -> Result<std::collections::BTreeSet<String>, ApiError> {
    let mut capability_kinds = std::collections::BTreeSet::new();
    for input in preset_inputs.iter().filter(|input| input.provider_kind == provider.provider_kind)
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
        capability_kinds.insert(model.capability_kind.clone());
    }
    Ok(capability_kinds)
}

pub(super) async fn validate_bootstrap_model_list_preset_inputs(
    provider: &ProviderCatalogEntry,
    credential: &ProviderCredential,
    preset_inputs: &[BootstrapAiPresetInput],
    models: &[ModelCatalogEntry],
) -> Result<(), ApiError> {
    if provider.credential_policy.validation_mode != ProviderCredentialValidationMode::ModelList {
        return Ok(());
    }
    let Some(base_url) = runtime_provider_base_url(provider, credential.base_url.as_deref())?
    else {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires a baseUrl",
            provider.provider_kind
        )));
    };
    let capability_kinds = bootstrap_model_list_capability_kinds(provider, preset_inputs, models)?;
    let discovered_model_names = fetch_provider_model_names_for_capabilities(
        provider,
        credential.api_key.as_deref(),
        &base_url,
        &capability_kinds,
    )
    .await?;
    let missing_model_names = missing_bootstrap_model_list_models(
        provider,
        preset_inputs,
        models,
        &discovered_model_names,
    )?;
    if missing_model_names.is_empty() {
        return Ok(());
    }

    Err(ApiError::BadRequest(format!(
        "bootstrap provider {} selected preset model(s) not returned by provider model discovery: {}",
        provider.provider_kind,
        missing_model_names.join(", ")
    )))
}

pub(super) async fn ensure_bootstrap_provider_credential(
    service: &AiCatalogService,
    state: &AppState,
    provider: &ProviderCatalogEntry,
    credential_input: Option<BootstrapAiCredentialInput>,
    existing_credentials: &[ProviderCredential],
    updated_by_principal_id: Option<Uuid>,
) -> Result<ProviderCredential, ApiError> {
    let canonical_label = format!("Bootstrap {}", provider.display_name);
    let provider_credentials =
        bootstrap_provider_credentials_for_provider(existing_credentials, provider.id);
    let canonical_credential =
        bootstrap_resolve_provider_credential(&canonical_label, &provider_credentials);
    let api_key =
        credential_input.as_ref().and_then(|input| normalize_optional(input.api_key.as_deref()));
    let base_url =
        credential_input.as_ref().and_then(|input| normalize_optional(input.base_url.as_deref()));
    if api_key.is_some() || base_url.is_some() {
        if let Some(existing) = canonical_credential {
            return match service
                .update_provider_credential(
                    state,
                    UpdateProviderCredentialCommand {
                        credential_id: existing.id,
                        label: canonical_label.clone(),
                        api_key,
                        base_url,
                        credential_state: "active".to_string(),
                    },
                )
                .await
            {
                Ok(updated) => Ok(updated),
                Err(ApiError::Conflict(_)) => {
                    bootstrap_reload_provider_credential(service, state, provider, &canonical_label)
                        .await
                }
                Err(error) => Err(error),
            };
        }
        return match service
            .create_provider_credential(
                state,
                CreateProviderCredentialCommand {
                    scope_kind: AiScopeKind::Instance,
                    workspace_id: None,
                    library_id: None,
                    provider_catalog_id: provider.id,
                    label: canonical_label.clone(),
                    api_key,
                    base_url,
                    created_by_principal_id: updated_by_principal_id,
                },
            )
            .await
        {
            Ok(created) => Ok(created),
            Err(ApiError::Conflict(_)) => {
                bootstrap_reload_provider_credential(service, state, provider, &canonical_label)
                    .await
            }
            Err(error) => Err(error),
        };
    }

    canonical_credential.ok_or_else(|| {
        let required_field = if provider.api_key_required { "apiKey" } else { "baseUrl" };
        ApiError::BadRequest(format!(
            "bootstrap ai setup requires {required_field} for provider {}",
            provider.provider_kind
        ))
    })
}

fn bootstrap_provider_credentials_for_provider(
    credentials: &[ProviderCredential],
    provider_catalog_id: Uuid,
) -> Vec<ProviderCredential> {
    credentials
        .iter()
        .filter(|credential| credential.provider_catalog_id == provider_catalog_id)
        .cloned()
        .collect()
}

fn bootstrap_resolve_provider_credential(
    canonical_label: &str,
    credentials: &[ProviderCredential],
) -> Option<ProviderCredential> {
    credentials
        .iter()
        .find(|credential| credential.label == canonical_label)
        .cloned()
        .or_else(|| (credentials.len() == 1).then(|| credentials[0].clone()))
        .or_else(|| {
            credentials.iter().find(|credential| credential.credential_state == "active").cloned()
        })
}

pub(super) async fn bootstrap_reload_provider_credential(
    service: &AiCatalogService,
    state: &AppState,
    provider: &ProviderCatalogEntry,
    canonical_label: &str,
) -> Result<ProviderCredential, ApiError> {
    let reloaded = service
        .list_provider_credentials_exact(
            state,
            AiScopeRef { scope_kind: AiScopeKind::Instance, workspace_id: None, library_id: None },
        )
        .await?;
    bootstrap_resolve_provider_credential(
        canonical_label,
        &bootstrap_provider_credentials_for_provider(&reloaded, provider.id),
    )
    .ok_or_else(|| ApiError::Conflict("AI catalog resource already exists".to_string()))
}

fn bootstrap_find_runtime_preset(
    presets: &[ModelPreset],
    model_catalog_id: Uuid,
    canonical_name: &str,
) -> Option<ModelPreset> {
    let matching = presets
        .iter()
        .filter(|preset| preset.model_catalog_id == model_catalog_id)
        .cloned()
        .collect::<Vec<_>>();
    select_runtime_preset(&matching, canonical_name).cloned()
}

pub(super) async fn ensure_bootstrap_model_preset(
    service: &AiCatalogService,
    state: &AppState,
    preset_input: &BootstrapAiPresetInput,
    presets: &mut Vec<ModelPreset>,
    created_by_principal_id: Option<Uuid>,
) -> Result<ModelPreset, ApiError> {
    if let Some(existing) = bootstrap_find_runtime_preset(
        presets,
        preset_input.model_catalog_id,
        &preset_input.preset_name,
    ) {
        let needs_update = existing.system_prompt != preset_input.system_prompt
            || existing.temperature != preset_input.temperature
            || existing.top_p != preset_input.top_p
            || existing.max_output_tokens_override != preset_input.max_output_tokens_override
            || existing.extra_parameters_json != preset_input.extra_parameters_json;
        if !needs_update {
            return Ok(existing);
        }

        let updated = service
            .update_model_preset(
                state,
                UpdateModelPresetCommand {
                    preset_id: existing.id,
                    preset_name: preset_input.preset_name.clone(),
                    system_prompt: preset_input.system_prompt.clone(),
                    temperature: preset_input.temperature,
                    top_p: preset_input.top_p,
                    max_output_tokens_override: preset_input.max_output_tokens_override,
                    extra_parameters_json: preset_input.extra_parameters_json.clone(),
                },
            )
            .await?;
        if let Some(index) = presets.iter().position(|preset| preset.id == updated.id) {
            presets[index] = updated.clone();
        }
        return Ok(updated);
    }

    match service
        .create_model_preset(
            state,
            CreateModelPresetCommand {
                scope_kind: AiScopeKind::Instance,
                workspace_id: None,
                library_id: None,
                model_catalog_id: preset_input.model_catalog_id,
                preset_name: preset_input.preset_name.clone(),
                system_prompt: preset_input.system_prompt.clone(),
                temperature: preset_input.temperature,
                top_p: preset_input.top_p,
                max_output_tokens_override: preset_input.max_output_tokens_override,
                extra_parameters_json: preset_input.extra_parameters_json.clone(),
                created_by_principal_id,
            },
        )
        .await
    {
        Ok(created) => {
            presets.push(created.clone());
            Ok(created)
        }
        Err(ApiError::Conflict(_)) => {
            *presets = service
                .list_model_presets_exact(
                    state,
                    AiScopeRef {
                        scope_kind: AiScopeKind::Instance,
                        workspace_id: None,
                        library_id: None,
                    },
                )
                .await?;
            bootstrap_find_runtime_preset(
                presets,
                preset_input.model_catalog_id,
                &preset_input.preset_name,
            )
            .ok_or_else(|| ApiError::Conflict("AI catalog resource already exists".to_string()))
        }
        Err(error) => Err(error),
    }
}

fn bootstrap_find_binding_assignment(
    bindings: &[AiBindingAssignment],
    purpose: AiBindingPurpose,
) -> Option<AiBindingAssignment> {
    bindings.iter().find(|binding| binding.binding_purpose == purpose).cloned()
}

pub(super) async fn ensure_bootstrap_binding_assignment(
    service: &AiCatalogService,
    state: &AppState,
    binding_purpose: AiBindingPurpose,
    provider_credential_id: Uuid,
    model_preset_id: Uuid,
    bindings: &mut Vec<AiBindingAssignment>,
    updated_by_principal_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let existing = bootstrap_find_binding_assignment(bindings, binding_purpose);
    let operation = if let Some(existing) = existing {
        service
            .update_binding_assignment(
                state,
                UpdateBindingAssignmentCommand {
                    binding_id: existing.id,
                    provider_credential_id,
                    model_preset_id,
                    binding_state: "active".to_string(),
                    updated_by_principal_id,
                },
            )
            .await
    } else {
        service
            .create_binding_assignment(
                state,
                CreateBindingAssignmentCommand {
                    scope_kind: AiScopeKind::Instance,
                    workspace_id: None,
                    library_id: None,
                    binding_purpose,
                    provider_credential_id,
                    model_preset_id,
                    updated_by_principal_id,
                },
            )
            .await
    };

    match operation {
        Ok(binding) => {
            if let Some(index) =
                bindings.iter().position(|entry| entry.binding_purpose == binding_purpose)
            {
                bindings[index] = binding;
            } else {
                bindings.push(binding);
            }
            Ok(())
        }
        Err(ApiError::Conflict(_)) => {
            *bindings = service
                .list_binding_assignments(
                    state,
                    AiScopeRef {
                        scope_kind: AiScopeKind::Instance,
                        workspace_id: None,
                        library_id: None,
                    },
                )
                .await?;
            let existing = bootstrap_find_binding_assignment(bindings, binding_purpose)
                .ok_or_else(|| {
                    ApiError::Conflict("AI catalog resource already exists".to_string())
                })?;
            let updated = service
                .update_binding_assignment(
                    state,
                    UpdateBindingAssignmentCommand {
                        binding_id: existing.id,
                        provider_credential_id,
                        model_preset_id,
                        binding_state: "active".to_string(),
                        updated_by_principal_id,
                    },
                )
                .await?;
            if let Some(index) =
                bindings.iter().position(|entry| entry.binding_purpose == binding_purpose)
            {
                bindings[index] = updated;
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}
