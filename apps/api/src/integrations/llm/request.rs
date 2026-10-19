use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use crate::domains::provider_profiles::{
    ANTHROPIC_MESSAGES_RUNTIME_KIND, GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
    ProviderStructuredOutputMode, REASONING_MODE_FIELD, ReasoningWireControl,
};

use super::{
    ChatRequest, ToolUseRequest, UnifiedGateway,
    openai_compatible::{
        OpenAiCompatibleMessage, OpenAiCompatibleRequest, OpenAiCompatibleToolDef,
        OpenAiCompatibleToolUseChatRequest, OpenAiCompatibleToolUseMessage,
        openai_compatible_token_limit_fields,
    },
    provider::ResolvedProviderRuntime,
};

impl UnifiedGateway {
    /// Build the provider body's extra parameters from the binding's
    /// `extra_parameters_json`.
    ///
    /// Internal envelopes are never forwarded: `_provider*` metadata and the
    /// typed `reasoningMode` selection are stripped. A resolved wire control is
    /// applied here — the single emitter every OpenAI-compatible caller (chat,
    /// stream, tool use, vision) shares — clearing any automatic reasoning
    /// parameter first so exactly one control is sent. `reasoning_wire` is
    /// `None` when the binding selected nothing, so the automatic policy and
    /// the binding's own parameters are forwarded untouched.
    pub(super) fn upstream_extra_parameters(
        extra_parameters_json: &serde_json::Value,
        reasoning_wire: Option<ReasoningWireControl>,
    ) -> serde_json::Value {
        let mut filtered = extra_parameters_json
            .as_object()
            .map(|extra| {
                extra
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
                            && key.as_str() != REASONING_MODE_FIELD
                    })
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();
        if let Some(control) = reasoning_wire {
            control.apply(&mut filtered);
        }
        serde_json::Value::Object(filtered)
    }
}

pub(super) struct OpenAiCompatibleRequestOptions<'a> {
    pub(super) system_prompt: Option<&'a str>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) max_output_tokens: Option<i32>,
    pub(super) response_format: Option<&'a serde_json::Value>,
    pub(super) extra_parameters_json: &'a serde_json::Value,
    pub(super) stream: bool,
}

#[derive(Debug)]
pub(super) struct ChatRequestControls {
    pub(super) response_format: Option<serde_json::Value>,
    pub(super) system_prompt: Option<String>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
}

/// Resolve the same structured-output and sampling policy for chat and streaming.
pub(super) fn chat_request_controls(
    request: &ChatRequest,
    resolved: &ResolvedProviderRuntime,
) -> Result<ChatRequestControls> {
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
    Ok(ChatRequestControls { response_format, system_prompt, temperature, top_p })
}

pub(super) fn openai_compatible_request<'a>(
    provider_kind: &'a str,
    model_name: &'a str,
    messages: Vec<OpenAiCompatibleMessage>,
    resolved: &'a ResolvedProviderRuntime,
    options: OpenAiCompatibleRequestOptions<'a>,
) -> OpenAiCompatibleRequest<'a> {
    OpenAiCompatibleRequest {
        provider_kind,
        api_key: resolved.api_key.as_deref(),
        base_url: resolved.base_url.as_str(),
        auth_scheme: resolved.runtime.auth_scheme,
        chat_path: resolved.runtime.chat_path.clone(),
        model_name,
        messages,
        system_prompt: options.system_prompt,
        temperature: options.temperature,
        top_p: options.top_p,
        max_output_tokens: options.max_output_tokens,
        token_limit_parameter: resolved.runtime.token_limit_parameter,
        response_format: options.response_format,
        extra_parameters_json: options.extra_parameters_json,
        stream: options.stream,
    }
}

pub(super) fn tool_use_request_payload<'a>(
    request: &'a ToolUseRequest,
    resolved: &ResolvedProviderRuntime,
    stream: bool,
) -> OpenAiCompatibleToolUseChatRequest<'a> {
    let messages = request.messages.iter().map(OpenAiCompatibleToolUseMessage::from).collect();
    let tools = request.tools.iter().map(OpenAiCompatibleToolDef::from).collect::<Vec<_>>();
    let max_output_tokens =
        resolved.request_policy.tool_max_output_tokens(request.max_output_tokens_override);
    let (max_completion_tokens, max_tokens) = openai_compatible_token_limit_fields(
        resolved.runtime.token_limit_parameter,
        max_output_tokens,
    );
    let tool_choice =
        resolved.request_policy.tool_choice(!tools.is_empty(), request.require_tool_call);
    let (temperature, top_p) =
        resolved.request_policy.sampling_params(request.temperature, request.top_p);

    OpenAiCompatibleToolUseChatRequest {
        model: &request.model_name,
        messages,
        tools,
        temperature,
        top_p,
        max_completion_tokens,
        max_tokens,
        tool_choice,
        stream,
        extra: UnifiedGateway::upstream_extra_parameters(
            &request.extra_parameters_json,
            resolved.reasoning_wire,
        ),
    }
}

/// Model, prompt, and sampling controls shared by every native chat runtime.
///
/// Grouping them keeps the runtime selector (`runtime_kind`) separate from the
/// payload it selects, and keeps the call sites from passing seven positional
/// values.
pub(super) struct NativeChatRequest<'a> {
    pub(super) model_name: &'a str,
    pub(super) prompt: &'a str,
    pub(super) system_prompt: Option<&'a str>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) max_output_tokens: Option<i32>,
    pub(super) image: Option<(&'a [u8], &'a str)>,
}

pub(super) fn native_chat_request_body(
    runtime_kind: &str,
    request: NativeChatRequest<'_>,
) -> Result<serde_json::Value> {
    match runtime_kind {
        ANTHROPIC_MESSAGES_RUNTIME_KIND => Ok(anthropic_native_chat_request_body(&request)),
        GEMINI_GENERATE_CONTENT_RUNTIME_KIND => Ok(gemini_native_chat_request_body(&request)),
        _ => Err(anyhow!("unsupported native provider runtime kind")),
    }
}

fn anthropic_native_chat_request_body(request: &NativeChatRequest<'_>) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), serde_json::json!(request.model_name));
    body.insert(
        "max_tokens".to_string(),
        serde_json::json!(request.max_output_tokens.unwrap_or(1024)),
    );
    if let Some(system) = request.system_prompt.filter(|value| !value.trim().is_empty()) {
        body.insert("system".to_string(), serde_json::json!(system));
    }
    if let Some(value) = request.temperature {
        body.insert("temperature".to_string(), serde_json::json!(value));
    }
    if let Some(value) = request.top_p {
        body.insert("top_p".to_string(), serde_json::json!(value));
    }
    body.insert(
        "messages".to_string(),
        serde_json::json!([{"role":"user", "content":anthropic_message_content(request)}]),
    );
    serde_json::Value::Object(body)
}

fn anthropic_message_content(request: &NativeChatRequest<'_>) -> serde_json::Value {
    request.image.map_or_else(
        || serde_json::json!(request.prompt),
        |(bytes, mime_type)| {
            serde_json::json!([
                {"type":"text", "text":request.prompt},
                {"type":"image", "source":{"type":"base64", "media_type":mime_type,
                    "data":BASE64_STANDARD.encode(bytes)}}
            ])
        },
    )
}

fn gemini_native_chat_request_body(request: &NativeChatRequest<'_>) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "contents".to_string(),
        serde_json::json!([{"role":"user", "parts":gemini_message_parts(request)}]),
    );
    if let Some(system) = request.system_prompt.filter(|value| !value.trim().is_empty()) {
        body.insert(
            "systemInstruction".to_string(),
            serde_json::json!({"parts":[{"text":system}]}),
        );
    }
    let config = gemini_generation_config(request);
    if !config.is_empty() {
        body.insert("generationConfig".to_string(), serde_json::Value::Object(config));
    }
    serde_json::Value::Object(body)
}

fn gemini_message_parts(request: &NativeChatRequest<'_>) -> serde_json::Value {
    request.image.map_or_else(
        || serde_json::json!([{"text":request.prompt}]),
        |(bytes, mime_type)| {
            serde_json::json!([
                {"text":request.prompt},
                {"inlineData":{"mimeType":mime_type, "data":BASE64_STANDARD.encode(bytes)}}
            ])
        },
    )
}

fn gemini_generation_config(
    request: &NativeChatRequest<'_>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut config = serde_json::Map::new();
    if let Some(value) = request.temperature {
        config.insert("temperature".to_string(), serde_json::json!(value));
    }
    if let Some(value) = request.top_p {
        config.insert("topP".to_string(), serde_json::json!(value));
    }
    if let Some(value) = request.max_output_tokens {
        config.insert("maxOutputTokens".to_string(), serde_json::json!(value));
    }
    config
}

pub(super) fn provider_response_format(
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

pub(super) fn provider_system_prompt(
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
        "For runtimes that accept JSON object mode, this JSON Schema is the required output \
contract. Return exactly one JSON object that conforms to it. Use only the field names defined by \
this schema; do not invent alternate keys.\nJSON Schema:\n",
    );
    system_prompt.push_str(&schema_json);

    Ok(Some(system_prompt))
}
