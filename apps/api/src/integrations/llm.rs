use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;

mod openai_compatible;
mod streaming;

use self::{
    openai_compatible::{
        OpenAiCompatibleContentPart, OpenAiCompatibleImageUrl, OpenAiCompatibleMessage,
        OpenAiCompatibleMessageContent, OpenAiCompatibleRequest, OpenAiCompatibleToolDef,
        OpenAiCompatibleToolUseChatRequest, OpenAiCompatibleToolUseMessage,
        extract_message_content_text, openai_compatible_token_limit_fields,
    },
    streaming::{drain_openai_compatible_stream, drain_tool_use_stream},
};

#[cfg(test)]
use self::streaming::consume_openai_compatible_stream_frame;

use crate::{
    app::config::Settings,
    domains::provider_profiles::{
        OPENAI_COMPATIBLE_RUNTIME_KIND, ProviderAuthScheme, ProviderBaseUrlPolicy,
        ProviderCredentialPolicy, ProviderRuntimeProfile, ProviderStructuredOutputMode,
    },
    integrations::retry::{ProviderCallError, RetryPolicy, provider_http_status_error, with_retry},
    shared::provider_base_url::resolve_runtime_provider_base_url,
};

#[cfg(test)]
use crate::domains::provider_profiles::ProviderTokenLimitParameter;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub prompt: String,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub response_format: Option<serde_json::Value>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatRequestSeed {
    pub provider_kind: String,
    pub model_name: String,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

#[must_use]
pub fn build_text_chat_request(seed: ChatRequestSeed, prompt: String) -> ChatRequest {
    ChatRequest {
        provider_kind: seed.provider_kind,
        model_name: seed.model_name,
        prompt,
        api_key_override: seed.api_key_override,
        base_url_override: seed.base_url_override,
        system_prompt: seed.system_prompt,
        temperature: seed.temperature,
        top_p: seed.top_p,
        max_output_tokens_override: seed.max_output_tokens_override,
        response_format: None,
        extra_parameters_json: seed.extra_parameters_json,
    }
}

#[must_use]
pub fn build_structured_chat_request(
    seed: ChatRequestSeed,
    prompt: String,
    response_format: serde_json::Value,
) -> ChatRequest {
    ChatRequest {
        provider_kind: seed.provider_kind,
        model_name: seed.model_name,
        prompt,
        api_key_override: seed.api_key_override,
        base_url_override: seed.base_url_override,
        system_prompt: seed.system_prompt,
        temperature: seed.temperature,
        top_p: seed.top_p,
        max_output_tokens_override: seed.max_output_tokens_override,
        response_format: Some(response_format),
        extra_parameters_json: seed.extra_parameters_json,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatResponse {
    pub provider_kind: String,
    pub model_name: String,
    pub output_text: String,
    pub usage_json: serde_json::Value,
}

// =============================================================================
// Tool-use types (used by external MCP agents and tool-capable providers)
// =============================================================================

/// JSON-schema description of a single tool the LLM may call.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// One tool invocation requested by the LLM in its response.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON string of arguments as returned by the model.
    pub arguments_json: String,
}

/// Multi-turn conversation message used by answer calls and external
/// tool-capable agents. Mirrors the OpenAI chat.completions message shape
/// so the same wire format works for every OpenAI-compatible provider
/// (OpenAI, Qwen, DeepSeek, Ollama, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatMessage {
    /// One of: "system", "user", "assistant", "tool".
    pub role: String,
    /// Plain text content. Optional because assistant messages can be
    /// tool-call only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Provider-emitted reasoning trace echoed back by DeepSeek thinking
    /// models when continuing a multi-turn tool-loop. Other providers
    /// ignore the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Tool calls produced by the assistant on its previous turn.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<ChatToolCall>,
    /// For role="tool" messages: the id of the call this message answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// For role="tool" messages: the tool name (some providers want it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    #[must_use]
    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    #[must_use]
    pub fn assistant_with_tool_calls(tool_calls: Vec<ChatToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content: None,
            reasoning_content: None,
            tool_calls,
            tool_call_id: None,
            name: None,
        }
    }

    /// Assistant turn that carries a `reasoning_content` echo plus its tool
    /// calls. DeepSeek thinking-mode rejects subsequent tool-loop calls
    /// when the prior reasoning is not echoed back, so the agent loop must
    /// preserve it across iterations.
    #[must_use]
    pub fn assistant_with_reasoning_and_tool_calls(
        reasoning_content: Option<String>,
        tool_calls: Vec<ChatToolCall>,
    ) -> Self {
        Self {
            role: "assistant".into(),
            content: None,
            reasoning_content,
            tool_calls,
            tool_call_id: None,
            name: None,
        }
    }

    #[must_use]
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(tool_name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolUseRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ChatToolDef>,
    pub extra_parameters_json: serde_json::Value,
    /// When true, the gateway sends `tool_choice="required"` so the
    /// provider must invoke at least one declared tool on this turn.
    /// The MCP-agent loop sets this on the first iteration to satisfy
    /// the constitution §16 requirement that UI answers stay grounded.
    /// Default `false` keeps the existing `tool_choice="auto"` behavior
    /// for callers that do not need a forced tool call.
    #[serde(default)]
    pub require_tool_call: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolUseResponse {
    pub provider_kind: String,
    pub model_name: String,
    /// Final text output. Populated when finish_reason is "stop".
    pub output_text: String,
    /// Tool calls the model wants the caller to execute. Populated when
    /// finish_reason is "tool_calls".
    pub tool_calls: Vec<ChatToolCall>,
    pub finish_reason: Option<String>,
    pub usage_json: serde_json::Value,
    /// Provider reasoning trace, used by DeepSeek thinking models — must
    /// be echoed back on the next turn or the provider returns 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmbeddingRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub input: String,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmbeddingBatchRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub inputs: Vec<String>,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmbeddingResponse {
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: usize,
    pub embedding: Vec<f32>,
    pub usage_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmbeddingBatchResponse {
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: usize,
    pub embeddings: Vec<Vec<f32>>,
    pub usage_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct VisionRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub prompt: String,
    pub image_bytes: Vec<u8>,
    pub mime_type: String,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct VisionResponse {
    pub provider_kind: String,
    pub model_name: String,
    pub output_text: String,
    pub usage_json: serde_json::Value,
}

#[async_trait]
pub trait LlmGateway: Send + Sync {
    async fn generate(&self, request: ChatRequest) -> Result<ChatResponse>;
    async fn generate_stream(
        &self,
        request: ChatRequest,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ChatResponse> {
        let response = self.generate(request).await?;
        if !response.output_text.is_empty() {
            on_delta(response.output_text.clone());
        }
        Ok(response)
    }
    /// Tool-use capable chat completion. The provider must be OpenAI-compatible
    /// (OpenAI, Qwen, DeepSeek, Ollama with tool-capable models, etc.).
    /// Default implementation rejects the request — concrete gateways MUST
    /// override it. Test fakes are free to keep the default.
    async fn generate_with_tools(&self, _request: ToolUseRequest) -> Result<ToolUseResponse> {
        Err(anyhow!("generate_with_tools is not implemented for this LlmGateway"))
    }
    /// Streaming variant of [`LlmGateway::generate_with_tools`]. When the
    /// model emits assistant text (the final answer), `on_text_delta` is
    /// invoked with each chunk immediately. Tool calls are buffered and
    /// returned in the final [`ToolUseResponse`] — there is no sensible
    /// way to react to a partial tool-call payload mid-stream. Default
    /// implementation falls back to the non-streaming path so providers
    /// that don't support streaming (or test fakes) still work.
    async fn generate_with_tools_stream(
        &self,
        request: ToolUseRequest,
        on_text_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ToolUseResponse> {
        let response = self.generate_with_tools(request).await?;
        if !response.output_text.is_empty() {
            on_text_delta(response.output_text.clone());
        }
        Ok(response)
    }
    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse>;
    async fn embed_many(&self, request: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse>;
    async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse>;
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeProviderProfileEnvelope {
    runtime: ProviderRuntimeProfile,
    base_url: ProviderBaseUrlPolicy,
    credentials: Option<ProviderCredentialPolicy>,
}

#[derive(Debug, Clone)]
struct ResolvedProviderRuntime {
    api_key: Option<String>,
    base_url: String,
    runtime: ProviderRuntimeProfile,
}

#[derive(Clone)]
pub struct UnifiedGateway {
    client: Client,
}

impl UnifiedGateway {
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        let timeout = Duration::from_secs(settings.llm_http_timeout_seconds.max(1));
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { client }
    }

    async fn call_openai_compatible(
        &self,
        request: OpenAiCompatibleRequest<'_>,
    ) -> Result<(String, serde_json::Value)> {
        let request_body = request.body()?;
        let endpoint_url =
            provider_endpoint_url(request.provider_kind, request.base_url, &request.chat_path)?;

        with_retry(
            || async {
                let request_builder = self
                    .client
                    .post(endpoint_url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json");
                let request_builder =
                    apply_provider_auth(request_builder, request.auth_scheme, request.api_key);
                let request_builder = crate::observability::inject_trace_context(request_builder);
                let response =
                    request_builder.body(request_body.clone()).send().await.map_err(|source| {
                        ProviderCallError::transport(
                            format!(
                                "provider transport failed: provider={}",
                                request.provider_kind
                            ),
                            source,
                        )
                    })?;

                let status = response.status();
                let headers = response.headers().clone();
                let body_text = response.text().await.map_err(|source| {
                    ProviderCallError::response_body(
                        format!(
                            "failed to read provider response body: provider={}",
                            request.provider_kind
                        ),
                        source,
                    )
                })?;

                if !status.is_success() {
                    return Err(provider_http_status_error(
                        request.provider_kind,
                        status,
                        &headers,
                        &body_text,
                    ));
                }

                let body = serde_json::from_str::<serde_json::Value>(&body_text)
                    .unwrap_or_else(|_| serde_json::json!({ "raw_body": body_text }));

                let output_text = body
                    .get("choices")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|v| v.get("message"))
                    .and_then(|v| v.get("content"))
                    .map(extract_message_content_text)
                    .unwrap_or_default();

                let usage_json =
                    body.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));

                Ok((output_text, usage_json))
            },
            RetryPolicy::default(),
        )
        .await
        .map_err(Into::into)
    }

    async fn call_openai_compatible_stream(
        &self,
        request: OpenAiCompatibleRequest<'_>,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<(String, serde_json::Value)> {
        let request_body = request.body()?;
        let endpoint_url =
            provider_endpoint_url(request.provider_kind, request.base_url, &request.chat_path)?;

        let response = with_retry(
            || async {
                let request_builder = self
                    .client
                    .post(endpoint_url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "text/event-stream");
                let request_builder =
                    apply_provider_auth(request_builder, request.auth_scheme, request.api_key);
                let request_builder = crate::observability::inject_trace_context(request_builder);
                let response =
                    request_builder.body(request_body.clone()).send().await.map_err(|source| {
                        ProviderCallError::transport(
                            format!(
                                "provider transport failed: provider={}",
                                request.provider_kind
                            ),
                            source,
                        )
                    })?;

                let status = response.status();
                if status.is_success() {
                    return Ok(response);
                }

                let headers = response.headers().clone();
                let body_text = response.text().await.unwrap_or_default();
                Err(provider_http_status_error(request.provider_kind, status, &headers, &body_text))
            },
            RetryPolicy::default(),
        )
        .await?;

        drain_openai_compatible_stream(response, on_delta).await
    }

    fn parse_embedding_vector(value: &serde_json::Value) -> Vec<f32> {
        value
            .as_array()
            .map(|arr| {
                #[allow(clippy::cast_possible_truncation)]
                arr.iter()
                    .filter_map(serde_json::Value::as_f64)
                    .filter(|embedding_value| embedding_value.is_finite())
                    .filter(|embedding_value| {
                        *embedding_value >= f64::from(f32::MIN)
                            && *embedding_value <= f64::from(f32::MAX)
                    })
                    .map(|embedding_value| embedding_value as f32)
                    .collect::<Vec<f32>>()
            })
            .unwrap_or_default()
    }

    fn embedding_request_body(
        model_name: &str,
        input: serde_json::Value,
        extra_parameters_json: &serde_json::Value,
    ) -> serde_json::Value {
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), serde_json::Value::String(model_name.to_string()));
        body.insert("input".to_string(), input);

        if let Some(extra) = extra_parameters_json.as_object() {
            for (key, value) in extra {
                if key == "model" || key == "input" || key.starts_with("_provider") {
                    continue;
                }
                body.insert(key.clone(), value.clone());
            }
        }

        serde_json::Value::Object(body)
    }

    fn upstream_extra_parameters(extra_parameters_json: &serde_json::Value) -> serde_json::Value {
        let Some(extra) = extra_parameters_json.as_object() else {
            return serde_json::json!({});
        };
        let filtered = extra
            .iter()
            .filter(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "model"
                        | "messages"
                        | "tools"
                        | "tool_choice"
                        | "temperature"
                        | "top_p"
                        | "max_completion_tokens"
                        | "max_tokens"
                        | "response_format"
                        | "stream"
                        | "stream_options"
                ) && !key.starts_with("_provider")
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>();
        serde_json::Value::Object(filtered)
    }

    fn resolve_provider(
        provider_kind: &str,
        api_key_override: Option<&str>,
        base_url_override: Option<&str>,
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
        Ok(ResolvedProviderRuntime {
            api_key,
            base_url: resolve_runtime_provider_base_url(
                runtime_profile.base_url.allow_private_network,
                base_url,
            ),
            runtime: runtime_profile.runtime,
        })
    }
}

fn resolve_runtime_profile(
    provider_kind: &str,
    extra_parameters_json: &serde_json::Value,
) -> Result<RuntimeProviderProfileEnvelope> {
    if let Some(value) = extra_parameters_json.get("_providerProfile") {
        let profile = serde_json::from_value::<RuntimeProviderProfileEnvelope>(value.clone())
            .with_context(|| {
                format!("invalid runtime provider profile for provider={provider_kind}")
            })?;
        if profile.runtime.kind != OPENAI_COMPATIBLE_RUNTIME_KIND {
            return Err(anyhow!("unsupported provider runtime kind for provider={provider_kind}"));
        }
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

fn provider_endpoint_url(provider_kind: &str, base_url: &str, path: &str) -> Result<Url> {
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
    if !policy.allow_private_network && is_private_runtime_url(&url) {
        return Err(anyhow!(
            "provider base URL must not target a private, loopback, or link-local network for provider={provider_kind}"
        ));
    }
    Ok(())
}

fn is_private_runtime_url(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(host)) => is_private_runtime_ip(IpAddr::V4(host)),
        Some(url::Host::Ipv6(host)) => is_private_runtime_ip(IpAddr::V6(host)),
        None => false,
    }
}

fn is_private_runtime_ip(ip: IpAddr) -> bool {
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

fn apply_provider_auth(
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
    }
}

/// Parsed fields extracted from a non-streaming OpenAI-compatible
/// chat-completions response: `(output_text, tool_calls, finish_reason,
/// usage_json, reasoning_content)`. Bundled as a tuple alias to keep the
/// parser signature legible at the call sites.
type ParsedToolUseResponse =
    (String, Vec<ChatToolCall>, Option<String>, serde_json::Value, Option<String>);

fn parse_tool_use_response(
    body: &serde_json::Value,
) -> std::result::Result<ParsedToolUseResponse, ProviderCallError> {
    let choice =
        body.get("choices").and_then(|v| v.as_array()).and_then(|arr| arr.first()).ok_or_else(
            || ProviderCallError::protocol("tool-use response missing choices array"),
        )?;

    let message = choice
        .get("message")
        .ok_or_else(|| ProviderCallError::protocol("tool-use response choice missing message"))?;
    let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str()).map(str::to_string);

    let output_text = message.get("content").map(extract_message_content_text).unwrap_or_default();
    let reasoning_content = message
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let tool_calls = message
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|calls| {
            calls
                .iter()
                .filter_map(|raw| {
                    let id = raw.get("id").and_then(|v| v.as_str())?.to_string();
                    let function = raw.get("function")?;
                    let name = function.get("name").and_then(|v| v.as_str())?.to_string();
                    let arguments = function
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or_else(|| function.get("arguments").map(|v| v.to_string()))
                        .unwrap_or_default();
                    Some(ChatToolCall { id, name, arguments_json: arguments })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let usage_json = body.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));
    Ok((output_text, tool_calls, finish_reason, usage_json, reasoning_content))
}

fn provider_response_format(
    provider_kind: &str,
    requested: Option<&serde_json::Value>,
    mode: ProviderStructuredOutputMode,
) -> Result<Option<serde_json::Value>> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    match mode {
        ProviderStructuredOutputMode::JsonSchema => Ok(Some(requested.clone())),
        ProviderStructuredOutputMode::JsonObject => {
            Ok(Some(serde_json::json!({ "type": "json_object" })))
        }
        ProviderStructuredOutputMode::Unsupported => {
            Err(anyhow!("provider {provider_kind} does not support required structured output"))
        }
    }
}

fn provider_system_prompt(
    provider_kind: &str,
    requested_system_prompt: Option<&str>,
    requested_response_format: Option<&serde_json::Value>,
    mode: ProviderStructuredOutputMode,
) -> Result<Option<String>> {
    let Some(requested_response_format) = requested_response_format else {
        return Ok(requested_system_prompt.map(ToOwned::to_owned));
    };
    if mode != ProviderStructuredOutputMode::JsonObject {
        return Ok(requested_system_prompt.map(ToOwned::to_owned));
    }

    let schema = requested_response_format.pointer("/json_schema/schema").ok_or_else(|| {
        anyhow!("provider {provider_kind} json_object mode requires a JSON schema")
    })?;
    let schema_json = serde_json::to_string(schema).with_context(|| {
        format!("failed to serialize structured output schema for provider={provider_kind}")
    })?;

    let mut system_prompt = requested_system_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(String::new, ToOwned::to_owned);
    if !system_prompt.is_empty() {
        system_prompt.push_str("\n\n");
    }
    system_prompt.push_str(
        "For runtimes that accept JSON object mode, this JSON Schema is the canonical output \
contract. Return exactly one JSON object that conforms to it. Use only the field names defined by \
this schema; do not invent alternate keys.\nJSON Schema:\n",
    );
    system_prompt.push_str(&schema_json);

    Ok(Some(system_prompt))
}

#[async_trait]
impl LlmGateway for UnifiedGateway {
    async fn generate(&self, request: ChatRequest) -> Result<ChatResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;
        let upstream_extra = Self::upstream_extra_parameters(&request.extra_parameters_json);
        let response_format = provider_response_format(
            &request.provider_kind,
            request.response_format.as_ref(),
            resolved.runtime.structured_output,
        )?;
        let system_prompt = provider_system_prompt(
            &request.provider_kind,
            request.system_prompt.as_deref(),
            request.response_format.as_ref(),
            resolved.runtime.structured_output,
        )?;
        let (output_text, usage_json) = self
            .call_openai_compatible(OpenAiCompatibleRequest {
                provider_kind: &request.provider_kind,
                api_key: resolved.api_key.as_deref(),
                base_url: resolved.base_url.as_str(),
                auth_scheme: resolved.runtime.auth_scheme,
                chat_path: resolved.runtime.chat_path.clone(),
                model_name: &request.model_name,
                messages: vec![OpenAiCompatibleMessage {
                    role: "user".to_string(),
                    content: OpenAiCompatibleMessageContent::Text(request.prompt.clone()),
                }],
                system_prompt: system_prompt.as_deref(),
                temperature: request.temperature,
                top_p: request.top_p,
                max_output_tokens: request.max_output_tokens_override,
                token_limit_parameter: resolved.runtime.token_limit_parameter,
                response_format: response_format.as_ref(),
                extra_parameters_json: &upstream_extra,
                stream: false,
            })
            .await?;
        Ok(ChatResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            output_text,
            usage_json,
        })
    }

    async fn generate_stream(
        &self,
        request: ChatRequest,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ChatResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;
        let upstream_extra = Self::upstream_extra_parameters(&request.extra_parameters_json);
        let response_format = provider_response_format(
            &request.provider_kind,
            request.response_format.as_ref(),
            resolved.runtime.structured_output,
        )?;
        let system_prompt = provider_system_prompt(
            &request.provider_kind,
            request.system_prompt.as_deref(),
            request.response_format.as_ref(),
            resolved.runtime.structured_output,
        )?;
        let (output_text, usage_json) = self
            .call_openai_compatible_stream(
                OpenAiCompatibleRequest {
                    provider_kind: &request.provider_kind,
                    api_key: resolved.api_key.as_deref(),
                    base_url: resolved.base_url.as_str(),
                    auth_scheme: resolved.runtime.auth_scheme,
                    chat_path: resolved.runtime.chat_path.clone(),
                    model_name: &request.model_name,
                    messages: vec![OpenAiCompatibleMessage {
                        role: "user".to_string(),
                        content: OpenAiCompatibleMessageContent::Text(request.prompt.clone()),
                    }],
                    system_prompt: system_prompt.as_deref(),
                    temperature: request.temperature,
                    top_p: request.top_p,
                    max_output_tokens: request.max_output_tokens_override,
                    token_limit_parameter: resolved.runtime.token_limit_parameter,
                    response_format: response_format.as_ref(),
                    extra_parameters_json: &upstream_extra,
                    stream: true,
                },
                on_delta,
            )
            .await?;
        Ok(ChatResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            output_text,
            usage_json,
        })
    }

    async fn generate_with_tools(&self, request: ToolUseRequest) -> Result<ToolUseResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;

        let messages =
            request.messages.iter().map(OpenAiCompatibleToolUseMessage::from).collect::<Vec<_>>();
        let tools = request.tools.iter().map(OpenAiCompatibleToolDef::from).collect::<Vec<_>>();
        let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
            resolved.runtime.token_limit_parameter,
            request.max_output_tokens_override,
        );
        let upstream_extra = Self::upstream_extra_parameters(&request.extra_parameters_json);
        // Some reasoning models (DeepSeek `*-pro` / `*-reasoner` family,
        // OpenAI `o*` series) reject `tool_choice` overrides. They only
        // accept the implicit "auto", so coercing them into "required"
        // returns 400 with `does not support this tool_choice`. Drop the
        // override for those families and rely on the system prompt to
        // push them through `grounded_answer`.
        let model_lc = request.model_name.to_ascii_lowercase();
        let provider_lc = request.provider_kind.to_ascii_lowercase();
        // DeepSeek's hosted API exposes every `deepseek-v4-*` model
        // through the reasoner backend (verified empirically: v4-flash,
        // v4-pro, and the `*-reasoner` aliases all return
        // `deepseek-reasoner does not support this tool_choice` when
        // sent `tool_choice="required"`). Treat the entire DeepSeek v4
        // family as reasoners; OpenAI/Qwen/etc. only need the
        // o-series + explicit reasoner suffix detection.
        let is_reasoner = model_lc.contains("reasoner")
            || model_lc.starts_with("o1")
            || model_lc.starts_with("o3")
            || model_lc.starts_with("o4")
            || (provider_lc == "deepseek" && model_lc.contains("v4"));
        let tool_choice = if tools.is_empty() {
            None
        } else if request.require_tool_call && !is_reasoner {
            Some("required")
        } else {
            Some("auto")
        };

        let payload = OpenAiCompatibleToolUseChatRequest {
            model: &request.model_name,
            messages,
            tools,
            temperature: request.temperature,
            top_p: request.top_p,
            max_completion_tokens,
            max_tokens,
            tool_choice,
            stream: false,
            extra: upstream_extra,
        };
        let request_body =
            serde_json::to_vec(&payload).context("failed to serialize tool-use request body")?;

        let endpoint_url = provider_endpoint_url(
            &request.provider_kind,
            &resolved.base_url,
            &resolved.runtime.chat_path,
        )?;

        let (output_text, tool_calls, finish_reason, usage_json, reasoning_content) = with_retry(
            || async {
                let request_builder = self
                    .client
                    .post(endpoint_url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json");
                let request_builder = apply_provider_auth(
                    request_builder,
                    resolved.runtime.auth_scheme,
                    resolved.api_key.as_deref(),
                );
                let request_builder = crate::observability::inject_trace_context(request_builder);
                let response =
                    request_builder.body(request_body.clone()).send().await.map_err(|source| {
                        ProviderCallError::transport(
                            format!(
                                "tool-use transport failed: provider={}",
                                request.provider_kind
                            ),
                            source,
                        )
                    })?;

                let status = response.status();
                let headers = response.headers().clone();
                let body_text = response.text().await.map_err(|source| {
                    ProviderCallError::response_body(
                        format!(
                            "failed to read tool-use response body: provider={}",
                            request.provider_kind
                        ),
                        source,
                    )
                })?;
                if !status.is_success() {
                    return Err(provider_http_status_error(
                        &request.provider_kind,
                        status,
                        &headers,
                        &body_text,
                    ));
                }

                let body =
                    serde_json::from_str::<serde_json::Value>(&body_text).map_err(|source| {
                        ProviderCallError::json(
                            format!(
                                "failed to parse tool-use response from provider {}",
                                request.provider_kind
                            ),
                            source,
                        )
                    })?;

                parse_tool_use_response(&body)
            },
            RetryPolicy::default(),
        )
        .await?;

        Ok(ToolUseResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            output_text,
            tool_calls,
            finish_reason,
            usage_json,
            reasoning_content,
        })
    }

    async fn generate_with_tools_stream(
        &self,
        request: ToolUseRequest,
        on_text_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ToolUseResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;

        let messages =
            request.messages.iter().map(OpenAiCompatibleToolUseMessage::from).collect::<Vec<_>>();
        let tools = request.tools.iter().map(OpenAiCompatibleToolDef::from).collect::<Vec<_>>();
        let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
            resolved.runtime.token_limit_parameter,
            request.max_output_tokens_override,
        );
        let upstream_extra = Self::upstream_extra_parameters(&request.extra_parameters_json);
        // Some reasoning models (DeepSeek `*-pro` / `*-reasoner` family,
        // OpenAI `o*` series) reject `tool_choice` overrides. They only
        // accept the implicit "auto", so coercing them into "required"
        // returns 400 with `does not support this tool_choice`. Drop the
        // override for those families and rely on the system prompt to
        // push them through `grounded_answer`.
        let model_lc = request.model_name.to_ascii_lowercase();
        let provider_lc = request.provider_kind.to_ascii_lowercase();
        // DeepSeek's hosted API exposes every `deepseek-v4-*` model
        // through the reasoner backend (verified empirically: v4-flash,
        // v4-pro, and the `*-reasoner` aliases all return
        // `deepseek-reasoner does not support this tool_choice` when
        // sent `tool_choice="required"`). Treat the entire DeepSeek v4
        // family as reasoners; OpenAI/Qwen/etc. only need the
        // o-series + explicit reasoner suffix detection.
        let is_reasoner = model_lc.contains("reasoner")
            || model_lc.starts_with("o1")
            || model_lc.starts_with("o3")
            || model_lc.starts_with("o4")
            || (provider_lc == "deepseek" && model_lc.contains("v4"));
        let tool_choice = if tools.is_empty() {
            None
        } else if request.require_tool_call && !is_reasoner {
            Some("required")
        } else {
            Some("auto")
        };

        let payload = OpenAiCompatibleToolUseChatRequest {
            model: &request.model_name,
            messages,
            tools,
            temperature: request.temperature,
            top_p: request.top_p,
            max_completion_tokens,
            max_tokens,
            tool_choice,
            stream: true,
            extra: upstream_extra,
        };
        let request_body = serde_json::to_vec(&payload)
            .context("failed to serialize streaming tool-use request body")?;

        let endpoint_url = provider_endpoint_url(
            &request.provider_kind,
            &resolved.base_url,
            &resolved.runtime.chat_path,
        )?;

        let response = with_retry(
            || async {
                let request_builder = self
                    .client
                    .post(endpoint_url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "text/event-stream");
                let request_builder = apply_provider_auth(
                    request_builder,
                    resolved.runtime.auth_scheme,
                    resolved.api_key.as_deref(),
                );
                let request_builder = crate::observability::inject_trace_context(request_builder);
                let response =
                    request_builder.body(request_body.clone()).send().await.map_err(|source| {
                        ProviderCallError::transport(
                            format!(
                                "tool-use stream transport failed: provider={}",
                                request.provider_kind
                            ),
                            source,
                        )
                    })?;

                let status = response.status();
                if status.is_success() {
                    return Ok(response);
                }

                let headers = response.headers().clone();
                let body_text = response.text().await.unwrap_or_default();
                Err(provider_http_status_error(
                    &request.provider_kind,
                    status,
                    &headers,
                    &body_text,
                ))
            },
            RetryPolicy::default(),
        )
        .await?;

        let stream_state = drain_tool_use_stream(response, on_text_delta).await?;
        let (output_text, finish_reason, usage_json, tool_calls) = stream_state.finalize();
        Ok(ToolUseResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            output_text,
            tool_calls,
            finish_reason,
            usage_json,
            // Streaming path does not yet capture `reasoning_content`.
            // The non-streaming gateway is the canonical path for the
            // UI MCP-agent loop (run_mcp_agent_turn → generate_with_tools),
            // and streaming is reserved for direct provider passthroughs
            // that do not echo reasoning back to the model.
            reasoning_content: None,
        })
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;
        let embeddings_path = resolved.runtime.embeddings_path.as_deref().ok_or_else(|| {
            anyhow!("provider {} does not support embeddings", request.provider_kind)
        })?;

        let endpoint_url =
            provider_endpoint_url(&request.provider_kind, &resolved.base_url, embeddings_path)?;

        let body = with_retry(
            || async {
                let request_builder = self.client.post(endpoint_url.clone());
                let request_builder = apply_provider_auth(
                    request_builder,
                    resolved.runtime.auth_scheme,
                    resolved.api_key.as_deref(),
                );
                let request_builder = crate::observability::inject_trace_context(request_builder);
                let response = request_builder
                    .json(&Self::embedding_request_body(
                        &request.model_name,
                        serde_json::Value::String(request.input.clone()),
                        &request.extra_parameters_json,
                    ))
                    .send()
                    .await
                    .map_err(|source| {
                        ProviderCallError::transport(
                            format!(
                                "embedding transport failed: provider={}",
                                request.provider_kind
                            ),
                            source,
                        )
                    })?;

                let status = response.status();
                let headers = response.headers().clone();
                let body_text = response.text().await.map_err(|source| {
                    ProviderCallError::response_body(
                        format!(
                            "failed to read embedding response body: provider={}",
                            request.provider_kind
                        ),
                        source,
                    )
                })?;
                if !status.is_success() {
                    return Err(provider_http_status_error(
                        &request.provider_kind,
                        status,
                        &headers,
                        &body_text,
                    ));
                }
                serde_json::from_str::<serde_json::Value>(&body_text).map_err(|source| {
                    ProviderCallError::json(
                        format!(
                            "failed to parse embedding response from provider {}",
                            request.provider_kind
                        ),
                        source,
                    )
                })
            },
            RetryPolicy::default(),
        )
        .await?;

        let embedding = body
            .get("data")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("embedding"))
            .map(Self::parse_embedding_vector)
            .unwrap_or_default();

        let usage_json = body.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));

        Ok(EmbeddingResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            dimensions: embedding.len(),
            embedding,
            usage_json,
        })
    }

    async fn embed_many(&self, request: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse> {
        if request.inputs.is_empty() {
            return Ok(EmbeddingBatchResponse {
                provider_kind: request.provider_kind,
                model_name: request.model_name,
                dimensions: 0,
                embeddings: Vec::new(),
                usage_json: serde_json::json!({}),
            });
        }

        if request.inputs.len() == 1 {
            let response = self
                .embed(EmbeddingRequest {
                    provider_kind: request.provider_kind.clone(),
                    model_name: request.model_name.clone(),
                    input: request.inputs[0].clone(),
                    api_key_override: request.api_key_override.clone(),
                    base_url_override: request.base_url_override.clone(),
                    extra_parameters_json: request.extra_parameters_json.clone(),
                })
                .await?;
            return Ok(EmbeddingBatchResponse {
                provider_kind: response.provider_kind,
                model_name: response.model_name,
                dimensions: response.dimensions,
                embeddings: vec![response.embedding],
                usage_json: response.usage_json,
            });
        }

        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;
        let embeddings_path = resolved.runtime.embeddings_path.as_deref().ok_or_else(|| {
            anyhow!("provider {} does not support embeddings", request.provider_kind)
        })?;
        let endpoint_url =
            provider_endpoint_url(&request.provider_kind, &resolved.base_url, embeddings_path)?;

        let body = with_retry(
            || async {
                let request_builder = self.client.post(endpoint_url.clone());
                let request_builder = apply_provider_auth(
                    request_builder,
                    resolved.runtime.auth_scheme,
                    resolved.api_key.as_deref(),
                );
                let request_builder = crate::observability::inject_trace_context(request_builder);
                let response = request_builder
                    .json(&Self::embedding_request_body(
                        &request.model_name,
                        serde_json::json!(request.inputs.clone()),
                        &request.extra_parameters_json,
                    ))
                    .send()
                    .await
                    .map_err(|source| {
                        ProviderCallError::transport(
                            format!(
                                "embedding batch transport failed: provider={}",
                                request.provider_kind
                            ),
                            source,
                        )
                    })?;

                let status = response.status();
                let headers = response.headers().clone();
                let body_text = response.text().await.map_err(|source| {
                    ProviderCallError::response_body(
                        format!(
                            "failed to read embedding batch response body: provider={}",
                            request.provider_kind
                        ),
                        source,
                    )
                })?;
                if !status.is_success() {
                    return Err(provider_http_status_error(
                        &request.provider_kind,
                        status,
                        &headers,
                        &body_text,
                    ));
                }
                serde_json::from_str::<serde_json::Value>(&body_text).map_err(|source| {
                    ProviderCallError::json(
                        format!(
                            "failed to parse embedding batch response from provider {}",
                            request.provider_kind
                        ),
                        source,
                    )
                })
            },
            RetryPolicy::default(),
        )
        .await?;

        let embeddings = body
            .get("data")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.get("embedding").map(Self::parse_embedding_vector).unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let dimensions = embeddings.first().map(Vec::len).unwrap_or_default();
        let usage_json = body.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));

        Ok(EmbeddingBatchResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            dimensions,
            embeddings,
            usage_json,
        })
    }

    async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;
        let upstream_extra = Self::upstream_extra_parameters(&request.extra_parameters_json);
        let image_data_url = format!(
            "data:{};base64,{}",
            request.mime_type,
            BASE64_STANDARD.encode(&request.image_bytes)
        );
        let (output_text, usage_json) = self
            .call_openai_compatible(OpenAiCompatibleRequest {
                provider_kind: &request.provider_kind,
                api_key: resolved.api_key.as_deref(),
                base_url: resolved.base_url.as_str(),
                auth_scheme: resolved.runtime.auth_scheme,
                chat_path: resolved.runtime.chat_path.clone(),
                model_name: &request.model_name,
                messages: vec![OpenAiCompatibleMessage {
                    role: "user".to_string(),
                    content: OpenAiCompatibleMessageContent::Parts(vec![
                        OpenAiCompatibleContentPart::Text { text: request.prompt.clone() },
                        OpenAiCompatibleContentPart::ImageUrl {
                            image_url: OpenAiCompatibleImageUrl { url: image_data_url },
                        },
                    ]),
                }],
                system_prompt: request.system_prompt.as_deref(),
                temperature: request.temperature,
                top_p: request.top_p,
                max_output_tokens: request.max_output_tokens_override,
                token_limit_parameter: resolved.runtime.token_limit_parameter,
                response_format: None,
                extra_parameters_json: &upstream_extra,
                stream: false,
            })
            .await?;

        Ok(VisionResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            output_text,
            usage_json,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChatToolDef, OpenAiCompatibleMessage, OpenAiCompatibleMessageContent,
        OpenAiCompatibleRequest, OpenAiCompatibleToolDef, OpenAiCompatibleToolUseChatRequest,
        ProviderAuthScheme, ProviderStructuredOutputMode, ProviderTokenLimitParameter,
        UnifiedGateway, consume_openai_compatible_stream_frame, extract_message_content_text,
        provider_response_format, provider_system_prompt,
    };

    #[test]
    fn extracts_plain_string_content() {
        let value = serde_json::json!("ok");
        assert_eq!(extract_message_content_text(&value), "ok");
    }

    #[test]
    fn extracts_text_from_content_parts() {
        let value = serde_json::json!([
            {"type": "text", "text": "hello"},
            {"type": "text", "text": {"value": "world"}}
        ]);
        assert_eq!(extract_message_content_text(&value), "hello\nworld");
    }

    #[test]
    fn serializes_openai_compatible_chat_request_as_valid_json() {
        let body = OpenAiCompatibleRequest {
            provider_kind: "openai",
            api_key: Some("test"),
            base_url: "https://api.openai.com/v1",
            auth_scheme: ProviderAuthScheme::Bearer,
            chat_path: "/chat/completions".to_string(),
            model_name: "gpt-5.4-mini",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            token_limit_parameter: ProviderTokenLimitParameter::MaxCompletionTokens,
            response_format: None,
            extra_parameters_json: &serde_json::json!({}),
            stream: false,
        }
        .body()
        .expect("request body should serialize");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("serialized body should stay valid json");
        assert_eq!(value.get("model").and_then(serde_json::Value::as_str), Some("gpt-5.4-mini"));
        assert_eq!(
            value
                .get("messages")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("content"))
                .and_then(serde_json::Value::as_str),
            Some("hello"),
        );
    }

    #[test]
    fn serializes_response_format_when_schema_is_requested() {
        let body = OpenAiCompatibleRequest {
            provider_kind: "openai",
            api_key: Some("test"),
            base_url: "https://api.openai.com/v1",
            auth_scheme: ProviderAuthScheme::Bearer,
            chat_path: "/chat/completions".to_string(),
            model_name: "gpt-5.4-mini",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            token_limit_parameter: ProviderTokenLimitParameter::MaxCompletionTokens,
            response_format: Some(&serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "graph_extraction",
                    "strict": true,
                    "schema": {"type": "object"}
                }
            })),
            extra_parameters_json: &serde_json::json!({}),
            stream: false,
        }
        .body()
        .expect("request body should serialize");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("serialized body should stay valid json");
        assert_eq!(
            value
                .get("response_format")
                .and_then(|item| item.get("type"))
                .and_then(serde_json::Value::as_str),
            Some("json_schema"),
        );
    }

    #[test]
    fn tool_use_request_omits_empty_tools_and_choice() {
        let payload = OpenAiCompatibleToolUseChatRequest {
            model: "provider-alpha-tool-model",
            messages: vec![],
            tools: vec![],
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
            max_tokens: Some(16),
            tool_choice: None,
            stream: false,
            extra: serde_json::json!({}),
        };
        let value =
            serde_json::to_value(payload).expect("tool-use request should serialize to JSON");

        assert!(value.get("tools").is_none());
        assert!(value.get("tool_choice").is_none());
    }

    #[test]
    fn tool_use_request_includes_tools_and_choice_when_present() {
        let def = ChatToolDef {
            name: "lookup".to_string(),
            description: "Lookup structured facts".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"}
                },
                "required": ["id"]
            }),
        };
        let payload = OpenAiCompatibleToolUseChatRequest {
            model: "provider-alpha-tool-model",
            messages: vec![],
            tools: vec![OpenAiCompatibleToolDef::from(&def)],
            temperature: None,
            top_p: None,
            max_completion_tokens: Some(16),
            max_tokens: None,
            tool_choice: Some("auto"),
            stream: false,
            extra: serde_json::json!({}),
        };
        let value =
            serde_json::to_value(payload).expect("tool-use request should serialize to JSON");

        assert_eq!(value.get("tools").and_then(serde_json::Value::as_array).map(Vec::len), Some(1));
        assert_eq!(value.get("tool_choice").and_then(serde_json::Value::as_str), Some("auto"));
    }

    #[test]
    fn json_object_runtime_lowers_structured_response_format() {
        let requested = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "query_ir",
                "strict": true,
                "schema": {"type": "object"}
            }
        });
        let lowered = provider_response_format(
            "provider-alpha",
            Some(&requested),
            ProviderStructuredOutputMode::JsonObject,
        )
        .expect("json_object providers should lower structured output")
        .expect("requested structured output should remain present");

        assert_eq!(lowered.get("type").and_then(serde_json::Value::as_str), Some("json_object"));
        assert!(lowered.get("json_schema").is_none());
    }

    #[test]
    fn json_schema_runtime_keeps_structured_system_prompt_unchanged() {
        let requested = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "query_ir",
                "strict": true,
                "schema": {"type": "object", "properties": {"target_entities": {"type": "array"}}}
            }
        });
        let system_prompt = provider_system_prompt(
            "provider-alpha",
            Some("Base compiler prompt"),
            Some(&requested),
            ProviderStructuredOutputMode::JsonSchema,
        )
        .expect("json_schema prompt should remain valid")
        .expect("prompt should remain present");

        assert_eq!(system_prompt, "Base compiler prompt");
    }

    #[test]
    fn json_object_runtime_injects_schema_into_system_prompt() {
        let requested = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "query_ir",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "target_entities": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": {"type": "string"},
                                    "role": {"type": "string"}
                                }
                            }
                        }
                    }
                }
            }
        });
        let system_prompt = provider_system_prompt(
            "provider-alpha",
            Some("Base compiler prompt"),
            Some(&requested),
            ProviderStructuredOutputMode::JsonObject,
        )
        .expect("json_object prompt should be built")
        .expect("prompt should remain present");

        assert!(system_prompt.starts_with("Base compiler prompt\n\n"));
        assert!(system_prompt.contains("JSON Schema:"));
        assert!(system_prompt.contains("\"target_entities\""));
        assert!(system_prompt.contains("\"label\""));
        assert!(system_prompt.contains("\"role\""));
    }

    #[test]
    fn json_object_runtime_requires_canonical_schema_for_prompt_injection() {
        let requested = serde_json::json!({"type": "json_schema"});
        let error = provider_system_prompt(
            "provider-alpha",
            Some("Base compiler prompt"),
            Some(&requested),
            ProviderStructuredOutputMode::JsonObject,
        )
        .expect_err("json_object structured output without schema must fail loud");

        assert!(error.to_string().contains("requires a JSON schema"));
    }

    #[test]
    fn unsupported_structured_output_fails_loud() {
        let requested = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "query_ir",
                "strict": true,
                "schema": {"type": "object"}
            }
        });
        let error = provider_response_format(
            "unsupported-provider",
            Some(&requested),
            ProviderStructuredOutputMode::Unsupported,
        )
        .expect_err("unsupported structured output must fail loud");

        assert!(error.to_string().contains("does not support required structured output"));
    }

    #[test]
    fn serializes_openai_token_limit_as_max_completion_tokens() {
        let body = OpenAiCompatibleRequest {
            provider_kind: "openai",
            api_key: Some("test"),
            base_url: "https://api.openai.com/v1",
            auth_scheme: ProviderAuthScheme::Bearer,
            chat_path: "/chat/completions".to_string(),
            model_name: "gpt-5.4-mini",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: Some(16),
            token_limit_parameter: ProviderTokenLimitParameter::MaxCompletionTokens,
            response_format: None,
            extra_parameters_json: &serde_json::json!({}),
            stream: false,
        }
        .body()
        .expect("request body should serialize");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("serialized body should stay valid json");
        assert_eq!(
            value.get("max_completion_tokens").and_then(serde_json::Value::as_i64),
            Some(16),
        );
        assert!(value.get("max_tokens").is_none());
    }

    #[test]
    fn serializes_non_openai_token_limit_as_max_tokens() {
        let body = OpenAiCompatibleRequest {
            provider_kind: "deepseek",
            api_key: Some("test"),
            base_url: "https://example.invalid/v1",
            auth_scheme: ProviderAuthScheme::Bearer,
            chat_path: "/chat/completions".to_string(),
            model_name: "deepseek-chat",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: Some(16),
            token_limit_parameter: ProviderTokenLimitParameter::MaxTokens,
            response_format: None,
            extra_parameters_json: &serde_json::json!({}),
            stream: false,
        }
        .body()
        .expect("request body should serialize");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("serialized body should stay valid json");
        assert_eq!(value.get("max_tokens").and_then(serde_json::Value::as_i64), Some(16),);
        assert!(value.get("max_completion_tokens").is_none());
    }

    #[test]
    fn allows_ollama_provider_without_api_key() {
        let provider_profile = serde_json::json!({
            "runtime": {
                "kind": "openai_compatible",
                "authScheme": "bearer",
                "tokenLimitParameter": "max_tokens",
                "structuredOutput": "json_schema",
                "chatPath": "/chat/completions",
                "embeddingsPath": "/embeddings",
                "modelsPath": "/models"
            },
            "baseUrl": {
                "allowOverride": true,
                "requireHttps": false,
                "allowPrivateNetwork": true,
                "trimSuffixes": ["/v1"]
            },
            "credentials": {
                "apiKeyRequired": false,
                "baseUrlRequired": true,
                "baseUrlMode": "required",
                "validationMode": "model_list"
            }
        });
        let extra_parameters_json = serde_json::json!({
            "_providerProfile": provider_profile,
        });

        let resolved = UnifiedGateway::resolve_provider(
            "ollama",
            None,
            Some("http://localhost:11434/v1"),
            &extra_parameters_json,
        )
        .expect("ollama should resolve without token");
        assert!(resolved.api_key.is_none());
        assert_eq!(resolved.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn resolves_raw_authorization_runtime_profile() {
        let resolved = UnifiedGateway::resolve_provider(
            "synthetic-router",
            Some("plain-secret"),
            Some("https://router.example/v1"),
            &serde_json::json!({
                "_providerProfile": {
                    "runtime": {
                        "kind": "openai_compatible",
                        "authScheme": "raw_authorization",
                        "tokenLimitParameter": "max_tokens",
                        "structuredOutput": "json_schema",
                        "chatPath": "/chat/completions",
                        "embeddingsPath": null,
                        "modelsPath": "/models"
                    },
                    "baseUrl": {
                        "allowOverride": false,
                        "requireHttps": true,
                        "allowPrivateNetwork": false,
                        "trimSuffixes": []
                    },
                    "credentials": {
                        "apiKeyRequired": true,
                        "baseUrlRequired": false,
                        "baseUrlMode": "fixed",
                        "validationMode": "model_list"
                    }
                }
            }),
        )
        .expect("raw authorization profile should resolve");
        assert_eq!(resolved.api_key.as_deref(), Some("plain-secret"));
        assert_eq!(resolved.runtime.auth_scheme, ProviderAuthScheme::RawAuthorization);
        assert_eq!(resolved.base_url, "https://router.example/v1");
    }

    #[test]
    fn runtime_profile_is_required() {
        let error = UnifiedGateway::resolve_provider(
            "synthetic-router",
            Some("plain-secret"),
            Some("https://router.example/v1"),
            &serde_json::json!({}),
        )
        .expect_err("runtime must be catalog-profile driven");
        assert!(error.to_string().contains("missing runtime provider profile"));
    }

    #[test]
    fn runtime_profile_rejects_incomplete_provider_profile() {
        let error = UnifiedGateway::resolve_provider(
            "synthetic-router",
            Some("plain-secret"),
            Some("https://router.example/v1"),
            &serde_json::json!({
                "_providerProfile": {
                    "runtime": {
                        "kind": "openai_compatible",
                        "authScheme": "bearer"
                    }
                }
            }),
        )
        .expect_err("runtime profile must be the full canonical shape");

        assert!(error.to_string().contains("invalid runtime provider profile"));
    }

    #[test]
    fn runtime_profile_rejects_unsupported_runtime_kind() {
        let error = UnifiedGateway::resolve_provider(
            "synthetic-router",
            Some("plain-secret"),
            Some("https://router.example/v1"),
            &serde_json::json!({
                "_providerProfile": {
                    "runtime": {
                        "kind": "unsupported_runtime",
                        "authScheme": "bearer",
                        "tokenLimitParameter": "max_tokens",
                        "structuredOutput": "json_schema",
                        "chatPath": "/chat/completions",
                        "embeddingsPath": "/embeddings",
                        "modelsPath": "/models"
                    },
                    "baseUrl": {
                        "allowOverride": false,
                        "requireHttps": true,
                        "allowPrivateNetwork": false,
                        "trimSuffixes": []
                    },
                    "credentials": {
                        "apiKeyRequired": true,
                        "baseUrlRequired": false,
                        "baseUrlMode": "fixed",
                        "validationMode": "model_list"
                    }
                }
            }),
        )
        .expect_err("runtime kind must stay canonical");

        assert!(error.to_string().contains("unsupported provider runtime kind"));
    }

    #[test]
    fn runtime_profile_rejects_private_hosted_base_url() {
        let error = UnifiedGateway::resolve_provider(
            "synthetic-router",
            Some("plain-secret"),
            Some("https://127.0.0.1/v1"),
            &serde_json::json!({
                "_providerProfile": {
                    "runtime": {
                        "kind": "openai_compatible",
                        "authScheme": "bearer",
                        "tokenLimitParameter": "max_tokens",
                        "structuredOutput": "json_schema",
                        "chatPath": "/chat/completions",
                        "embeddingsPath": null,
                        "modelsPath": "/models"
                    },
                    "baseUrl": {
                        "allowOverride": false,
                        "requireHttps": true,
                        "allowPrivateNetwork": false,
                        "trimSuffixes": []
                    },
                    "credentials": {
                        "apiKeyRequired": true,
                        "baseUrlRequired": false,
                        "baseUrlMode": "fixed",
                        "validationMode": "model_list"
                    }
                }
            }),
        )
        .expect_err("hosted runtime must reject stale private base URLs");
        assert!(error.to_string().contains("private"));
    }

    #[test]
    fn runtime_profile_rejects_non_http_base_url() {
        let error = UnifiedGateway::resolve_provider(
            "synthetic-router",
            Some("plain-secret"),
            Some("file:///tmp/provider.sock"),
            &serde_json::json!({
                "_providerProfile": {
                    "runtime": {
                        "kind": "openai_compatible",
                        "authScheme": "bearer",
                        "tokenLimitParameter": "max_tokens",
                        "structuredOutput": "json_schema",
                        "chatPath": "/chat/completions",
                        "embeddingsPath": null,
                        "modelsPath": "/models"
                    },
                    "baseUrl": {
                        "allowOverride": false,
                        "requireHttps": true,
                        "allowPrivateNetwork": false,
                        "trimSuffixes": []
                    },
                    "credentials": {
                        "apiKeyRequired": true,
                        "baseUrlRequired": false,
                        "baseUrlMode": "fixed",
                        "validationMode": "model_list"
                    }
                }
            }),
        )
        .expect_err("runtime must reject non-http provider URLs");
        assert!(error.to_string().contains("http or https"));
    }

    #[test]
    fn embedding_request_body_includes_extra_parameters_without_overriding_core_fields() {
        let body = UnifiedGateway::embedding_request_body(
            "text-embedding-3-large",
            serde_json::json!(["alpha", "beta"]),
            &serde_json::json!({
                "dimensions": 1024,
                "encoding_format": "float",
                "model": "ignored",
                "input": "ignored",
                "_providerProfile": {"runtime": {"authScheme": "bearer"}}
            }),
        );

        assert_eq!(
            body.get("model").and_then(serde_json::Value::as_str),
            Some("text-embedding-3-large")
        );
        assert_eq!(body.get("input"), Some(&serde_json::json!(["alpha", "beta"])));
        assert_eq!(body.get("dimensions").and_then(serde_json::Value::as_i64), Some(1024));
        assert_eq!(body.get("encoding_format").and_then(serde_json::Value::as_str), Some("float"));
        assert!(body.get("_providerProfile").is_none());
    }

    #[test]
    fn consumes_stream_delta_frames() {
        let mut output_text = String::new();
        let mut usage_json = serde_json::json!({});
        let mut emitted = String::new();
        let done = consume_openai_compatible_stream_frame(
            r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#,
            &mut output_text,
            &mut usage_json,
            &mut |delta| emitted.push_str(&delta),
        )
        .expect("stream frame should parse");
        assert!(!done);
        assert_eq!(output_text, "Hello");
        assert_eq!(emitted, "Hello");
        assert_eq!(usage_json, serde_json::json!({}));
    }

    #[test]
    fn marks_done_for_done_frame() {
        let mut output_text = String::new();
        let mut usage_json = serde_json::json!({});
        let done = consume_openai_compatible_stream_frame(
            "data: [DONE]",
            &mut output_text,
            &mut usage_json,
            &mut |_delta| {},
        )
        .expect("done frame should parse");
        assert!(done);
    }
}
