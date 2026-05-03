//! LLM turn helpers used by the grounded-answer pipeline.
//!
//! The canonical answer path is single-shot over the retrieval stage's
//! prepared evidence, with optional fixed-evidence revision when the
//! verifier finds unsupported literals. Tool-using document reads are not
//! part of answer generation; retrieval owns evidence selection.

use anyhow::Context as _;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::provider_profiles::ProviderModelSelection,
    integrations::llm::{ChatMessage, ToolUseRequest},
    services::query::assistant_grounding::AssistantGroundingEvidence,
};

/// Final result of one assistant turn.
#[derive(Debug, Clone)]
pub struct AgentTurnResult {
    pub answer: String,
    pub provider: ProviderModelSelection,
    pub usage_json: serde_json::Value,
    pub iterations: usize,
    pub assistant_grounding: AssistantGroundingEvidence,
    /// Per-iteration capture of the exact LLM request/response chain,
    /// for the assistant debug panel. Populated unconditionally — the
    /// cost is a few clones and the operator toggles the UI to view.
    pub debug_iterations: Vec<super::llm_context_debug::LlmIterationDebug>,
}

/// Run one assistant turn as a single grounded-answer LLM call,
/// without exposing tools to the model.
///
/// This is the fast path for the common case where the retrieval
/// stage already assembled enough evidence to answer the question -
/// `prepare_answer_query` builds `answer_context` out of the top
/// retrieved chunks, graph-aware neighbours, recent documents, and
/// the library summary. Handing that context to the model in one or
/// two fixed-evidence round-trips keeps UI and MCP on the same
/// citation set.
///
/// Verification is the caller's responsibility: if the output is empty
/// or trips the verifier, the caller either revises over the same
/// grounded context or returns the verifier state to the user.
pub async fn run_single_shot_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: Option<&str>,
    grounded_context: &str,
) -> anyhow::Result<AgentTurnResult> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("no active query_answer binding configured for library {library_id}")
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.parse().unwrap_or_default(),
        model_name: binding.model_name.clone(),
    };

    // The grounded context is already baked into the system prompt and
    // no tool catalogue is exposed, so the model can only answer from
    // retrieval-owned evidence. The caller emits `AnswerDelta` after
    // the verifier accepts the fixed-evidence candidate.
    let system_prompt =
        super::assistant_prompt::render_single_shot(grounded_context, conversation_history);
    let messages =
        vec![ChatMessage::system(system_prompt), ChatMessage::user(user_question.to_string())];

    let tool_use_request = ToolUseRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        temperature: binding.temperature,
        top_p: binding.top_p,
        max_output_tokens_override: binding.max_output_tokens_override,
        messages: messages.clone(),
        tools: Vec::new(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
    };

    let response = state
        .llm_gateway
        .generate_with_tools(tool_use_request)
        .await
        .with_context(|| "single-shot grounded-answer LLM call failed")?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::llm_context_debug::LlmIterationDebug {
        iteration: 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: response.usage_json.clone(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        // Single-shot did not observe any tool results — the grounding
        // evidence the runtime collected is already baked into the
        // verifier's `prompt_context`, so there is nothing to record
        // here. The verifier still sees the same chunks / structured
        // evidence it would have seen for a deterministic-preflight
        // answer.
        assistant_grounding: AssistantGroundingEvidence::default(),
        debug_iterations: vec![debug_iteration],
    })
}

pub async fn run_literal_fidelity_revision_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: Option<&str>,
    original_answer: &str,
    unsupported_literals: &[String],
    grounded_context: &str,
) -> anyhow::Result<AgentTurnResult> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("no active query_answer binding configured for library {library_id}")
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.parse().unwrap_or_default(),
        model_name: binding.model_name.clone(),
    };

    let system_prompt = super::assistant_prompt::render_literal_fidelity_revision(
        grounded_context,
        original_answer,
        unsupported_literals,
        conversation_history,
    );
    let messages =
        vec![ChatMessage::system(system_prompt), ChatMessage::user(user_question.to_string())];

    let tool_use_request = ToolUseRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        temperature: binding.temperature,
        top_p: binding.top_p,
        max_output_tokens_override: binding.max_output_tokens_override,
        messages: messages.clone(),
        tools: Vec::new(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
    };

    let response = state
        .llm_gateway
        .generate_with_tools(tool_use_request)
        .await
        .with_context(|| "literal-fidelity revision LLM call failed")?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::llm_context_debug::LlmIterationDebug {
        iteration: 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: response.usage_json.clone(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        assistant_grounding: AssistantGroundingEvidence::default(),
        debug_iterations: vec![debug_iteration],
    })
}

/// Run one grounded-answer turn as a short clarification call.
///
/// The post-retrieval router decided (see
/// `answer_pipeline::classify_answer_disposition`) that the topic
/// the user asked about spans several distinct variants in the
/// library and no single-shot answer will usefully cover them all.
/// The caller passes those variant labels — pulled from retrieved
/// document titles, graph node labels, or grouped-reference titles
/// on the current `answer_context` — and this function asks the
/// answer model to write one short clarifying question enumerating
/// them.
///
/// Uses the same `QueryAnswer` binding as `run_single_shot_turn`
/// so the clarify reply shares model identity, temperature caps
/// and per-turn billing plumbing.
pub async fn run_clarify_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: Option<&str>,
    variants: &[String],
) -> anyhow::Result<AgentTurnResult> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("no active query_answer binding configured for library {library_id}")
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.parse().unwrap_or_default(),
        model_name: binding.model_name.clone(),
    };

    let system_prompt = super::assistant_prompt::render_clarify(variants, conversation_history);
    let messages =
        vec![ChatMessage::system(system_prompt), ChatMessage::user(user_question.to_string())];

    let tool_use_request = ToolUseRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        temperature: binding.temperature,
        top_p: binding.top_p,
        max_output_tokens_override: binding.max_output_tokens_override,
        messages: messages.clone(),
        tools: Vec::new(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
    };

    let response = state
        .llm_gateway
        .generate_with_tools(tool_use_request)
        .await
        .with_context(|| "clarify-path LLM call failed")?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::llm_context_debug::LlmIterationDebug {
        iteration: 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: response.usage_json.clone(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        assistant_grounding: AssistantGroundingEvidence::default(),
        debug_iterations: vec![debug_iteration],
    })
}

/// Accumulate one iteration's `usage_json` into the running total for
/// a turn. The billing pipeline (`services::ops::billing`) reads token
/// counts from any of the provider-specific key aliases (`prompt_tokens`
/// / `input_tokens`, `completion_tokens` / `output_tokens`, plus cached
/// input variants); we canonicalise to the OpenAI shape on write so a
/// mixed-provider trace still produces one correct billing row.
///
/// Numbers are summed, and per-iteration counters (`iteration_count`,
/// `provider_call_count`) expose the round-trip volume separately from
/// raw tokens so an operator reading the debug snapshot or the billing
/// `usage_json` can tell a single-shot call apart from a 6-iteration
/// escalation without cross-referencing `debug_iterations`.
pub(crate) fn merge_usage_into(accumulator: &mut serde_json::Value, iteration: &serde_json::Value) {
    fn sum_key(
        accumulator: &mut serde_json::Map<String, serde_json::Value>,
        canonical_key: &str,
        source: &serde_json::Value,
        aliases: &[&str],
    ) {
        let value =
            aliases.iter().find_map(|alias| source.get(*alias)).and_then(serde_json::Value::as_i64);
        let Some(delta) = value else {
            return;
        };
        let existing =
            accumulator.get(canonical_key).and_then(serde_json::Value::as_i64).unwrap_or(0);
        accumulator.insert(canonical_key.to_string(), serde_json::json!(existing + delta));
    }

    if !accumulator.is_object() {
        *accumulator = serde_json::json!({});
    }
    // The branch above guarantees `accumulator` is a JSON object, so
    // `as_object_mut()` returns `Some`; the fallback path is unreachable
    // but keeps the type checker happy without introducing a panic.
    let Some(obj) = accumulator.as_object_mut() else {
        return;
    };

    sum_key(obj, "prompt_tokens", iteration, &["prompt_tokens", "input_tokens"]);
    sum_key(obj, "completion_tokens", iteration, &["completion_tokens", "output_tokens"]);
    sum_key(obj, "total_tokens", iteration, &["total_tokens"]);
    sum_key(
        obj,
        "cached_input_tokens",
        iteration,
        &["cached_input_tokens", "cache_read_input_tokens", "input_cached_tokens"],
    );
    // Nested `{"prompt_tokens_details": {"cached_tokens": N}}` shape
    // some providers emit — merge it into the flat canonical key too
    // so billing sees it regardless of which path upstream used.
    let nested_cached = iteration
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .or_else(|| {
            iteration.get("input_tokens_details").and_then(|details| details.get("cached_tokens"))
        })
        .and_then(serde_json::Value::as_i64);
    if let Some(delta) = nested_cached {
        let existing =
            obj.get("cached_input_tokens").and_then(serde_json::Value::as_i64).unwrap_or(0);
        obj.insert("cached_input_tokens".to_string(), serde_json::json!(existing + delta));
    }

    let existing_iterations =
        obj.get("iteration_count").and_then(serde_json::Value::as_i64).unwrap_or(0);
    obj.insert("iteration_count".to_string(), serde_json::json!(existing_iterations + 1));
}
