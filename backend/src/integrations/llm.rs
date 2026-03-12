use anyhow::{Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

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
pub struct EmbeddingResponse {
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: usize,
    pub embedding: Vec<f32>,
    pub usage_json: serde_json::Value,
}

#[async_trait]
pub trait LlmGateway: Send + Sync {
    async fn generate(&self, request: ChatRequest) -> Result<ChatResponse>;
    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse>;
}

#[derive(Clone)]
pub struct UnifiedGateway {
    client: Client,
    openai_api_key: Option<String>,
    deepseek_api_key: Option<String>,
}

impl UnifiedGateway {
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            client: Client::new(),
            openai_api_key: settings.openai_api_key.clone(),
            deepseek_api_key: settings.deepseek_api_key.clone(),
        }
    }

    async fn call_openai_compatible(
        &self,
        provider_kind: &str,
        api_key: &str,
        base_url: &str,
        model_name: &str,
        prompt: &str,
    ) -> Result<ChatResponse> {
        let response = self
            .client
            .post(format!("{base_url}/chat/completions"))
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": model_name,
                "messages": [
                    {"role": "user", "content": prompt}
                ]
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
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let usage_json = body.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));

        Ok(ChatResponse {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            output_text,
            usage_json,
        })
    }
}

#[async_trait]
impl LlmGateway for UnifiedGateway {
    async fn generate(&self, request: ChatRequest) -> Result<ChatResponse> {
        match request.provider_kind.as_str() {
            "openai" => {
                let key = self
                    .openai_api_key
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing OpenAI API key"))?;
                self.call_openai_compatible(
                    "openai",
                    key,
                    "https://api.openai.com/v1",
                    &request.model_name,
                    &request.prompt,
                )
                .await
            }
            "deepseek" => {
                let key = self
                    .deepseek_api_key
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing DeepSeek API key"))?;
                self.call_openai_compatible(
                    "deepseek",
                    key,
                    "https://api.deepseek.com",
                    &request.model_name,
                    &request.prompt,
                )
                .await
            }
            other => Err(anyhow!("unsupported provider kind: {other}")),
        }
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let (api_key, base_url) = match request.provider_kind.as_str() {
            "openai" => (
                self.openai_api_key.as_deref().ok_or_else(|| anyhow!("missing OpenAI API key"))?,
                "https://api.openai.com/v1",
            ),
            "deepseek" => (
                self.deepseek_api_key
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing DeepSeek API key"))?,
                "https://api.deepseek.com",
            ),
            other => return Err(anyhow!("unsupported provider kind: {other}")),
        };

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
            .and_then(|v| v.as_array())
            .map(|arr| {
                #[allow(clippy::cast_possible_truncation)]
                arr.iter()
                    .filter_map(serde_json::Value::as_f64)
                    .filter(|value| value.is_finite())
                    .filter(|value| *value >= f64::from(f32::MIN) && *value <= f64::from(f32::MAX))
                    .map(|value| value as f32)
                    .collect::<Vec<f32>>()
            })
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
}
