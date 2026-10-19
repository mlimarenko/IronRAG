use anyhow::{Context, Result, anyhow};
use reqwest::{Url, header::AUTHORIZATION};
use serde::Deserialize;
use zeroize::Zeroize as _;

use crate::{
    domains::provider_profiles::{
        ANTHROPIC_MESSAGES_RUNTIME_KIND, GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
        ModelReasoningCapability, OPENAI_COMPATIBLE_RUNTIME_KIND, ProviderAuthScheme,
        ProviderBaseUrlPolicy, ProviderCredentialPolicy, ProviderRequestPolicy,
        ProviderRuntimeProfile, ProviderSamplingPolicy, ProviderToolChoicePolicy,
        RESOLVED_REASONING_CAPABILITY_FIELD, ReasoningWireControl, SelectedReasoningMode,
    },
    shared::provider_base_url::{is_private_provider_url, resolve_runtime_provider_base_url},
};

use super::UnifiedGateway;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeProviderProfileEnvelope {
    runtime: ProviderRuntimeProfile,
    base_url: ProviderBaseUrlPolicy,
    credentials: Option<ProviderCredentialPolicy>,
    #[serde(default)]
    request_policy: ProviderRequestPolicy,
}

#[derive(Clone)]
pub(super) struct ResolvedProviderRuntime {
    pub(super) api_key: Option<String>,
    pub(super) base_url: String,
    pub(super) allow_private_network: bool,
    pub(super) runtime: ProviderRuntimeProfile,
    pub(super) request_policy: ProviderRequestPolicy,
    /// The confirmed wire control for the binding's typed reasoning selection,
    /// or `None` when no mode was selected. `None` means the binding's own raw
    /// parameters and the existing automatic provider policy apply untouched;
    /// `Some` means exactly one declared control the request body must emit.
    /// Never `Some` for a selection the exact model has not confirmed.
    pub(super) reasoning_wire: Option<ReasoningWireControl>,
}

impl Drop for ResolvedProviderRuntime {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for ResolvedProviderRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedProviderRuntime")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &"<redacted>")
            .field("allow_private_network", &self.allow_private_network)
            .field("runtime", &self.runtime)
            .field("request_policy", &self.request_policy)
            .field("reasoning_wire", &self.reasoning_wire)
            .finish()
    }
}

impl ProviderRequestPolicy {
    pub(super) const fn sampling_params(
        &self,
        temperature: Option<f64>,
        top_p: Option<f64>,
    ) -> (Option<f64>, Option<f64>) {
        match self.sampling {
            ProviderSamplingPolicy::Forward => (temperature, top_p),
            ProviderSamplingPolicy::Omit => (None, None),
        }
    }

    pub(super) const fn tool_choice(
        &self,
        has_tools: bool,
        require_tool_call: bool,
    ) -> Option<&'static str> {
        if !has_tools {
            return None;
        }
        match (self.tool_choice, require_tool_call) {
            (ProviderToolChoicePolicy::RequiredCapable, true) => Some("required"),
            (ProviderToolChoicePolicy::RequiredCapable | ProviderToolChoicePolicy::AutoOnly, _) => {
                Some("auto")
            }
        }
    }

    pub(super) fn tool_max_output_tokens(&self, requested: Option<i32>) -> Option<i32> {
        requested.or(self.default_tool_max_output_tokens)
    }
}

impl UnifiedGateway {
    pub(super) fn resolve_provider(
        provider_kind: &str,
        api_key_override: Option<&str>,
        base_url_override: Option<&str>,
        model_name: &str,
        extra_parameters_json: &serde_json::Value,
    ) -> Result<ResolvedProviderRuntime> {
        let runtime_profile = resolve_runtime_profile(provider_kind, extra_parameters_json)?;
        let api_key = api_key_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string);
        let base_url = base_url_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("missing provider base URL for provider={provider_kind}"))?;
        validate_runtime_base_url(provider_kind, base_url, &runtime_profile.base_url)?;
        if api_key.is_none()
            && runtime_profile.credentials.as_ref().is_none_or(|policy| policy.api_key_required)
        {
            return Err(anyhow!("missing provider API key for provider={provider_kind}"));
        }
        let reasoning_wire = resolve_selected_reasoning_wire(model_name, extra_parameters_json)?;
        // The only confirmed wire forms this contract owns are the
        // OpenAI-compatible reasoning controls, so a selection on any other
        // runtime has no adapter and must fail closed rather than travel as an
        // accepted-but-unemitted parameter.
        if reasoning_wire.is_some()
            && runtime_profile.runtime.kind != OPENAI_COMPATIBLE_RUNTIME_KIND
        {
            return Err(anyhow!(
                "selected reasoning mode has no confirmed wire form for runtime kind {} for provider={provider_kind}",
                runtime_profile.runtime.kind
            ));
        }
        Ok(ResolvedProviderRuntime {
            api_key,
            base_url: resolve_runtime_provider_base_url(
                runtime_profile.base_url.allow_private_network,
                base_url,
            ),
            allow_private_network: runtime_profile.base_url.allow_private_network,
            runtime: runtime_profile.runtime,
            request_policy: runtime_profile.request_policy,
            reasoning_wire,
        })
    }
}

pub(super) fn resolve_provider_request_policy(
    provider_kind: &str,
    extra_parameters_json: &serde_json::Value,
    profile_policy: ProviderRequestPolicy,
) -> Result<ProviderRequestPolicy> {
    let policy = match extra_parameters_json.get("_providerRequestPolicy") {
        Some(value) => serde_json::from_value::<ProviderRequestPolicy>(value.clone())
            .with_context(|| {
                format!("invalid provider request policy for provider={provider_kind}")
            })?,
        None => profile_policy,
    };
    if !policy.is_valid() {
        return Err(anyhow!(
            "invalid provider request policy for provider={provider_kind}: \
             defaultToolMaxOutputTokens must be greater than zero"
        ));
    }
    Ok(policy)
}
/// Resolve the typed request policy a binding carries in its merged provider
/// profile.
///
/// `_providerRequestPolicy` (model-level metadata) replaces the provider
/// profile's own policy exactly as it does on the provider-call path. A
/// missing or malformed envelope yields the default policy, where reasoning
/// stays possible and the caller must therefore budget for it; the provider
/// call itself still fails closed on the same envelope.
#[must_use]
pub(crate) fn binding_request_policy(
    provider_kind: &str,
    extra_parameters_json: &serde_json::Value,
) -> ProviderRequestPolicy {
    let profile_policy = extra_parameters_json
        .get("_providerProfile")
        .and_then(|profile| profile.get("requestPolicy"))
        .and_then(|value| serde_json::from_value::<ProviderRequestPolicy>(value.clone()).ok())
        .unwrap_or_default();
    let profile_tool_history = profile_policy.synthetic_tool_history;
    let mut policy =
        resolve_provider_request_policy(provider_kind, extra_parameters_json, profile_policy)
            .unwrap_or_default();
    // The accepted wire shape is an endpoint capability, not a per-model
    // preference: a model-level envelope cannot grant the endpoint the ability
    // to carry inserted tool calls, so the profile keeps that bit.
    policy.synthetic_tool_history = profile_tool_history;
    policy
}

/// Resolve the binding's typed reasoning selection into the one declared wire
/// control of the exact bound model.
///
/// `None` means no mode was selected: the binding's own raw parameters and the
/// existing automatic provider policy apply untouched. A present selection is
/// checked against the server-projected capability for this exact model, and
/// the projected identity must match the requested model; a missing,
/// malformed, blank-evidence or non-matching capability, or a mode the model
/// has not confirmed, fails closed so a persisted selection can never travel
/// as an accepted-but-ignored parameter.
fn resolve_selected_reasoning_wire(
    model_name: &str,
    extra_parameters_json: &serde_json::Value,
) -> Result<Option<ReasoningWireControl>> {
    let Some(selection) = SelectedReasoningMode::from_binding_json(extra_parameters_json)
        .map_err(|reason| anyhow!("{reason}"))?
    else {
        return Ok(None);
    };
    let projected =
        extra_parameters_json.get(RESOLVED_REASONING_CAPABILITY_FIELD).ok_or_else(|| {
            anyhow!("resolved model reasoning capability is missing for model `{model_name}`")
        })?;
    let projected_model = projected
        .get("modelName")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("resolved model reasoning capability has no model identity"))?;
    if projected_model != model_name {
        return Err(anyhow!(
            "resolved reasoning capability is for model `{projected_model}`, not `{model_name}`"
        ));
    }
    let capability: ModelReasoningCapability = serde_json::from_value(
        projected.get("capability").cloned().unwrap_or(serde_json::Value::Null),
    )
    .map_err(|error| anyhow!("resolved model reasoning capability is invalid: {error}"))?;
    capability.validate().map_err(|reason| anyhow!("{reason}"))?;
    if !capability.allows(selection.mode()) {
        return Err(anyhow!(
            "selected reasoning mode `{}` is not confirmed for model `{model_name}`",
            selection.mode().as_str()
        ));
    }
    let wire = capability
        .wire()
        .ok_or_else(|| anyhow!("confirmed model reasoning capability has no wire control"))?;
    Ok(Some(wire))
}

/// Switch hidden reasoning off for a request whose completion budget is sized
/// for the answer alone, driven only by the binding's typed provider policy.
///
/// Returns whether reasoning was switched off. The binding's unrelated
/// parameters, and the reasoning parameters of every provider whose typed
/// policy does not declare the control, are left untouched; this function
/// never inspects the provider or model name.
pub(crate) fn disable_bounded_answer_reasoning(
    provider_kind: &str,
    extra_parameters_json: &mut serde_json::Value,
) -> bool {
    let Some(effort) =
        binding_request_policy(provider_kind, extra_parameters_json).reasoning_effort_override()
    else {
        return false;
    };
    let Some(parameters) = extra_parameters_json.as_object_mut() else {
        return false;
    };
    parameters
        .insert("reasoning_effort".to_string(), serde_json::Value::String(effort.to_string()));
    true
}

fn resolve_runtime_profile(
    provider_kind: &str,
    extra_parameters_json: &serde_json::Value,
) -> Result<RuntimeProviderProfileEnvelope> {
    if let Some(value) = extra_parameters_json.get("_providerProfile") {
        let mut profile = serde_json::from_value::<RuntimeProviderProfileEnvelope>(value.clone())
            .with_context(|| {
            format!("invalid runtime provider profile for provider={provider_kind}")
        })?;
        if !matches!(
            profile.runtime.kind.as_str(),
            OPENAI_COMPATIBLE_RUNTIME_KIND
                | ANTHROPIC_MESSAGES_RUNTIME_KIND
                | GEMINI_GENERATE_CONTENT_RUNTIME_KIND
        ) {
            return Err(anyhow!("unsupported provider runtime kind for provider={provider_kind}"));
        }
        profile.request_policy = resolve_provider_request_policy(
            provider_kind,
            extra_parameters_json,
            profile.request_policy,
        )?;
        return Ok(profile);
    }

    Err(anyhow!("missing runtime provider profile for provider={provider_kind}"))
}

fn normalize_runtime_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!("/{}", trimmed.trim_start_matches('/').trim_end_matches('/'))
}

pub(super) fn provider_endpoint_url(
    provider_kind: &str,
    base_url: &str,
    path: &str,
) -> Result<Url> {
    let mut url = Url::parse(base_url)
        .with_context(|| format!("invalid provider base URL for provider={provider_kind}"))?;
    reject_url_userinfo_query_fragment(provider_kind, &url)?;

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

fn reject_url_userinfo_query_fragment(provider_kind: &str, url: &Url) -> Result<()> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow!(
            "provider base URL must not include userinfo for provider={provider_kind}"
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(anyhow!(
            "provider base URL must not include query or fragment components for provider={provider_kind}"
        ));
    }
    Ok(())
}

fn validate_runtime_base_url(
    provider_kind: &str,
    base_url: &str,
    policy: &ProviderBaseUrlPolicy,
) -> Result<()> {
    let url = Url::parse(base_url)
        .with_context(|| format!("invalid provider base URL for provider={provider_kind}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(anyhow!(
            "provider base URL must use http or https for provider={provider_kind}"
        ));
    }
    reject_url_userinfo_query_fragment(provider_kind, &url)?;
    if policy.require_https && url.scheme() != "https" {
        return Err(anyhow!("provider base URL must use https for provider={provider_kind}"));
    }
    if !policy.allow_private_network && is_private_provider_url(&url) {
        return Err(anyhow!(
            "provider base URL must not target a private, loopback, or link-local network for provider={provider_kind}"
        ));
    }
    Ok(())
}

pub(super) fn apply_provider_auth(
    request: reqwest::RequestBuilder,
    auth_scheme: ProviderAuthScheme,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let Some(token) = api_key.map(str::trim).filter(|value| !value.is_empty()) else {
        return request;
    };
    match auth_scheme {
        ProviderAuthScheme::Bearer => request.bearer_auth(token),
        ProviderAuthScheme::RawAuthorization => request.header(AUTHORIZATION, token),
        ProviderAuthScheme::AnthropicApiKey => request.header("x-api-key", token),
        ProviderAuthScheme::GoogleApiKey => request.header("x-goog-api-key", token),
    }
}
