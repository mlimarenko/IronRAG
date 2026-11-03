use std::time::Duration;

use crate::services::ops::billing::provider_reservation::ProviderCallReservation;

use ironrag_contracts::assistant::semantic_verification::{
    SemanticFailureCode, SemanticNotRunReason, SemanticVerificationReport,
};
use tokio::time::Instant;

use crate::{
    agent_runtime::tasks::query_answer::{
        QueryProviderCall, QueryProviderCallAttribution, QueryProviderCallKind,
    },
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        provider_profiles::{
            OPENAI_COMPATIBLE_RUNTIME_KIND, ProviderModelSelection, ProviderStructuredOutputMode,
        },
    },
    integrations::llm::{
        ChatCompletionStatus, ChatMessage, ChatRequest, ChatResponse,
        build_structured_chat_request, disable_bounded_answer_reasoning, provider_system_prompt,
    },
    interfaces::http::router_support::ApiError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        query::{
            llm_context_debug::LlmIterationDebug, provider_billing::QueryProviderExecutionContext,
        },
    },
};

use super::{
    MAX_OUTPUT_TOKENS, MAX_OUTPUT_UTF8_BYTES, PreparedSemanticVerification, SYSTEM_PROMPT,
    SemanticOutputStatus, StructuredSemanticOutput, response_json_schema,
};

pub(crate) const SEMANTIC_PROVIDER_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_REQUEST_UTF8_BYTES: usize = 96_000;

#[cfg(test)]
tokio::task_local! {
    static CONTROLLED_BINDING: ResolvedRuntimeBinding;
}

/// Narrow the canonical resolved route without replacing resolution,
/// reservation, transport or response evaluation.
#[cfg(test)]
pub(crate) async fn with_controlled_binding<F: std::future::Future>(
    binding: ResolvedRuntimeBinding,
    future: F,
) -> F::Output {
    CONTROLLED_BINDING.scope(binding, future).await
}

#[cfg(test)]
fn require_controlled_binding(actual: &ResolvedRuntimeBinding) -> Result<(), ApiError> {
    CONTROLLED_BINDING
        .try_with(|expected| {
            let identity_matches = actual.binding_id == expected.binding_id
                && actual.workspace_id == expected.workspace_id
                && actual.library_id == expected.library_id
                && actual.binding_purpose == expected.binding_purpose
                && actual.provider_catalog_id == expected.provider_catalog_id
                && actual.account_id == expected.account_id
                && actual.model_catalog_id == expected.model_catalog_id;
            let transport_matches = actual.provider_kind == expected.provider_kind
                && actual.model_name == expected.model_name
                && actual.provider_base_url == expected.provider_base_url
                && actual.provider_api_style == expected.provider_api_style
                && actual.api_key == expected.api_key;
            let controls_match = actual.effective_embedding_dimensions
                == expected.effective_embedding_dimensions
                && actual.system_prompt == expected.system_prompt
                && actual.temperature == expected.temperature
                && actual.top_p == expected.top_p
                && actual.max_output_tokens_override == expected.max_output_tokens_override
                && actual.extra_parameters_json == expected.extra_parameters_json;
            if identity_matches && transport_matches && controls_match {
                Ok(())
            } else {
                Err(ApiError::Conflict(
                    "controlled semantic binding changed before reservation".into(),
                ))
            }
        })
        .unwrap_or(Ok(()))
}

#[derive(Debug)]
pub(crate) struct SemanticProviderAttempt {
    pub(crate) report: SemanticVerificationReport,
    pub(crate) provider_call: Option<QueryProviderCall>,
    pub(crate) iteration: Option<LlmIterationDebug>,
    /// A paid response is retained even if durable completion failed. Callers
    /// retain its attribution/debug payload and then fail the query closed.
    pub(crate) completion_error: Option<ApiError>,
}

impl SemanticProviderAttempt {
    fn without_response(report: SemanticVerificationReport) -> Self {
        Self { report, provider_call: None, iteration: None, completion_error: None }
    }
}

#[derive(Debug)]
pub(super) struct SemanticProviderRequest {
    pub(super) request: ChatRequest,
    pub(super) messages: Vec<ChatMessage>,
}

pub(super) fn build_verification_request(
    binding: &ResolvedRuntimeBinding,
    prepared: &PreparedSemanticVerification,
) -> Result<SemanticProviderRequest, SemanticNotRunReason> {
    let Some(
        mode
        @ (ProviderStructuredOutputMode::JsonSchema | ProviderStructuredOutputMode::JsonObject),
    ) = binding.provider_structured_output_mode()
    else {
        return Err(SemanticNotRunReason::UnsupportedStructuredTransport);
    };
    if binding.provider_runtime_kind() != Some(OPENAI_COMPATIBLE_RUNTIME_KIND) {
        return Err(SemanticNotRunReason::UnsupportedStructuredTransport);
    }
    let max_tokens = binding
        .max_output_tokens_override
        .unwrap_or(MAX_OUTPUT_TOKENS as i32)
        .min(MAX_OUTPUT_TOKENS as i32);
    if max_tokens <= 0 {
        return Err(SemanticNotRunReason::InputLimitExceeded);
    }
    let response_format = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "semantic_verification",
            "strict": true,
            "schema": response_json_schema(),
        }
    });
    let prompt = serde_json::to_string(&prepared.request_json())
        .map_err(|_| SemanticNotRunReason::InputLimitExceeded)?;
    let mut seed = binding.chat_request_seed();
    protect_semantic_wire_controls(&mut seed.extra_parameters_json)?;
    // Generation-specific operator prose cannot replace this machine protocol.
    seed.system_prompt = Some(SYSTEM_PROMPT.to_string());
    seed.max_output_tokens_override = Some(max_tokens);
    disable_bounded_answer_reasoning(&binding.provider_kind, &mut seed.extra_parameters_json);
    let request = build_structured_chat_request(seed, prompt, response_format);
    let serialized =
        serde_json::to_vec(&request).map_err(|_| SemanticNotRunReason::InputLimitExceeded)?;
    if serialized.len() > MAX_REQUEST_UTF8_BYTES {
        return Err(SemanticNotRunReason::InputLimitExceeded);
    }
    // Reuse the gateway's effective-system owner, including its JSON-object
    // schema suffix, rather than recording a different debug prompt.
    let system = provider_system_prompt(
        &binding.provider_kind,
        request.system_prompt.as_deref(),
        request.response_format.as_ref(),
        mode,
    )
    .map_err(|_| SemanticNotRunReason::UnsupportedStructuredTransport)?;
    let mut messages = Vec::new();
    if let Some(system) = system {
        messages.push(ChatMessage::system(system));
    }
    messages.push(ChatMessage::user(request.prompt.clone()));
    Ok(SemanticProviderRequest { request, messages })
}

/// Flattened extras cannot override the machine protocol, dispatch identity,
/// single-response count or output budget. Keep unrelated provider options and
/// the internal profile/reasoning envelopes for their existing typed owners.
fn protect_semantic_wire_controls(
    extra: &mut serde_json::Value,
) -> Result<(), SemanticNotRunReason> {
    let fields =
        extra.as_object_mut().ok_or(SemanticNotRunReason::UnsupportedStructuredTransport)?;
    for key in [
        "model",
        "model_name",
        "messages",
        "prompt",
        "input",
        "instructions",
        "system",
        "system_prompt",
        "response_format",
        "stream",
        "stream_options",
        "tools",
        "tool_choice",
        "functions",
        "function_call",
        "parallel_tool_calls",
        "max_tokens",
        "max_completion_tokens",
        "max_output_tokens",
        "maxOutputTokens",
        "maxCompletionTokens",
        "max_new_tokens",
        "max_tokens_to_sample",
        "num_predict",
        "n",
        "best_of",
    ] {
        fields.remove(key);
    }
    Ok(())
}

/// One selected-candidate call, without semantic repair or purpose fallback.
/// Provider waiting is bounded; durable usage completion is still awaited
/// before the caller may persist a terminal query outcome.
pub(crate) async fn execute_semantic_verification(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    prepared: &PreparedSemanticVerification,
    timeout: Duration,
) -> Result<SemanticProviderAttempt, ApiError> {
    let started = Instant::now();
    let timeout = timeout.min(SEMANTIC_PROVIDER_TIMEOUT);
    if timeout.is_zero() {
        return Ok(SemanticProviderAttempt::without_response(prepared.failed(
            SemanticFailureCode::Timeout,
            0,
            None,
        )));
    }
    let deadline = started + timeout;
    let binding = match tokio::time::timeout_at(
        deadline,
        state.application_services.ai_catalog.resolve_active_runtime_binding(
            state,
            execution.library_id,
            AiBindingPurpose::QueryAnswer,
        ),
    )
    .await
    {
        Ok(Ok(Some(binding))) => binding,
        Ok(Ok(None)) => {
            return Ok(SemanticProviderAttempt::without_response(
                prepared.not_run(SemanticNotRunReason::BindingUnavailable, elapsed_ms(started)),
            ));
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            return Ok(SemanticProviderAttempt::without_response(prepared.failed(
                SemanticFailureCode::Timeout,
                elapsed_ms(started),
                None,
            )));
        }
    };
    if binding.workspace_id != execution.workspace_id
        || binding.library_id != execution.library_id
        || binding.binding_purpose != AiBindingPurpose::QueryAnswer
    {
        return Err(ApiError::Conflict("semantic verification binding scope mismatch".into()));
    }
    #[cfg(test)]
    require_controlled_binding(&binding)?;
    let request = match build_verification_request(&binding, prepared) {
        Ok(request) => request,
        Err(reason) => {
            return Ok(SemanticProviderAttempt::without_response(
                prepared.not_run(reason, elapsed_ms(started)),
            ));
        }
    };
    execute_reserved_request(
        state,
        execution,
        prepared,
        &binding,
        request,
        SemanticProviderDeadline { started, deadline },
    )
    .await
}

struct SemanticProviderDeadline {
    started: Instant,
    deadline: Instant,
}

async fn execute_reserved_request(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    prepared: &PreparedSemanticVerification,
    binding: &ResolvedRuntimeBinding,
    request: SemanticProviderRequest,
    timing: SemanticProviderDeadline,
) -> Result<SemanticProviderAttempt, ApiError> {
    let SemanticProviderDeadline { started, deadline } = timing;
    let kind = QueryProviderCallKind::QueryAnswerSemanticVerification;
    let attribution = QueryProviderCallAttribution::try_new(
        binding.binding_id,
        binding.binding_purpose,
        ProviderModelSelection {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
        },
        kind,
    )
    .map_err(|_| ApiError::Conflict("semantic verification call attribution mismatch".into()))?;
    let mut reservation = ProviderCallReservation::reserve(
        state,
        execution,
        binding,
        kind.binding_purpose(),
        kind.as_str(),
    )
    .await?;
    let provider_call_id = reservation.provider_call_id();
    if Instant::now() >= deadline {
        reservation.fail().await?;
        return Ok(SemanticProviderAttempt::without_response(prepared.failed(
            SemanticFailureCode::Timeout,
            elapsed_ms(started),
            Some(provider_call_id),
        )));
    }
    let provider_started = Instant::now();
    let response = tokio::time::timeout_at(
        deadline,
        crate::observability::with_provider_call_id(
            provider_call_id,
            state.llm_gateway.generate(request.request),
        ),
    )
    .await;
    let response = match response {
        Ok(Ok(response)) => response,
        failure => {
            let code = if failure.is_err() {
                SemanticFailureCode::Timeout
            } else {
                SemanticFailureCode::ProviderError
            };
            reservation.fail().await?;
            return Ok(SemanticProviderAttempt::without_response(prepared.failed(
                code,
                elapsed_ms(started),
                Some(provider_call_id),
            )));
        }
    };
    let duration_ms = elapsed_ms(provider_started);
    // A malformed, truncated or refused response is still paid. Completion
    // owns exact usage before validation or any terminal result is produced.
    let completion = reservation.complete(&response.usage_json).await;
    let provider_call = attribution.record(provider_call_id, response.usage_json.clone());
    let report = if completion.is_ok() {
        validate_response(prepared, &response, elapsed_ms(started), provider_call_id)
    } else {
        prepared.failed(
            SemanticFailureCode::ProviderError,
            elapsed_ms(started),
            Some(provider_call_id),
        )
    };
    let iteration = LlmIterationDebug {
        iteration: 0,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: request.messages,
        response_text: (response.output_text.len() <= MAX_OUTPUT_UTF8_BYTES)
            .then_some(response.output_text),
        response_tool_calls: Vec::new(),
        usage: response.usage_json,
        duration_ms: Some(duration_ms),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };
    Ok(SemanticProviderAttempt {
        report,
        provider_call: Some(provider_call),
        iteration: Some(iteration),
        completion_error: completion.err(),
    })
}

pub(super) fn validate_response(
    prepared: &PreparedSemanticVerification,
    response: &ChatResponse,
    elapsed: u64,
    provider_call_id: uuid::Uuid,
) -> SemanticVerificationReport {
    let status = match response.completion_status {
        ChatCompletionStatus::Complete => SemanticOutputStatus::Complete,
        ChatCompletionStatus::Truncated => SemanticOutputStatus::Truncated,
        ChatCompletionStatus::Refused => {
            return prepared.failed(
                SemanticFailureCode::ProviderRefusal,
                elapsed,
                Some(provider_call_id),
            );
        }
        ChatCompletionStatus::Unknown => {
            return prepared.failed(
                SemanticFailureCode::InvalidResponse,
                elapsed,
                Some(provider_call_id),
            );
        }
    };
    let output_tokens = response
        .usage_json
        .get("completion_tokens")
        .or_else(|| response.usage_json.get("output_tokens"))
        .and_then(serde_json::Value::as_u64)
        .map(|tokens| u32::try_from(tokens).unwrap_or(u32::MAX));
    prepared
        .parse_response(
            StructuredSemanticOutput { json: &response.output_text, status, output_tokens },
            elapsed,
            Some(provider_call_id),
        )
        .unwrap_or_else(|error| {
            prepared.failed(error.failure_code(), elapsed, Some(provider_call_id))
        })
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::assert_hostile_controls_cannot_override_gateway_body;
