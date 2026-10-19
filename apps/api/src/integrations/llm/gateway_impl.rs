use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use crate::{
    domains::provider_profiles::{
        ANTHROPIC_MESSAGES_RUNTIME_KIND, GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
        OPENAI_COMPATIBLE_RUNTIME_KIND,
    },
    integrations::retry::{ProviderCallError, provider_status_error_from_read, with_retry},
};

use super::{
    ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
    EmbeddingResponse, LlmGateway, ToolUseRequest, ToolUseResponse, UnifiedGateway, VisionRequest,
    VisionResponse,
    chat::NativeChatCall,
    embeddings::EmbeddingsCall,
    openai_compatible::{
        OpenAiCompatibleContentPart, OpenAiCompatibleImageUrl, OpenAiCompatibleMessage,
        OpenAiCompatibleMessageContent,
    },
    request::{
        OpenAiCompatibleRequestOptions, chat_request_controls, openai_compatible_request,
        tool_use_request_payload,
    },
    response::{
        parse_provider_json_body, parse_tool_use_response, provider_http_error_from_response,
        provider_usage_json, read_provider_response_body,
    },
    streaming::drain_tool_use_stream,
};

#[async_trait]
impl LlmGateway for UnifiedGateway {
    async fn generate(&self, mut request: ChatRequest) -> Result<ChatResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.model_name,
            &request.extra_parameters_json,
        )?;
        let upstream_extra = Self::upstream_extra_parameters(
            &request.extra_parameters_json,
            resolved.reasoning_wire,
        );
        let controls = chat_request_controls(&request, &resolved)?;
        let (output_text, usage_json) = match resolved.runtime.kind.as_str() {
            OPENAI_COMPATIBLE_RUNTIME_KIND => {
                self.call_openai_compatible(
                    openai_compatible_request(
                        &request.provider_kind,
                        &request.model_name,
                        vec![OpenAiCompatibleMessage {
                            role: "user".to_string(),
                            content: OpenAiCompatibleMessageContent::Text(request.prompt.clone()),
                        }],
                        &resolved,
                        OpenAiCompatibleRequestOptions {
                            system_prompt: controls.system_prompt.as_deref(),
                            temperature: controls.temperature,
                            top_p: controls.top_p,
                            max_output_tokens: request.max_output_tokens_override,
                            response_format: controls.response_format.as_ref(),
                            extra_parameters_json: &upstream_extra,
                            stream: false,
                        },
                    ),
                    resolved.allow_private_network,
                )
                .await?
            }
            ANTHROPIC_MESSAGES_RUNTIME_KIND | GEMINI_GENERATE_CONTENT_RUNTIME_KIND => {
                if request.response_format.is_some() {
                    return Err(anyhow!(
                        "native provider structured output is not implemented for provider={}",
                        request.provider_kind
                    ));
                }
                self.call_native_chat(NativeChatCall {
                    provider_kind: &request.provider_kind,
                    runtime: &resolved.runtime,
                    api_key: resolved.api_key.as_deref(),
                    base_url: &resolved.base_url,
                    allow_private_network: resolved.allow_private_network,
                    model_name: &request.model_name,
                    prompt: &request.prompt,
                    system_prompt: request.system_prompt.as_deref(),
                    temperature: controls.temperature,
                    top_p: controls.top_p,
                    max_output_tokens: request.max_output_tokens_override,
                    image: None,
                })
                .await?
            }
            _ => return Err(anyhow!("unsupported provider runtime kind")),
        };
        Ok(ChatResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            usage_json,
        })
    }

    async fn generate_stream(
        &self,
        mut request: ChatRequest,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ChatResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.model_name,
            &request.extra_parameters_json,
        )?;
        if matches!(
            resolved.runtime.kind.as_str(),
            ANTHROPIC_MESSAGES_RUNTIME_KIND | GEMINI_GENERATE_CONTENT_RUNTIME_KIND
        ) {
            let response = self.generate(request).await?;
            if !response.output_text.is_empty() {
                on_delta(response.output_text.clone());
            }
            return Ok(response);
        }
        let upstream_extra = Self::upstream_extra_parameters(
            &request.extra_parameters_json,
            resolved.reasoning_wire,
        );
        let controls = chat_request_controls(&request, &resolved)?;
        let (output_text, usage_json) = self
            .call_openai_compatible_stream(
                openai_compatible_request(
                    &request.provider_kind,
                    &request.model_name,
                    vec![OpenAiCompatibleMessage {
                        role: "user".to_string(),
                        content: OpenAiCompatibleMessageContent::Text(request.prompt.clone()),
                    }],
                    &resolved,
                    OpenAiCompatibleRequestOptions {
                        system_prompt: controls.system_prompt.as_deref(),
                        temperature: controls.temperature,
                        top_p: controls.top_p,
                        max_output_tokens: request.max_output_tokens_override,
                        response_format: controls.response_format.as_ref(),
                        extra_parameters_json: &upstream_extra,
                        stream: true,
                    },
                ),
                resolved.allow_private_network,
                on_delta,
            )
            .await?;
        Ok(ChatResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            usage_json,
        })
    }

    async fn generate_with_tools(&self, mut request: ToolUseRequest) -> Result<ToolUseResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.model_name,
            &request.extra_parameters_json,
        )?;
        if matches!(
            resolved.runtime.kind.as_str(),
            ANTHROPIC_MESSAGES_RUNTIME_KIND | GEMINI_GENERATE_CONTENT_RUNTIME_KIND
        ) {
            anyhow::bail!(
                "native provider tool use is not implemented for provider={}",
                request.provider_kind
            );
        }

        let payload = tool_use_request_payload(&request, &resolved, false);
        let request_body =
            serde_json::to_vec(&payload).context("failed to serialize tool-use request body")?;

        let (endpoint_url, target) =
            self.open_chat_endpoint(&request.provider_kind, &resolved).await?;

        let (output_text, tool_calls, finish_reason, usage_json, reasoning_content) = with_retry(
            || async {
                let _provider_guard =
                    self.begin_provider_attempt(&request.provider_kind, &resolved.base_url).await?;
                let request_builder = Self::prepared_provider_request(
                    &target,
                    &endpoint_url,
                    &request.provider_kind,
                    "application/json",
                    resolved.runtime.auth_scheme,
                    resolved.api_key.as_deref(),
                )?;
                let response = Self::send_provider_request(
                    request_builder.body(request_body.clone()),
                    &endpoint_url,
                    &request.provider_kind,
                    "tool-use",
                )
                .await
                .map_err(|source| {
                    ProviderCallError::transport(
                        format!("tool-use transport failed: provider={}", request.provider_kind),
                        source,
                    )
                })?;

                let (status, headers, body_bytes) =
                    read_provider_response_body(response, &request.provider_kind, "tool-use")
                        .await?;
                if !status.is_success() {
                    return Err(provider_status_error_from_read(
                        &request.provider_kind,
                        status,
                        &headers,
                        body_bytes,
                    ));
                }
                let body_bytes = body_bytes.unwrap_or_default();

                let body =
                    parse_provider_json_body(&body_bytes, &request.provider_kind, "tool-use")?;

                parse_tool_use_response(&body)
            },
            self.retry_policy,
        )
        .await?;

        Ok(ToolUseResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            tool_calls,
            finish_reason,
            usage_json,
            reasoning_content,
        })
    }

    async fn generate_with_tools_stream(
        &self,
        mut request: ToolUseRequest,
        on_text_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ToolUseResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.model_name,
            &request.extra_parameters_json,
        )?;

        let payload = tool_use_request_payload(&request, &resolved, true);
        let request_body = serde_json::to_vec(&payload)
            .context("failed to serialize streaming tool-use request body")?;

        let (endpoint_url, target) =
            self.open_chat_endpoint(&request.provider_kind, &resolved).await?;

        let (response, _provider_guard) = with_retry(
            || async {
                let provider_guard =
                    self.begin_provider_attempt(&request.provider_kind, &resolved.base_url).await?;
                let request_builder = Self::prepared_provider_request(
                    &target,
                    &endpoint_url,
                    &request.provider_kind,
                    "text/event-stream",
                    resolved.runtime.auth_scheme,
                    resolved.api_key.as_deref(),
                )?;
                let response = Self::send_provider_request(
                    request_builder.body(request_body.clone()),
                    &endpoint_url,
                    &request.provider_kind,
                    "tool-use-stream",
                )
                .await
                .map_err(|source| {
                    ProviderCallError::transport(
                        format!(
                            "tool-use stream transport failed: provider={}",
                            request.provider_kind
                        ),
                        source,
                    )
                })?;

                let status = response.status();
                if status.is_success() {
                    return Ok((response, provider_guard));
                }

                Err(provider_http_error_from_response(response, &request.provider_kind).await)
            },
            self.retry_policy,
        )
        .await?;

        let stream_state = drain_tool_use_stream(response, on_text_delta).await?;
        let (output_text, finish_reason, usage_json, tool_calls) = stream_state.finalize();
        Ok(ToolUseResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            tool_calls,
            finish_reason,
            usage_json,
            // Streaming path does not yet capture `reasoning_content`.
            // The non-streaming gateway is the primary tool-use path; streaming
            // is reserved for direct provider passthroughs that do not echo
            // reasoning back to the model.
            reasoning_content: None,
        })
    }

    async fn embed(&self, mut request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.model_name,
            &request.extra_parameters_json,
        )?;
        let (endpoint_url, target) =
            self.open_embeddings_endpoint(&request.provider_kind, &resolved).await?;

        let body = self
            .call_embeddings(EmbeddingsCall {
                provider_kind: &request.provider_kind,
                model_name: &request.model_name,
                input: serde_json::Value::String(request.input.clone()),
                extra_parameters_json: &request.extra_parameters_json,
                resolved: &resolved,
                endpoint_url: &endpoint_url,
                target: &target,
                operation: "embedding",
                transport_error: "embedding transport failed",
            })
            .await?;

        let embedding_value = body
            .get("data")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("embedding"))
            .context("embedding response did not contain data[0].embedding")?;
        let embedding = Self::parse_embedding_vector(embedding_value)
            .context("embedding response contained an invalid vector")?;

        let usage_json = provider_usage_json(&body);

        Ok(EmbeddingResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions: embedding.len(),
            embedding,
            usage_json,
        })
    }

    async fn embed_many(
        &self,
        mut request: EmbeddingBatchRequest,
    ) -> Result<EmbeddingBatchResponse> {
        if request.inputs.is_empty() {
            return Ok(EmbeddingBatchResponse {
                provider_kind: std::mem::take(&mut request.provider_kind),
                model_name: std::mem::take(&mut request.model_name),
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
                    extra_parameters_json: request.extra_parameters_json.clone(),
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

        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.model_name,
            &request.extra_parameters_json,
        )?;
        let (endpoint_url, target) =
            self.open_embeddings_endpoint(&request.provider_kind, &resolved).await?;

        let body = self
            .call_embeddings(EmbeddingsCall {
                provider_kind: &request.provider_kind,
                model_name: &request.model_name,
                input: serde_json::json!(request.inputs.clone()),
                extra_parameters_json: &request.extra_parameters_json,
                resolved: &resolved,
                endpoint_url: &endpoint_url,
                target: &target,
                operation: "embedding batch",
                transport_error: "embedding batch transport failed",
            })
            .await?;

        let items = body
            .get("data")
            .and_then(serde_json::Value::as_array)
            .context("embedding batch response did not contain a data array")?;
        let embeddings = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let value = item.get("embedding").ok_or_else(|| {
                    anyhow!("embedding batch item {index} did not contain an embedding")
                })?;
                Self::parse_embedding_vector(value)
                    .with_context(|| format!("embedding batch item {index} was invalid"))
            })
            .collect::<Result<Vec<_>>>()?;
        let dimensions = embeddings.first().map(Vec::len).unwrap_or_default();
        let usage_json = provider_usage_json(&body);

        Ok(EmbeddingBatchResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions,
            embeddings,
            usage_json,
        })
    }

    async fn vision_extract(&self, mut request: VisionRequest) -> Result<VisionResponse> {
        let resolved = Self::resolve_provider(
            &request.provider_kind,
            request.api_key_override.as_deref(),
            request.base_url_override.as_deref(),
            &request.model_name,
            &request.extra_parameters_json,
        )?;
        let upstream_extra = Self::upstream_extra_parameters(
            &request.extra_parameters_json,
            resolved.reasoning_wire,
        );
        let (temperature, top_p) =
            resolved.request_policy.sampling_params(request.temperature, request.top_p);
        let image_data_url = format!(
            "data:{};base64,{}",
            request.mime_type,
            BASE64_STANDARD.encode(&request.image_bytes)
        );
        let (output_text, usage_json) = match resolved.runtime.kind.as_str() {
            OPENAI_COMPATIBLE_RUNTIME_KIND => {
                self.call_openai_compatible(
                    openai_compatible_request(
                        &request.provider_kind,
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
                        &resolved,
                        OpenAiCompatibleRequestOptions {
                            system_prompt: request.system_prompt.as_deref(),
                            temperature,
                            top_p,
                            max_output_tokens: request.max_output_tokens_override,
                            response_format: None,
                            extra_parameters_json: &upstream_extra,
                            stream: false,
                        },
                    ),
                    resolved.allow_private_network,
                )
                .await?
            }
            ANTHROPIC_MESSAGES_RUNTIME_KIND | GEMINI_GENERATE_CONTENT_RUNTIME_KIND => {
                self.call_native_chat(NativeChatCall {
                    provider_kind: &request.provider_kind,
                    runtime: &resolved.runtime,
                    api_key: resolved.api_key.as_deref(),
                    base_url: &resolved.base_url,
                    allow_private_network: resolved.allow_private_network,
                    model_name: &request.model_name,
                    prompt: &request.prompt,
                    system_prompt: request.system_prompt.as_deref(),
                    temperature,
                    top_p,
                    max_output_tokens: request.max_output_tokens_override,
                    image: Some((&request.image_bytes, &request.mime_type)),
                })
                .await?
            }
            _ => return Err(anyhow!("unsupported provider runtime kind")),
        };

        Ok(VisionResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text,
            usage_json,
        })
    }
}
