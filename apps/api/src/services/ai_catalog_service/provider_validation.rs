use super::*;
use crate::{
    domains::provider_profiles::{
        ProviderAuthScheme, ProviderBaseUrlMode, ProviderModelDiscoveryMode,
    },
    integrations::llm::ChatRequest,
    shared::provider_base_url::provider_base_url_candidates,
};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use std::net::IpAddr;

const TEXT_CHAT_BINDING_PURPOSES: [AiBindingPurpose; 4] = [
    AiBindingPurpose::ExtractText,
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
];
const MULTIMODAL_CHAT_BINDING_PURPOSES: [AiBindingPurpose; 5] = [
    AiBindingPurpose::ExtractText,
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Vision,
];
const EMBEDDING_BINDING_PURPOSES: [AiBindingPurpose; 2] =
    [AiBindingPurpose::EmbedChunk, AiBindingPurpose::QueryRetrieve];

#[derive(Clone, Copy)]
pub(super) struct DiscoveredModelSignature {
    pub(super) capability_kind: &'static str,
    pub(super) modality_kind: &'static str,
    pub(super) allowed_binding_purposes: &'static [AiBindingPurpose],
}

#[derive(Clone)]
pub(super) struct DiscoveredProviderModel {
    pub(super) model_name: String,
    pub(super) signature: DiscoveredModelSignature,
}

pub(super) fn normalize_provider_base_url_input(
    provider: &ProviderCatalogEntry,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let Some(candidate) = normalize_optional(value) else {
        return Ok(None);
    };
    if !provider.base_url_policy.allow_override {
        return Err(ApiError::BadRequest(format!(
            "provider {} uses a hosted fixed baseUrl and does not allow baseUrl overrides",
            provider.provider_kind
        )));
    }
    canonicalize_provider_base_url(provider, &candidate).map(Some)
}

pub(super) fn provider_credential_base_url_for_create(
    provider: &ProviderCatalogEntry,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let base_url = normalize_provider_base_url_input(provider, value)?;
    if !provider.base_url_policy.allow_override {
        return Ok(None);
    }
    resolve_provider_base_url(provider, base_url.as_deref())
}

pub(super) fn provider_credential_base_url_for_update(
    provider: &ProviderCatalogEntry,
    existing: Option<&str>,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let base_url = normalize_provider_base_url_input(provider, value)?;
    if !provider.base_url_policy.allow_override {
        return Ok(None);
    }
    resolve_provider_base_url(provider, base_url.as_deref().or(existing))
}

pub(super) fn runtime_provider_base_url(
    provider: &ProviderCatalogEntry,
    credential_base_url: Option<&str>,
) -> Result<Option<String>, ApiError> {
    if provider.base_url_policy.allow_override {
        return resolve_provider_base_url(provider, credential_base_url);
    }
    resolve_provider_base_url(provider, None)
}

pub(super) fn resolve_provider_base_url(
    provider: &ProviderCatalogEntry,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    if let Some(base_url) = normalize_provider_base_url_input(provider, value)? {
        return Ok(Some(base_url));
    }
    match provider.credential_policy.base_url_mode {
        ProviderBaseUrlMode::Fixed | ProviderBaseUrlMode::Required => provider
            .default_base_url
            .as_deref()
            .map(|candidate| canonicalize_provider_base_url(provider, candidate))
            .transpose()
            .and_then(|base_url| {
                base_url.ok_or_else(|| {
                    ApiError::BadRequest(format!(
                        "provider {} requires a baseUrl",
                        provider.provider_kind
                    ))
                })
            })
            .map(Some),
        ProviderBaseUrlMode::Optional => Ok(provider
            .default_base_url
            .as_deref()
            .map(|candidate| canonicalize_provider_base_url(provider, candidate))
            .transpose()?),
    }
}

pub(super) fn canonicalize_provider_base_url(
    provider: &ProviderCatalogEntry,
    value: &str,
) -> Result<String, ApiError> {
    let mut url = Url::parse(value).map_err(|_| {
        ApiError::BadRequest(format!(
            "baseUrl must be a valid absolute URL for provider {}",
            provider.provider_kind
        ))
    })?;
    if matches!(url.scheme(), "http" | "https") {
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ApiError::BadRequest(format!(
                "baseUrl must not include userinfo for provider {}",
                provider.provider_kind
            )));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(ApiError::BadRequest(format!(
                "baseUrl must not include query or fragment components for provider {}",
                provider.provider_kind
            )));
        }
        if provider.base_url_policy.require_https && url.scheme() != "https" {
            return Err(ApiError::BadRequest(format!(
                "baseUrl must use https for provider {}",
                provider.provider_kind
            )));
        }
        if !provider.base_url_policy.allow_private_network && is_private_network_url(&url) {
            return Err(ApiError::BadRequest(format!(
                "baseUrl must not target a private, loopback, or link-local network for provider {}",
                provider.provider_kind
            )));
        }
        trim_provider_base_url_suffixes(provider, &mut url);
        if url.path() != "/" {
            let trimmed_path = url.path().trim_end_matches('/').to_string();
            url.set_path(&trimmed_path);
        }
        return Ok(url.to_string().trim_end_matches('/').to_string());
    }
    Err(ApiError::BadRequest(format!(
        "baseUrl must use http or https for provider {}",
        provider.provider_kind
    )))
}

fn trim_provider_base_url_suffixes(provider: &ProviderCatalogEntry, url: &mut Url) {
    let suffixes = provider
        .base_url_policy
        .trim_suffixes
        .iter()
        .map(|suffix| suffix.trim_matches('/'))
        .filter(|suffix| !suffix.is_empty())
        .collect::<Vec<_>>();
    if suffixes.is_empty() {
        return;
    }

    let mut path_segments = url
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    while path_segments
        .last()
        .is_some_and(|segment| suffixes.iter().any(|suffix| segment.eq_ignore_ascii_case(suffix)))
    {
        path_segments.pop();
    }
    url.set_path(&format!("/{}", path_segments.join("/")));
}

fn is_private_network_url(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(host)) => is_private_network_ip(IpAddr::V4(host)),
        Some(url::Host::Ipv6(host)) => is_private_network_ip(IpAddr::V6(host)),
        None => false,
    }
}

fn is_private_network_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(value) => {
            value.is_private()
                || value.is_loopback()
                || value.is_link_local()
                || value.is_broadcast()
                || value.is_documentation()
                || value.is_unspecified()
        }
        IpAddr::V6(value) => {
            value.is_loopback()
                || value.is_unique_local()
                || value.is_unicast_link_local()
                || value.is_unspecified()
        }
    }
}

fn text_chat_signature() -> DiscoveredModelSignature {
    DiscoveredModelSignature {
        capability_kind: "chat",
        modality_kind: "text",
        allowed_binding_purposes: &TEXT_CHAT_BINDING_PURPOSES,
    }
}

fn multimodal_chat_signature() -> DiscoveredModelSignature {
    DiscoveredModelSignature {
        capability_kind: "chat",
        modality_kind: "multimodal",
        allowed_binding_purposes: &MULTIMODAL_CHAT_BINDING_PURPOSES,
    }
}

fn embedding_signature() -> DiscoveredModelSignature {
    DiscoveredModelSignature {
        capability_kind: "embedding",
        modality_kind: "text",
        allowed_binding_purposes: &EMBEDDING_BINDING_PURPOSES,
    }
}

pub(super) fn discovered_provider_model_signature_for_capability(
    provider: &ProviderCatalogEntry,
    capability_kind: &str,
) -> Result<Option<DiscoveredModelSignature>, ApiError> {
    let capability_kind = capability_kind.trim();
    match capability_kind {
        "chat" if provider.capabilities.chat.is_supported() => Ok(Some(text_chat_signature())),
        "embedding" if provider.capabilities.embeddings.is_supported() => {
            Ok(Some(embedding_signature()))
        }
        "vision"
            if provider.capabilities.chat.is_supported()
                && provider.capabilities.vision.is_supported() =>
        {
            Ok(Some(multimodal_chat_signature()))
        }
        "chat" | "embedding" | "vision" => Ok(None),
        other => Err(ApiError::BadRequest(format!(
            "provider {} declares unsupported model discovery capability kind `{other}`",
            provider.provider_kind
        ))),
    }
}

pub(super) async fn ensure_discovered_provider_model_catalog_entry(
    state: &AppState,
    provider: &ProviderCatalogEntry,
    model_name: &str,
    signature: DiscoveredModelSignature,
) -> Result<(), ApiError> {
    let metadata_json = json!({
        "defaultRoles": signature
            .allowed_binding_purposes
            .iter()
            .map(|purpose| purpose.as_str())
            .collect::<Vec<_>>(),
        "seedSource": "provider_discovery",
    });
    ai_repository::upsert_model_catalog(
        &state.persistence.postgres,
        provider.id,
        model_name,
        signature.capability_kind,
        signature.modality_kind,
        metadata_json,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(())
}

pub(super) async fn sync_provider_model_catalog(
    state: &AppState,
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<Vec<String>, ApiError> {
    if provider.model_discovery.mode == ProviderModelDiscoveryMode::Unsupported {
        return Err(ApiError::BadRequest(format!(
            "provider {} does not support model discovery",
            provider.provider_kind
        )));
    }
    let Some(base_url) = runtime_provider_base_url(provider, base_url)? else {
        return Ok(Vec::new());
    };
    let discovered_models = fetch_provider_models(provider, api_key, &base_url).await?;
    for model in &discovered_models {
        ensure_discovered_provider_model_catalog_entry(
            state,
            provider,
            &model.model_name,
            model.signature,
        )
        .await?;
    }
    Ok(discovered_models
        .into_iter()
        .map(|model| model.model_name)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect())
}

pub(super) async fn fetch_provider_model_names(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
) -> Result<Vec<String>, ApiError> {
    let paths =
        provider.model_discovery.paths.iter().map(|path| path.path.as_str()).collect::<Vec<_>>();
    fetch_provider_model_names_from_paths(provider, api_key, base_url, paths).await
}

pub(super) async fn fetch_provider_models(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
) -> Result<Vec<DiscoveredProviderModel>, ApiError> {
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| Client::new());

    let mut discovery_paths = provider
        .model_discovery
        .paths
        .iter()
        .map(|path| (path.capability_kind.trim(), path.path.trim()))
        .filter(|(_, path)| !path.is_empty())
        .collect::<Vec<_>>();
    discovery_paths.sort_unstable();
    discovery_paths.dedup();
    if discovery_paths.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "provider {} does not define model discovery paths",
            provider.provider_kind
        )));
    }

    let candidate_urls =
        provider_base_url_candidates(provider.base_url_policy.allow_private_network, base_url);
    let mut discovered = Vec::new();
    for (capability_kind, model_path) in discovery_paths {
        let Some(signature) =
            discovered_provider_model_signature_for_capability(provider, capability_kind)?
        else {
            continue;
        };
        for model_name in fetch_provider_model_names_from_path(
            &client,
            provider,
            api_key,
            &candidate_urls,
            model_path,
        )
        .await?
        {
            discovered.push(DiscoveredProviderModel { model_name, signature });
        }
    }

    discovered.sort_by(|left, right| {
        left.model_name
            .cmp(&right.model_name)
            .then(left.signature.capability_kind.cmp(right.signature.capability_kind))
    });
    discovered.dedup_by(|left, right| {
        left.model_name == right.model_name
            && left.signature.capability_kind == right.signature.capability_kind
    });
    Ok(discovered)
}

pub(super) async fn fetch_provider_model_names_for_capabilities(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
    capability_kinds: &std::collections::BTreeSet<String>,
) -> Result<Vec<String>, ApiError> {
    if capability_kinds.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "provider {} model discovery requires at least one selected capability kind",
            provider.provider_kind
        )));
    }

    let paths = provider
        .model_discovery
        .paths
        .iter()
        .filter(|path| capability_kinds.contains(path.capability_kind.as_str()))
        .map(|path| path.path.as_str())
        .collect::<Vec<_>>();
    fetch_provider_model_names_from_paths(provider, api_key, base_url, paths).await
}

async fn fetch_provider_model_names_from_paths(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
    paths: Vec<&str>,
) -> Result<Vec<String>, ApiError> {
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| Client::new());

    let mut model_paths =
        paths.into_iter().map(str::trim).filter(|path| !path.is_empty()).collect::<Vec<_>>();
    model_paths.sort_unstable();
    model_paths.dedup();
    if model_paths.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "provider {} does not define model discovery paths for the selected capability kind(s)",
            provider.provider_kind
        )));
    }

    let candidate_urls =
        provider_base_url_candidates(provider.base_url_policy.allow_private_network, base_url);

    let mut discovered = Vec::new();
    for model_path in model_paths {
        discovered.extend(
            fetch_provider_model_names_from_path(
                &client,
                provider,
                api_key,
                &candidate_urls,
                model_path,
            )
            .await?,
        );
    }

    discovered.sort();
    discovered.dedup();
    Ok(discovered)
}

async fn fetch_provider_model_names_from_path(
    client: &Client,
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    candidate_urls: &[String],
    model_path: &str,
) -> Result<Vec<String>, ApiError> {
    let mut last_error = None;
    for candidate_url in candidate_urls {
        let request = client.get(provider_endpoint_url(candidate_url, model_path, provider)?);
        let request = apply_provider_auth(request, provider.runtime.auth_scheme, api_key);
        let request = crate::observability::inject_trace_context(request);
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                if !status.is_success() {
                    last_error = Some(ApiError::BadRequest(format!(
                        "provider credential validation failed for {} at {}: status={}",
                        provider.display_name, model_path, status
                    )));
                    continue;
                }

                let body = response.json::<Value>().await.map_err(|error| {
                    ApiError::BadRequest(format!(
                        "provider credential validation failed for {} at {}: invalid model list response: {error}",
                        provider.display_name, model_path
                    ))
                })?;
                return Ok(body
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(ToString::to_string)
                    })
                    .collect());
            }
            Err(error) => {
                last_error = Some(ApiError::BadRequest(format!(
                    "provider credential validation failed for {} at {}: {}",
                    provider.display_name,
                    model_path,
                    sanitize_upstream_error(&error.to_string())
                )));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        ApiError::BadRequest(format!(
            "provider credential validation failed for {} at {}: no candidate baseUrl succeeded",
            provider.display_name, model_path
        ))
    }))
}

pub(super) fn normalize_runtime_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!("/{}", trimmed.trim_start_matches('/').trim_end_matches('/'))
}

fn provider_endpoint_url(
    base_url: &str,
    path: &str,
    provider: &ProviderCatalogEntry,
) -> Result<Url, ApiError> {
    let mut url = Url::parse(base_url).map_err(|error| {
        ApiError::BadRequest(format!(
            "invalid baseUrl for provider {}: {error}",
            provider.provider_kind
        ))
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::BadRequest(format!(
            "baseUrl must not include userinfo for provider {}",
            provider.provider_kind
        )));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ApiError::BadRequest(format!(
            "baseUrl must not include query or fragment components for provider {}",
            provider.provider_kind
        )));
    }

    let endpoint_path = normalize_runtime_path(path);
    let base_path = url.path().trim_end_matches('/');
    let joined_path = if endpoint_path.is_empty() {
        if base_path.is_empty() { "/".to_string() } else { base_path.to_string() }
    } else if base_path.is_empty() {
        endpoint_path
    } else {
        format!("{base_path}{endpoint_path}")
    };
    url.set_path(&joined_path);
    Ok(url)
}

fn apply_provider_auth(
    request: reqwest::RequestBuilder,
    auth_scheme: ProviderAuthScheme,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let Some(token) = normalize_optional(api_key) else {
        return request;
    };
    match auth_scheme {
        ProviderAuthScheme::Bearer => request.bearer_auth(token),
        ProviderAuthScheme::RawAuthorization => {
            request.header(reqwest::header::AUTHORIZATION, token)
        }
    }
}

pub(super) fn sanitize_upstream_error(_message: &str) -> String {
    "upstream provider request failed; response details were redacted".to_string()
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
    ApiError::BadRequest(format!(
        "provider credential validation failed for {}: IronRAG cannot reach a provider bound only to host localhost from inside Docker; expose the provider on a host-reachable interface or use a host-reachable URL",
        provider.display_name
    ))
}

fn select_provider_validation_model<'a>(
    provider: &ProviderCatalogEntry,
    models: &'a [ModelCatalogEntry],
) -> Option<&'a ModelCatalogEntry> {
    for purpose in
        [AiBindingPurpose::QueryAnswer, AiBindingPurpose::ExtractGraph, AiBindingPurpose::Vision]
    {
        if let Some(profile) = bootstrap_preset_profile_for_provider_purpose(provider, purpose) {
            if let Some(model) = models.iter().find(|entry| {
                entry.provider_catalog_id == provider.id && entry.model_name == profile.model_name
            }) {
                return Some(model);
            }
        }
    }

    models
        .iter()
        .filter(|model| model.provider_catalog_id == provider.id && model.capability_kind == "chat")
        .min_by(|left, right| {
            left.model_name.cmp(&right.model_name).then_with(|| left.id.cmp(&right.id))
        })
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
                    extra_parameters_json: json!({
                        "_providerProfile": provider_runtime_profile_json(&provider.profile),
                    }),
                })
                .await
                .map(|_| ())
                .map_err(|error| {
                    let sanitized_error = sanitize_upstream_error(&error.to_string());
                    tracing::warn!(stage = "bootstrap", provider_kind = %provider.provider_kind, error = %sanitized_error, "provider credential validation failed");
                    ApiError::BadRequest(format!(
                        "provider credential validation failed for {}: {sanitized_error}",
                        provider.display_name
                    ))
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
        Err(error) if loopback_base_url => {
            let message = error.to_string();
            if message.contains("Connection refused")
                || message.contains("error trying to connect")
                || message.contains("timed out")
            {
                Err(loopback_runtime_error(provider))
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}
