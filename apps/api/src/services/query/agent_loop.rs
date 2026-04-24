//! Tool-using LLM agent loop that powers the in-app assistant.
//!
//! The in-app assistant is intentionally a "vanilla" agent: a single LLM
//! call loop with the canonical MCP tools handed to the model as functions.
//! Every action it takes must go through the same MCP handlers that
//! external Codex / Cursor / VS Code agents use, so the assistant cannot
//! see or do anything that an external agent with the same scope cannot.
//!
//! The loop is deliberately tiny — no custom retrieval, no verification,
//! no grounding-guard refusal logic. Whatever the LLM produces with the
//! grounded tool results is what reaches the user.

use anyhow::Context as _;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::provider_profiles::ProviderModelSelection,
    infra::repositories::catalog_repository,
    integrations::llm::{ChatMessage, ToolUseRequest},
    interfaces::http::auth::AuthContext,
    interfaces::http::mcp::agent_bridge::{dispatch_assistant_tool, list_assistant_tools},
    services::mcp::access::library_catalog_ref,
    services::query::assistant_grounding::AssistantGroundingEvidence,
};

/// Upper bound on tool-call rounds for the assistant agent loop.
///
/// A round is one LLM response + tool dispatch pair. The cap is a
/// circuit-breaker against runaway planning, NOT a product budget.
///
/// Hitting the cap is a signal that the answer-generation pipeline
/// mis-routed this question: the single-shot fast path
/// (`run_single_shot_turn`) should have answered it from the
/// pre-computed retrieval bundle, and any question that truly needs
/// iterative tool use is almost always decided by round 5–7. The
/// pre-0.3.2 cap of 20 masked that mis-routing and dragged every
/// escalation out to ~60–90 s on production traffic.
///
/// When the cap is reached we no longer `bail!` with an error —
/// instead `run_assistant_turn` asks the model one more time for a
/// final answer without tools (see the cap-exceeded branch below)
/// and returns what it produces. Partial evidence is a better user
/// experience than "internal server error" on hard questions.
///
/// Per-result truncation (see `MAX_TOOL_RESULT_CHARS`) keeps the
/// provider payload bounded regardless of how many iterations the
/// agent runs within this cap.
const MAX_AGENT_ITERATIONS: usize = 10;

/// Per-tool-result character budget appended to the conversation.
///
/// A single `read_document` with `mode=full` can return tens of
/// kilobytes of text; after 3-4 such reads the accumulated messages
/// body blows past the provider's request entity limit (we saw
/// `413 Payload Too Large` from DeepSeek after ~10 iterations). The
/// agent already has `continuationToken` as a canonical mechanism
/// for paging through long documents — when a tool result exceeds
/// this budget we truncate and append an explicit notice telling the
/// model to request the next window. 16 KB is enough for one dense
/// PDF section, small enough that 20 tool calls × 16 KB still leave
/// headroom for the system prompt, user question and assistant
/// thoughts inside a 128 k token window.
const MAX_TOOL_RESULT_CHARS: usize = 16 * 1024;

/// Progress events emitted by the assistant agent loop while it is
/// iterating through tool calls. Surfaced to the SSE stream so the UI
/// can render "searching documents…" / "reading Frontol 6 manual…"
/// live instead of sitting under keep-alive frames while the LLM
/// grinds through 8-11 iterations.
#[derive(Debug, Clone)]
pub enum AgentProgressEvent {
    /// Final assistant answer text. Emitted once, at the end.
    AnswerDelta(String),
    /// The agent just asked the runtime to dispatch a tool call.
    ToolCallStarted { iteration: usize, call_id: String, name: String, arguments_preview: String },
    /// The runtime returned from a tool dispatch.
    ToolCallCompleted {
        iteration: usize,
        call_id: String,
        name: String,
        is_error: bool,
        result_preview: String,
    },
}

/// Final result of one assistant turn.
#[derive(Debug, Clone)]
pub struct AgentTurnResult {
    pub answer: String,
    pub provider: ProviderModelSelection,
    pub usage_json: serde_json::Value,
    pub iterations: usize,
    pub tool_calls_total: usize,
    pub assistant_grounding: AssistantGroundingEvidence,
    /// Per-iteration capture of the exact LLM request/response chain,
    /// for the assistant debug panel. Populated unconditionally — the
    /// cost is a few clones and the operator toggles the UI to view.
    pub debug_iterations: Vec<super::llm_context_debug::LlmIterationDebug>,
}

/// Run one assistant turn through the LLM agent loop.
///
/// `library_id` is the active library; the agent is told to keep its work
/// scoped to it. `conversation_history` is a flat text rendering of the
/// prior turns (oldest first), used as a single system message so the
/// model can resolve references to earlier turns.
///
/// `on_progress` is invoked in real time with:
///  * [`AgentProgressEvent::ToolCallStarted`] immediately before each
///    MCP tool dispatch (so the UI can show "searching…" while the
///    dispatch is in flight);
///  * [`AgentProgressEvent::ToolCallCompleted`] right after each
///    dispatch with a short result preview and the error flag;
///  * [`AgentProgressEvent::AnswerDelta`] once, at the end, carrying
///    the final answer text. Token-level streaming through tool-using
///    models is provider-specific — the public surface stays stable
///    and the final text is emitted as a single delta.
pub async fn run_assistant_turn(
    state: &AppState,
    auth: &AuthContext,
    library_id: Uuid,
    request_id: &str,
    user_question: &str,
    conversation_history: Option<&str>,
    on_progress: Option<tokio::sync::mpsc::UnboundedSender<AgentProgressEvent>>,
) -> anyhow::Result<AgentTurnResult> {
    // 1. Resolve the configured provider/model for this library's QueryAnswer
    //    binding so the assistant uses whichever model the operator picked.
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

    // 2. Build the tool catalog from the MCP visibility list. The model only
    //    sees tools its auth permits — same as `tools/list` over MCP.
    let tools = list_assistant_tools(auth);

    // 3. Build the conversation messages for the LLM. The system
    //    prompt is the canonical one — exact same text external MCP
    //    clients get from `/v1/query/assistant/system-prompt`, with
    //    the active library ref substituted in. Keep this path
    //    trivially thin so the in-app assistant and external agents
    //    see the same guidance.
    let library = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
        .await
        .context("failed to load assistant library for canonical prompt")?
        .ok_or_else(|| anyhow::anyhow!("assistant library {library_id} does not exist"))?;
    let workspace =
        catalog_repository::get_workspace_by_id(&state.persistence.postgres, library.workspace_id)
            .await
            .context("failed to load assistant workspace for canonical prompt")?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "assistant workspace {} for library {} does not exist",
                    library.workspace_id,
                    library_id
                )
            })?;
    let library_ref = library_catalog_ref(&workspace.slug, &library.slug);
    let mut messages = Vec::new();
    let system_prompt = super::assistant_prompt::render(&library_ref, conversation_history);
    messages.push(ChatMessage::system(system_prompt));
    messages.push(ChatMessage::user(user_question.to_string()));

    let mut total_tool_calls = 0usize;
    // Cumulative usage across every LLM round-trip in this turn. The
    // tool loop can make up to `MAX_AGENT_ITERATIONS` separate
    // `generate_with_tools_stream` calls, each returning its own
    // `usage_json`. Returning only the last one would hide every
    // intermediate prompt/completion from the billing pipeline — the
    // real cost of a 4-iteration turn is the sum of all 4 rounds, not
    // the last round alone. `merge_usage_into` normalises the shape
    // across providers (OpenAI `prompt_tokens` / Anthropic
    // `input_tokens`) before the billing layer sees it.
    let mut accumulated_usage = serde_json::json!({});
    let mut debug_iterations: Vec<super::llm_context_debug::LlmIterationDebug> = Vec::new();
    let mut assistant_grounding = AssistantGroundingEvidence::default();

    for iteration in 1..=MAX_AGENT_ITERATIONS {
        let iteration_started_at = std::time::Instant::now();
        let request_messages_snapshot = messages.clone();
        let tool_use_request = ToolUseRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url.clone(),
            temperature: binding.temperature,
            top_p: binding.top_p,
            max_output_tokens_override: binding.max_output_tokens_override,
            messages: messages.clone(),
            tools: tools.clone(),
            extra_parameters_json: binding.extra_parameters_json.clone(),
        };

        // Use the streaming variant so assistant text tokens are
        // forwarded to the UI the moment the provider emits them
        // instead of after the whole response finalizes. Tool-call
        // chunks are buffered inside the gateway and surfaced as the
        // usual `tool_calls` vector once the stream ends. If the
        // binding uses a provider that does not implement streaming,
        // the trait default falls back to non-streaming.
        //
        // The lifetime dance: `stream_delta_forwarder` captures a
        // mutable borrow of `on_progress`, keeps it alive for the
        // duration of the provider call, and drops it before we
        // touch `on_progress` again for tool-call events below.
        let llm_started_at = std::time::Instant::now();
        let response = {
            let progress = on_progress.clone();
            let mut stream_delta_forwarder = |delta: String| {
                if delta.is_empty() {
                    return;
                }
                if let Some(sender) = progress.as_ref() {
                    let _ = sender.send(AgentProgressEvent::AnswerDelta(delta));
                }
            };
            state
                .llm_gateway
                .generate_with_tools_stream(tool_use_request, &mut stream_delta_forwarder)
                .await
                .with_context(|| format!("LLM tool-use call failed (iteration {iteration})"))?
        };
        let llm_elapsed_ms = llm_started_at.elapsed().as_millis() as u64;

        let iteration_usage = response.usage_json.clone();
        merge_usage_into(&mut accumulated_usage, &iteration_usage);
        tracing::info!(
            iteration,
            llm_elapsed_ms,
            tool_call_count = response.tool_calls.len(),
            has_output_text = !response.output_text.is_empty(),
            provider_kind = %binding.provider_kind,
            model_name = %binding.model_name,
            request_id,
            %library_id,
            "assistant agent iteration: llm round-trip"
        );
        // iteration_started_at is measured here — the final "iteration completed"
        // line below reports llm + tool-dispatch in one delta.
        let _iteration_outer = iteration_started_at;

        // No tool calls? The model produced its final answer.
        if response.tool_calls.is_empty() {
            let answer = response.output_text.trim().to_string();
            debug_iterations.push(super::llm_context_debug::LlmIterationDebug {
                iteration,
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                request_messages: request_messages_snapshot,
                response_text: (!answer.is_empty()).then(|| answer.clone()),
                response_tool_calls: Vec::new(),
                usage: iteration_usage,
            });
            // Text has already been forwarded live through
            // `stream_delta_forwarder` as the provider produced it,
            // so we deliberately do NOT re-emit the whole answer
            // here — doing so would double every character in the
            // UI bubble. The final `Completed` frame from turn.rs
            // still carries the authoritative answer text.
            return Ok(AgentTurnResult {
                answer,
                provider,
                usage_json: accumulated_usage,
                iterations: iteration,
                tool_calls_total: total_tool_calls,
                assistant_grounding,
                debug_iterations,
            });
        }

        // Append the assistant's tool-call message so the model sees its own
        // history on the next iteration.
        messages.push(ChatMessage::assistant_with_tool_calls(response.tool_calls.clone()));

        // Execute every tool call in this iteration concurrently and
        // append each result as a `tool` message in the original order.
        //
        // Model-dictated order matters: when multiple tool_call blocks
        // are emitted, they must be echoed back to the model paired
        // with the same ids in the same position on the next
        // iteration. We preserve that by awaiting the futures in
        // lockstep (collecting results into a `Vec` indexed by the
        // original position) rather than using `join_all`'s arbitrary
        // completion order.
        //
        // Parallelism gain: a typical 5-tool iteration on a reference-sized library used
        // to cost ~sum(per-tool dispatch latency) ~1.5–3 s; concurrent
        // dispatch pins that at max(per-tool latency) ~300–700 ms,
        // trimming ~1.0–2.3 s per iteration on 6–10 iteration turns.
        total_tool_calls = total_tool_calls.saturating_add(response.tool_calls.len());
        let tool_dispatches: Vec<_> = response
            .tool_calls
            .iter()
            .map(|call| {
                let arguments_value: serde_json::Value = serde_json::from_str(&call.arguments_json)
                    .unwrap_or_else(|_| serde_json::json!({}));
                let progress = on_progress.clone();
                let call_id = call.id.clone();
                let name = call.name.clone();
                let arguments_preview = preview_text(&call.arguments_json, 240);
                async move {
                    if let Some(sender) = progress.as_ref() {
                        let _ = sender.send(AgentProgressEvent::ToolCallStarted {
                            iteration,
                            call_id: call_id.clone(),
                            name: name.clone(),
                            arguments_preview,
                        });
                    }
                    let dispatch =
                        dispatch_assistant_tool(state, auth, request_id, &name, &arguments_value)
                            .await;
                    let tool_text = truncate_tool_result(&dispatch.tool_message_text);
                    if let Some(sender) = progress.as_ref() {
                        let _ = sender.send(AgentProgressEvent::ToolCallCompleted {
                            iteration,
                            call_id: call_id.clone(),
                            name: name.clone(),
                            is_error: dispatch.is_error,
                            result_preview: preview_text(&tool_text, 240),
                        });
                    }
                    (dispatch, tool_text)
                }
            })
            .collect();
        let dispatch_outcomes = futures::future::join_all(tool_dispatches).await;

        let mut iteration_tool_debugs: Vec<super::llm_context_debug::ResponseToolCallDebug> =
            Vec::with_capacity(response.tool_calls.len());
        for (call, (dispatch, tool_text)) in
            response.tool_calls.iter().zip(dispatch_outcomes)
        {
            tracing::debug!(
                tool = %call.name,
                arguments = %call.arguments_json,
                is_error = dispatch.is_error,
                "assistant agent tool call"
            );
            assistant_grounding.record_tool_result(
                &call.name,
                &dispatch.tool_message_text,
                dispatch.is_error,
            );
            iteration_tool_debugs.push(super::llm_context_debug::ResponseToolCallDebug {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments_json: call.arguments_json.clone(),
                result_text: Some(tool_text.clone()),
                is_error: dispatch.is_error,
            });
            messages.push(ChatMessage::tool_result(call.id.clone(), call.name.clone(), tool_text));
        }
        debug_iterations.push(super::llm_context_debug::LlmIterationDebug {
            iteration,
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            request_messages: request_messages_snapshot,
            response_text: (!response.output_text.is_empty()).then(|| response.output_text.clone()),
            response_tool_calls: iteration_tool_debugs,
            usage: iteration_usage,
        });
        let iteration_total_ms = _iteration_outer.elapsed().as_millis() as u64;
        tracing::info!(
            iteration,
            iteration_total_ms,
            llm_ms = llm_elapsed_ms,
            tool_dispatch_ms = iteration_total_ms.saturating_sub(llm_elapsed_ms),
            request_id,
            %library_id,
            "assistant agent iteration: completed"
        );

        // Trim runaway tool messages so we never blow past context limits.
        if messages.len() > 80 {
            anyhow::bail!(
                "assistant agent loop exceeded {} messages without producing a final answer",
                messages.len()
            );
        }
    }

    // Iteration cap reached. Instead of bailing with an error — which
    // surfaces to the MCP client as `internal server error` and wastes
    // every tool call the agent already made — ask the model once more
    // for a final answer without tools, using the evidence it has
    // already accumulated. Any grounded partial answer is a strictly
    // better user experience than a hard fail on hard questions, and
    // the model still honours the grounding-discipline rules from the
    // system prompt (no hallucinated facts; say "library does not
    // contain this" when the evidence really isn't there).
    tracing::warn!(
        iterations = MAX_AGENT_ITERATIONS,
        tool_calls_total = total_tool_calls,
        "assistant agent loop cap reached — requesting finalize-from-evidence answer"
    );
    messages.push(ChatMessage::user(
        "You have reached the tool-call budget for this turn. Produce the final answer now in the \
         user's language, grounded strictly in the evidence you have already gathered from the \
         previous tool calls. No further tool calls are available. If that evidence is not enough \
         to answer the question, say so honestly — do not invent facts."
            .to_string(),
    ));
    let finalize_request = ToolUseRequest {
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
    let finalize_response = state
        .llm_gateway
        .generate_with_tools(finalize_request)
        .await
        .with_context(|| "assistant agent loop finalize LLM call failed")?;
    let answer = finalize_response.output_text.trim().to_string();
    debug_iterations.push(super::llm_context_debug::LlmIterationDebug {
        iteration: MAX_AGENT_ITERATIONS + 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: finalize_response.usage_json.clone(),
    });
    if let Some(sender) = on_progress.as_ref() {
        let _ = sender.send(AgentProgressEvent::AnswerDelta(answer.clone()));
    }
    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: finalize_response.usage_json,
        iterations: MAX_AGENT_ITERATIONS + 1,
        tool_calls_total: total_tool_calls,
        assistant_grounding,
        debug_iterations,
    })
}

/// Run one assistant turn as a single grounded-answer LLM call,
/// without exposing tools to the model.
///
/// This is the fast path for the common case where the retrieval
/// stage already assembled enough evidence to answer the question —
/// `prepare_answer_query` builds `answer_context` out of the top
/// retrieved chunks, graph-aware neighbours, recent documents, and
/// the library summary. Handing that context to the model in one
/// round-trip typically lands an answer in 3–8 s, versus the 45–90 s
/// the tool-using loop costs when it re-retrieves the same evidence
/// via 8–11 MCP tool calls.
///
/// Escalation is the caller's responsibility: if the single-shot
/// output is empty, admits it could not answer, or trips the
/// verifier, the caller should rerun the question through
/// [`run_assistant_turn`] with the full tool catalogue.
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

    // Same system + user message shape the tool loop uses on its
    // first iteration, but with the grounded context already baked
    // into the system prompt and no tool catalogue — the model can
    // only reply with text. Progress streaming is intentionally
    // skipped here: if the verifier later forces an escalation to
    // the tool loop we don't want UI/SSE clients to have already
    // received a partial single-shot answer that will be overwritten
    // by the tool-loop output. The caller is responsible for
    // emitting `AnswerDelta` once the single-shot answer is accepted.
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
        tool_calls_total: 0,
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
        tool_calls_total: 0,
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
fn merge_usage_into(accumulator: &mut serde_json::Value, iteration: &serde_json::Value) {
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

/// Shorten a string to `max_chars` characters on a UTF-8 char
/// boundary, appending an ellipsis when truncation occurred. Used for
/// tool-call arguments and result previews pushed to the UI — the
/// full text is still carried in the debug snapshot.
fn preview_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = String::with_capacity(max_chars + 1);
    for (i, ch) in text.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Enforce [`MAX_TOOL_RESULT_CHARS`] on a single tool result string.
///
/// The input is allowed to be any length; the returned string is at
/// most `MAX_TOOL_RESULT_CHARS + notice.len()` characters, truncated
/// on a UTF-8 char boundary and tagged with an explicit instruction
/// so the model knows to use `continuationToken` (or a tighter
/// search / page parameter) to fetch the remainder instead of
/// assuming the first window is complete.
fn truncate_tool_result(text: &str) -> String {
    if text.len() <= MAX_TOOL_RESULT_CHARS {
        return text.to_string();
    }
    let mut boundary = MAX_TOOL_RESULT_CHARS;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut truncated = String::with_capacity(boundary + 160);
    truncated.push_str(&text[..boundary]);
    truncated.push_str(
        "\n\n[tool result truncated to keep the provider payload under limit. \
If you need more of this document, call `read_document` again with a \
`continuationToken`, or narrow the query via `search_documents`.]",
    );
    truncated
}

#[cfg(test)]
mod tests {
    use super::{MAX_TOOL_RESULT_CHARS, truncate_tool_result};

    #[test]
    fn short_tool_results_pass_through() {
        let text = "compact result";
        assert_eq!(truncate_tool_result(text), text);
    }

    #[test]
    fn long_tool_results_are_truncated_with_notice() {
        let text = "x".repeat(MAX_TOOL_RESULT_CHARS * 4);
        let result = truncate_tool_result(&text);
        assert!(result.len() <= MAX_TOOL_RESULT_CHARS + 400);
        assert!(result.contains("[tool result truncated"));
    }

    #[test]
    fn truncation_respects_utf8_char_boundary() {
        // Cyrillic: every char is 2 bytes. If we happened to cut mid-char
        // the slicing below would panic; the point of the test is that it
        // returns a valid `String`.
        let text = "ы".repeat(MAX_TOOL_RESULT_CHARS);
        let result = truncate_tool_result(&text);
        assert!(result.contains("[tool result truncated"));
    }
}
