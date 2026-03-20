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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingBatchRequest {
    pub provider_kind: String,
    pub model_name: String,
    pub inputs: Vec<String>,
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
    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse>;
    async fn embed_many(&self, request: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse>;
    async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse>;
}

#[derive(Clone)]
pub struct UnifiedGateway {
    client: Client,
    openai_api_key: Option<String>,
    deepseek_api_key: Option<String>,
    qwen_api_key: Option<String>,
    qwen_api_base_url: String,
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
            openai_api_key: settings.openai_api_key.clone(),
            deepseek_api_key: settings.deepseek_api_key.clone(),
            qwen_api_key: settings.qwen_api_key.clone(),
            qwen_api_base_url: settings.qwen_api_base_url.clone(),
            transport_retry_attempts: settings.llm_transport_retry_attempts.max(1),
            transport_retry_base_delay_ms: settings.llm_transport_retry_base_delay_ms.max(25),
        }
    }

    async fn call_openai_compatible(
        &self,
        provider_kind: &str,
        api_key: &str,
        base_url: &str,
        model_name: &str,
        messages: Vec<OpenAiCompatibleMessage>,
    ) -> Result<(String, serde_json::Value)> {
        let request_body = serialize_openai_compatible_chat_request(model_name, messages)?;
        let request_body_is_valid_json = true;
        let max_attempts = self.transport_retry_attempts.max(1);

        let mut last_error = None;
        for attempt in 1..=max_attempts {
            let response = match self
                .client
                .post(format!("{base_url}/chat/completions"))
                .bearer_auth(api_key)
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
                            "provider transport failed: provider={provider_kind} attempt={attempt}/{max_attempts} error={error}",
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
                    "provider request failed: provider={provider_kind} status={status} body={body}",
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
                    return Err(anyhow!(
                        "upstream protocol failure: upstream rejected a locally valid JSON request body after {attempt} attempt(s): {}",
                        last_error
                            .as_ref()
                            .expect("last_error is set before retryable parse failure returns"),
                    ));
                }
                return Err(last_error.take().unwrap_or_else(|| {
                    anyhow!("provider request failed: provider={provider_kind}")
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

            let _ = provider_kind;
            return Ok((output_text, usage_json));
        }

        Err(last_error
            .unwrap_or_else(|| anyhow!("provider request failed: provider={provider_kind}")))
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

    fn resolve_provider<'a>(&'a self, provider_kind: &str) -> Result<(&'a str, &'a str)> {
        match provider_kind {
            "openai" => Ok((
                self.openai_api_key.as_deref().ok_or_else(|| anyhow!("missing OpenAI API key"))?,
                "https://api.openai.com/v1",
            )),
            "deepseek" => Ok((
                self.deepseek_api_key
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing DeepSeek API key"))?,
                "https://api.deepseek.com",
            )),
            "qwen" => Ok((
                self.qwen_api_key.as_deref().ok_or_else(|| anyhow!("missing Qwen API key"))?,
                self.qwen_api_base_url.as_str(),
            )),
            other => Err(anyhow!("unsupported provider kind: {other}")),
        }
    }
}

#[async_trait]
impl LlmGateway for UnifiedGateway {
    async fn generate(&self, request: ChatRequest) -> Result<ChatResponse> {
        let (api_key, base_url) = self.resolve_provider(&request.provider_kind)?;
        let (output_text, usage_json) = self
            .call_openai_compatible(
                &request.provider_kind,
                api_key,
                base_url,
                &request.model_name,
                vec![OpenAiCompatibleMessage {
                    role: "user".to_string(),
                    content: OpenAiCompatibleMessageContent::Text(request.prompt.clone()),
                }],
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
        let (api_key, base_url) = self.resolve_provider(&request.provider_kind)?;

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

        let (api_key, base_url) = self.resolve_provider(&request.provider_kind)?;
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
                    "embedding batch request failed: provider={} status={status} body={body}; fallback_error={fallback_error:#}",
                    provider_kind,
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
        let (api_key, base_url) = self.resolve_provider(&request.provider_kind)?;
        let image_data_url = format!(
            "data:{};base64,{}",
            request.mime_type,
            BASE64_STANDARD.encode(&request.image_bytes)
        );
        let (output_text, usage_json) = self
            .call_openai_compatible(
                &request.provider_kind,
                api_key,
                base_url,
                &request.model_name,
                vec![OpenAiCompatibleMessage {
                    role: "user".to_string(),
                    content: OpenAiCompatibleMessageContent::Parts(vec![
                        OpenAiCompatibleContentPart::Text { text: request.prompt.clone() },
                        OpenAiCompatibleContentPart::ImageUrl {
                            image_url: OpenAiCompatibleImageUrl { url: image_data_url },
                        },
                    ]),
                }],
            )
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

    parts
        .iter()
        .filter_map(|item| {
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
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn serialize_openai_compatible_chat_request(
    model_name: &str,
    messages: Vec<OpenAiCompatibleMessage>,
) -> Result<Vec<u8>> {
    let payload = OpenAiCompatibleChatCompletionRequest { model: model_name.to_string(), messages };
    let body = serde_json::to_vec(&payload).context("failed to serialize provider request body")?;
    serde_json::from_slice::<serde_json::Value>(&body)
        .context("serialized provider request body was not valid json")?;
    Ok(body)
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

fn is_retryable_upstream_status(status_code: u16) -> bool {
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

fn transport_retry_delay(base_delay_ms: u64, attempt: usize) -> Duration {
    let multiplier = 1_u64 << attempt.saturating_sub(1).min(4);
    Duration::from_millis(base_delay_ms.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests {
    use super::{
        OpenAiCompatibleMessage, OpenAiCompatibleMessageContent, extract_message_content_text,
        is_retryable_transport_error_text, is_retryable_upstream_json_parse_failure,
        is_retryable_upstream_status, serialize_openai_compatible_chat_request,
        transport_retry_delay,
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
        let body = serialize_openai_compatible_chat_request(
            "gpt-5.4-mini",
            vec![OpenAiCompatibleMessage {
                role: "user".to_string(),
                content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
            }],
        )
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
}
