use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::domains::provider_profiles::{ProviderAuthScheme, ProviderTokenLimitParameter};

use super::{ChatMessage, ChatToolDef};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub(super) enum OpenAiCompatibleMessageContent {
    Text(String),
    Parts(Vec<OpenAiCompatibleContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub(super) struct OpenAiCompatibleMessage {
    pub(super) role: String,
    pub(super) content: OpenAiCompatibleMessageContent,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct OpenAiCompatibleToolUseMessage {
    pub(super) role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) content: Option<String>,
    /// DeepSeek thinking-mode requires that any `reasoning_content` the
    /// assistant emitted on a previous turn be echoed back verbatim on
    /// subsequent calls. Other OpenAI-compatible providers ignore the
    /// field, so it stays optional and is omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) tool_calls: Vec<OpenAiCompatibleToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub(super) struct OpenAiCompatibleToolCall {
    pub(super) id: String,
    #[serde(rename = "type")]
    pub(super) call_type: String,
    pub(super) function: OpenAiCompatibleToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub(super) struct OpenAiCompatibleToolCallFunction {
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct OpenAiCompatibleToolDef {
    #[serde(rename = "type")]
    pub(super) tool_type: String,
    pub(super) function: OpenAiCompatibleToolDefFunction,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct OpenAiCompatibleToolDefFunction {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) parameters: serde_json::Value,
}

impl From<&ChatMessage> for OpenAiCompatibleToolUseMessage {
    fn from(message: &ChatMessage) -> Self {
        Self {
            role: message.role.clone(),
            content: message.content.clone(),
            reasoning_content: message.reasoning_content.clone(),
            tool_calls: message
                .tool_calls
                .iter()
                .map(|call| OpenAiCompatibleToolCall {
                    id: call.id.clone(),
                    call_type: "function".to_string(),
                    function: OpenAiCompatibleToolCallFunction {
                        name: call.name.clone(),
                        arguments: call.arguments_json.clone(),
                    },
                })
                .collect(),
            tool_call_id: message.tool_call_id.clone(),
            name: message.name.clone(),
        }
    }
}

impl From<&ChatToolDef> for OpenAiCompatibleToolDef {
    fn from(def: &ChatToolDef) -> Self {
        Self {
            tool_type: "function".to_string(),
            function: OpenAiCompatibleToolDefFunction {
                name: def.name.clone(),
                description: def.description.clone(),
                parameters: def.parameters.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct OpenAiCompatibleToolUseChatRequest<'a> {
    pub(super) model: &'a str,
    pub(super) messages: Vec<OpenAiCompatibleToolUseMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) tools: Vec<OpenAiCompatibleToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_completion_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_choice: Option<&'a str>,
    /// When true, request SSE streaming from the provider. Omitted on
    /// the wire when false so non-streaming calls keep a stable request
    /// body across providers that don't recognise the field.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) stream: bool,
    #[serde(flatten)]
    pub(super) extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub(super) struct OpenAiCompatibleImageUrl {
    pub(super) url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum OpenAiCompatibleContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenAiCompatibleImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
struct OpenAiCompatibleChatCompletionRequest {
    model: String,
    messages: Vec<OpenAiCompatibleMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<serde_json::Value>,
    #[serde(flatten)]
    extra_parameters_json: serde_json::Value,
}

pub(super) struct OpenAiCompatibleRequest<'a> {
    pub(super) provider_kind: &'a str,
    pub(super) api_key: Option<&'a str>,
    pub(super) base_url: &'a str,
    pub(super) auth_scheme: ProviderAuthScheme,
    pub(super) chat_path: String,
    pub(super) model_name: &'a str,
    pub(super) messages: Vec<OpenAiCompatibleMessage>,
    pub(super) system_prompt: Option<&'a str>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) max_output_tokens: Option<i32>,
    pub(super) token_limit_parameter: ProviderTokenLimitParameter,
    pub(super) response_format: Option<&'a serde_json::Value>,
    pub(super) extra_parameters_json: &'a serde_json::Value,
    pub(super) stream: bool,
}

impl OpenAiCompatibleRequest<'_> {
    pub(super) fn body(&self) -> Result<Vec<u8>> {
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
        let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
            self.token_limit_parameter,
            self.max_output_tokens,
        );
        let payload = OpenAiCompatibleChatCompletionRequest {
            model: self.model_name.to_string(),
            messages: request_messages,
            temperature: self.temperature,
            top_p: self.top_p,
            max_completion_tokens,
            max_tokens,
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

pub(super) fn openai_compatible_token_limit_fields(
    token_limit_parameter: ProviderTokenLimitParameter,
    max_output_tokens: Option<i32>,
) -> (Option<i32>, Option<i32>) {
    match token_limit_parameter {
        ProviderTokenLimitParameter::MaxCompletionTokens => (max_output_tokens, None),
        ProviderTokenLimitParameter::MaxTokens => (None, max_output_tokens),
    }
}

pub(super) fn extract_message_content_text(content: &serde_json::Value) -> String {
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
