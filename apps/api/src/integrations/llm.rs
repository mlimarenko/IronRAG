use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroize as _;

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
        ProviderCredentialPolicy, ProviderRequestPolicy, ProviderRuntimeProfile,
        ProviderSamplingPolicy, ProviderStructuredOutputMode, ProviderToolChoicePolicy,
    },
    integrations::retry::{ProviderCallError, RetryPolicy, provider_http_status_error, with_retry},
    shared::{
        outbound_http::read_response_bytes_with_limit,
        provider_base_url::{is_private_provider_url, resolve_runtime_provider_base_url},
        provider_http::{
            PROVIDER_ERROR_BODY_MAX_BYTES, PROVIDER_SUCCESS_BODY_MAX_BYTES, PreparedProviderTarget,
            ProviderHttpTransport, ProviderHttpTransportConfig,
        },
    },
};

#[cfg(test)]
use crate::domains::provider_profiles::ProviderTokenLimitParameter;

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub prompt: String,
    #[serde(skip)]
    #[schema(ignore)]
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub response_format: Option<serde_json::Value>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatRequestSeed {
    pub provider_kind: String,
    pub model_name: String,
    #[serde(skip)]
    #[schema(ignore)]
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

#[must_use]
pub fn build_text_chat_request(mut seed: ChatRequestSeed, prompt: String) -> ChatRequest {
    ChatRequest {
        provider_kind: std::mem::take(&mut seed.provider_kind),
        model_name: std::mem::take(&mut seed.model_name),
        prompt,
        api_key_override: seed.api_key_override.take(),
        base_url_override: seed.base_url_override.take(),
        system_prompt: seed.system_prompt.take(),
        temperature: seed.temperature,
        top_p: seed.top_p,
        max_output_tokens_override: seed.max_output_tokens_override,
        response_format: None,
        extra_parameters_json: std::mem::take(&mut seed.extra_parameters_json),
    }
}

#[must_use]
pub fn build_structured_chat_request(
    mut seed: ChatRequestSeed,
    prompt: String,
    response_format: serde_json::Value,
) -> ChatRequest {
    ChatRequest {
        provider_kind: std::mem::take(&mut seed.provider_kind),
        model_name: std::mem::take(&mut seed.model_name),
        prompt,
        api_key_override: seed.api_key_override.take(),
        base_url_override: seed.base_url_override.take(),
        system_prompt: seed.system_prompt.take(),
        temperature: seed.temperature,
        top_p: seed.top_p,
        max_output_tokens_override: seed.max_output_tokens_override,
        response_format: Some(response_format),
        extra_parameters_json: std::mem::take(&mut seed.extra_parameters_json),
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
/// tool-capable agents. Mirrors the `OpenAI` chat.completions message shape
/// so the same wire format works across OpenAI-compatible runtimes.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChatMessage {
    /// One of: "system", "user", "assistant", "tool".
    pub role: String,
    /// Plain text content. Optional because assistant messages can be
    /// tool-call only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Optional provider-emitted reasoning trace echoed back when an upstream
    /// runtime requires it for continuation of a multi-turn tool loop.
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
    /// calls. Runtimes that require reasoning continuity can reject a later
    /// tool-loop request when this trace is not preserved.
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

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolUseRequest {
    pub provider_kind: String,
    pub model_name: String,
    #[serde(skip)]
    #[schema(ignore)]
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
    /// Default `false` keeps normal `tool_choice="auto"` behavior for
    /// callers that want the model to decide whether tools are useful.
    #[serde(default)]
    pub require_tool_call: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolUseResponse {
    pub provider_kind: String,
    pub model_name: String,
    /// Final text output. Populated when `finish_reason` is "stop".
    pub output_text: String,
    /// Tool calls the model wants the caller to execute. Populated when
    /// `finish_reason` is "`tool_calls`".
    pub tool_calls: Vec<ChatToolCall>,
    pub finish_reason: Option<String>,
    pub usage_json: serde_json::Value,
    /// Provider reasoning trace for runtimes that require continuity across
    /// tool-loop turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmbeddingRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub input: String,
    #[serde(skip)]
    #[schema(ignore)]
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmbeddingBatchRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub inputs: Vec<String>,
    #[serde(skip)]
    #[schema(ignore)]
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

/// Parameters that are sent to the embedding endpoint in addition to the
/// protocol-owned `model` and `input` fields. Provider-control metadata is
/// consumed locally and therefore cannot define the remote vector space.
pub(crate) fn embedding_request_parameters(
    extra_parameters_json: &serde_json::Value,
) -> serde_json::Value {
    let Some(extra) = extra_parameters_json.as_object() else {
        return serde_json::json!({});
    };
    serde_json::Value::Object(
        extra
            .iter()
            .filter(|(key, _)| {
                !matches!(key.as_str(), "model" | "input") && !key.starts_with("_provider")
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct VisionRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub prompt: String,
    pub image_bytes: Vec<u8>,
    pub mime_type: String,
    #[serde(skip)]
    #[schema(ignore)]
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

macro_rules! impl_redacted_api_key_holder {
    ($type:ty, $name:literal) => {
        impl Drop for $type {
            fn drop(&mut self) {
                if let Some(api_key) = self.api_key_override.as_mut() {
                    api_key.zeroize();
                }
            }
        }

        impl std::fmt::Debug for $type {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter
                    .debug_struct($name)
                    .field("provider_kind", &self.provider_kind)
                    .field("model_name", &self.model_name)
                    .field(
                        "api_key_override",
                        &self.api_key_override.as_ref().map(|_| "<redacted>"),
                    )
                    .finish_non_exhaustive()
            }
        }
    };
}

impl_redacted_api_key_holder!(ChatRequest, "ChatRequest");
impl_redacted_api_key_holder!(ChatRequestSeed, "ChatRequestSeed");
impl_redacted_api_key_holder!(ToolUseRequest, "ToolUseRequest");
impl_redacted_api_key_holder!(EmbeddingRequest, "EmbeddingRequest");
impl_redacted_api_key_holder!(EmbeddingBatchRequest, "EmbeddingBatchRequest");
impl_redacted_api_key_holder!(VisionRequest, "VisionRequest");

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
    /// Tool-use capable chat completion for an OpenAI-compatible runtime.
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
    #[serde(default)]
    request_policy: ProviderRequestPolicy,
}

#[derive(Clone)]
struct ResolvedProviderRuntime {
    api_key: Option<String>,
    base_url: String,
    allow_private_network: bool,
    runtime: ProviderRuntimeProfile,
    request_policy: ProviderRequestPolicy,
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
            .finish()
    }
}

impl ProviderRequestPolicy {
    const fn sampling_params(
        self,
        temperature: Option<f64>,
        top_p: Option<f64>,
    ) -> (Option<f64>, Option<f64>) {
        match self.sampling {
            ProviderSamplingPolicy::Forward => (temperature, top_p),
            ProviderSamplingPolicy::Omit => (None, None),
        }
    }

    const fn tool_choice(self, has_tools: bool, require_tool_call: bool) -> Option<&'static str> {
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

    fn tool_max_output_tokens(self, requested: Option<i32>) -> Option<i32> {
        requested.or(self.default_tool_max_output_tokens)
    }
}

#[derive(Clone)]
pub struct UnifiedGateway {
    transport: Arc<ProviderHttpTransport>,
}

async fn read_provider_response_body(
    response: reqwest::Response,
    provider_kind: &str,
    operation: &str,
) -> Result<(reqwest::StatusCode, reqwest::header::HeaderMap, Vec<u8>), ProviderCallError> {
    let status = response.status();
    let headers = response.headers().clone();
    let body_limit = if status.is_success() {
        PROVIDER_SUCCESS_BODY_MAX_BYTES
    } else {
        PROVIDER_ERROR_BODY_MAX_BYTES
    };
    let body_bytes = match read_response_bytes_with_limit(response, body_limit).await {
        Ok(bytes) => bytes,
        Err(_error) if !status.is_success() => Vec::new(),
        Err(error) => {
            return Err(ProviderCallError::response_policy(
                format!("failed to read {operation} response body: provider={provider_kind}"),
                error,
            ));
        }
    };
    Ok((status, headers, body_bytes))
}

fn provider_response_body_text(body_bytes: &[u8]) -> String {
    String::from_utf8_lossy(body_bytes).into_owned()
}

fn parse_provider_json_body(
    body_bytes: &[u8],
    provider_kind: &str,
    operation: &str,
) -> Result<serde_json::Value, ProviderCallError> {
    serde_json::from_slice::<serde_json::Value>(body_bytes).map_err(|source| {
        ProviderCallError::json(
            format!("failed to parse {operation} response from provider {provider_kind}"),
            source,
        )
    })
}

impl UnifiedGateway {
    /// Builds the provider gateway with a fail-closed HTTP transport.
    ///
    /// # Errors
    /// Returns an error when the bounded no-redirect client cannot be built.
    pub fn from_settings(settings: &Settings) -> Result<Self> {
        let timeout = Duration::from_secs(settings.llm_http_timeout_seconds.max(1));
        // Aggressive pool / keep-alive tuning to defeat a 90-second
        // stale-connection hang observed on the canonical SBP turn:
        // a `generate_with_tools` POST that direct-curl completed in
        // ~8.5 s spent 102 s inside the gateway. Root cause was a dead
        // HTTP keep-alive socket waiting on the OS TCP timeout (~90 s)
        // before reqwest reissued the request. `pool_idle_timeout`
        // drops cached sockets aggressively; `tcp_keepalive` lets the
        // kernel probe sockets every 15 s so dead peers (load
        // balancer reaping, NAT-table eviction, provider scaling)
        // surface long before they cost a full turn budget. None of
        // these settings change the canonical timeout semantics —
        // the gateway still aborts after `llm_http_timeout_seconds`
        // — they only prevent the path from blocking on a known-dead
        // socket.
        let transport = ProviderHttpTransport::try_new(ProviderHttpTransportConfig::llm(timeout))
            .context("failed to build provider HTTP transport")?;
        Ok(Self { transport: Arc::new(transport) })
    }

    fn prepared_request(
        target: &PreparedProviderTarget,
        method: Method,
        endpoint: &Url,
        provider_kind: &str,
    ) -> Result<reqwest::RequestBuilder, ProviderCallError> {
        target.request(method, endpoint).map_err(|error| {
            ProviderCallError::protocol(format!(
                "provider request policy failed: provider={provider_kind}: {error}"
            ))
        })
    }

    async fn call_openai_compatible(
        &self,
        request: OpenAiCompatibleRequest<'_>,
        allow_private_network: bool,
    ) -> Result<(String, serde_json::Value)> {
        let request_body = request.body()?;
        let endpoint_url =
            provider_endpoint_url(request.provider_kind, request.base_url, &request.chat_path)?;
        let target =
            self.transport.prepare(&endpoint_url, allow_private_network).await.with_context(
                || format!("provider target policy failed: provider={}", request.provider_kind),
            )?;

        with_retry(
            || async {
                let request_builder = Self::prepared_request(
                    &target,
                    Method::POST,
                    &endpoint_url,
                    request.provider_kind,
                )?
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

                let (status, headers, body_bytes) =
                    read_provider_response_body(response, request.provider_kind, "chat").await?;

                if !status.is_success() {
                    let body_text = provider_response_body_text(&body_bytes);
                    return Err(provider_http_status_error(
                        request.provider_kind,
                        status,
                        &headers,
                        &body_text,
                    ));
                }

                let body = parse_provider_json_body(&body_bytes, request.provider_kind, "chat")?;

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
        allow_private_network: bool,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<(String, serde_json::Value)> {
        let request_body = request.body()?;
        let endpoint_url =
            provider_endpoint_url(request.provider_kind, request.base_url, &request.chat_path)?;
        let target =
            self.transport.prepare(&endpoint_url, allow_private_network).await.with_context(
                || format!("provider target policy failed: provider={}", request.provider_kind),
            )?;

        let response = with_retry(
            || async {
                let request_builder = Self::prepared_request(
                    &target,
                    Method::POST,
                    &endpoint_url,
                    request.provider_kind,
                )?
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
                let body_bytes =
                    read_response_bytes_with_limit(response, PROVIDER_ERROR_BODY_MAX_BYTES)
                        .await
                        .unwrap_or_default();
                let body_text = provider_response_body_text(&body_bytes);
                Err(provider_http_status_error(request.provider_kind, status, &headers, &body_text))
            },
            RetryPolicy::default(),
        )
        .await?;

        drain_openai_compatible_stream(response, on_delta).await
    }

    fn parse_embedding_vector(value: &serde_json::Value) -> Result<Vec<f32>> {
        let values = value.as_array().context("embedding must be a JSON array")?;
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = value
                    .as_f64()
                    .ok_or_else(|| anyhow!("embedding element {index} must be a number"))?;
                if !value.is_finite() || value < f64::from(f32::MIN) || value > f64::from(f32::MAX)
                {
                    return Err(anyhow!("embedding element {index} is not a finite f32 value"));
                }
                Ok(value as f32)
            })
            .collect()
    }

    fn embedding_request_body(
        model_name: &str,
        input: serde_json::Value,
        extra_parameters_json: &serde_json::Value,
    ) -> serde_json::Value {
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), serde_json::Value::String(model_name.to_string()));
        body.insert("input".to_string(), input);

        if let Some(extra) = embedding_request_parameters(extra_parameters_json).as_object() {
            body.extend(extra.clone());
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
            allow_private_network: runtime_profile.base_url.allow_private_network,
            runtime: runtime_profile.runtime,
            request_policy: runtime_profile.request_policy,
        })
    }
}

fn resolve_provider_request_policy(
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

fn resolve_runtime_profile(
    provider_kind: &str,
    extra_parameters_json: &serde_json::Value,
) -> Result<RuntimeProviderProfileEnvelope> {
    if let Some(value) = extra_parameters_json.get("_providerProfile") {
        let mut profile = serde_json::from_value::<RuntimeProviderProfileEnvelope>(value.clone())
            .with_context(|| {
            format!("invalid runtime provider profile for provider={provider_kind}")
        })?;
        if profile.runtime.kind != OPENAI_COMPATIBLE_RUNTIME_KIND {
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
    if !policy.allow_private_network && is_private_provider_url(&url) {
        return Err(anyhow!(
            "provider base URL must not target a private, loopback, or link-local network for provider={provider_kind}"
        ));
    }
    Ok(())
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
                        .or_else(|| function.get("arguments").map(std::string::ToString::to_string))
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
        ProviderStructuredOutputMode::PromptOnlyJsonObject => Ok(None),
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
    if !matches!(
        mode,
        ProviderStructuredOutputMode::JsonObject
            | ProviderStructuredOutputMode::PromptOnlyJsonObject
    ) {
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
    async fn generate(&self, mut request: ChatRequest) -> Result<ChatResponse> {
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
        let (temperature, top_p) =
            resolved.request_policy.sampling_params(request.temperature, request.top_p);
        let (output_text, usage_json) = self
            .call_openai_compatible(
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
                    temperature,
                    top_p,
                    max_output_tokens: request.max_output_tokens_override,
                    token_limit_parameter: resolved.runtime.token_limit_parameter,
                    response_format: response_format.as_ref(),
                    extra_parameters_json: &upstream_extra,
                    stream: false,
                },
                resolved.allow_private_network,
            )
            .await?;
        Ok(ChatResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            usage_json,
        })
    }

    async fn generate_stream(
        &self,
        mut request: ChatRequest,
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
        let (temperature, top_p) =
            resolved.request_policy.sampling_params(request.temperature, request.top_p);
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
                    temperature,
                    top_p,
                    max_output_tokens: request.max_output_tokens_override,
                    token_limit_parameter: resolved.runtime.token_limit_parameter,
                    response_format: response_format.as_ref(),
                    extra_parameters_json: &upstream_extra,
                    stream: true,
                },
                resolved.allow_private_network,
                on_delta,
            )
            .await?;
        Ok(ChatResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            usage_json,
        })
    }

    async fn generate_with_tools(&self, mut request: ToolUseRequest) -> Result<ToolUseResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;

        let messages =
            request.messages.iter().map(OpenAiCompatibleToolUseMessage::from).collect::<Vec<_>>();
        let tools = request.tools.iter().map(OpenAiCompatibleToolDef::from).collect::<Vec<_>>();
        let max_output_tokens =
            resolved.request_policy.tool_max_output_tokens(request.max_output_tokens_override);
        let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
            resolved.runtime.token_limit_parameter,
            max_output_tokens,
        );
        let upstream_extra = Self::upstream_extra_parameters(&request.extra_parameters_json);
        let tool_choice =
            resolved.request_policy.tool_choice(!tools.is_empty(), request.require_tool_call);
        let (temperature, top_p) =
            resolved.request_policy.sampling_params(request.temperature, request.top_p);

        let payload = OpenAiCompatibleToolUseChatRequest {
            model: &request.model_name,
            messages,
            tools,
            temperature,
            top_p,
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
        let target = self
            .transport
            .prepare(&endpoint_url, resolved.allow_private_network)
            .await
            .with_context(|| {
                format!("provider target policy failed: provider={}", request.provider_kind)
            })?;

        let (output_text, tool_calls, finish_reason, usage_json, reasoning_content) = with_retry(
            || async {
                let request_builder = Self::prepared_request(
                    &target,
                    Method::POST,
                    &endpoint_url,
                    &request.provider_kind,
                )?
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

                let (status, headers, body_bytes) =
                    read_provider_response_body(response, &request.provider_kind, "tool-use")
                        .await?;
                if !status.is_success() {
                    let body_text = provider_response_body_text(&body_bytes);
                    return Err(provider_http_status_error(
                        &request.provider_kind,
                        status,
                        &headers,
                        &body_text,
                    ));
                }

                let body =
                    parse_provider_json_body(&body_bytes, &request.provider_kind, "tool-use")?;

                parse_tool_use_response(&body)
            },
            RetryPolicy::default(),
        )
        .await?;

        Ok(ToolUseResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            tool_calls,
            finish_reason,
            usage_json,
            reasoning_content,
        })
    }

    async fn generate_with_tools_stream(
        &self,
        mut request: ToolUseRequest,
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
        let max_output_tokens =
            resolved.request_policy.tool_max_output_tokens(request.max_output_tokens_override);
        let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
            resolved.runtime.token_limit_parameter,
            max_output_tokens,
        );
        let upstream_extra = Self::upstream_extra_parameters(&request.extra_parameters_json);
        let tool_choice =
            resolved.request_policy.tool_choice(!tools.is_empty(), request.require_tool_call);
        let (temperature, top_p) =
            resolved.request_policy.sampling_params(request.temperature, request.top_p);

        let payload = OpenAiCompatibleToolUseChatRequest {
            model: &request.model_name,
            messages,
            tools,
            temperature,
            top_p,
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
        let target = self
            .transport
            .prepare(&endpoint_url, resolved.allow_private_network)
            .await
            .with_context(|| {
                format!("provider target policy failed: provider={}", request.provider_kind)
            })?;

        let response = with_retry(
            || async {
                let request_builder = Self::prepared_request(
                    &target,
                    Method::POST,
                    &endpoint_url,
                    &request.provider_kind,
                )?
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
                let body_bytes =
                    read_response_bytes_with_limit(response, PROVIDER_ERROR_BODY_MAX_BYTES)
                        .await
                        .unwrap_or_default();
                let body_text = provider_response_body_text(&body_bytes);
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
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            tool_calls,
            finish_reason,
            usage_json,
            // Streaming path does not yet capture `reasoning_content`.
            // The non-streaming gateway is the canonical tool-use path; streaming
            // is reserved for direct provider passthroughs that do not echo
            // reasoning back to the model.
            reasoning_content: None,
        })
    }

    async fn embed(&self, mut request: EmbeddingRequest) -> Result<EmbeddingResponse> {
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
        let target = self
            .transport
            .prepare(&endpoint_url, resolved.allow_private_network)
            .await
            .with_context(|| {
                format!("provider target policy failed: provider={}", request.provider_kind)
            })?;

        let body = with_retry(
            || async {
                let request_builder = Self::prepared_request(
                    &target,
                    Method::POST,
                    &endpoint_url,
                    &request.provider_kind,
                )?;
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

                let (status, headers, body_bytes) =
                    read_provider_response_body(response, &request.provider_kind, "embedding")
                        .await?;
                if !status.is_success() {
                    let body_text = provider_response_body_text(&body_bytes);
                    return Err(provider_http_status_error(
                        &request.provider_kind,
                        status,
                        &headers,
                        &body_text,
                    ));
                }
                parse_provider_json_body(&body_bytes, &request.provider_kind, "embedding")
            },
            RetryPolicy::default(),
        )
        .await?;

        let embedding_value = body
            .get("data")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("embedding"))
            .context("embedding response did not contain data[0].embedding")?;
        let embedding = Self::parse_embedding_vector(embedding_value)
            .context("embedding response contained an invalid vector")?;

        let usage_json = body.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));

        Ok(EmbeddingResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions: embedding.len(),
            embedding,
            usage_json,
        })
    }

    async fn embed_many(
        &self,
        mut request: EmbeddingBatchRequest,
    ) -> Result<EmbeddingBatchResponse> {
        if request.inputs.is_empty() {
            return Ok(EmbeddingBatchResponse {
                provider_kind: std::mem::take(&mut request.provider_kind),
                model_name: std::mem::take(&mut request.model_name),
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
        let target = self
            .transport
            .prepare(&endpoint_url, resolved.allow_private_network)
            .await
            .with_context(|| {
                format!("provider target policy failed: provider={}", request.provider_kind)
            })?;

        let body = with_retry(
            || async {
                let request_builder = Self::prepared_request(
                    &target,
                    Method::POST,
                    &endpoint_url,
                    &request.provider_kind,
                )?;
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

                let (status, headers, body_bytes) = read_provider_response_body(
                    response,
                    &request.provider_kind,
                    "embedding batch",
                )
                .await?;
                if !status.is_success() {
                    let body_text = provider_response_body_text(&body_bytes);
                    return Err(provider_http_status_error(
                        &request.provider_kind,
                        status,
                        &headers,
                        &body_text,
                    ));
                }
                parse_provider_json_body(&body_bytes, &request.provider_kind, "embedding batch")
            },
            RetryPolicy::default(),
        )
        .await?;

        let items = body
            .get("data")
            .and_then(serde_json::Value::as_array)
            .context("embedding batch response did not contain a data array")?;
        let embeddings = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let value = item.get("embedding").ok_or_else(|| {
                    anyhow!("embedding batch item {index} did not contain an embedding")
                })?;
                Self::parse_embedding_vector(value)
                    .with_context(|| format!("embedding batch item {index} was invalid"))
            })
            .collect::<Result<Vec<_>>>()?;
        let dimensions = embeddings.first().map(Vec::len).unwrap_or_default();
        let usage_json = body.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));

        Ok(EmbeddingBatchResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions,
            embeddings,
            usage_json,
        })
    }

    async fn vision_extract(&self, mut request: VisionRequest) -> Result<VisionResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.extra_parameters_json,
        )?;
        let upstream_extra = Self::upstream_extra_parameters(&request.extra_parameters_json);
        let (temperature, top_p) =
            resolved.request_policy.sampling_params(request.temperature, request.top_p);
        let image_data_url = format!(
            "data:{};base64,{}",
            request.mime_type,
            BASE64_STANDARD.encode(&request.image_bytes)
        );
        let (output_text, usage_json) = self
            .call_openai_compatible(
                OpenAiCompatibleRequest {
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
                    temperature,
                    top_p,
                    max_output_tokens: request.max_output_tokens_override,
                    token_limit_parameter: resolved.runtime.token_limit_parameter,
                    response_format: None,
                    extra_parameters_json: &upstream_extra,
                    stream: false,
                },
                resolved.allow_private_network,
            )
            .await?;

        Ok(VisionResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            usage_json,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChatRequest, ChatRequestSeed, ChatToolDef, EmbeddingBatchRequest, EmbeddingRequest,
        OpenAiCompatibleMessage, OpenAiCompatibleMessageContent, OpenAiCompatibleRequest,
        OpenAiCompatibleToolDef, OpenAiCompatibleToolUseChatRequest, ProviderAuthScheme,
        ProviderRequestPolicy, ProviderSamplingPolicy, ProviderStructuredOutputMode,
        ProviderTokenLimitParameter, ProviderToolChoicePolicy, ToolUseRequest, UnifiedGateway,
        VisionRequest, consume_openai_compatible_stream_frame, extract_message_content_text,
        parse_provider_json_body, parse_tool_use_response, provider_response_format,
        provider_system_prompt, resolve_provider_request_policy,
    };

    fn assert_runtime_secret_is_redacted(
        value: &(impl serde::Serialize + std::fmt::Debug),
        secret: &str,
    ) {
        let serialized = serde_json::to_value(value).expect("request should serialize safely");
        let debug = format!("{value:?}");

        assert!(serialized.get("api_key_override").is_none());
        assert!(!serialized.to_string().contains(secret));
        assert!(!debug.contains(secret));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn internal_chat_request_never_serializes_provider_credentials() {
        let request = ChatRequest {
            provider_kind: "provider-alpha".to_string(),
            model_name: "model-alpha".to_string(),
            prompt: "synthetic prompt".to_string(),
            api_key_override: Some("serialization-regression-secret".to_string()),
            base_url_override: Some("https://example.com".to_string()),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            response_format: None,
            extra_parameters_json: serde_json::json!({}),
        };

        assert_runtime_secret_is_redacted(&request, "serialization-regression-secret");

        let seed = ChatRequestSeed {
            provider_kind: "provider-alpha".to_string(),
            model_name: "model-alpha".to_string(),
            api_key_override: Some("seed-regression-secret".to_string()),
            base_url_override: Some("https://example.com".to_string()),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        };
        assert_runtime_secret_is_redacted(&seed, "seed-regression-secret");

        let tool = ToolUseRequest {
            provider_kind: "provider-alpha".to_string(),
            model_name: "model-alpha".to_string(),
            api_key_override: Some("tool-regression-secret".to_string()),
            base_url_override: Some("https://example.com".to_string()),
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            messages: Vec::new(),
            tools: Vec::new(),
            extra_parameters_json: serde_json::json!({}),
            require_tool_call: false,
        };
        assert_runtime_secret_is_redacted(&tool, "tool-regression-secret");

        let embedding = EmbeddingRequest {
            provider_kind: "provider-alpha".to_string(),
            model_name: "model-alpha".to_string(),
            input: "synthetic input".to_string(),
            api_key_override: Some("embedding-regression-secret".to_string()),
            base_url_override: Some("https://example.com".to_string()),
            extra_parameters_json: serde_json::json!({}),
        };
        assert_runtime_secret_is_redacted(&embedding, "embedding-regression-secret");

        let embedding_batch = EmbeddingBatchRequest {
            provider_kind: "provider-alpha".to_string(),
            model_name: "model-alpha".to_string(),
            inputs: vec!["synthetic input".to_string()],
            api_key_override: Some("batch-regression-secret".to_string()),
            base_url_override: Some("https://example.com".to_string()),
            extra_parameters_json: serde_json::json!({}),
        };
        assert_runtime_secret_is_redacted(&embedding_batch, "batch-regression-secret");

        let vision = VisionRequest {
            provider_kind: "provider-alpha".to_string(),
            model_name: "model-alpha".to_string(),
            prompt: "synthetic prompt".to_string(),
            image_bytes: vec![0],
            mime_type: "image/png".to_string(),
            api_key_override: Some("vision-regression-secret".to_string()),
            base_url_override: Some("https://example.com".to_string()),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        };
        assert_runtime_secret_is_redacted(&vision, "vision-regression-secret");
    }

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
    fn parses_provider_json_from_utf8_bytes_without_charset_roundtrip() {
        let body = b"{\"value\":\"\xd0\xa1\xd1\x82\xd1\x80\xd0\xbe\xd0\xba\xd0\xb0\"}";
        let latin1_misdecoded = body.iter().map(|byte| char::from(*byte)).collect::<String>();
        assert!(latin1_misdecoded.contains('\u{00d0}'));

        let parsed =
            parse_provider_json_body(body, "provider-alpha", "chat").expect("body is UTF-8 JSON");

        assert_eq!(parsed["value"], "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}");
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

    fn normalized_tool_request_projection(
        provider_kind: &str,
        model_name: &str,
        extra_parameters_json: &serde_json::Value,
    ) -> serde_json::Value {
        let policy = resolve_provider_request_policy(
            provider_kind,
            extra_parameters_json,
            ProviderRequestPolicy::default(),
        )
        .expect("synthetic request policy should resolve");
        let (temperature, top_p) = policy.sampling_params(Some(0.3), Some(0.9));
        let max_output_tokens = policy.tool_max_output_tokens(None);
        let (max_completion_tokens, max_tokens) = super::openai_compatible_token_limit_fields(
            ProviderTokenLimitParameter::MaxTokens,
            max_output_tokens,
        );
        let payload = OpenAiCompatibleToolUseChatRequest {
            model: model_name,
            messages: vec![],
            tools: vec![OpenAiCompatibleToolDef::from(&ChatToolDef {
                name: "lookup".to_string(),
                description: "Lookup structured facts".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            })],
            temperature,
            top_p,
            max_completion_tokens,
            max_tokens,
            tool_choice: policy.tool_choice(true, true),
            stream: false,
            extra: UnifiedGateway::upstream_extra_parameters(extra_parameters_json),
        };
        let mut projection =
            serde_json::to_value(payload).expect("tool-use request should serialize to JSON");
        projection.as_object_mut().expect("tool-use request should be an object").remove("model");
        projection
    }

    #[test]
    fn unseen_provider_and_model_names_share_standard_request_behavior() {
        let first = normalized_tool_request_projection(
            "provider-nebula",
            "model-orbit",
            &serde_json::json!({}),
        );
        let second = normalized_tool_request_projection(
            "provider-quartz",
            "model-vector",
            &serde_json::json!({}),
        );

        assert_eq!(first, second);
        assert_eq!(first.get("temperature").and_then(serde_json::Value::as_f64), Some(0.3));
        assert_eq!(first.get("top_p").and_then(serde_json::Value::as_f64), Some(0.9));
        assert_eq!(first.get("tool_choice").and_then(serde_json::Value::as_str), Some("required"));
        assert!(first.get("max_tokens").is_none());
    }

    #[test]
    fn explicit_typed_policy_changes_request_behavior() {
        let projection = normalized_tool_request_projection(
            "provider-nebula",
            "model-orbit",
            &serde_json::json!({
                "_providerRequestPolicy": {
                    "sampling": "omit",
                    "toolChoice": "auto_only",
                    "defaultToolMaxOutputTokens": 2048
                }
            }),
        );

        assert!(projection.get("temperature").is_none());
        assert!(projection.get("top_p").is_none());
        assert_eq!(projection.get("tool_choice").and_then(serde_json::Value::as_str), Some("auto"));
        assert_eq!(projection.get("max_tokens").and_then(serde_json::Value::as_i64), Some(2048));
    }

    #[test]
    fn partial_explicit_policy_uses_standard_field_defaults() {
        let resolved = resolve_provider_request_policy(
            "provider-nebula",
            &serde_json::json!({
                "_providerRequestPolicy": {"sampling": "omit"}
            }),
            ProviderRequestPolicy::default(),
        )
        .expect("partial binding policy should resolve with serde defaults");

        assert_eq!(resolved.sampling, ProviderSamplingPolicy::Omit);
        assert_eq!(resolved.tool_choice, ProviderToolChoicePolicy::RequiredCapable);
        assert_eq!(resolved.default_tool_max_output_tokens, None);
    }

    #[test]
    fn binding_policy_has_explicit_precedence_over_profile_policy() {
        let profile_policy = ProviderRequestPolicy {
            sampling: ProviderSamplingPolicy::Omit,
            tool_choice: ProviderToolChoicePolicy::AutoOnly,
            default_tool_max_output_tokens: Some(1024),
        };
        let resolved = resolve_provider_request_policy(
            "provider-nebula",
            &serde_json::json!({
                "_providerRequestPolicy": {
                    "sampling": "forward",
                    "toolChoice": "required_capable",
                    "defaultToolMaxOutputTokens": 4096
                }
            }),
            profile_policy,
        )
        .expect("binding policy should resolve");

        assert_eq!(resolved.sampling, ProviderSamplingPolicy::Forward);
        assert_eq!(resolved.tool_choice, ProviderToolChoicePolicy::RequiredCapable);
        assert_eq!(resolved.default_tool_max_output_tokens, Some(4096));
    }

    #[test]
    fn invalid_explicit_policy_fails_closed() {
        let unknown_mode = resolve_provider_request_policy(
            "provider-nebula",
            &serde_json::json!({
                "_providerRequestPolicy": {"sampling": "guess_from_model_name"}
            }),
            ProviderRequestPolicy::default(),
        )
        .expect_err("unknown policy variants must not be ignored");
        assert!(unknown_mode.to_string().contains("invalid provider request policy"));

        let invalid_limit = resolve_provider_request_policy(
            "provider-nebula",
            &serde_json::json!({
                "_providerRequestPolicy": {"defaultToolMaxOutputTokens": 0}
            }),
            ProviderRequestPolicy::default(),
        )
        .expect_err("non-positive default token limits must fail closed");
        assert!(invalid_limit.to_string().contains("greater than zero"));
    }

    #[test]
    fn internal_request_policy_is_never_forwarded_upstream() {
        let upstream = UnifiedGateway::upstream_extra_parameters(&serde_json::json!({
            "_providerRequestPolicy": {"sampling": "omit"},
            "_providerProfile": {"requestPolicy": {"toolChoice": "auto_only"}},
            "vendor_option": true
        }));

        assert!(upstream.get("_providerRequestPolicy").is_none());
        assert!(upstream.get("_providerProfile").is_none());
        assert_eq!(upstream.get("vendor_option").and_then(serde_json::Value::as_bool), Some(true));
    }

    #[test]
    fn parse_tool_use_response_returns_final_text_when_tool_calls_are_absent() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "Use the grounded answer fallback."
                }
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 5}
        });

        let (output_text, tool_calls, finish_reason, usage_json, reasoning_content) =
            parse_tool_use_response(&body).expect("tool-call-less response should parse");

        assert_eq!(output_text, "Use the grounded answer fallback.");
        assert!(tool_calls.is_empty());
        assert_eq!(finish_reason.as_deref(), Some("stop"));
        assert_eq!(usage_json["completion_tokens"], 5);
        assert!(reasoning_content.is_none());
    }

    #[test]
    fn standard_policy_omits_choice_without_tools_and_honors_explicit_max_tokens() {
        let policy = ProviderRequestPolicy::default();

        assert_eq!(policy.tool_choice(false, true), None);
        assert_eq!(policy.tool_max_output_tokens(Some(512)), Some(512));
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
    fn prompt_only_json_object_runtime_omits_response_format() {
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
            ProviderStructuredOutputMode::PromptOnlyJsonObject,
        )
        .expect("prompt-only providers should accept structured output by prompt contract");

        assert!(lowered.is_none());
    }

    #[test]
    fn prompt_only_json_object_runtime_injects_schema_into_system_prompt() {
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
                                    "label": {"type": "string"}
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
            ProviderStructuredOutputMode::PromptOnlyJsonObject,
        )
        .expect("prompt-only prompt should be built")
        .expect("prompt should remain present");

        assert!(system_prompt.starts_with("Base compiler prompt\n\n"));
        assert!(system_prompt.contains("JSON Schema:"));
        assert!(system_prompt.contains("\"target_entities\""));
        assert!(system_prompt.contains("\"label\""));
    }

    #[test]
    fn prompt_only_json_object_request_body_has_no_response_format() {
        let requested = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "graph_extraction",
                "strict": true,
                "schema": {"type": "object"}
            }
        });
        let response_format = provider_response_format(
            "provider-alpha",
            Some(&requested),
            ProviderStructuredOutputMode::PromptOnlyJsonObject,
        )
        .expect("prompt-only response format should resolve");
        let system_prompt = provider_system_prompt(
            "provider-alpha",
            Some("Base prompt"),
            Some(&requested),
            ProviderStructuredOutputMode::PromptOnlyJsonObject,
        )
        .expect("prompt-only system prompt should resolve");
        let body = OpenAiCompatibleRequest {
            provider_kind: "provider-alpha",
            api_key: Some("test"),
            base_url: "https://example.invalid/v1",
            auth_scheme: ProviderAuthScheme::Bearer,
            chat_path: "/chat/completions".to_string(),
            model_name: "provider-model",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: system_prompt.as_deref(),
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            token_limit_parameter: ProviderTokenLimitParameter::MaxCompletionTokens,
            response_format: response_format.as_ref(),
            extra_parameters_json: &serde_json::json!({}),
            stream: false,
        }
        .body()
        .expect("request body should serialize");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("serialized body should stay valid json");

        assert!(value.get("response_format").is_none());
        assert_eq!(
            value
                .get("messages")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("content"))
                .and_then(serde_json::Value::as_str)
                .map(|text| text.contains("JSON Schema:")),
            Some(true),
        );
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
        assert!(
            crate::shared::provider_base_url::provider_base_url_candidates(
                true,
                "http://localhost:11434/v1"
            )
            .contains(&resolved.base_url)
        );
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
                    },
                    "requestPolicy": {
                        "sampling": "omit",
                        "toolChoice": "auto_only",
                        "defaultToolMaxOutputTokens": 1024
                    }
                }
            }),
        )
        .expect("raw authorization profile should resolve");
        assert_eq!(resolved.api_key.as_deref(), Some("plain-secret"));
        assert_eq!(resolved.runtime.auth_scheme, ProviderAuthScheme::RawAuthorization);
        assert_eq!(resolved.request_policy.sampling, ProviderSamplingPolicy::Omit);
        assert_eq!(resolved.request_policy.tool_choice, ProviderToolChoicePolicy::AutoOnly);
        assert_eq!(resolved.request_policy.default_tool_max_output_tokens, Some(1024));
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
    fn embedding_vector_parser_rejects_invalid_elements_instead_of_dropping_them() {
        assert_eq!(
            UnifiedGateway::parse_embedding_vector(&serde_json::json!([0.25, -0.5])).unwrap(),
            vec![0.25, -0.5],
        );
        assert!(
            UnifiedGateway::parse_embedding_vector(&serde_json::json!([0.25, "invalid", -0.5]))
                .is_err()
        );
        assert!(
            UnifiedGateway::parse_embedding_vector(&serde_json::json!([0.25, 1.0e100])).is_err()
        );
        assert!(UnifiedGateway::parse_embedding_vector(&serde_json::json!({})).is_err());
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
