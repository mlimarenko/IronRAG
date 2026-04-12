use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use super::{ChatMessage, ChatToolDef};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum OpenAiCompatibleMessageContent {
    Text(String),
    Parts(Vec<OpenAiCompatibleContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleMessage {
    pub(super) role: String,
    pub(super) content: OpenAiCompatibleMessageContent,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct OpenAiCompatibleToolUseMessage {
    pub(super) role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) tool_calls: Vec<OpenAiCompatibleToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleToolCall {
    pub(super) id: String,
    #[serde(rename = "type")]
    pub(super) call_type: String,
    pub(super) function: OpenAiCompatibleToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleToolCallFunction {
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct OpenAiCompatibleToolDef {
    #[serde(rename = "type")]
    pub(super) tool_type: String,
    pub(super) function: OpenAiCompatibleToolDefFunction,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub(super) struct OpenAiCompatibleToolUseChatRequest<'a> {
    pub(super) model: &'a str,
    pub(super) messages: Vec<OpenAiCompatibleToolUseMessage>,
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
    #[serde(flatten)]
    pub(super) extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleImageUrl {
    pub(super) url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum OpenAiCompatibleContentPart {
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
    pub(super) model_name: &'a str,
    pub(super) messages: Vec<OpenAiCompatibleMessage>,
    pub(super) system_prompt: Option<&'a str>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) max_output_tokens: Option<i32>,
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
        let (max_completion_tokens, max_tokens) =
            openai_compatible_token_limit_fields(self.provider_kind, self.max_output_tokens);
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
    provider_kind: &str,
    max_output_tokens: Option<i32>,
) -> (Option<i32>, Option<i32>) {
    match provider_kind {
        "openai" => (max_output_tokens, None),
        _ => (None, max_output_tokens),
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

pub(super) fn is_retryable_upstream_json_parse_failure(
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

pub(super) fn retryable_openai_parse_failure_error(
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
