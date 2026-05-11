//! In-process MCP-agent tool-loop for the UI assistant path.
//!
//! # Scope (v1 — single-tool catalog)
//!
//! This version exposes **only `grounded_answer`** as the tool available
//! to the agent. That satisfies the MCP-UI parity requirement (constitution
//! §16): every content question is routed through the same
//! `execute_turn` pipeline the UI uses, so citations, verifier verdicts,
//! and `runtimeExecutionId` are identical.
//!
//! Future versions can broaden the tool catalog to include metadata tools
//! (`search_documents`, `read_document`, etc.) without changing the loop
//! contract. The scope decision is intentional: exposing a wide catalog
//! adds latency risk on the 25 s wall-clock deadline and complicates
//! result reconciliation (which execution do we promote as the canonical
//! answer?). Keeping it at one tool keeps the control flow auditable.
//!
//! # Result reconciliation
//!
//! The loop must return a `QueryTurnExecutionResult`. After the agent
//! finishes, we promote the **last** `grounded_answer` execution as the
//! canonical result. If the agent did not call `grounded_answer` at all
//! we return `ApiError::Conflict` — constitution §16 forbids returning
//! un-grounded answers.

use std::time::Duration;

use anyhow::Context as _;
use chrono::Utc;
use tokio::time::Instant;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose, catalog::CatalogLifecycleState, query::QueryConversationState,
    },
    infra::repositories::query_repository,
    integrations::llm::{ChatMessage, ChatToolDef, ToolUseRequest},
    interfaces::http::{
        auth::AuthContext,
        mcp::tools::{ToolCallContext, grounded},
        router_support::ApiError,
    },
    services::query::{
        llm_context_debug::{
            AgentLoopMetadata, AgentStopReason, LlmContextSnapshot, LlmIterationDebug,
            ResponseToolCallDebug,
        },
        service::{QueryTurnExecutionResult, normalize_required_text},
    },
};

/// Hard caps enforced by the agent loop.
/// These are also asserted in the smoke test below.
const MAX_ITERATIONS: usize = 6;
const WALL_CLOCK_DEADLINE: Duration = Duration::from_secs(28);
const PER_TOOL_TIMEOUT: Duration = Duration::from_secs(28);

/// Run one in-process MCP-agent turn.
///
/// Validates the conversation, resolves the `agent` binding, then drives a
/// tool-loop of up to `MAX_ITERATIONS` iterations (wall-clock capped at
/// `WALL_CLOCK_DEADLINE`). Each iteration may call `grounded_answer`; the
/// last successful call is promoted as the canonical result.
///
/// Returns `Err(ApiError::Conflict)` when the agent finishes without ever
/// invoking `grounded_answer` — constitution §16 requires grounded answers.
pub async fn run_mcp_agent_turn(
    state: &AppState,
    request_id: &str,
    auth: &AuthContext,
    conversation_id: Uuid,
    content_text: String,
    include_debug: bool,
) -> Result<QueryTurnExecutionResult, ApiError> {
    // ── 1. Validate conversation + library ────────────────────────────────────
    let conversation =
        query_repository::get_conversation_by_id(&state.persistence.postgres, conversation_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("conversation", conversation_id))?;

    if conversation.conversation_state != QueryConversationState::Active {
        return Err(ApiError::Conflict(format!("conversation {} is not active", conversation.id)));
    }

    let library =
        state.canonical_services.catalog.get_library(state, conversation.library_id).await?;
    // The grounded_answer MCP tool expects a `<workspace>/<library>`
    // catalog ref built from the canonical slugs (not UUIDs); we fetch
    // the workspace alongside the library so we can synthesize that ref
    // when invoking the tool.
    let workspace =
        state.canonical_services.catalog.get_workspace(state, library.workspace_id).await?;

    if library.workspace_id != conversation.workspace_id {
        return Err(ApiError::Conflict(format!(
            "conversation {} has library {} outside workspace {}",
            conversation.id, library.id, conversation.workspace_id
        )));
    }
    if library.lifecycle_state != CatalogLifecycleState::Active {
        return Err(ApiError::Conflict(format!("library {} is not active", library.id)));
    }

    // ── 2. Resolve agent binding ──────────────────────────────────────────────
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library.id, AiBindingPurpose::Agent)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "resolve agent binding"))?
        .ok_or_else(|| {
            ApiError::Conflict(format!(
                "library {} has no active 'agent' binding configured \
                 (configure one via admin → AI → bindings)",
                library.id
            ))
        })?;

    // ── 3. Derive synthetic library-scoped auth ───────────────────────────────
    let derived_auth = super::auth::derive_library_scoped_auth(
        auth.principal_id,
        library.workspace_id,
        library.id,
    );

    // ── 4. Create user turn record ────────────────────────────────────────────
    let content_text = normalize_required_text(&content_text, "contentText")?;
    let _request_turn = query_repository::create_turn(
        &state.persistence.postgres,
        &query_repository::NewQueryTurn {
            conversation_id: conversation.id,
            turn_kind: "user",
            author_principal_id: Some(auth.principal_id),
            content_text: &content_text,
            execution_id: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    // ── 5. Build grounded_answer tool descriptor ──────────────────────────────
    let descriptor = grounded::descriptor("grounded_answer").ok_or_else(|| {
        ApiError::internal_with_log(
            anyhow::anyhow!("grounded_answer descriptor missing"),
            "internal",
        )
    })?;
    // The MCP descriptor exposes `library` as required because external
    // MCP clients must pick which library their token addresses. The
    // in-process UI agent always operates inside one conversation and
    // its library is fixed; we strip `library` from the schema we hand
    // to the LLM so the model does not have to guess an identifier it
    // cannot know, then auto-inject the correct id when invoking the
    // tool below.
    let mut parameters = descriptor.input_schema.clone();
    if let Some(obj) = parameters.as_object_mut() {
        if let Some(properties) = obj.get_mut("properties").and_then(|v| v.as_object_mut()) {
            properties.remove("library");
        }
        if let Some(required) = obj.get_mut("required").and_then(|v| v.as_array_mut()) {
            required.retain(|item| item.as_str() != Some("library"));
        }
    }
    let grounded_tool = ChatToolDef {
        name: descriptor.name.to_string(),
        description: descriptor.description.to_string(),
        parameters,
    };

    // ── 6. System prompt ──────────────────────────────────────────────────────
    let system_prompt = super::prompt::render_agent_system_prompt(&library.display_name, None);

    // ── 7. Initialize message history ─────────────────────────────────────────
    let mut messages =
        vec![ChatMessage::system(system_prompt), ChatMessage::user(content_text.clone())];

    // ── 8. Tool-loop ──────────────────────────────────────────────────────────
    let wall_clock_start = Instant::now();
    let mut debug_iterations: Vec<LlmIterationDebug> = Vec::new();
    let mut tool_call_count: usize = 0;
    let mut last_grounded_execution_id: Option<Uuid> = None;
    let mut stopped_reason = AgentStopReason::IterationCap;

    'agent_loop: for _iter_index in 0..MAX_ITERATIONS {
        // Build the ToolUseRequest for this iteration.
        let tool_use_request = ToolUseRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url.clone(),
            temperature: binding.temperature,
            top_p: binding.top_p,
            max_output_tokens_override: binding.max_output_tokens_override,
            messages: messages.clone(),
            tools: vec![grounded_tool.clone()],
            extra_parameters_json: binding.extra_parameters_json.clone(),
            // Force the model to invoke grounded_answer at least once.
            // Constitution §16 requires UI assistant turns stay grounded
            // in retrieved citations; without `tool_choice="required"`
            // some models (DeepSeek thinking, gpt-5.4-mini on short
            // questions) answer from training and the post-loop guard
            // 409s. After a tool result lands in the message history,
            // tool_choice falls back to "auto" so the model can produce
            // the final natural-language answer.
            require_tool_call: tool_call_count == 0,
        };

        let response = state
            .llm_gateway
            .generate_with_tools(tool_use_request)
            .await
            .with_context(|| "agent loop LLM call failed")
            .map_err(|e| ApiError::internal_with_log(e, "agent llm call"))?;

        let mut iteration_tool_call_debugs: Vec<ResponseToolCallDebug> = Vec::new();
        let mut iteration_child_ids: Vec<Uuid> = Vec::new();

        if response.tool_calls.is_empty() {
            // Final answer — record the iteration and break.
            let answer_text = response.output_text.trim().to_string();
            debug_iterations.push(LlmIterationDebug {
                iteration: 0, // auto-numbered by append_iteration
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                request_messages: messages.clone(),
                response_text: (!answer_text.is_empty()).then(|| answer_text),
                response_tool_calls: Vec::new(),
                usage: response.usage_json,
                child_runtime_execution_ids: Vec::new(),
            });
            stopped_reason = AgentStopReason::FinalAnswer;
            break 'agent_loop;
        }

        // Append assistant tool-call message before the tool results.
        // DeepSeek thinking-mode requires the prior `reasoning_content` to
        // be echoed back on subsequent calls, so preserve it on the
        // assistant turn even though OpenAI/Qwen ignore the field.
        messages.push(ChatMessage::assistant_with_reasoning_and_tool_calls(
            response.reasoning_content.clone(),
            response.tool_calls.clone(),
        ));

        // Execute each tool call.
        for tool_call in &response.tool_calls {
            if tool_call.name != "grounded_answer" {
                // Unknown tool — return a generic error result so the model
                // can self-correct without crashing the loop.
                let error_text =
                    format!("unknown tool '{}'; only grounded_answer is available", tool_call.name);
                messages.push(ChatMessage::tool_result(
                    tool_call.id.clone(),
                    tool_call.name.clone(),
                    error_text.clone(),
                ));
                iteration_tool_call_debugs.push(ResponseToolCallDebug {
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    arguments_json: tool_call.arguments_json.clone(),
                    result_text: Some(error_text),
                    is_error: true,
                });
                continue;
            }

            tool_call_count += 1;

            let mut args: serde_json::Value = serde_json::from_str(&tool_call.arguments_json)
                .map_err(|e| ApiError::internal_with_log(e, "agent tool args"))?;

            // Propagate the caller's include_debug flag to grounded_answer
            // only when the LLM did not set it explicitly — so the UI agent
            // path surfaces debug metadata when the handler requests it.
            //
            // Auto-inject the `library` argument with the conversation's
            // library id. The MCP grounded_answer descriptor lists
            // `library` as required because external MCP clients
            // (openclaw, Telegram bots) must explicitly target a library
            // their token has access to. The in-process UI agent already
            // knows the library from the conversation, and exposing the
            // library ref to the LLM only invites it to invent a wrong
            // identifier and skip the call. Always overwrite whatever the
            // model emitted so the tool call resolves to the correct
            // library deterministically.
            if let Some(obj) = args.as_object_mut() {
                obj.entry("includeDebug").or_insert_with(|| include_debug.into());
                obj.insert(
                    "library".to_string(),
                    serde_json::Value::String(format!("{}/{}", workspace.slug, library.slug)),
                );
            }

            let ctx = ToolCallContext {
                auth: &derived_auth,
                state,
                request_id,
                surface_kind: crate::domains::agent_runtime::RuntimeSurfaceKind::Ui,
            };

            let tool_result = tokio::time::timeout(
                PER_TOOL_TIMEOUT,
                grounded::call_tool("grounded_answer", ctx, &args),
            )
            .await;

            let (result_text, is_error, child_exec_id) = match tool_result {
                Ok(Some(mcp_result)) => {
                    // Extract executionId from structured_content for tracking.
                    let exec_id = mcp_result
                        .structured_content
                        .get("executionId")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|s| s.parse::<Uuid>().ok());

                    let _text =
                        mcp_result.content.first().map(|b| b.text.clone()).unwrap_or_default();
                    let is_err = mcp_result.is_error;

                    if let Some(id) = exec_id {
                        if !is_err {
                            last_grounded_execution_id = Some(id);
                            iteration_child_ids.push(id);
                        }
                    }

                    let result_json =
                        serde_json::to_string(&mcp_result.structured_content).unwrap_or_default();

                    (result_json, is_err, exec_id)
                }
                Ok(None) => {
                    let error_text = "grounded_answer returned no result".to_string();
                    (error_text, true, None)
                }
                Err(_timeout) => {
                    let error_text =
                        "grounded_answer timed out (exceeded per-tool deadline)".to_string();
                    stopped_reason = AgentStopReason::Deadline;
                    messages.push(ChatMessage::tool_result(
                        tool_call.id.clone(),
                        tool_call.name.clone(),
                        error_text.clone(),
                    ));
                    iteration_tool_call_debugs.push(ResponseToolCallDebug {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments_json: tool_call.arguments_json.clone(),
                        result_text: Some(error_text),
                        is_error: true,
                    });
                    break 'agent_loop;
                }
            };

            messages.push(ChatMessage::tool_result(
                tool_call.id.clone(),
                tool_call.name.clone(),
                result_text.clone(),
            ));
            iteration_tool_call_debugs.push(ResponseToolCallDebug {
                id: tool_call.id.clone(),
                name: tool_call.name.clone(),
                arguments_json: tool_call.arguments_json.clone(),
                result_text: Some(result_text),
                is_error,
            });
            let _ = child_exec_id; // already handled above
        }

        // Record this iteration.
        debug_iterations.push(LlmIterationDebug {
            iteration: 0, // auto-numbered by append_iteration
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            request_messages: messages.clone(),
            response_text: None,
            response_tool_calls: iteration_tool_call_debugs,
            usage: response.usage_json,
            child_runtime_execution_ids: iteration_child_ids,
        });

        // Check wall-clock deadline.
        if wall_clock_start.elapsed() >= WALL_CLOCK_DEADLINE {
            stopped_reason = AgentStopReason::Deadline;
            break 'agent_loop;
        }
    }

    // ── 9. Result reconciliation ──────────────────────────────────────────────
    let canonical_execution_id = last_grounded_execution_id.ok_or_else(|| {
        ApiError::Conflict(
            "agent finished without invoking grounded_answer; \
             UI assistant answers must be grounded"
                .to_string(),
        )
    })?;

    let detail =
        state.canonical_services.query.get_execution(state, canonical_execution_id).await?;

    // Write agent-loop debug snapshot keyed by the canonical execution_id.
    let mut snapshot = LlmContextSnapshot {
        execution_id: canonical_execution_id,
        library_id: library.id,
        question: content_text.clone(),
        iterations: Vec::new(),
        total_iterations: debug_iterations.len(),
        final_answer: detail.response_turn.as_ref().map(|t| t.content_text.clone()),
        captured_at: Utc::now(),
        query_ir: None,
        agent_loop: Some(AgentLoopMetadata {
            iteration_cap: MAX_ITERATIONS,
            deadline_ms: WALL_CLOCK_DEADLINE.as_millis() as u64,
            stopped_reason,
            tool_call_count,
        }),
    };
    for iteration in debug_iterations {
        snapshot.append_iteration(iteration);
    }
    state.llm_context_debug.insert(snapshot);

    // Fetch the conversation record to populate QueryTurnExecutionResult.
    let canonical_conversation = query_repository::get_conversation_by_id(
        &state.persistence.postgres,
        detail.execution.conversation_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| {
        ApiError::resource_not_found("conversation", detail.execution.conversation_id)
    })?;

    // Map QueryConversation row into the domain type.
    use crate::services::query::service::map_conversation_row;
    let canonical_conversation = map_conversation_row(canonical_conversation);

    let request_turn = detail.request_turn.clone().unwrap_or_else(|| {
        // Synthetic fallback: the user turn record we created above. The
        // underlying execution always stores a request_turn_id, so this
        // branch is defensive and should not occur in practice.
        use crate::domains::query::{QueryTurn, QueryTurnKind};
        QueryTurn {
            id: _request_turn.id,
            conversation_id: conversation.id,
            turn_index: _request_turn.turn_index,
            turn_kind: QueryTurnKind::User,
            author_principal_id: Some(auth.principal_id),
            content_text: content_text.clone(),
            execution_id: None,
            created_at: Utc::now(),
        }
    });

    let context_bundle_id = detail.execution.context_bundle_id;
    Ok(QueryTurnExecutionResult {
        conversation: canonical_conversation,
        request_turn,
        response_turn: detail.response_turn,
        execution: detail.execution,
        runtime_summary: detail.runtime_summary,
        runtime_stage_summaries: detail.runtime_stage_summaries,
        context_bundle_id,
        chunk_references: detail.chunk_references,
        prepared_segment_references: detail.prepared_segment_references,
        technical_fact_references: detail.technical_fact_references,
        graph_node_references: detail.graph_node_references,
        graph_edge_references: detail.graph_edge_references,
        verification_state: detail.verification_state,
        verification_warnings: detail.verification_warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_loop_constants_match_consensus() {
        assert_eq!(MAX_ITERATIONS, 6);
        assert_eq!(WALL_CLOCK_DEADLINE.as_secs(), 28);
        assert_eq!(PER_TOOL_TIMEOUT.as_secs(), 28);
    }
}
