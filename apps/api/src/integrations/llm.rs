use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::time::Duration;

mod openai_compatible;
mod streaming;

use self::{
    openai_compatible::{
        OpenAiCompatibleContentPart, OpenAiCompatibleImageUrl, OpenAiCompatibleMessage,
        OpenAiCompatibleMessageContent, OpenAiCompatibleRequest, OpenAiCompatibleToolDef,
        OpenAiCompatibleToolUseChatRequest, OpenAiCompatibleToolUseMessage,
        extract_message_content_text, is_retryable_upstream_json_parse_failure,
        openai_compatible_token_limit_fields, retryable_openai_parse_failure_error,
    },
    streaming::{
        drain_openai_compatible_stream, drain_tool_use_stream, is_retryable_transport_error,
        is_retryable_upstream_status, transport_retry_delay,
    },
};

#[cfg(test)]
use self::streaming::{consume_openai_compatible_stream_frame, is_retryable_transport_error_text};

use crate::{app::config::Settings, shared::provider_base_url::resolve_runtime_provider_base_url};

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// One tool invocation requested by the LLM in its response.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// One of: "system", "user", "assistant", "tool".
    pub role: String,
    /// Plain text content. Optional because assistant messages can be
    /// tool-call only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
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
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    #[must_use]
    pub fn assistant_with_tool_calls(tool_calls: Vec<ChatToolCall>) -> Self {
        Self { role: "assistant".into(), content: None, tool_calls, tool_call_id: None, name: None }
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
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(tool_name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub input: String,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingBatchRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub inputs: Vec<String>,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: usize,
    pub embedding: Vec<f32>,
    pub usage_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingBatchResponse {
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: usize,
    pub embeddings: Vec<Vec<f32>>,
    pub usage_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Clone)]
pub struct UnifiedGateway {
    client: Client,
    transport_retry_attempts: usize,
    transport_retry_base_delay_ms: u64,
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
        Self {
            client,
            transport_retry_attempts: settings.llm_transport_retry_attempts.max(1),
            transport_retry_base_delay_ms: settings.llm_transport_retry_base_delay_ms.max(25),
        }
    }

    async fn call_openai_compatible(
        &self,
        request: OpenAiCompatibleRequest<'_>,
    ) -> Result<(String, serde_json::Value)> {
        let request_body = request.body()?;
        let request_body_is_valid_json = true;
        let max_attempts = self.transport_retry_attempts.max(1);

        let mut last_error = None;
        for attempt in 1..=max_attempts {
            let request_builder = self
                .client
                .post(format!("{}/chat/completions", request.base_url))
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json");
            let request_builder = if let Some(api_key) = request.api_key {
                request_builder.bearer_auth(api_key)
            } else {
                request_builder
            };
            let response = match request_builder.body(request_body.clone()).send().await {
                Ok(response) => response,
                Err(error) => {
                    if attempt < max_attempts && is_retryable_transport_error(&error) {
                        last_error = Some(anyhow!(
                            "provider transport failed: provider={} attempt={}/{} error={error}",
                            request.provider_kind,
                            attempt,
                            max_attempts,
                        ));
                        tokio::time::sleep(transport_retry_delay(
                            self.transport_retry_base_delay_ms,
                            attempt,
                        ))
                        .await;
                        continue;
                    }
                    return Err(error.into());
                }
            };

            let status = response.status();
            let body_text = response.text().await?;
            let body = serde_json::from_str::<serde_json::Value>(&body_text)
                .unwrap_or_else(|_| serde_json::json!({ "raw_body": body_text }));

            if !status.is_success() {
                last_error = Some(anyhow!(
                    "provider request failed: provider={} status={status} body={body}",
                    request.provider_kind,
                ));
                let retryable_parse_failure = is_retryable_upstream_json_parse_failure(
                    status.as_u16(),
                    &body,
                    request_body_is_valid_json,
                );
                let retryable_status = is_retryable_upstream_status(status.as_u16());
                if attempt < max_attempts && (retryable_parse_failure || retryable_status) {
                    tokio::time::sleep(transport_retry_delay(
                        self.transport_retry_base_delay_ms,
                        attempt,
                    ))
                    .await;
                    continue;
                }
                if retryable_parse_failure {
                    return Err(retryable_openai_parse_failure_error(
                        request.provider_kind,
                        attempt,
                        last_error.as_ref(),
                    ));
                }
                return Err(last_error.take().unwrap_or_else(|| {
                    anyhow!("provider request failed: provider={}", request.provider_kind)
                }));
            }

            let output_text = body
                .get("choices")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("message"))
                .and_then(|v| v.get("content"))
                .map(extract_message_content_text)
                .unwrap_or_default();

            let usage_json = body.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));

            let _ = request.provider_kind;
            return Ok((output_text, usage_json));
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow!("provider request failed: provider={}", request.provider_kind)
        }))
    }

    async fn call_openai_compatible_stream(
        &self,
        request: OpenAiCompatibleRequest<'_>,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<(String, serde_json::Value)> {
        let request_body = request.body()?;
        let request_body_is_valid_json = true;
        let max_attempts = self.transport_retry_attempts.max(1);

        let mut last_error = None;
        for attempt in 1..=max_attempts {
            let request_builder = self
                .client
                .post(format!("{}/chat/completions", request.base_url))
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "text/event-stream");
            let request_builder = if let Some(api_key) = request.api_key {
                request_builder.bearer_auth(api_key)
            } else {
                request_builder
            };
            let response = match request_builder.body(request_body.clone()).send().await {
                Ok(response) => response,
                Err(error) => {
                    if attempt < max_attempts && is_retryable_transport_error(&error) {
                        last_error = Some(anyhow!(
                            "provider transport failed: provider={} attempt={}/{} error={error}",
                            request.provider_kind,
                            attempt,
                            max_attempts,
                        ));
                        tokio::time::sleep(transport_retry_delay(
                            self.transport_retry_base_delay_ms,
                            attempt,
                        ))
                        .await;
                        continue;
                    }
                    return Err(error.into());
                }
            };

            let status = response.status();
            if !status.is_success() {
                let body_text = response.text().await?;
                let body = serde_json::from_str::<serde_json::Value>(&body_text)
                    .unwrap_or_else(|_| serde_json::json!({ "raw_body": body_text }));
                last_error = Some(anyhow!(
                    "provider request failed: provider={} status={status} body={body}",
                    request.provider_kind,
                ));
                let retryable_parse_failure = is_retryable_upstream_json_parse_failure(
                    status.as_u16(),
                    &body,
                    request_body_is_valid_json,
                );
                let retryable_status = is_retryable_upstream_status(status.as_u16());
                if attempt < max_attempts && (retryable_parse_failure || retryable_status) {
                    tokio::time::sleep(transport_retry_delay(
                        self.transport_retry_base_delay_ms,
                        attempt,
                    ))
                    .await;
                    continue;
                }
                if retryable_parse_failure {
                    return Err(retryable_openai_parse_failure_error(
                        request.provider_kind,
                        attempt,
                        last_error.as_ref(),
                    ));
                }
                return Err(last_error.take().unwrap_or_else(|| {
                    anyhow!("provider request failed: provider={}", request.provider_kind)
                }));
            }

            return drain_openai_compatible_stream(response, on_delta).await;
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow!("provider request failed: provider={}", request.provider_kind)
        }))
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
                if key == "model" || key == "input" {
                    continue;
                }
                body.insert(key.clone(), value.clone());
            }
        }

        serde_json::Value::Object(body)
    }

    async fn embed_many_sequential(
        &self,
        request: EmbeddingBatchRequest,
    ) -> Result<EmbeddingBatchResponse> {
        let mut embeddings = Vec::with_capacity(request.inputs.len());
        let mut prompt_tokens = 0_i64;
        let mut total_tokens = 0_i64;
        let mut completion_tokens = 0_i64;
        let mut saw_prompt_tokens = false;
        let mut saw_total_tokens = false;
        let mut saw_completion_tokens = false;

        for input in request.inputs {
            let response = self
                .embed(EmbeddingRequest {
                    provider_kind: request.provider_kind.clone(),
                    model_name: request.model_name.clone(),
                    input,
                    api_key_override: request.api_key_override.clone(),
                    base_url_override: request.base_url_override.clone(),
                    extra_parameters_json: request.extra_parameters_json.clone(),
                })
                .await?;
            if let Some(value) =
                response.usage_json.get("prompt_tokens").and_then(serde_json::Value::as_i64)
            {
                prompt_tokens += value;
                saw_prompt_tokens = true;
            }
            if let Some(value) =
                response.usage_json.get("total_tokens").and_then(serde_json::Value::as_i64)
            {
                total_tokens += value;
                saw_total_tokens = true;
            }
            if let Some(value) =
                response.usage_json.get("completion_tokens").and_then(serde_json::Value::as_i64)
            {
                completion_tokens += value;
                saw_completion_tokens = true;
            }
            embeddings.push(response.embedding);
        }

        let dimensions = embeddings.first().map(Vec::len).unwrap_or_default();
        Ok(EmbeddingBatchResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            dimensions,
            embeddings,
            usage_json: serde_json::json!({
                "prompt_tokens": saw_prompt_tokens.then_some(prompt_tokens),
                "completion_tokens": saw_completion_tokens.then_some(completion_tokens),
                "total_tokens": saw_total_tokens.then_some(total_tokens),
            }),
        })
    }

    fn resolve_provider(
        provider_kind: &str,
        api_key_override: Option<&str>,
        base_url_override: Option<&str>,
    ) -> Result<(Option<String>, String)> {
        let api_key = api_key_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string);
        match provider_kind {
            "ollama" => Ok((
                api_key,
                resolve_runtime_provider_base_url(
                    provider_kind,
                    base_url_override.ok_or_else(|| anyhow!("missing provider base URL"))?,
                ),
            )),
            "openai" => {
                let api_key = api_key.ok_or_else(|| anyhow!("missing provider API key"))?;
                Ok((
                    Some(api_key),
                    resolve_runtime_provider_base_url(
                        provider_kind,
                        base_url_override.unwrap_or("https://api.openai.com/v1"),
                    ),
                ))
            }
            "deepseek" => {
                let api_key = api_key.ok_or_else(|| anyhow!("missing provider API key"))?;
                Ok((
                    Some(api_key),
                    resolve_runtime_provider_base_url(
                        provider_kind,
                        base_url_override.unwrap_or("https://api.deepseek.com"),
                    ),
                ))
            }
            "qwen" => Ok((
                Some(api_key.ok_or_else(|| anyhow!("missing provider API key"))?),
                resolve_runtime_provider_base_url(
                    provider_kind,
                    base_url_override
                        .unwrap_or("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
                ),
            )),
            // Any OpenAI-compatible provider (Ollama, vLLM, llama.cpp, LM Studio, etc.)
            // works when a custom base URL is configured.
            _ => match base_url_override {
                Some(url) => Ok((api_key, resolve_runtime_provider_base_url(provider_kind, url))),
                None => Err(anyhow!(
                    "unsupported provider kind without base_url_override: {provider_kind}"
                )),
            },
        }
    }
}

#[async_trait]
impl LlmGateway for UnifiedGateway {
    async fn generate(&self, request: ChatRequest) -> Result<ChatResponse> {
        let (api_key, base_url) = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
        )?;
        let (output_text, usage_json) = self
            .call_openai_compatible(OpenAiCompatibleRequest {
                provider_kind: &request.provider_kind,
                api_key: api_key.as_deref(),
                base_url: base_url.as_str(),
                model_name: &request.model_name,
                messages: vec![OpenAiCompatibleMessage {
                    role: "user".to_string(),
                    content: OpenAiCompatibleMessageContent::Text(request.prompt.clone()),
                }],
                system_prompt: request.system_prompt.as_deref(),
                temperature: request.temperature,
                top_p: request.top_p,
                max_output_tokens: request.max_output_tokens_override,
                response_format: request.response_format.as_ref(),
                extra_parameters_json: &request.extra_parameters_json,
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
        let (api_key, base_url) = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
        )?;
        let (output_text, usage_json) = self
            .call_openai_compatible_stream(
                OpenAiCompatibleRequest {
                    provider_kind: &request.provider_kind,
                    api_key: api_key.as_deref(),
                    base_url: base_url.as_str(),
                    model_name: &request.model_name,
                    messages: vec![OpenAiCompatibleMessage {
                        role: "user".to_string(),
                        content: OpenAiCompatibleMessageContent::Text(request.prompt.clone()),
                    }],
                    system_prompt: request.system_prompt.as_deref(),
                    temperature: request.temperature,
                    top_p: request.top_p,
                    max_output_tokens: request.max_output_tokens_override,
                    response_format: request.response_format.as_ref(),
                    extra_parameters_json: &request.extra_parameters_json,
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
        let (api_key, base_url) = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
        )?;

        let messages =
            request.messages.iter().map(OpenAiCompatibleToolUseMessage::from).collect::<Vec<_>>();
        let tools = request.tools.iter().map(OpenAiCompatibleToolDef::from).collect::<Vec<_>>();
        let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
            &request.provider_kind,
            request.max_output_tokens_override,
        );

        let payload = OpenAiCompatibleToolUseChatRequest {
            model: &request.model_name,
            messages,
            tools,
            temperature: request.temperature,
            top_p: request.top_p,
            max_completion_tokens,
            max_tokens,
            tool_choice: Some("auto"),
            stream: false,
            extra: request.extra_parameters_json.clone(),
        };
        let request_body =
            serde_json::to_vec(&payload).context("failed to serialize tool-use request body")?;

        let max_attempts = self.transport_retry_attempts.max(1);
        let mut last_error: Option<anyhow::Error> = None;
        for attempt in 1..=max_attempts {
            let request_builder = self
                .client
                .post(format!("{}/chat/completions", base_url))
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json");
            let request_builder = if let Some(api_key) = api_key.as_deref() {
                request_builder.bearer_auth(api_key)
            } else {
                request_builder
            };
            let response = match request_builder.body(request_body.clone()).send().await {
                Ok(response) => response,
                Err(error) => {
                    if attempt < max_attempts && is_retryable_transport_error(&error) {
                        last_error = Some(anyhow!(
                            "tool-use transport failed: provider={} attempt={}/{}: {error}",
                            request.provider_kind,
                            attempt,
                            max_attempts
                        ));
                        tokio::time::sleep(transport_retry_delay(
                            self.transport_retry_base_delay_ms,
                            attempt,
                        ))
                        .await;
                        continue;
                    }
                    return Err(error.into());
                }
            };

            let status = response.status();
            let body_text = response.text().await?;
            if !status.is_success() {
                let body = serde_json::from_str::<serde_json::Value>(&body_text)
                    .unwrap_or_else(|_| serde_json::json!({ "raw_body": body_text }));
                last_error = Some(anyhow!(
                    "tool-use request failed: provider={} status={status} body={body}",
                    request.provider_kind
                ));
                if attempt < max_attempts && is_retryable_upstream_status(status.as_u16()) {
                    tokio::time::sleep(transport_retry_delay(
                        self.transport_retry_base_delay_ms,
                        attempt,
                    ))
                    .await;
                    continue;
                }
                return Err(last_error.take().unwrap_or_else(|| {
                    anyhow!("tool-use request failed: provider={}", request.provider_kind)
                }));
            }

            let body =
                serde_json::from_str::<serde_json::Value>(&body_text).with_context(|| {
                    format!(
                        "failed to parse tool-use response from provider {}",
                        request.provider_kind
                    )
                })?;

            let choice = body
                .get("choices")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .ok_or_else(|| anyhow!("tool-use response missing choices array"))?;

            let message = choice
                .get("message")
                .ok_or_else(|| anyhow!("tool-use response choice missing message"))?;
            let finish_reason =
                choice.get("finish_reason").and_then(|v| v.as_str()).map(str::to_string);

            let output_text =
                message.get("content").map(extract_message_content_text).unwrap_or_default();

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

            return Ok(ToolUseResponse {
                provider_kind: request.provider_kind,
                model_name: request.model_name,
                output_text,
                tool_calls,
                finish_reason,
                usage_json,
            });
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow!("tool-use request failed: provider={}", request.provider_kind)
        }))
    }

    async fn generate_with_tools_stream(
        &self,
        request: ToolUseRequest,
        on_text_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ToolUseResponse> {
        let (api_key, base_url) = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
        )?;

        let messages =
            request.messages.iter().map(OpenAiCompatibleToolUseMessage::from).collect::<Vec<_>>();
        let tools = request.tools.iter().map(OpenAiCompatibleToolDef::from).collect::<Vec<_>>();
        let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
            &request.provider_kind,
            request.max_output_tokens_override,
        );

        let payload = OpenAiCompatibleToolUseChatRequest {
            model: &request.model_name,
            messages,
            tools,
            temperature: request.temperature,
            top_p: request.top_p,
            max_completion_tokens,
            max_tokens,
            tool_choice: Some("auto"),
            stream: true,
            extra: request.extra_parameters_json.clone(),
        };
        let request_body = serde_json::to_vec(&payload)
            .context("failed to serialize streaming tool-use request body")?;

        let max_attempts = self.transport_retry_attempts.max(1);
        let mut last_error: Option<anyhow::Error> = None;
        for attempt in 1..=max_attempts {
            let request_builder = self
                .client
                .post(format!("{}/chat/completions", base_url))
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "text/event-stream");
            let request_builder = if let Some(api_key) = api_key.as_deref() {
                request_builder.bearer_auth(api_key)
            } else {
                request_builder
            };
            let response = match request_builder.body(request_body.clone()).send().await {
                Ok(response) => response,
                Err(error) => {
                    if attempt < max_attempts && is_retryable_transport_error(&error) {
                        last_error = Some(anyhow!(
                            "tool-use stream transport failed: provider={} attempt={}/{}: {error}",
                            request.provider_kind,
                            attempt,
                            max_attempts
                        ));
                        tokio::time::sleep(transport_retry_delay(
                            self.transport_retry_base_delay_ms,
                            attempt,
                        ))
                        .await;
                        continue;
                    }
                    return Err(error.into());
                }
            };

            let status = response.status();
            if !status.is_success() {
                let body_text = response.text().await?;
                let body = serde_json::from_str::<serde_json::Value>(&body_text)
                    .unwrap_or_else(|_| serde_json::json!({ "raw_body": body_text }));
                last_error = Some(anyhow!(
                    "tool-use stream request failed: provider={} status={status} body={body}",
                    request.provider_kind
                ));
                if attempt < max_attempts && is_retryable_upstream_status(status.as_u16()) {
                    tokio::time::sleep(transport_retry_delay(
                        self.transport_retry_base_delay_ms,
                        attempt,
                    ))
                    .await;
                    continue;
                }
                return Err(last_error.take().unwrap_or_else(|| {
                    anyhow!("tool-use stream request failed: provider={}", request.provider_kind)
                }));
            }

            let stream_state = drain_tool_use_stream(response, on_text_delta).await?;
            let (output_text, finish_reason, usage_json, tool_calls) = stream_state.finalize();
            return Ok(ToolUseResponse {
                provider_kind: request.provider_kind,
                model_name: request.model_name,
                output_text,
                tool_calls,
                finish_reason,
                usage_json,
            });
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow!("tool-use stream request failed: provider={}", request.provider_kind)
        }))
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let (api_key, base_url) = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
        )?;

        let request_builder = self.client.post(format!("{base_url}/embeddings"));
        let request_builder = if let Some(api_key) = api_key.as_deref() {
            request_builder.bearer_auth(api_key)
        } else {
            request_builder
        };
        let response = request_builder
            .json(&Self::embedding_request_body(
                &request.model_name,
                serde_json::Value::String(request.input),
                &request.extra_parameters_json,
            ))
            .send()
            .await?;

        let status = response.status();
        let body: serde_json::Value = response.json().await?;

        if !status.is_success() {
            return Err(anyhow!(
                "embedding request failed: provider={} status={status} body={body}",
                request.provider_kind,
            ));
        }

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

        let (api_key, base_url) = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
        )?;
        let request_builder = self.client.post(format!("{base_url}/embeddings"));
        let request_builder = if let Some(api_key) = api_key.as_deref() {
            request_builder.bearer_auth(api_key)
        } else {
            request_builder
        };
        let response = request_builder
            .json(&Self::embedding_request_body(
                &request.model_name,
                serde_json::json!(request.inputs),
                &request.extra_parameters_json,
            ))
            .send()
            .await?;

        let status = response.status();
        let body: serde_json::Value = response.json().await?;

        if !status.is_success() {
            let provider_kind = request.provider_kind.clone();
            return self.embed_many_sequential(request).await.map_err(|fallback_error| {
                anyhow!(
                    "embedding batch request failed: provider={provider_kind} status={status} body={body}; fallback_error={fallback_error:#}",
                )
            });
        }

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
        let (api_key, base_url) = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
        )?;
        let image_data_url = format!(
            "data:{};base64,{}",
            request.mime_type,
            BASE64_STANDARD.encode(&request.image_bytes)
        );
        let (output_text, usage_json) = self
            .call_openai_compatible(OpenAiCompatibleRequest {
                provider_kind: &request.provider_kind,
                api_key: api_key.as_deref(),
                base_url: base_url.as_str(),
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
                response_format: None,
                extra_parameters_json: &request.extra_parameters_json,
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
        OpenAiCompatibleMessage, OpenAiCompatibleMessageContent, OpenAiCompatibleRequest,
        UnifiedGateway, consume_openai_compatible_stream_frame, extract_message_content_text,
        is_retryable_transport_error_text, is_retryable_upstream_json_parse_failure,
        is_retryable_upstream_status, transport_retry_delay,
    };
    use std::time::Duration;

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
            model_name: "gpt-5.4-mini",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
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
            model_name: "gpt-5.4-mini",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
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
    fn serializes_openai_token_limit_as_max_completion_tokens() {
        let body = OpenAiCompatibleRequest {
            provider_kind: "openai",
            api_key: Some("test"),
            base_url: "https://api.openai.com/v1",
            model_name: "gpt-5.4-mini",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: Some(16),
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
            model_name: "deepseek-chat",
            messages: vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens: Some(16),
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
        let (api_key, base_url) =
            UnifiedGateway::resolve_provider("ollama", None, Some("http://localhost:11434/v1"))
                .expect("ollama should resolve without token");
        assert!(api_key.is_none());
        assert_eq!(base_url, "http://localhost:11434/v1");
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
                "input": "ignored"
            }),
        );

        assert_eq!(
            body.get("model").and_then(serde_json::Value::as_str),
            Some("text-embedding-3-large")
        );
        assert_eq!(body.get("input"), Some(&serde_json::json!(["alpha", "beta"])));
        assert_eq!(body.get("dimensions").and_then(serde_json::Value::as_i64), Some(1024));
        assert_eq!(body.get("encoding_format").and_then(serde_json::Value::as_str), Some("float"));
    }

    #[test]
    fn retries_upstream_json_parse_failures_only_for_valid_local_json() {
        let body = serde_json::json!({
            "error": {
                "message": "We could not parse the JSON body of your request. The OpenAI API expects a JSON payload."
            }
        });
        assert!(is_retryable_upstream_json_parse_failure(400, &body, true));
        assert!(!is_retryable_upstream_json_parse_failure(400, &body, false));
        assert!(!is_retryable_upstream_json_parse_failure(422, &body, true));
    }

    #[test]
    fn recognizes_retryable_upstream_status_codes() {
        assert!(is_retryable_upstream_status(520));
        assert!(is_retryable_upstream_status(429));
        assert!(is_retryable_upstream_status(503));
        assert!(!is_retryable_upstream_status(400));
        assert!(!is_retryable_upstream_status(401));
    }

    #[test]
    fn recognizes_retryable_transport_error_strings() {
        assert!(is_retryable_transport_error_text(
            "client error (SendRequest): connection closed before message completed"
        ));
        assert!(is_retryable_transport_error_text(
            "error sending request for url (...): connection reset by peer"
        ));
        // Mid-response TLS shutdown — observed in production when LLM
        // providers terminate the TLS session abruptly under load.
        assert!(is_retryable_transport_error_text(
            "error decoding response body: request or response body error: error reading a body from connection: peer closed connection without sending TLS close_notify"
        ));
        assert!(is_retryable_transport_error_text(
            "peer closed connection without sending TLS close_notify"
        ));
        assert!(!is_retryable_transport_error_text("missing OpenAI API key"));
    }

    #[test]
    fn transport_retry_delay_follows_fixed_schedule() {
        assert_eq!(transport_retry_delay(0, 1), Duration::from_secs(1));
        assert_eq!(transport_retry_delay(0, 2), Duration::from_secs(3));
        assert_eq!(transport_retry_delay(0, 3), Duration::from_secs(10));
        assert_eq!(transport_retry_delay(0, 4), Duration::from_secs(30));
        assert_eq!(transport_retry_delay(0, 5), Duration::from_secs(90));
        // Past-end saturates to the last step rather than looping.
        assert_eq!(transport_retry_delay(0, 99), Duration::from_secs(90));
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
