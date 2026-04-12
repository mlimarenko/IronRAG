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
};

/// Maximum number of LLM <-> tool round trips per turn. Each iteration is
/// one LLM call. Real assistants almost never need more than 4–5; the cap
/// exists purely as a runaway guard.
const MAX_AGENT_ITERATIONS: usize = 10;

/// Final result of one assistant turn.
#[derive(Debug, Clone)]
pub struct AgentTurnResult {
    pub answer: String,
    pub provider: ProviderModelSelection,
    pub usage_json: serde_json::Value,
    pub iterations: usize,
    pub tool_calls_total: usize,
}

/// Run one assistant turn through the LLM agent loop.
///
/// `library_id` is the active library; the agent is told to keep its work
/// scoped to it. `conversation_history` is a flat text rendering of the
/// prior turns (oldest first), used as a single system message so the
/// model can resolve references to earlier turns.
///
/// `on_delta` is invoked with the final assistant answer in one shot once
/// the loop exits. Token-level streaming through tool-using models is
/// provider-specific; we keep the public surface stable by emitting the
/// final text as a single delta event.
pub async fn run_assistant_turn(
    state: &AppState,
    auth: &AuthContext,
    library_id: Uuid,
    request_id: &str,
    user_question: &str,
    conversation_history: Option<&str>,
    mut on_delta: Option<&mut (dyn FnMut(String) + Send)>,
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

    // 3. Build the conversation messages for the LLM.
    let mut messages = Vec::new();
    let system_prompt = build_assistant_system_prompt(library_id, conversation_history);
    messages.push(ChatMessage::system(system_prompt));
    messages.push(ChatMessage::user(user_question.to_string()));

    let mut total_tool_calls = 0usize;
    let mut last_usage = serde_json::json!({});

    for iteration in 1..=MAX_AGENT_ITERATIONS {
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

        let response = state
            .llm_gateway
            .generate_with_tools(tool_use_request)
            .await
            .with_context(|| format!("LLM tool-use call failed (iteration {iteration})"))?;

        last_usage = response.usage_json.clone();

        // No tool calls? The model produced its final answer.
        if response.tool_calls.is_empty() {
            let answer = response.output_text.trim().to_string();
            if let Some(emit) = on_delta.as_deref_mut() {
                if !answer.is_empty() {
                    emit(answer.clone());
                }
            }
            return Ok(AgentTurnResult {
                answer,
                provider,
                usage_json: last_usage,
                iterations: iteration,
                tool_calls_total: total_tool_calls,
            });
        }

        // Append the assistant's tool-call message so the model sees its own
        // history on the next iteration.
        messages.push(ChatMessage::assistant_with_tool_calls(response.tool_calls.clone()));

        // Execute each tool call and append the result as a `tool` message.
        for call in &response.tool_calls {
            total_tool_calls = total_tool_calls.saturating_add(1);
            let arguments_value: serde_json::Value = serde_json::from_str(&call.arguments_json)
                .unwrap_or_else(|_| serde_json::json!({}));

            let dispatch =
                dispatch_assistant_tool(state, auth, request_id, &call.name, &arguments_value)
                    .await;

            tracing::debug!(
                tool = %call.name,
                arguments = %call.arguments_json,
                is_error = dispatch.is_error,
                "assistant agent tool call"
            );

            messages.push(ChatMessage::tool_result(
                call.id.clone(),
                call.name.clone(),
                dispatch.tool_message_text,
            ));
        }

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

fn build_assistant_system_prompt(library_id: Uuid, conversation_history: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are an in-app assistant connected to the IronRAG knowledge platform via MCP \
tools. You behave like a vanilla MCP user agent: you have NO built-in retrieval, no \
hidden context, and no special access — only the tools listed below.\n\n",
    );
    prompt.push_str(&format!(
        "The user is currently working in library `{library_id}`. Pass this library id \
to every tool that requires a `libraryId` argument unless the user explicitly asks you \
to look at a different library.\n\n",
    ));
    prompt.push_str(
        "Workflow:\n\
        1. Decide which tool(s) you need to answer the question.\n\
        2. Call them through the function-calling interface; the runtime will execute \
each call and return the JSON result.\n\
        3. Iterate until you have enough grounded information.\n\
        4. Produce a clear, concise answer in the user's language. Cite document or \
table names when they are useful, but do not narrate the tool calls themselves.\n\
        5. If the tools return nothing useful, say so honestly — do NOT invent facts.\n\n",
    );
    prompt.push_str(
        "When the user asks a meta question (\"what is this library about\", \"what \
documents do you have\"), call `list_documents` (and optionally `list_libraries` / \
`get_graph_topology`) before answering.\n\n",
    );
    prompt.push_str(
        "When the user asks about specific records or aggregates (\"top customers\", \
\"how many products\", \"popular cities\"), call `search_documents` or \
`read_document` to load the actual table content before computing the answer.\n",
    );
    prompt.push_str(
        "When the user asks about images or photos, identify the relevant image documents \
with `list_documents` or `search_documents`, then call `read_document`. Image reads may \
include `sourceAccess` plus a `visualDescription` derived from the original source image; \
prefer that grounded description over guessing from OCR fragments alone.\n",
    );

    if let Some(history) = conversation_history.map(str::trim).filter(|h| !h.is_empty()) {
        prompt.push_str("\nRecent conversation (oldest first):\n");
        prompt.push_str(history);
    }

    prompt
}
