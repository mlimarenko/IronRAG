use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize as _;

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
