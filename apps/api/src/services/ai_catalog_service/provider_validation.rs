use super::*;
use crate::{
    domains::provider_profiles::{
        ProviderAuthScheme, ProviderBaseUrlMode, ProviderModelDiscoveryMode,
    },
    integrations::{llm::ChatRequest, retry::ProviderCallError},
    shared::{
        outbound_http::{PublicHttpUrlError, read_response_bytes_with_limit},
        provider_base_url::{is_private_provider_url, provider_base_url_candidates},
        provider_http::{
            PROVIDER_MODEL_LIST_BODY_MAX_BYTES, ProviderHttpError, ProviderHttpTransport,
            ProviderHttpTransportConfig,
        },
    },
};
use reqwest::{Method, Url};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};

const TEXT_CHAT_BINDING_PURPOSES: [AiBindingPurpose; 3] =
    [AiBindingPurpose::ExtractGraph, AiBindingPurpose::QueryCompile, AiBindingPurpose::QueryAnswer];
const TEXT_TOOL_CHAT_BINDING_PURPOSES: [AiBindingPurpose; 4] = [
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Agent,
];
const MULTIMODAL_CHAT_BINDING_PURPOSES: [AiBindingPurpose; 4] = [
    AiBindingPurpose::ExtractText,
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
];
const MULTIMODAL_TOOL_CHAT_BINDING_PURPOSES: [AiBindingPurpose; 5] = [
    AiBindingPurpose::ExtractText,
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Agent,
];
const EMBEDDING_BINDING_PURPOSES: [AiBindingPurpose; 1] = [AiBindingPurpose::EmbedChunk];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderCredentialValidationFailureKind {
    Rejected,
    Transport,
    LoopbackUnreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatRoundTripValidationFailureKind {
    CredentialRejected,
    ProviderUnavailable,
    Internal,
}

impl ProviderCredentialValidationFailureKind {
    const fn api_error_kind(self) -> &'static str {
        match self {
            Self::Rejected => "provider_credential_validation_rejected",
            Self::Transport => "provider_credential_validation_transport_failed",
            Self::LoopbackUnreachable => "provider_credential_validation_loopback_unreachable",
        }
    }

    fn from_api_error_kind(value: &str) -> Option<Self> {
        if value == Self::Rejected.api_error_kind() {
            Some(Self::Rejected)
        } else if value == Self::Transport.api_error_kind() {
            Some(Self::Transport)
        } else if value == Self::LoopbackUnreachable.api_error_kind() {
            Some(Self::LoopbackUnreachable)
        } else {
            None
        }
    }
}

pub(super) fn provider_credential_validation_error(
    failure_kind: ProviderCredentialValidationFailureKind,
    message: impl Into<String>,
) -> ApiError {
    ApiError::service_unavailable(message, failure_kind.api_error_kind())
}

fn provider_credential_validation_failure_kind(
    error: &ApiError,
) -> Option<ProviderCredentialValidationFailureKind> {
    match error {
        ApiError::ServiceUnavailable { kind, .. } => {
            ProviderCredentialValidationFailureKind::from_api_error_kind(kind)
        }
        _ => None,
    }
}

pub(super) fn chat_round_trip_validation_failure_kind(
    error: &anyhow::Error,
) -> ChatRoundTripValidationFailureKind {
    let Some(error) = error.downcast_ref::<ProviderCallError>() else {
        return ChatRoundTripValidationFailureKind::Internal;
    };
    match error {
        ProviderCallError::Transport { .. } | ProviderCallError::ResponseBody { .. } => {
            ChatRoundTripValidationFailureKind::ProviderUnavailable
        }
        ProviderCallError::ResponsePolicy {
            source: PublicHttpUrlError::BodyReadFailed(_), ..
        } => ChatRoundTripValidationFailureKind::ProviderUnavailable,
        ProviderCallError::HttpStatus { status, .. }
            if matches!(
                *status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            ) =>
        {
            ChatRoundTripValidationFailureKind::CredentialRejected
        }
        ProviderCallError::HttpStatus { status, .. }
            if *status == reqwest::StatusCode::REQUEST_TIMEOUT
                || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error() =>
        {
            ChatRoundTripValidationFailureKind::ProviderUnavailable
        }
        ProviderCallError::ResponsePolicy { .. }
        | ProviderCallError::ResponseJson { .. }
        | ProviderCallError::Json { .. }
        | ProviderCallError::HttpStatus { .. }
        | ProviderCallError::Protocol { .. } => ChatRoundTripValidationFailureKind::Internal,
    }
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
            "provider {} does not allow baseUrl overrides",
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
    if base_url.is_some() {
        return Ok(base_url);
    }
    if matches!(provider.credential_policy.base_url_mode, ProviderBaseUrlMode::Required) {
        return resolve_provider_base_url(provider, None);
    }
    Ok(None)
}

pub(super) fn provider_credential_base_url_for_update(
    provider: &ProviderCatalogEntry,
    existing: Option<&str>,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    if value.is_none() && provider.base_url_policy.allow_override {
        return normalize_provider_base_url_input(provider, existing);
    }
    let base_url = normalize_provider_base_url_input(provider, value)?;
    if base_url.is_some() {
        return Ok(base_url);
    }
    if matches!(provider.credential_policy.base_url_mode, ProviderBaseUrlMode::Required) {
        return resolve_provider_base_url(provider, None);
    }
    Ok(None)
}

/// Prevents a caller that can edit account metadata but cannot read its secret
/// from moving that hidden secret to a different provider origin. Reusing a
/// key on the same canonical authority is safe; changing scheme, host, or port
/// requires the caller to submit a replacement key explicitly.
pub(super) fn validate_provider_base_url_key_reuse(
    provider: &ProviderCatalogEntry,
    existing_base_url: Option<&str>,
    updated_base_url: Option<&str>,
    existing_key_is_present: bool,
    replacement_key_is_present: bool,
) -> Result<(), ApiError> {
    if !existing_key_is_present || replacement_key_is_present {
        return Ok(());
    }
    let existing_origin = effective_provider_origin(provider, existing_base_url)?;
    let updated_origin = effective_provider_origin(provider, updated_base_url)?;
    if existing_origin != updated_origin {
        return Err(ApiError::BadRequest(format!(
            "provider {} requires an explicit apiKey when baseUrl authority changes",
            provider.provider_kind
        )));
    }
    Ok(())
}

fn effective_provider_origin(
    provider: &ProviderCatalogEntry,
    credential_base_url: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let candidate = normalize_optional(credential_base_url)
        .or_else(|| normalize_optional(provider.default_base_url.as_deref()));
    candidate
        .map(|value| {
            let canonical = canonicalize_provider_base_url(provider, &value)?;
            Url::parse(&canonical).map(|url| url.origin().ascii_serialization()).map_err(|_| {
                ApiError::BadRequest(format!(
                    "baseUrl must be a valid absolute URL for provider {}",
                    provider.provider_kind
                ))
            })
        })
        .transpose()
}

pub(super) fn runtime_provider_base_url(
    provider: &ProviderCatalogEntry,
    credential_base_url: Option<&str>,
) -> Result<Option<String>, ApiError> {
    resolve_provider_base_url(provider, credential_base_url)
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
        if !provider.base_url_policy.allow_private_network && is_private_provider_url(&url) {
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

const fn text_chat_signature(tools_supported: bool) -> DiscoveredModelSignature {
    DiscoveredModelSignature {
        capability_kind: "chat",
        modality_kind: "text",
        allowed_binding_purposes: if tools_supported {
            &TEXT_TOOL_CHAT_BINDING_PURPOSES
        } else {
            &TEXT_CHAT_BINDING_PURPOSES
        },
    }
}

const fn multimodal_chat_signature(tools_supported: bool) -> DiscoveredModelSignature {
    DiscoveredModelSignature {
        capability_kind: "chat",
        modality_kind: "multimodal",
        allowed_binding_purposes: if tools_supported {
            &MULTIMODAL_TOOL_CHAT_BINDING_PURPOSES
        } else {
            &MULTIMODAL_CHAT_BINDING_PURPOSES
        },
    }
}

const fn embedding_signature() -> DiscoveredModelSignature {
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
        "chat" if provider.capabilities.chat.is_supported() => {
            Ok(Some(text_chat_signature(provider.capabilities.tools.is_supported())))
        }
        "embedding" if provider.capabilities.embeddings.is_supported() => {
            Ok(Some(embedding_signature()))
        }
        "vision"
            if provider.capabilities.chat.is_supported()
                && provider.capabilities.vision.is_supported() =>
        {
            Ok(Some(multimodal_chat_signature(provider.capabilities.tools.is_supported())))
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
    let transport = provider_validation_transport()?;

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
            transport,
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
    let transport = provider_validation_transport()?;

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
                transport,
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
    transport: &ProviderHttpTransport,
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    candidate_urls: &[String],
    model_path: &str,
) -> Result<Vec<String>, ApiError> {
    let mut last_error = None;
    for candidate_url in candidate_urls {
        let endpoint = provider_endpoint_url(candidate_url, model_path, provider)?;
        let target = match transport
            .prepare(&endpoint, provider.base_url_policy.allow_private_network)
            .await
        {
            Ok(target) => target,
            Err(error) => {
                last_error = Some(provider_model_list_error(
                    provider,
                    model_path,
                    provider_http_validation_failure_kind(&error),
                ));
                continue;
            }
        };
        let request = match target.request(Method::GET, &endpoint) {
            Ok(request) => request,
            Err(error) => {
                last_error = Some(provider_model_list_error(
                    provider,
                    model_path,
                    provider_http_validation_failure_kind(&error),
                ));
                continue;
            }
        };
        let request = apply_provider_auth(request, provider.runtime.auth_scheme, api_key);
        let request = crate::observability::inject_trace_context(request);
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                if !status.is_success() {
                    last_error = Some(provider_credential_validation_error(
                        ProviderCredentialValidationFailureKind::Rejected,
                        format!(
                            "provider credential validation failed for {} at {}: status={status}",
                            provider.display_name, model_path
                        ),
                    ));
                    continue;
                }

                let body_bytes = match read_response_bytes_with_limit(
                    response,
                    PROVIDER_MODEL_LIST_BODY_MAX_BYTES,
                )
                .await
                {
                    Ok(body) => body,
                    Err(error) => {
                        last_error = Some(provider_model_list_error(
                            provider,
                            model_path,
                            response_body_validation_failure_kind(&error),
                        ));
                        continue;
                    }
                };
                let Ok(body) = serde_json::from_slice::<Value>(&body_bytes) else {
                    last_error = Some(provider_credential_validation_error(
                        ProviderCredentialValidationFailureKind::Rejected,
                        format!(
                            "provider credential validation failed for {} at {}: invalid model list response",
                            provider.display_name, model_path
                        ),
                    ));
                    continue;
                };
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
                last_error = Some(provider_model_list_error(
                    provider,
                    model_path,
                    reqwest_validation_failure_kind(&error),
                ));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        provider_credential_validation_error(
            ProviderCredentialValidationFailureKind::Rejected,
            format!(
                "provider credential validation failed for {} at {}: no candidate baseUrl succeeded",
                provider.display_name, model_path
            ),
        )
    }))
}

fn provider_validation_transport() -> Result<&'static Arc<ProviderHttpTransport>, ApiError> {
    static TRANSPORT: OnceLock<Result<Arc<ProviderHttpTransport>, ProviderHttpError>> =
        OnceLock::new();
    match TRANSPORT.get_or_init(|| {
        ProviderHttpTransport::try_new(ProviderHttpTransportConfig::provider_validation())
            .map(Arc::new)
    }) {
        Ok(transport) => Ok(transport),
        Err(_) => Err(ApiError::Internal),
    }
}

fn provider_model_list_error(
    provider: &ProviderCatalogEntry,
    model_path: &str,
    failure_kind: ProviderCredentialValidationFailureKind,
) -> ApiError {
    provider_credential_validation_error(
        failure_kind,
        format!(
            "provider credential validation failed for {} at {}: upstream provider request failed; response details were redacted",
            provider.display_name, model_path
        ),
    )
}

const fn provider_http_validation_failure_kind(
    error: &ProviderHttpError,
) -> ProviderCredentialValidationFailureKind {
    if matches!(
        error,
        ProviderHttpError::ResolveFailed
            | ProviderHttpError::ResolveTimedOut
            | ProviderHttpError::NoAddresses
    ) {
        ProviderCredentialValidationFailureKind::Transport
    } else {
        ProviderCredentialValidationFailureKind::Rejected
    }
}

const fn response_body_validation_failure_kind(
    error: &PublicHttpUrlError,
) -> ProviderCredentialValidationFailureKind {
    if matches!(error, PublicHttpUrlError::BodyReadFailed(_)) {
        ProviderCredentialValidationFailureKind::Transport
    } else {
        ProviderCredentialValidationFailureKind::Rejected
    }
}

fn reqwest_validation_failure_kind(
    error: &reqwest::Error,
) -> ProviderCredentialValidationFailureKind {
    if error.is_connect() || error.is_timeout() || error.is_body() {
        ProviderCredentialValidationFailureKind::Transport
    } else {
        ProviderCredentialValidationFailureKind::Rejected
    }
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
                entry.provider_catalog_id == provider.id && entry.model_name == profile.model_name
            })
        {
            return Some(model);
        }
    }

    models
        .iter()
        .filter(|model| model.provider_catalog_id == provider.id && model.capability_kind == "chat")
        .min_by(|left, right| {
            left.model_name.cmp(&right.model_name).then_with(|| left.id.cmp(&right.id))
        })
}

pub(super) fn provider_validation_extra_parameters(
    provider: &ProviderCatalogEntry,
    model: &ModelCatalogEntry,
) -> Value {
    merge_provider_runtime_profile(
        merge_model_request_policy(json!({}), &model.metadata_json),
        &provider.profile,
    )
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
