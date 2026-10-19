use super::provider_validation::{
    ChatRoundTripValidationFailureKind, ProviderCredentialValidationFailureKind,
    chat_round_trip_validation_failure_kind, fetch_provider_model_names,
    provider_credential_validation_error, provider_credential_validation_failure_kind,
    resolve_provider_base_url,
};
use super::*;
use crate::integrations::llm::ChatRequest;
use reqwest::Url;
use serde_json::{Value, json};

pub(super) fn is_provider_credential_validation_error(error: &ApiError) -> bool {
    provider_credential_validation_failure_kind(error).is_some()
}

pub(super) fn is_loopback_base_url(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .and_then(|url| {
            url.host().map(|host| match host {
                url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
                url::Host::Ipv4(host) => host.is_loopback(),
                url::Host::Ipv6(host) => host.is_loopback(),
            })
        })
        .unwrap_or(false)
}

fn loopback_runtime_error(provider: &ProviderCatalogEntry) -> ApiError {
    provider_credential_validation_error(
        ProviderCredentialValidationFailureKind::LoopbackUnreachable,
        format!(
            "provider credential validation failed for {}: IronRAG cannot reach a provider bound only to host localhost from inside Docker; expose the provider on a host-reachable interface or use a host-reachable URL",
            provider.display_name
        ),
    )
}

fn select_provider_validation_model<'a>(
    provider: &ProviderCatalogEntry,
    models: &'a [ModelCatalogEntry],
) -> Option<&'a ModelCatalogEntry> {
    for purpose in [
        AiBindingPurpose::QueryAnswer,
        AiBindingPurpose::ExtractGraph,
        AiBindingPurpose::ExtractText,
    ] {
        if let Some(profile) = bootstrap_binding_profile_for_provider_purpose(provider, purpose)
            && let Some(model) = models.iter().find(|entry| {
                entry.provider_catalog_id == provider.id
                    && entry.model_name == profile.model_name
                    && entry.lifecycle_state != "disabled"
            })
        {
            return Some(model);
        }
    }

    models
        .iter()
        .filter(|model| {
            model.provider_catalog_id == provider.id
                && model.allowed_binding_purposes.contains(&AiBindingPurpose::QueryAnswer)
                && model.lifecycle_state != "disabled"
        })
        .min_by(|left, right| {
            left.model_name.cmp(&right.model_name).then_with(|| left.id.cmp(&right.id))
        })
}

pub(super) fn provider_validation_extra_parameters(
    provider: &ProviderCatalogEntry,
    model: &ModelCatalogEntry,
) -> Value {
    merge_binding_request_context(json!({}), model, &provider.profile)
}

pub(super) async fn validate_provider_access(
    state: &AppState,
    provider: &ProviderCatalogEntry,
    models: &[ModelCatalogEntry],
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<(), ApiError> {
    let policy = provider_credential_policy(provider);
    let normalized_api_key = normalize_optional(api_key);
    let normalized_base_url = resolve_provider_base_url(provider, base_url)?;

    if policy.api_key_required && normalized_api_key.is_none() {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires an apiKey",
            provider.provider_kind
        )));
    }
    if policy.base_url_required && normalized_base_url.is_none() {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires a baseUrl",
            provider.provider_kind
        )));
    }

    match policy.validation_mode {
        ProviderCredentialValidationMode::ChatRoundTrip => {
            let model = select_provider_validation_model(provider, models).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "provider {} does not expose a chat model for credential validation",
                    provider.provider_kind
                ))
            })?;

            state
                .llm_gateway
                .generate(ChatRequest {
                    provider_kind: provider.provider_kind.clone(),
                    model_name: model.model_name.clone(),
                    prompt: "Reply with OK.".to_string(),
                    api_key_override: normalized_api_key.clone(),
                    base_url_override: normalized_base_url.clone(),
                    system_prompt: Some(
                        "Validate the supplied provider credentials by replying with the single token OK.".to_string(),
                    ),
                    temperature: Some(0.0),
                    top_p: Some(1.0),
                    max_output_tokens_override: Some(16),
                    response_format: None,
                    extra_parameters_json: provider_validation_extra_parameters(provider, model),
                })
                .await
                .map(|_| ())
                .map_err(|error| {
                    let failure_kind = chat_round_trip_validation_failure_kind(&error);
                    tracing::warn!(
                        stage = "bootstrap",
                        provider_kind = %provider.provider_kind,
                        ?failure_kind,
                        "provider credential validation failed"
                    );
                    let credential_failure_kind = match failure_kind {
                        ChatRoundTripValidationFailureKind::CredentialRejected => {
                            ProviderCredentialValidationFailureKind::Rejected
                        }
                        ChatRoundTripValidationFailureKind::ProviderUnavailable => {
                            ProviderCredentialValidationFailureKind::Transport
                        }
                        ChatRoundTripValidationFailureKind::Internal => return ApiError::Internal,
                    };
                    provider_credential_validation_error(
                        credential_failure_kind,
                        format!(
                            "provider credential validation failed for {}: upstream provider request failed; response details were redacted",
                            provider.display_name
                        ),
                    )
                })
        }
        ProviderCredentialValidationMode::ModelList => {
            validate_provider_model_listing(
                provider,
                normalized_api_key.as_deref(),
                normalized_base_url.as_deref(),
            )
            .await
        }
        ProviderCredentialValidationMode::None => Ok(()),
    }
}

pub(super) async fn validate_provider_model_listing(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<(), ApiError> {
    let Some(base_url) = base_url else {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires a baseUrl",
            provider.provider_kind
        )));
    };
    let loopback_base_url =
        provider.base_url_policy.allow_private_network && is_loopback_base_url(base_url);
    match fetch_provider_model_names(provider, api_key, base_url).await {
        Ok(_) => Ok(()),
        Err(error)
            if loopback_base_url
                && provider_credential_validation_failure_kind(&error)
                    == Some(ProviderCredentialValidationFailureKind::Transport) =>
        {
            Err(loopback_runtime_error(provider))
        }
        Err(error) => Err(error),
    }
}
