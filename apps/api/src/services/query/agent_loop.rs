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
    integrations::llm::{ChatMessage, ToolUseRequest},
    interfaces::http::auth::AuthContext,
    interfaces::http::mcp::agent_bridge::{dispatch_assistant_tool, list_assistant_tools},
    services::query::assistant_grounding::AssistantGroundingEvidence,
};

/// Maximum number of LLM <-> tool round trips per turn. Each iteration is
/// one LLM call. Real assistants almost never need more than 4–5; the cap
/// exists purely as a runaway guard.
/// Upper bound on tool-call rounds for the assistant agent loop.
///
/// A round is one LLM response + tool dispatch pair; the cap is a
/// circuit-breaker against runaway planning, NOT a product budget.
/// Empirically the old cap of 10 was not enough for grounded answers
/// on large libraries that span many documents: the agent routinely
/// needs 1 list + 2-3 search refinements + 4-6 read_document calls
/// (with continuation tokens on long docs) before it has enough
/// evidence. The per-result truncation below keeps the provider
/// payload bounded regardless of how many iterations the agent runs.
const MAX_AGENT_ITERATIONS: usize = 20;

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
    mut on_progress: Option<&mut (dyn FnMut(AgentProgressEvent) + Send)>,
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
    //    the active library id substituted in. Keep this path
    //    trivially thin so the in-app assistant and external agents
    //    see the same guidance.
    let mut messages = Vec::new();
    let system_prompt = super::assistant_prompt::render(library_id, conversation_history);
    messages.push(ChatMessage::system(system_prompt));
    messages.push(ChatMessage::user(user_question.to_string()));

    let mut total_tool_calls = 0usize;
    let mut last_usage = serde_json::json!({});
    let mut debug_iterations: Vec<super::llm_context_debug::LlmIterationDebug> = Vec::new();
    let mut assistant_grounding = AssistantGroundingEvidence::default();

    for iteration in 1..=MAX_AGENT_ITERATIONS {
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
        let response = {
            let progress_slot: &mut Option<&mut (dyn FnMut(AgentProgressEvent) + Send)> =
                &mut on_progress;
            let mut stream_delta_forwarder = |delta: String| {
                if delta.is_empty() {
                    return;
                }
                if let Some(emit) = progress_slot.as_deref_mut() {
                    emit(AgentProgressEvent::AnswerDelta(delta));
                }
            };
            state
                .llm_gateway
                .generate_with_tools_stream(tool_use_request, &mut stream_delta_forwarder)
                .await
                .with_context(|| format!("LLM tool-use call failed (iteration {iteration})"))?
        };

        last_usage = response.usage_json.clone();

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
                usage: last_usage.clone(),
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
                usage_json: last_usage,
                iterations: iteration,
                tool_calls_total: total_tool_calls,
                assistant_grounding,
                debug_iterations,
            });
        }

        // Append the assistant's tool-call message so the model sees its own
        // history on the next iteration.
        messages.push(ChatMessage::assistant_with_tool_calls(response.tool_calls.clone()));

        // Execute each tool call and append the result as a `tool` message.
        // (Sequential for now — parallelizing with buffered streams hits
        // an HRTB-lifetime Send overflow in the surrounding async
        // body that needs a larger refactor to fix cleanly.)
        let mut iteration_tool_debugs: Vec<super::llm_context_debug::ResponseToolCallDebug> =
            Vec::with_capacity(response.tool_calls.len());
        for call in &response.tool_calls {
            total_tool_calls = total_tool_calls.saturating_add(1);
            let arguments_value: serde_json::Value = serde_json::from_str(&call.arguments_json)
                .unwrap_or_else(|_| serde_json::json!({}));
            if let Some(emit) = on_progress.as_deref_mut() {
                emit(AgentProgressEvent::ToolCallStarted {
                    iteration,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments_preview: preview_text(&call.arguments_json, 240),
                });
            }
            let dispatch =
                dispatch_assistant_tool(state, auth, request_id, &call.name, &arguments_value)
                    .await;
            tracing::debug!(
                tool = %call.name,
                arguments = %call.arguments_json,
                is_error = dispatch.is_error,
                "assistant agent tool call"
            );
            let tool_text = truncate_tool_result(&dispatch.tool_message_text);
            assistant_grounding.record_tool_result(
                &call.name,
                &dispatch.tool_message_text,
                dispatch.is_error,
            );
            if let Some(emit) = on_progress.as_deref_mut() {
                emit(AgentProgressEvent::ToolCallCompleted {
                    iteration,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    is_error: dispatch.is_error,
                    result_preview: preview_text(&tool_text, 240),
                });
            }
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
            usage: last_usage.clone(),
        });

        // Trim runaway tool messages so we never blow past context limits.
        if messages.len() > 80 {
            anyhow::bail!(
                "assistant agent loop exceeded {} messages without producing a final answer",
                messages.len()
            );
        }
    }

    anyhow::bail!(
        "assistant agent loop exceeded {MAX_AGENT_ITERATIONS} iterations without producing a final answer"
    )
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
