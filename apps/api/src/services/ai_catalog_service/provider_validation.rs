pub(super) use super::provider_access::{
    is_provider_credential_validation_error, validate_provider_access,
};
use super::*;
use crate::{
    domains::provider_profiles::{
        ProviderAuthScheme, ProviderBaseUrlMode, ProviderModelDiscoveryMode,
    },
    integrations::retry::ProviderCallError,
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
use rust_decimal::Decimal;
use serde_json::Value;
use std::sync::{Arc, OnceLock};

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

pub(super) fn provider_credential_validation_failure_kind(
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
        ProviderCallError::Transport { .. }
        | ProviderCallError::ResponseBody { .. }
        | ProviderCallError::Timeout { .. } => {
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
        ProviderCallError::Admission { .. }
        | ProviderCallError::ResponsePolicy { .. }
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
    normalize_provider_base_url(provider, &candidate).map(Some)
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
/// key on the same current authority is safe; changing scheme, host, or port
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
            let normalized_base_url = normalize_provider_base_url(provider, &value)?;
            Url::parse(&normalized_base_url).map(|url| url.origin().ascii_serialization()).map_err(
                |_| {
                    ApiError::BadRequest(format!(
                        "baseUrl must be a valid absolute URL for provider {}",
                        provider.provider_kind
                    ))
                },
            )
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
            .map(|candidate| normalize_provider_base_url(provider, candidate))
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
            .map(|candidate| normalize_provider_base_url(provider, candidate))
            .transpose()?),
    }
}

pub(super) fn normalize_provider_base_url(
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

pub(super) async fn fetch_available_provider_models(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<Vec<DiscoveredProviderModel>, ApiError> {
    if provider.model_discovery.mode == ProviderModelDiscoveryMode::Unsupported {
        return Err(ApiError::BadRequest(format!(
            "provider {} does not support model discovery",
            provider.provider_kind
        )));
    }
    let Some(base_url) = runtime_provider_base_url(provider, base_url)? else {
        return Ok(Vec::new());
    };
    fetch_provider_models(provider, api_key, &base_url).await
}

pub(super) async fn fetch_provider_model_names(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
) -> Result<Vec<String>, ApiError> {
    Ok(fetch_provider_models(provider, api_key, base_url)
        .await?
        .into_iter()
        .map(|model| model.model_name)
        .collect())
}

async fn fetch_provider_models(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
) -> Result<Vec<DiscoveredProviderModel>, ApiError> {
    let paths =
        provider.model_discovery.paths.iter().map(|path| path.path.as_str()).collect::<Vec<_>>();
    fetch_provider_models_from_paths(provider, api_key, base_url, paths).await
}

async fn fetch_provider_models_from_paths(
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    base_url: &str,
    paths: Vec<&str>,
) -> Result<Vec<DiscoveredProviderModel>, ApiError> {
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

    let mut discovered: Vec<DiscoveredProviderModel> = Vec::new();
    for model_path in model_paths {
        discovered.extend(
            fetch_provider_models_from_path(
                transport,
                provider,
                api_key,
                &candidate_urls,
                model_path,
            )
            .await?,
        );
    }

    // A provider can list the same id under two discovery paths (chat and
    // embeddings on one endpoint is the common case). Keep the first record so
    // the result is one row per id, not one row per path it appeared on.
    discovered.sort_by(|left, right| left.model_name.cmp(&right.model_name));
    discovered.dedup_by(|left, right| left.model_name == right.model_name);
    Ok(discovered)
}

async fn fetch_provider_models_from_path(
    transport: &ProviderHttpTransport,
    provider: &ProviderCatalogEntry,
    api_key: Option<&str>,
    candidate_urls: &[String],
    model_path: &str,
) -> Result<Vec<DiscoveredProviderModel>, ApiError> {
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
                let listed_currency = provider.model_discovery.listed_pricing_currency.as_deref();
                return Ok(body
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| discovered_model_from_listing_entry(entry, listed_currency))
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

/// Reads one entry of an OpenAI-style `data[]` model listing.
///
/// Only fields the provider declares are read. Nothing is guessed from the id:
/// a name-based classifier would be the hand-written dictionary the product
/// rules forbid, and it would also be wrong — routers proxy vendors whose
/// naming conventions disagree with each other.
pub(super) fn discovered_model_from_listing_entry(
    entry: &Value,
    listed_currency: Option<&str>,
) -> Option<DiscoveredProviderModel> {
    let model_name = entry
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    Some(DiscoveredProviderModel {
        model_name,
        context_window: listing_token_count(entry, "context_length")
            .or_else(|| listing_token_count(entry, "context_window")),
        max_output_tokens: entry
            .get("top_provider")
            .and_then(|provider| listing_token_count(provider, "max_completion_tokens")),
        input_modalities: entry
            .get("architecture")
            .and_then(|architecture| architecture.get("input_modalities"))
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        pricing: listing_pricing(entry.get("pricing"), listed_currency),
    })
}

fn listing_token_count(entry: &Value, field: &str) -> Option<i32> {
    entry.get(field).and_then(Value::as_i64).and_then(|value| i32::try_from(value).ok())
}

/// Per-token rates as OpenAI-compatible routers publish them: a bare number
/// with no unit attached. The unit comes from the provider profile, because the
/// listing never states it — a rouble-priced router fills the same field a
/// dollar-priced one does, and reading one as the other overstates spend a
/// hundredfold. Without a declared unit there is no price.
///
/// Both directions must parse or the record carries no price at all: half a
/// price would be recorded as a discount.
fn listing_pricing(pricing: Option<&Value>, listed_currency: Option<&str>) -> Option<ModelPricing> {
    let pricing = pricing?;
    let currency_code = listed_currency?.trim();
    if currency_code.is_empty() {
        return None;
    }
    let input_price_per_1m = listing_price_per_1m(pricing, "prompt")?;
    let output_price_per_1m = listing_price_per_1m(pricing, "completion")?;
    Some(ModelPricing {
        currency_code: currency_code.to_owned(),
        input_price_per_1m,
        cached_input_price_per_1m: listing_price_per_1m(pricing, "input_cache_read")
            .unwrap_or(input_price_per_1m),
        cache_write_input_price_per_1m: listing_price_per_1m(pricing, "input_cache_write")
            .unwrap_or(input_price_per_1m),
        output_price_per_1m,
        long_context: None,
    })
}

fn listing_price_per_1m(pricing: &Value, field: &str) -> Option<Decimal> {
    let raw = pricing.get(field)?;
    let per_token = match raw {
        Value::String(value) => Decimal::from_str_exact(value.trim()).ok()?,
        Value::Number(value) => Decimal::from_str_exact(&value.to_string()).ok()?,
        _ => return None,
    };
    (!per_token.is_sign_negative()).then(|| per_token * Decimal::from(1_000_000))
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

fn provider_http_validation_failure_kind(
    error: &ProviderHttpError,
) -> ProviderCredentialValidationFailureKind {
    if error.resolution_failure().is_some() {
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
        ProviderAuthScheme::AnthropicApiKey => request.header("x-api-key", token),
        ProviderAuthScheme::GoogleApiKey => request.header("x-goog-api-key", token),
    }
}
