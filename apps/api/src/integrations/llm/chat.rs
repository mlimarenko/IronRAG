use anyhow::Result;
use reqwest::{
    Method,
    header::{ACCEPT, CONTENT_TYPE},
};
use tracing::Instrument as _;

use crate::{
    domains::provider_profiles::{ANTHROPIC_MESSAGES_RUNTIME_KIND, ProviderRuntimeProfile},
    integrations::retry::{ProviderCallError, provider_status_error_from_read, with_retry},
};

use super::{
    ChatCompletionStatus, UnifiedGateway,
    openai_compatible::OpenAiCompatibleRequest,
    provider::{apply_provider_auth, provider_endpoint_url},
    request::{NativeChatRequest, native_chat_request_body},
    response::{
        native_chat_response, openai_chat_response, parse_provider_json_body,
        provider_http_error_from_response, read_provider_response_body,
    },
    streaming::drain_openai_compatible_stream,
};

pub(super) struct NativeChatCall<'a> {
    pub(super) provider_kind: &'a str,
    pub(super) runtime: &'a ProviderRuntimeProfile,
    pub(super) api_key: Option<&'a str>,
    pub(super) base_url: &'a str,
    pub(super) allow_private_network: bool,
    pub(super) model_name: &'a str,
    pub(super) prompt: &'a str,
    pub(super) system_prompt: Option<&'a str>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) max_output_tokens: Option<i32>,
    pub(super) image: Option<(&'a [u8], &'a str)>,
    pub(super) dispatch: Option<&'a crate::shared::provider_http::ProviderDispatchContext>,
}

/// The span for one logical chat call: request preparation, every retry
/// attempt and the response parse.
///
/// The per-attempt `provider.http.request` span stays its child, so one
/// `llm.chat` node shows how many transport attempts the call needed, and
/// `provider.call.id` connects it to the billing row of the same call.
pub(super) fn chat_span(provider_kind: &str, model_name: &str) -> tracing::Span {
    let span = tracing::info_span!(
        "llm.chat",
        otel.kind = "client",
        "provider.kind" = provider_kind,
        "provider.model" = model_name,
        "provider.call.id" = tracing::field::Empty,
        "llm.completion.status" = tracing::field::Empty,
    );
    if let Some(provider_call_id) = crate::observability::current_provider_call_id() {
        span.record("provider.call.id", provider_call_id);
    }
    span
}

/// Records the protocol-reported outcome on the chat span. A provider that
/// truncated or refused the answer is otherwise indistinguishable in a trace
/// from a normal completion.
fn record_chat_status(
    span: &tracing::Span,
    result: &Result<(String, serde_json::Value, ChatCompletionStatus)>,
) {
    let Ok((_, _, status)) = result else {
        return;
    };
    let label = match status {
        ChatCompletionStatus::Complete => "complete",
        ChatCompletionStatus::Truncated => "truncated",
        ChatCompletionStatus::Refused => "refused",
        ChatCompletionStatus::Unknown => "unknown",
    };
    span.record("llm.completion.status", label);
}

impl UnifiedGateway {
    pub(super) async fn call_native_chat(
        &self,
        call: NativeChatCall<'_>,
    ) -> Result<(String, serde_json::Value, ChatCompletionStatus)> {
        let span = chat_span(call.provider_kind, call.model_name);
        let result = self.call_native_chat_inner(call).instrument(span.clone()).await;
        record_chat_status(&span, &result);
        result
    }

    async fn call_native_chat_inner(
        &self,
        call: NativeChatCall<'_>,
    ) -> Result<(String, serde_json::Value, ChatCompletionStatus)> {
        let endpoint_path = call.runtime.chat_path.replace("{model}", call.model_name);
        let endpoint_url =
            provider_endpoint_url(call.provider_kind, call.base_url, &endpoint_path)?;
        let target =
            self.transport.prepare(&endpoint_url, call.allow_private_network).await.map_err(
                |error| {
                    ProviderCallError::target_preparation(
                        &format!("provider target policy failed: provider={}", call.provider_kind),
                        &error,
                    )
                },
            )?;
        let body = native_chat_request_body(
            call.runtime.kind.as_str(),
            NativeChatRequest {
                model_name: call.model_name,
                prompt: call.prompt,
                system_prompt: call.system_prompt,
                temperature: call.temperature,
                top_p: call.top_p,
                max_output_tokens: call.max_output_tokens,
                image: call.image,
            },
        )?;

        with_retry(
            || async {
                let _provider_guard =
                    self.begin_provider_attempt(call.provider_kind, call.base_url).await?;
                let request_builder = Self::prepared_request(
                    &target,
                    Method::POST,
                    &endpoint_url,
                    call.provider_kind,
                )?
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json");
                let request_builder =
                    apply_provider_auth(request_builder, call.runtime.auth_scheme, call.api_key);
                let request_builder = if call.runtime.kind == ANTHROPIC_MESSAGES_RUNTIME_KIND {
                    request_builder.header("anthropic-version", "2023-06-01")
                } else {
                    request_builder
                };
                let dispatch = call.dispatch.ok_or_else(|| {
                    ProviderCallError::protocol("provider dispatch authority is missing")
                })?;
                let response = self
                    .send_provider_request(request_builder.json(&body), dispatch, "native-chat")
                    .await?;
                let (status, headers, body_bytes) =
                    read_provider_response_body(response, call.provider_kind, "native chat")
                        .await?;
                if !status.is_success() {
                    return Err(provider_status_error_from_read(
                        call.provider_kind,
                        status,
                        &headers,
                        body_bytes,
                    ));
                }
                let body_bytes = body_bytes.unwrap_or_default();
                let response =
                    parse_provider_json_body(&body_bytes, call.provider_kind, "native chat")?;
                native_chat_response(call.runtime.kind.as_str(), response)
            },
            self.retry_policy,
        )
        .await
        .map_err(Into::into)
    }

    pub(super) async fn call_openai_compatible(
        &self,
        request: OpenAiCompatibleRequest<'_>,
        allow_private_network: bool,
    ) -> Result<(String, serde_json::Value, ChatCompletionStatus)> {
        let span = chat_span(request.provider_kind, request.model_name);
        let result = self
            .call_openai_compatible_inner(request, allow_private_network)
            .instrument(span.clone())
            .await;
        record_chat_status(&span, &result);
        result
    }

    async fn call_openai_compatible_inner(
        &self,
        request: OpenAiCompatibleRequest<'_>,
        allow_private_network: bool,
    ) -> Result<(String, serde_json::Value, ChatCompletionStatus)> {
        let request_body = request.body()?;
        let endpoint_url =
            provider_endpoint_url(request.provider_kind, request.base_url, &request.chat_path)?;
        let target = self.transport.prepare(&endpoint_url, allow_private_network).await.map_err(
            |error| {
                ProviderCallError::target_preparation(
                    &format!("provider target policy failed: provider={}", request.provider_kind),
                    &error,
                )
            },
        )?;

        with_retry(
            || async {
                let _provider_guard =
                    self.begin_provider_attempt(request.provider_kind, request.base_url).await?;
                let request_builder = Self::prepared_provider_request(
                    &target,
                    &endpoint_url,
                    request.provider_kind,
                    "application/json",
                    request.auth_scheme,
                    request.api_key,
                )?;
                let dispatch = request.dispatch.ok_or_else(|| {
                    ProviderCallError::protocol("provider dispatch authority is missing")
                })?;
                let response = self
                    .send_provider_request(
                        request_builder.body(request_body.clone()),
                        dispatch,
                        "chat",
                    )
                    .await?;

                let (status, headers, body_bytes) =
                    read_provider_response_body(response, request.provider_kind, "chat").await?;

                if !status.is_success() {
                    return Err(provider_status_error_from_read(
                        request.provider_kind,
                        status,
                        &headers,
                        body_bytes,
                    ));
                }
                let body_bytes = body_bytes.unwrap_or_default();

                let body = parse_provider_json_body(&body_bytes, request.provider_kind, "chat")?;

                Ok(openai_chat_response(&body))
            },
            self.retry_policy,
        )
        .await
        .map_err(Into::into)
    }

    pub(super) async fn call_openai_compatible_stream(
        &self,
        request: OpenAiCompatibleRequest<'_>,
        allow_private_network: bool,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<(String, serde_json::Value, ChatCompletionStatus)> {
        let span = chat_span(request.provider_kind, request.model_name);
        let result = self
            .call_openai_compatible_stream_inner(request, allow_private_network, on_delta)
            .instrument(span.clone())
            .await;
        record_chat_status(&span, &result);
        result
    }

    async fn call_openai_compatible_stream_inner(
        &self,
        request: OpenAiCompatibleRequest<'_>,
        allow_private_network: bool,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<(String, serde_json::Value, ChatCompletionStatus)> {
        let request_body = request.body()?;
        let endpoint_url =
            provider_endpoint_url(request.provider_kind, request.base_url, &request.chat_path)?;
        let target = self.transport.prepare(&endpoint_url, allow_private_network).await.map_err(
            |error| {
                ProviderCallError::target_preparation(
                    &format!("provider target policy failed: provider={}", request.provider_kind),
                    &error,
                )
            },
        )?;

        let (response, _provider_guard) = with_retry(
            || async {
                let provider_guard =
                    self.begin_provider_attempt(request.provider_kind, request.base_url).await?;
                let request_builder = Self::prepared_provider_request(
                    &target,
                    &endpoint_url,
                    request.provider_kind,
                    "text/event-stream",
                    request.auth_scheme,
                    request.api_key,
                )?;
                let dispatch = request.dispatch.ok_or_else(|| {
                    ProviderCallError::protocol("provider dispatch authority is missing")
                })?;
                let response = self
                    .send_provider_request(
                        request_builder.body(request_body.clone()),
                        dispatch,
                        "chat-stream",
                    )
                    .await?;

                let status = response.status();
                if status.is_success() {
                    return Ok((response, provider_guard));
                }

                Err(provider_http_error_from_response(response, request.provider_kind).await)
            },
            self.retry_policy,
        )
        .await?;

        drain_openai_compatible_stream(response, on_delta).await
    }
}
