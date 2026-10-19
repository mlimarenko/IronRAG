use anyhow::Result;

use crate::{
    domains::provider_profiles::{
        ANTHROPIC_MESSAGES_RUNTIME_KIND, GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
    },
    integrations::retry::{ProviderCallError, provider_status_error_from_read},
    shared::{
        outbound_http::read_response_bytes_with_limit,
        provider_http::{PROVIDER_ERROR_BODY_MAX_BYTES, PROVIDER_SUCCESS_BODY_MAX_BYTES},
    },
};

use super::{ChatToolCall, openai_compatible::extract_message_content_text};

pub(super) async fn read_provider_response_body(
    response: reqwest::Response,
    provider_kind: &str,
    operation: &str,
) -> Result<(reqwest::StatusCode, reqwest::header::HeaderMap, Option<Vec<u8>>), ProviderCallError> {
    let status = response.status();
    let headers = response.headers().clone();
    let body_limit = if status.is_success() {
        PROVIDER_SUCCESS_BODY_MAX_BYTES
    } else {
        PROVIDER_ERROR_BODY_MAX_BYTES
    };
    let body_bytes = match read_response_bytes_with_limit(response, body_limit).await {
        Ok(bytes) => Some(bytes),
        // A non-success body we could not read is NOT an empty body. Carry the
        // read outcome instead of collapsing it to zero bytes, so the error
        // boundary never classifies an oversize/truncated 404 as blank.
        Err(_error) if !status.is_success() => None,
        Err(error) => {
            return Err(ProviderCallError::response_policy(
                format!("failed to read {operation} response body: provider={provider_kind}"),
                error,
            ));
        }
    };
    Ok((status, headers, body_bytes))
}

pub(super) async fn provider_http_error_from_response(
    response: reqwest::Response,
    provider_kind: &str,
) -> ProviderCallError {
    let status = response.status();
    let headers = response.headers().clone();
    provider_status_error_from_read(
        provider_kind,
        status,
        &headers,
        read_response_bytes_with_limit(response, PROVIDER_ERROR_BODY_MAX_BYTES).await.ok(),
    )
}

pub(super) fn parse_provider_json_body(
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

/// Response field carrying the actual service tier a provider served a request
/// under. Billing prices the tier the provider confirms here, never the tier the
/// binding requested: a provider may answer a `flex` request on the standard
/// tier under load and charge the standard rate for it.
pub(super) const SERVICE_TIER_FIELD: &str = "service_tier";

/// Applies one provider response (or streaming frame) to the accumulated usage
/// object. A non-null `usage` object replaces the counters; the response's
/// top-level `service_tier` is preserved alongside them, because a streaming
/// frame may carry the tier in a different frame than the final usage counters.
pub(super) fn overlay_provider_usage(
    usage_json: &mut serde_json::Value,
    payload: &serde_json::Value,
) {
    let service_tier = payload
        .get(SERVICE_TIER_FIELD)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            usage_json
                .get(SERVICE_TIER_FIELD)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    if let Some(usage) = payload.get("usage").filter(|value| !value.is_null()) {
        *usage_json = usage.clone();
    }
    if let Some(service_tier) = service_tier
        && let Some(object) = usage_json.as_object_mut()
    {
        object.insert(SERVICE_TIER_FIELD.to_string(), serde_json::Value::String(service_tier));
    }
}

/// Usage object persisted for one OpenAI-compatible provider call: the
/// response's `usage` counters with the actual `service_tier` preserved. The
/// single extraction point for every response shape the gateway recognises.
pub(super) fn provider_usage_json(body: &serde_json::Value) -> serde_json::Value {
    let mut usage_json = serde_json::json!({});
    overlay_provider_usage(&mut usage_json, body);
    usage_json
}

pub(super) fn native_chat_response(
    runtime_kind: &str,
    body: serde_json::Value,
) -> std::result::Result<(String, serde_json::Value), ProviderCallError> {
    let output_text = match runtime_kind {
        ANTHROPIC_MESSAGES_RUNTIME_KIND => body
            .get("content")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        GEMINI_GENERATE_CONTENT_RUNTIME_KIND => body
            .pointer("/candidates/0/content/parts")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        _ => return Err(ProviderCallError::protocol("unsupported native provider runtime kind")),
    };
    let usage_json = body
        .get("usage")
        .or_else(|| body.get("usageMetadata"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Ok((output_text, usage_json))
}

/// Parsed fields extracted from a non-streaming OpenAI-compatible
/// chat-completions response: `(output_text, tool_calls, finish_reason,
/// usage_json, reasoning_content)`. Bundled as a tuple alias to keep the
/// parser signature legible at the call sites.
pub(super) type ParsedToolUseResponse =
    (String, Vec<ChatToolCall>, Option<String>, serde_json::Value, Option<String>);

pub(super) fn parse_tool_use_response(
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

    let usage_json = provider_usage_json(body);
    Ok((output_text, tool_calls, finish_reason, usage_json, reasoning_content))
}
