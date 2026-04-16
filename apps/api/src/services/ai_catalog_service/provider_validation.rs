use super::*;
use crate::{
    domains::provider_profiles::SupportedProviderKind, integrations::llm::ChatRequest,
    shared::provider_base_url::provider_base_url_candidates,
};
use reqwest::{Client, Url};
use serde_json::{Value, json};

const TEXT_CHAT_BINDING_PURPOSES: [AiBindingPurpose; 2] =
    [AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer];
const MULTIMODAL_CHAT_BINDING_PURPOSES: [AiBindingPurpose; 3] =
    [AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryAnswer, AiBindingPurpose::Vision];
const EMBEDDING_BINDING_PURPOSES: [AiBindingPurpose; 1] = [AiBindingPurpose::EmbedChunk];

#[derive(Clone, Copy)]
pub(super) struct DiscoveredModelSignature {
    pub(super) capability_kind: &'static str,
    pub(super) modality_kind: &'static str,
    pub(super) allowed_binding_purposes: &'static [AiBindingPurpose],
}

pub(super) fn normalize_provider_base_url_input(
    provider: &ProviderCatalogEntry,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    normalize_optional(value)
        .map(|candidate| canonicalize_provider_base_url(provider, &candidate))
        .transpose()
}

pub(super) fn resolve_provider_base_url(
    provider: &ProviderCatalogEntry,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    if let Some(base_url) = normalize_provider_base_url_input(provider, value)? {
        return Ok(Some(base_url));
    }
    if provider.base_url_required {
        return provider
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
            .map(Some);
    }
    Ok(None)
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
        if provider.provider_kind == SupportedProviderKind::Ollama.as_str() {
            let mut path_segments = url
                .path_segments()
                .map(|segments| segments.filter(|segment| !segment.is_empty()).collect::<Vec<_>>())
                .unwrap_or_default();
            match path_segments.last().copied() {
                Some("v1") => {}
                Some("api") => {
                    path_segments.pop();
                    path_segments.push("v1");
                }
                _ => path_segments.push("v1"),
            }
            url.set_path(&format!("/{}", path_segments.join("/")));
        }
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

fn model_name_looks_like_embedding(normalized: &str) -> bool {
    normalized.contains("embedding")
        || normalized.contains("-embed")
        || normalized.contains("embed-")
        || normalized.starts_with("bge-")
        || normalized.starts_with("all-minilm")
}

fn model_name_looks_like_multimodal(normalized: &str) -> bool {
    normalized.contains("vision")
        || normalized.contains("vl")
        || normalized.contains("ocr")
        || normalized.contains("llava")
        || normalized.contains("bakllava")
        || normalized.contains("minicpm-v")
        || normalized.contains("minicpmv")
        || normalized.contains("moondream")
        || normalized.contains("smolvlm")
        || normalized.contains("pixtral")
        || normalized.contains("qvq")
}

fn discovered_openai_model_signature(normalized: &str) -> Option<DiscoveredModelSignature> {
    if normalized.starts_with("text-embedding-") {
        return Some(embedding_signature());
    }
    if normalized.starts_with("gpt-image")
        || normalized.starts_with("gpt-audio")
        || normalized.starts_with("gpt-realtime")
        || normalized.starts_with("omni-moderation")
        || normalized.starts_with("sora")
        || normalized.starts_with("whisper")
        || normalized.contains("-tts")
        || normalized.contains("-transcribe")
    {
        return None;
    }
    if normalized.contains("codex")
        || normalized.starts_with("gpt-5.3-chat")
        || normalized.starts_with("o1")
        || normalized.starts_with("o3-mini")
        || normalized.starts_with("o3-pro")
    {
        return Some(text_chat_signature());
    }
    if normalized == "o3" || normalized.starts_with("o4-mini") {
        return Some(multimodal_chat_signature());
    }
    if normalized.starts_with("gpt-5")
        || normalized.starts_with("gpt-4.1")
        || normalized.starts_with("gpt-4o")
        || normalized.starts_with("gpt-4")
    {
        return Some(multimodal_chat_signature());
    }
    None
}

fn discovered_qwen_model_signature(normalized: &str) -> Option<DiscoveredModelSignature> {
    if model_name_looks_like_embedding(normalized) {
        return Some(embedding_signature());
    }
    if model_name_looks_like_multimodal(normalized) {
        return Some(multimodal_chat_signature());
    }
    if normalized.starts_with("qwen")
        || normalized.starts_with("qwq")
        || normalized.starts_with("qwen-max")
        || normalized.starts_with("qvq")
    {
        return Some(text_chat_signature());
    }
    None
}

fn discovered_ollama_model_signature(model_name: &str) -> DiscoveredModelSignature {
    let normalized = model_name.trim().to_ascii_lowercase();
    if model_name_looks_like_embedding(&normalized) {
        return embedding_signature();
    }
    if model_name_looks_like_multimodal(&normalized)
        || normalized.starts_with("gemma3")
        || normalized.starts_with("llama4")
    {
        return multimodal_chat_signature();
    }
    text_chat_signature()
}

pub(super) fn discovered_provider_model_signature(
    provider_kind: &str,
    model_name: &str,
) -> Option<DiscoveredModelSignature> {
    let normalized = model_name.trim().to_ascii_lowercase();
    match provider_kind {
        "openai" => discovered_openai_model_signature(&normalized),
        "deepseek" => {
            if model_name_looks_like_embedding(&normalized) {
                Some(embedding_signature())
            } else if normalized.starts_with("deepseek") {
                Some(text_chat_signature())
            } else {
                None
            }
        }
        "qwen" => discovered_qwen_model_signature(&normalized),
        "ollama" => Some(discovered_ollama_model_signature(model_name)),
        _ => None,
    }
}

pub(super) async fn ensure_discovered_provider_model_catalog_entry(
    state: &AppState,
    provider: &ProviderCatalogEntry,
    model_name: &str,
) -> Result<(), ApiError> {
    let Some(signature) = discovered_provider_model_signature(&provider.provider_kind, model_name)
    else {
        return Ok(());
    };
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
    let Some(base_url) = resolve_provider_base_url(provider, base_url)? else {
        return Ok(Vec::new());
    };
    let model_names = fetch_provider_model_names(provider, api_key, &base_url).await?;
    for model_name in &model_names {
        ensure_discovered_provider_model_catalog_entry(state, provider, model_name).await?;
    }
    Ok(model_names)
}

pub(super) async fn fetch_provider_model_names(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
) -> Result<Vec<String>, ApiError> {
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| Client::new());

    let candidate_urls = provider_base_url_candidates(&provider.provider_kind, base_url);
    let mut last_error = None;
    for candidate_url in candidate_urls {
        let request = client.get(format!("{candidate_url}/models"));
        let request = if let Some(token) = normalize_optional(api_key) {
            request.bearer_auth(token)
        } else {
            request
        };
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    last_error = Some(ApiError::BadRequest(format!(
                        "provider credential validation failed for {}: status={} body={body}",
                        provider.display_name, status
                    )));
                    continue;
                }

                let body = response.json::<Value>().await.map_err(|error| {
                    ApiError::BadRequest(format!(
                        "provider credential validation failed for {}: invalid model list response: {error}",
                        provider.display_name
                    ))
                })?;
                let mut model_names = body
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
                    .collect::<Vec<_>>();
                model_names.sort();
                model_names.dedup();
                return Ok(model_names);
            }
            Err(error) => {
                last_error = Some(ApiError::BadRequest(format!(
                    "provider credential validation failed for {}: {error}",
                    provider.display_name
                )));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        ApiError::BadRequest(format!(
            "provider credential validation failed for {}",
            provider.display_name
        ))
    }))
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

fn ollama_loopback_runtime_error(provider: &ProviderCatalogEntry) -> ApiError {
    ApiError::BadRequest(format!(
        "provider credential validation failed for {}: IronRAG cannot reach an Ollama server bound only to host localhost from inside Docker; expose Ollama on 0.0.0.0:11434 or run Ollama in Docker, then use a host-reachable URL such as http://host.docker.internal:11434",
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
        if let Some(profile) =
            bootstrap_preset_profile_for_purpose(&provider.provider_kind, purpose)
        {
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
    let policy = provider_credential_policy(&provider.provider_kind);
    let normalized_api_key = normalize_optional(api_key);
    let normalized_base_url = match base_url {
        Some(url) => Some(canonicalize_provider_base_url(provider, url)?),
        None => resolve_provider_base_url(provider, None)?,
    };

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
                    extra_parameters_json: json!({}),
                })
                .await
                .map(|_| ())
                .map_err(|error| {
                    tracing::warn!(stage = "bootstrap", provider_kind = %provider.provider_kind, error = %error, "provider credential validation failed");
                    ApiError::BadRequest(format!(
                        "provider credential validation failed for {}: {error}",
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
    let ollama_loopback_base_url = provider.provider_kind == SupportedProviderKind::Ollama.as_str()
        && is_loopback_base_url(base_url);
    match fetch_provider_model_names(provider, api_key, base_url).await {
        Ok(_) => Ok(()),
        Err(error) if ollama_loopback_base_url => {
            let message = error.to_string();
            if message.contains("Connection refused")
                || message.contains("error trying to connect")
                || message.contains("timed out")
            {
                Err(ollama_loopback_runtime_error(provider))
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}
