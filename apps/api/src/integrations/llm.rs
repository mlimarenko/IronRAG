use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::app::config::Settings;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub input: String,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingBatchRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub inputs: Vec<String>,
    pub api_key_override: Option<String>,
    pub base_url_override: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum OpenAiCompatibleMessageContent {
    Text(String),
    Parts(Vec<OpenAiCompatibleContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiCompatibleMessage {
    role: String,
    content: OpenAiCompatibleMessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiCompatibleImageUrl {
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiCompatibleContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenAiCompatibleImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiCompatibleChatCompletionRequest {
    model: String,
    messages: Vec<OpenAiCompatibleMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<serde_json::Value>,
    #[serde(flatten)]
    extra_parameters_json: serde_json::Value,
}

struct OpenAiCompatibleRequest<'a> {
    provider_kind: &'a str,
    api_key: &'a str,
    base_url: &'a str,
    model_name: &'a str,
    messages: Vec<OpenAiCompatibleMessage>,
    system_prompt: Option<&'a str>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_output_tokens: Option<i32>,
    response_format: Option<&'a serde_json::Value>,
    extra_parameters_json: &'a serde_json::Value,
    stream: bool,
}

impl OpenAiCompatibleRequest<'_> {
    fn body(&self) -> Result<Vec<u8>> {
        let mut request_messages =
            Vec::with_capacity(self.messages.len() + usize::from(self.system_prompt.is_some()));
        if let Some(system_prompt) =
            self.system_prompt.map(str::trim).filter(|value| !value.is_empty())
        {
            request_messages.push(OpenAiCompatibleMessage {
                role: "system".to_string(),
                content: OpenAiCompatibleMessageContent::Text(system_prompt.to_string()),
            });
        }
        request_messages.extend(self.messages.clone());
        let payload = OpenAiCompatibleChatCompletionRequest {
            model: self.model_name.to_string(),
            messages: request_messages,
            temperature: self.temperature,
            top_p: self.top_p,
            max_output_tokens: self.max_output_tokens,
            response_format: self.response_format.cloned(),
            stream: self.stream.then_some(true),
            stream_options: self.stream.then(|| serde_json::json!({ "include_usage": true })),
            extra_parameters_json: self.extra_parameters_json.clone(),
        };
        let body =
            serde_json::to_vec(&payload).context("failed to serialize provider request body")?;
        serde_json::from_slice::<serde_json::Value>(&body)
            .context("serialized provider request body was not valid json")?;
        Ok(body)
    }
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
            let response = match self
                .client
                .post(format!("{}/chat/completions", request.base_url))
                .bearer_auth(request.api_key)
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json")
                .body(request_body.clone())
                .send()
                .await
            {
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
            let response = match self
                .client
                .post(format!("{}/chat/completions", request.base_url))
                .bearer_auth(request.api_key)
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "text/event-stream")
                .body(request_body.clone())
                .send()
                .await
            {
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
    ) -> Result<(String, String)> {
        let api_key = api_key_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("missing provider API key"))?
            .to_string();
        match provider_kind {
            "openai" => {
                Ok((api_key, base_url_override.unwrap_or("https://api.openai.com/v1").to_string()))
            }
            "deepseek" => {
                Ok((api_key, base_url_override.unwrap_or("https://api.deepseek.com").to_string()))
            }
            "qwen" => Ok((
                api_key,
                base_url_override
                    .unwrap_or("https://dashscope-intl.aliyuncs.com/compatible-mode/v1")
                    .to_string(),
            )),
            // Any OpenAI-compatible provider (Ollama, vLLM, llama.cpp, LM Studio, etc.)
            // works when a custom base URL is configured.
            _ => match base_url_override {
                Some(url) => Ok((api_key, url.to_string())),
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
                api_key: api_key.as_str(),
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
                    api_key: api_key.as_str(),
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

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let (api_key, base_url) = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
        )?;

        let response = self
            .client
            .post(format!("{base_url}/embeddings"))
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": request.model_name,
                "input": request.input,
            }))
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
        let response = self
            .client
            .post(format!("{base_url}/embeddings"))
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": request.model_name,
                "input": request.inputs,
            }))
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
                api_key: api_key.as_str(),
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

fn extract_message_content_text(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }

    let Some(parts) = content.as_array() else {
        return String::new();
    };

    let mut rendered = String::new();
    for part in parts.iter().filter_map(|item| {
        item.get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                item.get("text")
                    .and_then(|value| value.get("value"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| {
                item.get("type")
                    .and_then(serde_json::Value::as_str)
                    .filter(|kind| *kind == "text")
                    .and_then(|_| item.get("content"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
    }) {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(&part);
    }
    rendered
}

fn consume_openai_compatible_stream_frame(
    frame: &str,
    output_text: &mut String,
    usage_json: &mut serde_json::Value,
    on_delta: &mut (dyn FnMut(String) + Send),
) -> Result<bool> {
    if frame.trim().is_empty() || frame.starts_with(':') {
        return Ok(false);
    }

    let mut data_lines = Vec::new();
    for raw_line in frame.split('\n') {
        let line = raw_line.trim_end();
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
    }

    if data_lines.is_empty() {
        return Ok(false);
    }

    let mut payload_text = String::new();
    for (index, line) in data_lines.iter().enumerate() {
        if index > 0 {
            payload_text.push('\n');
        }
        payload_text.push_str(line);
    }
    if payload_text.trim() == "[DONE]" {
        return Ok(true);
    }

    let payload: serde_json::Value = serde_json::from_str(&payload_text)
        .context("failed to parse upstream streaming payload as json")?;
    let delta = extract_stream_delta_text(&payload);
    if !delta.is_empty() {
        output_text.push_str(&delta);
        on_delta(delta);
    }
    if let Some(usage) = payload.get("usage").filter(|value| !value.is_null()) {
        *usage_json = usage.clone();
    }
    Ok(false)
}

async fn drain_openai_compatible_stream(
    mut response: reqwest::Response,
    on_delta: &mut (dyn FnMut(String) + Send),
) -> Result<(String, serde_json::Value)> {
    let mut output_text = String::new();
    let mut usage_json = serde_json::json!({});
    let mut buffer = String::new();

    while let Some(chunk) = response.chunk().await? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        if buffer.contains('\r') {
            buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");
        }
        while let Some(boundary) = buffer.find("\n\n") {
            let frame = buffer[..boundary].to_string();
            buffer = buffer[boundary + 2..].to_string();
            if consume_openai_compatible_stream_frame(
                &frame,
                &mut output_text,
                &mut usage_json,
                on_delta,
            )? {
                return Ok((output_text, usage_json));
            }
        }
    }

    if !buffer.trim().is_empty() {
        let _ = consume_openai_compatible_stream_frame(
            &buffer,
            &mut output_text,
            &mut usage_json,
            on_delta,
        )?;
    }

    Ok((output_text, usage_json))
}

fn extract_stream_delta_text(payload: &serde_json::Value) -> String {
    let Some(choices) = payload.get("choices").and_then(serde_json::Value::as_array) else {
        return String::new();
    };

    let mut rendered = String::new();
    for value in choices.iter().filter_map(|choice| {
        choice
            .get("delta")
            .and_then(|delta| delta.get("content"))
            .map(extract_message_content_text)
            .filter(|value| !value.is_empty())
    }) {
        rendered.push_str(&value);
    }
    rendered
}

fn is_retryable_upstream_json_parse_failure(
    status_code: u16,
    body: &serde_json::Value,
    request_body_is_valid_json: bool,
) -> bool {
    if status_code != 400 || !request_body_is_valid_json {
        return false;
    }

    let normalized = body.to_string().to_ascii_lowercase();
    normalized.contains("could not parse the json body of your request")
        || normalized.contains("json body of your request")
        || normalized.contains("expects a json payload")
        || (normalized.contains("invalid_request_error")
            && normalized.contains("json payload")
            && normalized.contains("status"))
}

const fn is_retryable_upstream_status(status_code: u16) -> bool {
    matches!(
        status_code,
        408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 520 | 521 | 522 | 523 | 524 | 529
    )
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error.is_connect()
        || is_retryable_transport_error_text(&error.to_string())
}

fn is_retryable_transport_error_text(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("connection closed before message completed")
        || normalized.contains("connection reset")
        || normalized.contains("broken pipe")
        || normalized.contains("unexpected eof")
        || normalized.contains("http2")
        || normalized.contains("sendrequest")
        || normalized.contains("error sending request")
}

const fn transport_retry_delay(base_delay_ms: u64, attempt: usize) -> Duration {
    let attempt = if attempt == 0 { 0 } else { attempt - 1 };
    let shift = if attempt > 4 { 4 } else { attempt };
    let multiplier = 1_u64 << shift;
    Duration::from_millis(base_delay_ms.saturating_mul(multiplier))
}

fn retryable_openai_parse_failure_error(
    provider_kind: &str,
    attempt: usize,
    last_error: Option<&anyhow::Error>,
) -> anyhow::Error {
    last_error.map_or_else(
        || anyhow!(
            "upstream protocol failure: upstream rejected a locally valid JSON request body after {attempt} attempt(s) for provider={provider_kind}"
        ),
        |error| anyhow!(
            "upstream protocol failure: upstream rejected a locally valid JSON request body after {attempt} attempt(s): {error}"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        OpenAiCompatibleMessage, OpenAiCompatibleMessageContent, OpenAiCompatibleRequest,
        consume_openai_compatible_stream_frame, extract_message_content_text,
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
            api_key: "test",
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
            api_key: "test",
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
        assert!(!is_retryable_transport_error_text("missing OpenAI API key"));
    }

    #[test]
    fn transport_retry_delay_is_bounded_backoff() {
        assert_eq!(transport_retry_delay(250, 1), Duration::from_millis(250));
        assert_eq!(transport_retry_delay(250, 2), Duration::from_millis(500));
        assert_eq!(transport_retry_delay(250, 5), Duration::from_millis(4000));
    }

    #[test]
    fn consumes_stream_delta_frames() {
        let mut output_text = String::new();
        let mut usage_json = serde_json::json!({});
        let mut emitted = String::new();
        let done = consume_openai_compatible_stream_frame(
            r#"data: {"choices":[{"delta":{"content":"Привет"}}]}"#,
            &mut output_text,
            &mut usage_json,
            &mut |delta| emitted.push_str(&delta),
        )
        .expect("stream frame should parse");
        assert!(!done);
        assert_eq!(output_text, "Привет");
        assert_eq!(emitted, "Привет");
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
