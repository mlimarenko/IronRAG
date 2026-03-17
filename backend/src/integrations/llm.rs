use anyhow::{Result, anyhow};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::Client;
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
        }
    }

    async fn call_openai_compatible(
        &self,
        provider_kind: &str,
        api_key: &str,
        base_url: &str,
        model_name: &str,
        messages: serde_json::Value,
    ) -> Result<(String, serde_json::Value)> {
        let response = self
            .client
            .post(format!("{base_url}/chat/completions"))
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": model_name,
                "messages": messages,
            }))
            .send()
            .await?;

        let status = response.status();
        let body: serde_json::Value = response.json().await?;

        if !status.is_success() {
            return Err(anyhow!(
                "provider request failed: provider={provider_kind} status={status} body={body}",
            ));
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
        Ok((output_text, usage_json))
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
                serde_json::json!([
                    {"role": "user", "content": request.prompt}
                ]),
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
                serde_json::json!([
                    {
                        "role": "user",
                        "content": [
                            {"type": "text", "text": request.prompt},
                            {"type": "image_url", "image_url": {"url": image_data_url}}
                        ]
                    }
                ]),
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

#[cfg(test)]
mod tests {
    use super::extract_message_content_text;

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
}
