//! LLM turn helpers used by the assistant answer surfaces.
//!
//! The in-app UI assistant runs as the same kind of tool-using MCP
//! client agent an external chat client would run: the model sees the
//! answer-tool registry, chooses tool calls, receives tool
//! results, and then writes the final reply.

use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use futures::{StreamExt as _, stream};
use serde_json::Value;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::provider_profiles::ProviderModelSelection,
    domains::{agent_runtime::RuntimeSurfaceKind, ai::AiBindingPurpose},
    integrations::llm::{ChatMessage, ChatToolCall, ChatToolDef, ToolUseRequest},
    interfaces::http::{
        auth::AuthContext,
        mcp::{
            McpToolSurface,
            tools::{
                self, ToolCallContext,
                documents::{READ_DOCUMENT_TOOL_NAME, SEARCH_DOCUMENTS_TOOL_NAME},
            },
        },
    },
    services::query::{
        assistant_grounding::AssistantGroundingEvidence,
        error::QueryServiceError,
        llm_context_debug::{
            AgentLoopMetadata, AgentStopReason, LlmIterationDebug, ResponseToolCallDebug,
        },
    },
};

const RUNTIME_RETRIEVED_CONTEXT_TOOL: &str = "ironrag_retrieved_context";
const RUNTIME_LITERAL_REVISION_CONTEXT_TOOL: &str = "ironrag_literal_revision_context";
const RUNTIME_CLARIFY_VARIANTS_TOOL: &str = "ironrag_clarify_variants";
const GROUNDED_ANSWER_TOOL_NAME: &str = "grounded_answer";
const TOOL_MODEL_DEFAULT_CONTENT_CHAR_LIMIT: usize = 3_000;
const TOOL_MODEL_GROUNDED_ANSWER_CONTENT_CHAR_LIMIT: usize = 6_000;
const TOOL_MODEL_READ_DOCUMENT_CONTENT_CHAR_LIMIT: usize = 5_000;
const TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT: usize = 8_000;
const TOOL_VERIFICATION_CONTENT_CHAR_LIMIT: usize = 8_000;
const TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT: usize = 16_000;
const TOOL_MODEL_GROUNDED_REFERENCE_LIMIT: usize = 8;
const TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT: usize = 96_000;
const TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT: usize = 20_000;
const TOOL_GROUNDING_TOTAL_CHAR_LIMIT: usize = 80_000;
const SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS: usize = 4;
const MIN_COMPOSITE_DISTINCT_SUCCESSFUL_TOOLS_BEFORE_FINAL: usize = 3;

/// Final result of one assistant turn.
#[derive(Debug, Clone)]
pub struct AgentTurnResult {
    pub answer: String,
    pub provider: ProviderModelSelection,
    pub usage_json: serde_json::Value,
    pub iterations: usize,
    pub assistant_grounding: AssistantGroundingEvidence,
    pub child_query_execution_ids: Vec<Uuid>,
    /// Per-iteration capture of the exact LLM request/response chain,
    /// for the assistant debug panel. Populated unconditionally — the
    /// cost is a few clones and the operator toggles the UI to view.
    pub debug_iterations: Vec<super::llm_context_debug::LlmIterationDebug>,
    /// Present when a turn was driven by the MCP client-style agent
    /// loop instead of a single fixed-context answer stage.
    pub agent_loop: Option<AgentLoopMetadata>,
}

/// Agent-loop failure with the partial provider transcript preserved
/// for the debug panel.
#[derive(Debug)]
pub struct AgentTurnFailure {
    pub error: QueryServiceError,
    pub debug_iterations: Vec<LlmIterationDebug>,
    pub agent_loop: Option<AgentLoopMetadata>,
}

impl AgentTurnFailure {
    fn empty(error: impl Into<QueryServiceError>) -> Self {
        Self { error: error.into(), debug_iterations: Vec::new(), agent_loop: None }
    }

    fn with_loop(
        error: impl Into<QueryServiceError>,
        debug_iterations: Vec<LlmIterationDebug>,
        agent_loop: AgentLoopMetadata,
    ) -> Self {
        Self { error: error.into(), debug_iterations, agent_loop: Some(agent_loop) }
    }
}

impl fmt::Display for AgentTurnFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

impl Error for AgentTurnFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

/// Inputs for the UI assistant's MCP-backed tool loop.
#[derive(Clone)]
pub struct McpToolAgentTurnInput<'a> {
    pub state: &'a AppState,
    pub auth: &'a AuthContext,
    pub library_id: Uuid,
    pub library_ref: &'a str,
    pub user_question: &'a str,
    pub conversation_history: &'a [ChatMessage],
    pub request_id: &'a str,
    pub grounded_answer_top_k: usize,
    pub iteration_cap: usize,
    pub max_parallel_actions: usize,
    pub deadline: Duration,
    pub soft_final_answer_deadline: Option<Duration>,
    pub activity_tx: Option<Sender<AgentLoopActivityEvent>>,
}

#[derive(Debug, Clone)]
pub enum AgentLoopActivityEvent {
    ModelRequest {
        iteration: usize,
        provider_kind: String,
        model_name: String,
    },
    ModelResponse {
        iteration: usize,
        provider_kind: String,
        model_name: String,
        tool_call_count: usize,
        has_final_answer: bool,
    },
    ToolCallStarted {
        iteration: usize,
        tool_name: String,
    },
    ToolCallFinished {
        iteration: usize,
        tool_name: String,
        elapsed_ms: u64,
        is_error: bool,
        child_execution_id: Option<Uuid>,
        result_preview: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct ToolExecutionOutcome {
    message_content: String,
    result_text: Option<String>,
    result_json: Option<Value>,
    grounding_text: Option<String>,
    verified_grounded_answer_text: Option<String>,
    is_error: bool,
    child_query_execution_ids: Vec<Uuid>,
    child_runtime_execution_ids: Vec<Uuid>,
}

/// Build the LLM-facing tool definitions from the MCP
/// descriptors. MCP JSON-RPC and in-process UI agent calls therefore
/// share one schema source of truth.
pub(crate) fn answer_surface_tool_defs(auth: &AuthContext) -> Vec<ChatToolDef> {
    tools::visible_tool_names(auth, McpToolSurface::Answer)
        .into_iter()
        .filter_map(|name| tools::descriptor_for(&name))
        .map(|descriptor| ChatToolDef {
            name: descriptor.name.to_string(),
            description: descriptor.description.to_string(),
            parameters: descriptor.input_schema,
        })
        .collect()
}

/// Run the web UI assistant as a native tool-using agent over the
/// answer MCP surface. The model chooses tools, receives real tool
/// results, can fan out independent calls within one iteration, and
/// can refine the next query from prior results.
pub async fn run_mcp_tool_agent_turn(
    input: McpToolAgentTurnInput<'_>,
) -> Result<AgentTurnResult, AgentTurnFailure> {
    let binding = input
        .state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(
            input.state,
            input.library_id,
            AiBindingPurpose::QueryAnswer,
        )
        .await
        .map_err(|e| {
            AgentTurnFailure::empty(anyhow::anyhow!("failed to resolve query_answer binding: {e}"))
        })?
        .ok_or_else(|| {
            AgentTurnFailure::empty(anyhow::anyhow!(
                "no active query_answer binding configured for library {}",
                input.library_id
            ))
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };
    let tool_defs = answer_surface_tool_defs(input.auth);
    if tool_defs.is_empty() {
        return Err(AgentTurnFailure::empty(anyhow::anyhow!(
            "no MCP answer tools are visible for the current caller"
        )));
    }

    let iteration_cap = input.iteration_cap.max(1);
    let max_parallel_actions = input.max_parallel_actions.max(1);
    let deadline_started = Instant::now();
    let mut messages =
        Vec::with_capacity(input.conversation_history.len().saturating_add(iteration_cap * 3 + 2));
    messages.push(ChatMessage::system(super::assistant_prompt::render(input.library_ref, None)));
    messages.extend(input.conversation_history.iter().cloned());
    messages.push(ChatMessage::user(input.user_question.to_string()));

    let mut usage_json = serde_json::json!({});
    let mut debug_iterations = Vec::new();
    let mut total_tool_call_count = 0usize;
    let mut successful_tool_call_count = 0usize;
    let mut successful_tool_names = BTreeSet::new();
    let mut verified_grounded_answer_call_count = 0usize;
    let mut assistant_grounding = AssistantGroundingEvidence::default();
    let mut child_query_execution_ids = Vec::new();
    let mut stopped_reason = AgentStopReason::IterationCap;
    let mut last_required_tool_refusal_answer: Option<String> = None;

    // There is no hidden post-loop synthesis pass: the model must spend
    // one of these iterations on a final answer after seeing tool results.
    // The caller budgets one extra iteration beyond the tool-round cap.
    for iteration in 1..=iteration_cap {
        let Some(deadline_remaining) = deadline_remaining(deadline_started, input.deadline) else {
            stopped_reason = AgentStopReason::Deadline;
            break;
        };

        let force_final_answer = force_final_answer_iteration(
            iteration,
            iteration_cap,
            total_tool_call_count,
            successful_tool_call_count,
            &successful_tool_names,
            verified_grounded_answer_call_count,
            deadline_started,
            input.soft_final_answer_deadline,
        );
        let required_distinct_tool_count =
            required_distinct_tool_count(&tool_defs, &successful_tool_names);
        let require_tool_call = should_require_tool_call_before_final(
            force_final_answer,
            &tool_defs,
            &successful_tool_names,
        );
        let tools_for_iteration =
            tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, force_final_answer);
        let request_messages = messages.clone();
        emit_activity(
            &input.activity_tx,
            AgentLoopActivityEvent::ModelRequest {
                iteration,
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
            },
        );
        let response = match tokio::time::timeout(
            deadline_remaining,
            input.state.llm_gateway.generate_with_tools(ToolUseRequest {
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                api_key_override: binding.api_key.clone(),
                base_url_override: binding.provider_base_url.clone(),
                temperature: binding.temperature,
                top_p: binding.top_p,
                max_output_tokens_override: binding.max_output_tokens_override,
                messages: request_messages.clone(),
                tools: tools_for_iteration,
                extra_parameters_json: binding.extra_parameters_json.clone(),
                require_tool_call,
            }),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return Err(AgentTurnFailure::with_loop(
                    error.context("MCP-backed assistant agent LLM call failed"),
                    debug_iterations.clone(),
                    agent_loop_metadata(
                        iteration_cap,
                        input.deadline,
                        AgentStopReason::ProviderError,
                        total_tool_call_count,
                    ),
                ));
            }
            Err(_) => {
                return Err(AgentTurnFailure::with_loop(
                    anyhow::anyhow!(
                        "assistant agent exceeded its turn deadline while waiting for the model"
                    ),
                    debug_iterations.clone(),
                    agent_loop_metadata(
                        iteration_cap,
                        input.deadline,
                        AgentStopReason::Deadline,
                        total_tool_call_count,
                    ),
                ));
            }
        };
        merge_usage_into(&mut usage_json, &response.usage_json);

        if response.tool_calls.is_empty() {
            let answer = response.output_text.trim().to_string();
            emit_activity(
                &input.activity_tx,
                AgentLoopActivityEvent::ModelResponse {
                    iteration,
                    provider_kind: binding.provider_kind.clone(),
                    model_name: binding.model_name.clone(),
                    tool_call_count: 0,
                    has_final_answer: !answer.is_empty(),
                },
            );
            if answer.is_empty() {
                return Err(AgentTurnFailure::with_loop(
                    anyhow::anyhow!("assistant agent returned an empty final answer"),
                    debug_iterations,
                    agent_loop_metadata(
                        iteration_cap,
                        input.deadline,
                        AgentStopReason::ProviderError,
                        total_tool_call_count,
                    ),
                ));
            }
            debug_iterations.push(LlmIterationDebug {
                iteration,
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                request_messages,
                response_text: Some(answer.clone()),
                response_tool_calls: Vec::new(),
                usage: response.usage_json,
                child_runtime_execution_ids: Vec::new(),
            });
            if require_tool_call {
                if last_required_tool_refusal_answer.is_some() || iteration == iteration_cap {
                    stopped_reason = AgentStopReason::FinalAnswer;
                    return Ok(AgentTurnResult {
                        answer,
                        provider,
                        usage_json,
                        iterations: debug_iterations.len(),
                        assistant_grounding,
                        child_query_execution_ids,
                        debug_iterations,
                        agent_loop: Some(agent_loop_metadata(
                            iteration_cap,
                            input.deadline,
                            stopped_reason,
                            total_tool_call_count,
                        )),
                    });
                }
                last_required_tool_refusal_answer = Some(answer.clone());
                messages.push(ChatMessage::assistant_text(answer));
                messages.push(ChatMessage::system(tool_requirement_reminder(
                    successful_tool_names.len(),
                    required_distinct_tool_count,
                )));
                continue;
            }
            stopped_reason = AgentStopReason::FinalAnswer;
            return Ok(AgentTurnResult {
                answer,
                provider,
                usage_json,
                iterations: debug_iterations.len(),
                assistant_grounding,
                child_query_execution_ids,
                debug_iterations,
                agent_loop: Some(agent_loop_metadata(
                    iteration_cap,
                    input.deadline,
                    stopped_reason,
                    total_tool_call_count,
                )),
            });
        }

        let tool_calls = response.tool_calls.clone();
        emit_activity(
            &input.activity_tx,
            AgentLoopActivityEvent::ModelResponse {
                iteration,
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                tool_call_count: tool_calls.len(),
                has_final_answer: false,
            },
        );
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: (!response.output_text.trim().is_empty())
                .then(|| response.output_text.trim().to_string()),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: tool_calls.clone(),
            tool_call_id: None,
            name: None,
        });

        let outcomes = execute_tool_calls(
            input.clone(),
            iteration,
            &tool_calls,
            max_parallel_actions,
            deadline_started,
        )
        .await;
        total_tool_call_count = total_tool_call_count.saturating_add(tool_calls.len());

        let mut response_tool_calls = Vec::with_capacity(tool_calls.len());
        let mut child_runtime_execution_ids = Vec::new();
        let can_return_verified_grounded_answer =
            can_return_verified_grounded_answer_without_synthesis(&tool_calls);
        let mut verified_grounded_answer_final: Option<String> = None;
        for (call, outcome) in tool_calls.iter().zip(outcomes.iter()) {
            child_query_execution_ids.extend(outcome.child_query_execution_ids.iter().copied());
            child_runtime_execution_ids.extend(outcome.child_runtime_execution_ids.iter().copied());
            if !outcome.is_error {
                successful_tool_call_count = successful_tool_call_count.saturating_add(1);
                successful_tool_names.insert(call.name.clone());
                if let Some(answer_text) = &outcome.verified_grounded_answer_text {
                    verified_grounded_answer_call_count =
                        verified_grounded_answer_call_count.saturating_add(1);
                    if can_return_verified_grounded_answer {
                        verified_grounded_answer_final = Some(answer_text.clone());
                    }
                }
                if let Some(grounding_text) = &outcome.grounding_text {
                    push_tool_grounding_fragment(
                        &mut assistant_grounding,
                        &call.name,
                        grounding_text,
                    );
                }
            }
            response_tool_calls.push(ResponseToolCallDebug {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments_json: call.arguments_json.clone(),
                result_text: outcome.result_text.clone(),
                result_json: outcome.result_json.clone(),
                is_error: outcome.is_error,
            });
            messages.push(ChatMessage::tool_result(
                call.id.clone(),
                call.name.clone(),
                outcome.message_content.clone(),
            ));
        }

        debug_iterations.push(LlmIterationDebug {
            iteration,
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            request_messages,
            response_text: (!response.output_text.trim().is_empty())
                .then(|| response.output_text.trim().to_string()),
            response_tool_calls,
            usage: response.usage_json,
            child_runtime_execution_ids,
        });
        if let Some(answer) = verified_grounded_answer_final {
            stopped_reason = AgentStopReason::FinalAnswer;
            return Ok(AgentTurnResult {
                answer,
                provider,
                usage_json,
                iterations: debug_iterations.len(),
                assistant_grounding,
                child_query_execution_ids,
                debug_iterations,
                agent_loop: Some(agent_loop_metadata(
                    iteration_cap,
                    input.deadline,
                    stopped_reason,
                    total_tool_call_count,
                )),
            });
        }
    }

    if matches!(stopped_reason, AgentStopReason::IterationCap)
        && successful_tool_call_count == 0
        && total_tool_call_count > 0
    {
        stopped_reason = AgentStopReason::ToolError;
    }

    let mut message = match stopped_reason {
        AgentStopReason::Deadline => {
            "assistant agent exceeded its turn deadline before producing a final answer"
        }
        AgentStopReason::IterationCap => {
            "assistant agent reached its iteration cap before producing a final answer"
        }
        AgentStopReason::FinalAnswer => "assistant agent stopped before producing a final answer",
        AgentStopReason::ToolError => "assistant agent stopped after a tool error",
        AgentStopReason::ProviderError => "assistant agent stopped after a provider error",
    }
    .to_string();
    if successful_tool_call_count == 0 && total_tool_call_count > 0 {
        message.push_str("; no successful MCP tool result was received");
    }
    Err(AgentTurnFailure::with_loop(
        anyhow::anyhow!(message),
        debug_iterations,
        agent_loop_metadata(iteration_cap, input.deadline, stopped_reason, total_tool_call_count),
    ))
}

fn agent_loop_metadata(
    iteration_cap: usize,
    deadline: Duration,
    stopped_reason: AgentStopReason,
    total_tool_call_count: usize,
) -> AgentLoopMetadata {
    AgentLoopMetadata {
        iteration_cap,
        deadline_ms: deadline.as_millis().try_into().unwrap_or(u64::MAX),
        stopped_reason,
        tool_call_count: total_tool_call_count,
    }
}

fn force_final_answer_iteration(
    iteration: usize,
    iteration_cap: usize,
    total_tool_call_count: usize,
    successful_tool_call_count: usize,
    successful_tool_names: &BTreeSet<String>,
    verified_grounded_answer_call_count: usize,
    started: Instant,
    soft_final_answer_deadline: Option<Duration>,
) -> bool {
    if iteration == iteration_cap && total_tool_call_count > 0 {
        return true;
    }
    if verified_grounded_answer_call_count > 0 {
        return true;
    }
    if successful_tool_call_count >= SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS
        && successful_tool_names.contains(READ_DOCUMENT_TOOL_NAME)
        && has_composite_tool_signal(successful_tool_names)
    {
        return true;
    }

    let Some(soft_deadline) = soft_final_answer_deadline else {
        return false;
    };
    if started.elapsed() < soft_deadline {
        return false;
    }

    successful_tool_call_count >= SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS
        && has_composite_tool_signal(successful_tool_names)
}

fn required_distinct_tool_count(
    tool_defs: &[ChatToolDef],
    successful_tool_names: &BTreeSet<String>,
) -> usize {
    if tool_defs.is_empty() {
        return 0;
    }
    if successful_tool_names.is_empty()
        || !has_composite_tool_signal(successful_tool_names)
        || content_tool_defs(tool_defs, successful_tool_names).is_empty()
    {
        return 1;
    }
    MIN_COMPOSITE_DISTINCT_SUCCESSFUL_TOOLS_BEFORE_FINAL.min(tool_defs.len())
}

fn should_require_tool_call_before_final(
    force_final_answer: bool,
    tool_defs: &[ChatToolDef],
    successful_tool_names: &BTreeSet<String>,
) -> bool {
    if force_final_answer || tool_defs.is_empty() {
        return false;
    }
    successful_tool_names.len() < required_distinct_tool_count(tool_defs, successful_tool_names)
}

fn tool_defs_for_agent_iteration(
    tool_defs: &[ChatToolDef],
    successful_tool_names: &BTreeSet<String>,
    force_final_answer: bool,
) -> Vec<ChatToolDef> {
    if force_final_answer || tool_defs.is_empty() {
        return Vec::new();
    }

    if successful_tool_names.is_empty() {
        return tool_defs.to_vec();
    }

    if successful_tool_names.len() >= required_distinct_tool_count(tool_defs, successful_tool_names)
    {
        return tool_defs.to_vec();
    }

    let unused = tool_defs
        .iter()
        .filter(|tool| !successful_tool_names.contains(&tool.name))
        .cloned()
        .collect::<Vec<_>>();
    let content_tools = content_tool_defs(&unused, successful_tool_names);
    if !content_tools.is_empty() {
        return content_tools;
    }
    if unused.is_empty() { tool_defs.to_vec() } else { unused }
}

fn content_tool_defs(
    tool_defs: &[ChatToolDef],
    successful_tool_names: &BTreeSet<String>,
) -> Vec<ChatToolDef> {
    tool_defs
        .iter()
        .filter(|tool| !successful_tool_names.contains(&tool.name))
        .filter(|tool| !matches!(tool.name.as_str(), "list_workspaces" | "list_libraries"))
        .cloned()
        .collect()
}

fn has_composite_tool_signal(successful_tool_names: &BTreeSet<String>) -> bool {
    let categories = [
        successful_tool_names.iter().any(|name| is_document_content_tool(name)),
        successful_tool_names.iter().any(|name| is_graph_content_tool(name)),
        successful_tool_names.iter().any(|name| is_runtime_content_tool(name)),
        successful_tool_names.contains(GROUNDED_ANSWER_TOOL_NAME),
    ];
    categories.into_iter().filter(|present| *present).count() >= 2
}

fn is_document_content_tool(tool_name: &str) -> bool {
    matches!(tool_name, SEARCH_DOCUMENTS_TOOL_NAME | READ_DOCUMENT_TOOL_NAME)
}

fn is_graph_content_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "search_entities" | "get_graph_topology" | "list_relations" | "get_communities"
    )
}

fn is_runtime_content_tool(tool_name: &str) -> bool {
    matches!(tool_name, "get_runtime_execution" | "get_runtime_execution_trace")
}

fn tool_requirement_reminder(
    successful_distinct_tool_count: usize,
    required_distinct_tool_count: usize,
) -> String {
    format!(
        "Before writing the final answer, call MCP tools and inspect their results. Successful distinct tool types so far: {successful_distinct_tool_count}/{required_distinct_tool_count}. Use another relevant tool when one is available, and do not repeat an identical argument payload."
    )
}

async fn execute_tool_calls(
    input: McpToolAgentTurnInput<'_>,
    iteration: usize,
    tool_calls: &[ChatToolCall],
    max_parallel_actions: usize,
    deadline_started: Instant,
) -> Vec<ToolExecutionOutcome> {
    let mut outcomes: Vec<Option<ToolExecutionOutcome>> = vec![None; tool_calls.len()];
    let pending_results = stream::iter(tool_calls.iter().cloned().enumerate())
        .map(|(pending_index, call)| {
            let input = input.clone();
            async move {
                let started_at = Instant::now();
                emit_activity(
                    &input.activity_tx,
                    AgentLoopActivityEvent::ToolCallStarted {
                        iteration,
                        tool_name: call.name.clone(),
                    },
                );
                let outcome = match deadline_remaining(deadline_started, input.deadline) {
                    Some(_) => execute_one_tool_call(&input, &call).await,
                    None => tool_execution_error(format!(
                        "tool '{}' was not started because the assistant turn deadline expired",
                        call.name
                    )),
                };
                emit_activity(
                    &input.activity_tx,
                    AgentLoopActivityEvent::ToolCallFinished {
                        iteration,
                        tool_name: call.name.clone(),
                        elapsed_ms: started_at.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                        is_error: outcome.is_error,
                        child_execution_id: outcome.child_query_execution_ids.first().copied(),
                        result_preview: outcome.result_text.as_deref().map(activity_result_preview),
                    },
                );
                (pending_index, outcome)
            }
        })
        .buffer_unordered(max_parallel_actions)
        .collect::<Vec<_>>()
        .await;

    for (pending_index, outcome) in pending_results {
        outcomes[pending_index] = Some(outcome);
    }

    outcomes
        .into_iter()
        .map(|outcome| {
            outcome.unwrap_or_else(|| {
                tool_execution_error("internal agent tool dispatcher did not return a result")
            })
        })
        .collect()
}

fn emit_activity(sender: &Option<Sender<AgentLoopActivityEvent>>, event: AgentLoopActivityEvent) {
    if let Some(sender) = sender {
        // Diagnostic activity must not back-pressure tool execution; the
        // authoritative transcript is still persisted in debug_iterations.
        let _ = sender.try_send(event);
    }
}

async fn execute_one_tool_call(
    input: &McpToolAgentTurnInput<'_>,
    call: &ChatToolCall,
) -> ToolExecutionOutcome {
    let mut arguments = match serde_json::from_str::<Value>(&call.arguments_json) {
        Ok(arguments) => arguments,
        Err(error) => {
            return tool_execution_error(format!("invalid tool arguments JSON: {error}"));
        }
    };
    if let Err(message) =
        validate_agent_tool_library_scope(&call.name, &arguments, input.library_ref)
    {
        return tool_execution_error(message);
    }
    apply_agent_tool_argument_defaults(
        &call.name,
        &mut arguments,
        input.grounded_answer_top_k,
        input.library_ref,
    );

    let context = ToolCallContext {
        auth: input.auth,
        state: input.state,
        request_id: input.request_id,
        surface_kind: RuntimeSurfaceKind::Mcp,
    };
    let Some(result) = tools::call_named_tool(&call.name, context, &arguments).await else {
        return tool_execution_error(format!("unsupported MCP answer tool '{}'", call.name));
    };

    let result_text = tool_result_preview(&result.content);
    let child_query_execution_ids =
        extract_child_query_execution_ids(&call.name, &result.structured_content);
    let child_runtime_execution_ids =
        extract_child_runtime_execution_ids(&call.name, &result.structured_content);
    let is_error = result.is_error;
    let message_content = tool_result_model_message(&call.name, &result);
    let grounding_text = tool_result_verification_text(&call.name, &result);
    let verified_grounded_answer_text = verified_grounded_answer_text(&call.name, &result);
    let result_json = Some(debug_tool_result_json(&result));

    ToolExecutionOutcome {
        message_content,
        result_text,
        result_json,
        grounding_text,
        verified_grounded_answer_text,
        is_error,
        child_query_execution_ids,
        child_runtime_execution_ids,
    }
}

fn validate_agent_tool_library_scope(
    tool_name: &str,
    arguments: &Value,
    library_ref: &str,
) -> Result<(), String> {
    let Value::Object(object) = arguments else {
        return Ok(());
    };
    if tool_uses_single_library_scope(tool_name)
        && let Some(requested) = object.get("library").and_then(Value::as_str)
        && requested != library_ref
    {
        return Err(format!(
            "tool argument library scope mismatch: {tool_name} requested library `{requested}`, but this UI assistant session is scoped to `{library_ref}`"
        ));
    }
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME
        && let Some(requested_libraries) = object.get("libraries")
    {
        let Some(items) = requested_libraries.as_array() else {
            return Err(format!(
                "tool argument library scope mismatch: {tool_name} `libraries` must be an array scoped to `{library_ref}`"
            ));
        };
        if !items.is_empty()
            && (items.len() != 1 || items.first().and_then(Value::as_str) != Some(library_ref))
        {
            let requested = items
                .iter()
                .map(|item| item.as_str().unwrap_or("<non-string>"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "tool argument library scope mismatch: {tool_name} requested libraries [{requested}], but this UI assistant session is scoped to `{library_ref}`"
            ));
        }
    }
    Ok(())
}

fn apply_agent_tool_argument_defaults(
    tool_name: &str,
    arguments: &mut Value,
    grounded_top_k: usize,
    library_ref: &str,
) {
    let Value::Object(object) = arguments else {
        return;
    };
    if tool_uses_single_library_scope(tool_name) {
        object.insert("library".to_string(), serde_json::json!(library_ref));
    }
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME {
        object.insert("libraries".to_string(), serde_json::json!([library_ref]));
    }
    let bounded_top_k = grounded_top_k.max(1);
    if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        let requested_top_k = object
            .get("topK")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        if requested_top_k.is_none_or(|value| value > bounded_top_k) {
            object.insert("topK".to_string(), serde_json::json!(bounded_top_k));
        }
        return;
    }

    if agent_tool_limit_cap(tool_name).is_some() {
        // Static tool caps are ceilings; the parent turn's top-k budget
        // tightens them further so UI-agent subqueries cannot fan out wider
        // than the turn that spawned them.
        let bounded_limit = agent_tool_limit_cap(tool_name)
            .unwrap_or(bounded_top_k)
            .min(bounded_top_k.max(8))
            .max(1);
        let requested_limit = object
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        if requested_limit.is_none_or(|value| value > bounded_limit) {
            object.insert("limit".to_string(), serde_json::json!(bounded_limit));
        }
    }
}

fn tool_uses_single_library_scope(tool_name: &str) -> bool {
    matches!(
        tool_name,
        GROUNDED_ANSWER_TOOL_NAME
            | "list_documents"
            | "search_entities"
            | "get_graph_topology"
            | "list_relations"
            | "get_communities"
    )
}

fn can_return_verified_grounded_answer_without_synthesis(tool_calls: &[ChatToolCall]) -> bool {
    tool_calls.len() == 1 && tool_calls[0].name == GROUNDED_ANSWER_TOOL_NAME
}

fn activity_result_preview(text: &str) -> String {
    text.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).take(240).collect()
}

fn agent_tool_limit_cap(tool_name: &str) -> Option<usize> {
    match tool_name {
        SEARCH_DOCUMENTS_TOOL_NAME | "search_entities" => Some(12),
        "list_relations" | "get_communities" => Some(16),
        "get_graph_topology" => Some(24),
        _ => None,
    }
}

fn push_tool_grounding_fragment(
    grounding: &mut AssistantGroundingEvidence,
    tool_name: &str,
    message_content: &str,
) {
    let trimmed = message_content.trim();
    if trimmed.is_empty() {
        return;
    }
    let existing_chars = grounding
        .verification_corpus
        .iter()
        .map(|fragment| fragment.chars().count())
        .sum::<usize>();
    if existing_chars >= TOOL_GROUNDING_TOTAL_CHAR_LIMIT {
        return;
    }
    let remaining = TOOL_GROUNDING_TOTAL_CHAR_LIMIT - existing_chars;
    let fragment_limit = TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT.min(remaining);
    let fragment = trimmed.chars().take(fragment_limit).collect::<String>();
    grounding.verification_corpus.push(format!("[MCP tool result: {tool_name}]\n{fragment}"));
}

fn tool_execution_error(message: impl Into<String>) -> ToolExecutionOutcome {
    let message = message.into();
    let result_json = serde_json::json!({
        "content": [{
            "type": "text",
            "text": message.clone()
        }],
        "structuredContent": {
            "errorKind": "agent_tool_call",
            "message": message.clone()
        },
        "isError": true
    });
    ToolExecutionOutcome {
        message_content: serde_json::to_string(&result_json).unwrap_or_else(|_| {
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": "failed to serialize agent tool error"
                }],
                "structuredContent": {
                    "errorKind": "serialization"
                },
                "isError": true
            })
            .to_string()
        }),
        result_text: result_json
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        result_json: Some(result_json),
        grounding_text: None,
        verified_grounded_answer_text: None,
        is_error: true,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    }
}

fn deadline_remaining(started: Instant, deadline: Duration) -> Option<Duration> {
    deadline.checked_sub(started.elapsed()).filter(|remaining| !remaining.is_zero())
}

fn debug_tool_result_json(result: &crate::interfaces::http::mcp::McpToolResult) -> Value {
    let full = serde_json::to_value(result).unwrap_or_else(|error| {
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("failed to serialize MCP tool result: {error}")
            }],
            "structuredContent": {
                "errorKind": "serialization",
                "message": error.to_string()
            },
            "isError": true
        })
    });
    let serialized = match serde_json::to_string(&full) {
        Ok(serialized) => serialized,
        Err(_) => return full,
    };
    if serialized.chars().count() <= TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT {
        return full;
    }
    serde_json::json!({
        "content": serde_json::to_value(&result.content).unwrap_or_else(|_| serde_json::json!([])),
        "structuredContent": {
            "truncated": true,
            "jsonPrefix": compact_json_string(
                &result.structured_content,
                TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT
            ),
            "originalCharCount": serialized.chars().count()
        },
        "isError": result.is_error
    })
}

fn compact_json_string(value: &Value, char_limit: usize) -> String {
    let serialized = match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(error) => {
            return serde_json::json!({
                "truncated": true,
                "errorKind": "serialization",
                "message": error.to_string()
            })
            .to_string();
        }
    };
    if serialized.chars().count() <= char_limit {
        serialized
    } else {
        serde_json::json!({
            "truncated": true,
            "jsonPrefix": serialized.chars().take(char_limit).collect::<String>(),
            "originalCharCount": serialized.chars().count()
        })
        .to_string()
    }
}

fn tool_result_model_message(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> String {
    let content_text =
        tool_result_preview_with_limit(&result.content, tool_model_content_char_limit(tool_name))
            .unwrap_or_default();
    let structured_content = if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        compact_grounded_answer_structured_content(
            &result.structured_content,
            TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
        )
    } else {
        compact_structured_content_for_model(&result.structured_content)
    };
    serde_json::to_string(&serde_json::json!({
        "content": [{
            "type": "text",
            "text": content_text
        }],
        "structuredContent": structured_content,
        "isError": result.is_error
    }))
    .unwrap_or_else(|error| {
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("failed to serialize compact tool result: {error}")
            }],
            "structuredContent": {
                "errorKind": "serialization",
                "message": error.to_string()
            },
            "isError": true
        })
        .to_string()
    })
}

fn tool_result_verification_text(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<String> {
    if result.is_error || !tool_result_can_ground_final_answer(tool_name) {
        return None;
    }
    let content_text =
        tool_result_preview_with_limit(&result.content, TOOL_VERIFICATION_CONTENT_CHAR_LIMIT)
            .unwrap_or_default();
    let structured_content = if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        compact_grounded_answer_structured_content(
            &result.structured_content,
            TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT,
        )
    } else {
        compact_structured_content_for_verification(&result.structured_content)
    };
    let structured_text = serde_json::to_string_pretty(&structured_content)
        .unwrap_or_else(|error| format!("failed to serialize compact structured content: {error}"));
    let mut sections = Vec::new();
    if !content_text.trim().is_empty() {
        sections.push(format!("content:\n{}", content_text.trim()));
    }
    if structured_content != Value::Null && structured_content != serde_json::json!({}) {
        sections.push(format!("structuredContent:\n{structured_text}"));
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn tool_result_can_ground_final_answer(tool_name: &str) -> bool {
    matches!(tool_name, GROUNDED_ANSWER_TOOL_NAME)
        || is_document_content_tool(tool_name)
        || is_graph_content_tool(tool_name)
        || is_runtime_content_tool(tool_name)
}

fn tool_model_content_char_limit(tool_name: &str) -> usize {
    match tool_name {
        GROUNDED_ANSWER_TOOL_NAME => TOOL_MODEL_GROUNDED_ANSWER_CONTENT_CHAR_LIMIT,
        READ_DOCUMENT_TOOL_NAME => TOOL_MODEL_READ_DOCUMENT_CONTENT_CHAR_LIMIT,
        _ => TOOL_MODEL_DEFAULT_CONTENT_CHAR_LIMIT,
    }
}

fn verified_grounded_answer_text(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return None;
    }
    let verification_state = result
        .structured_content
        .pointer("/executionDetail/verificationState")
        .and_then(Value::as_str)?;
    if verification_state != "verified" {
        return None;
    }
    let lifecycle_state =
        result.structured_content.get("lifecycleState").and_then(Value::as_str).or_else(|| {
            result
                .structured_content
                .pointer("/executionDetail/execution/lifecycleState")
                .and_then(Value::as_str)
        });
    if lifecycle_state != Some("completed") {
        return None;
    }
    tool_result_full_text(&result.content)
}

fn tool_result_full_text(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
) -> Option<String> {
    let joined = content
        .iter()
        .map(|block| block.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!joined.is_empty()).then_some(joined)
}

fn compact_grounded_answer_structured_content(value: &Value, fallback_limit: usize) -> Value {
    let Some(execution_detail) = value.get("executionDetail") else {
        return compact_structured_content(value, fallback_limit);
    };
    serde_json::json!({
        "executionId": value.get("executionId").cloned().unwrap_or(Value::Null),
        "runtimeExecutionId": value.get("runtimeExecutionId").cloned().unwrap_or(Value::Null),
        "conversationId": value.get("conversationId").cloned().unwrap_or(Value::Null),
        "libraryId": value.get("libraryId").cloned().unwrap_or(Value::Null),
        "workspaceId": value.get("workspaceId").cloned().unwrap_or(Value::Null),
        "lifecycleState": value.get("lifecycleState").cloned().unwrap_or(Value::Null),
        "verificationState": execution_detail.get("verificationState").cloned().unwrap_or(Value::Null),
        "verificationWarnings": execution_detail.get("verificationWarnings").cloned().unwrap_or_else(|| serde_json::json!([])),
        "references": {
            "chunkReferences": compact_reference_array(execution_detail.get("chunkReferences")),
            "preparedSegmentReferences": compact_reference_array(execution_detail.get("preparedSegmentReferences")),
            "technicalFactReferences": compact_reference_array(execution_detail.get("technicalFactReferences")),
            "entityReferences": compact_reference_array(execution_detail.get("entityReferences")),
            "relationReferences": compact_reference_array(execution_detail.get("relationReferences"))
        }
    })
}

fn compact_reference_array(value: Option<&Value>) -> Value {
    let Some(Value::Array(items)) = value else {
        return serde_json::json!([]);
    };
    let mut truncated =
        items.iter().take(TOOL_MODEL_GROUNDED_REFERENCE_LIMIT).cloned().collect::<Vec<_>>();
    if items.len() > TOOL_MODEL_GROUNDED_REFERENCE_LIMIT {
        truncated.push(serde_json::json!({
            "truncated": true,
            "omittedCount": items.len() - TOOL_MODEL_GROUNDED_REFERENCE_LIMIT
        }));
    }
    Value::Array(truncated)
}

fn compact_structured_content_for_model(value: &Value) -> Value {
    compact_structured_content(value, TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT)
}

fn compact_structured_content_for_verification(value: &Value) -> Value {
    compact_structured_content(value, TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT)
}

fn compact_structured_content(value: &Value, char_limit: usize) -> Value {
    let serialized = match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(error) => {
            return serde_json::json!({
                "truncated": true,
                "errorKind": "serialization",
                "message": error.to_string()
            });
        }
    };
    if serialized.chars().count() <= char_limit {
        return value.clone();
    }
    serde_json::json!({
        "truncated": true,
        "jsonPrefix": serialized.chars().take(char_limit).collect::<String>(),
        "originalCharCount": serialized.chars().count()
    })
}

fn tool_result_preview(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
) -> Option<String> {
    tool_result_preview_with_limit(content, 2_000)
}

fn tool_result_preview_with_limit(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
    limit: usize,
) -> Option<String> {
    let joined = content
        .iter()
        .map(|block| block.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then(|| joined.chars().take(limit).collect())
}

fn extract_child_query_execution_ids(tool_name: &str, value: &Value) -> Vec<Uuid> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME {
        return Vec::new();
    }
    value
        .get("executionId")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .into_iter()
        .collect()
}

fn extract_child_runtime_execution_ids(tool_name: &str, value: &Value) -> Vec<Uuid> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME {
        return Vec::new();
    }
    value
        .get("runtimeExecutionId")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .into_iter()
        .collect()
}

/// Run one grounded-answer pipeline step as a single fixed-context LLM
/// call, without exposing tools to the model.
///
/// This belongs to the `grounded_answer` implementation, not to the
/// UI parent agent loop. The retrieval stage already assembled enough
/// evidence to answer the question: `prepare_answer_query` builds
/// `answer_context` out of the top retrieved chunks, graph-aware
/// neighbours, recent documents, and the library summary. Handing that
/// context to the model in one or two fixed-evidence round-trips keeps
/// direct MCP calls and UI-agent `grounded_answer` tool calls on the
/// same citation set.
///
/// Verification is the caller's responsibility: if the output is empty
/// or trips the verifier, the caller either revises over the same
/// grounded context or returns the verifier state to the user.
pub async fn run_single_shot_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: &[ChatMessage],
    grounded_context: &str,
) -> Result<AgentTurnResult, QueryServiceError> {
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
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };

    // The runtime has already performed retrieval. Model-visible
    // context is represented as the same chat transcript shape a
    // tool-using agent would see: prior messages, current user, an
    // assistant tool-call record, and the matching tool result.
    let messages = build_runtime_tool_answer_messages(
        super::assistant_prompt::render_single_shot(),
        conversation_history,
        user_question,
        RUNTIME_RETRIEVED_CONTEXT_TOOL,
        serde_json::json!({ "question": user_question }),
        grounded_context,
    );

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
        require_tool_call: false,
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
        child_runtime_execution_ids: Vec::new(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        // Single-shot did not observe any tool results. The answer
        // pipeline attaches the selected retrieval context as verifier
        // grounding when it records the generation stage.
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![debug_iteration],
        agent_loop: None,
    })
}

pub async fn run_literal_fidelity_revision_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: &[ChatMessage],
    original_answer: &str,
    unsupported_literals: &[String],
    grounded_context: &str,
) -> Result<AgentTurnResult, QueryServiceError> {
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
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };

    let system_prompt = super::assistant_prompt::render_literal_fidelity_revision(
        "Provided in the `ironrag_literal_revision_context` runtime tool result.",
        original_answer,
        unsupported_literals,
        None,
    );
    let messages = build_runtime_tool_answer_messages(
        system_prompt,
        conversation_history,
        user_question,
        RUNTIME_LITERAL_REVISION_CONTEXT_TOOL,
        serde_json::json!({
            "question": user_question,
            "unsupportedLiterals": unsupported_literals,
        }),
        grounded_context,
    );

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
        require_tool_call: false,
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
        child_runtime_execution_ids: Vec::new(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![debug_iteration],
        agent_loop: None,
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
    conversation_history: &[ChatMessage],
    variants: &[String],
) -> Result<AgentTurnResult, QueryServiceError> {
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
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };

    let system_prompt = super::assistant_prompt::render_clarify(variants, None);
    let variants_result = if variants.is_empty() {
        "- (none)".to_string()
    } else {
        variants.iter().map(|variant| format!("- {variant}")).collect::<Vec<_>>().join("\n")
    };
    let messages = build_runtime_tool_answer_messages(
        system_prompt,
        conversation_history,
        user_question,
        RUNTIME_CLARIFY_VARIANTS_TOOL,
        serde_json::json!({ "question": user_question }),
        &variants_result,
    );

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
        require_tool_call: false,
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
        child_runtime_execution_ids: Vec::new(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![debug_iteration],
        agent_loop: None,
    })
}

fn build_runtime_tool_answer_messages(
    system_prompt: String,
    conversation_history: &[ChatMessage],
    user_question: &str,
    tool_name: &str,
    tool_arguments: serde_json::Value,
    tool_result: &str,
) -> Vec<ChatMessage> {
    let tool_call_id = format!("call_{tool_name}");
    let mut messages = Vec::with_capacity(conversation_history.len().saturating_add(4));
    messages.push(ChatMessage::system(system_prompt));
    messages.extend(conversation_history.iter().cloned());
    messages.push(ChatMessage::user(user_question.to_string()));
    messages.push(ChatMessage::assistant_with_tool_calls(vec![ChatToolCall {
        id: tool_call_id.clone(),
        name: tool_name.to_string(),
        arguments_json: tool_arguments.to_string(),
    }]));
    messages.push(ChatMessage::tool_result(
        tool_call_id,
        tool_name.to_string(),
        tool_result.trim().to_string(),
    ));
    messages
}

/// Accumulate one iteration's `usage_json` into the running total for
/// a turn. The billing pipeline (`services::ops::billing`) reads token
/// counts from any of the provider-specific key aliases (`prompt_tokens`
/// / `input_tokens`, `completion_tokens` / `output_tokens`, plus cached
/// input variants); we normalize to the OpenAI shape on write so a
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
    // some providers emit — merge it into the flat key too
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::{
        domains::iam::PrincipalKind,
        interfaces::http::{
            auth::{AuthContext, AuthGrant, AuthTokenKind},
            authorization::{
                POLICY_LIBRARY_READ, POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN, POLICY_RUNTIME_READ,
            },
            mcp::{McpToolSurface, tools},
        },
    };

    use super::*;

    fn auth_with_answer_tool_access() -> AuthContext {
        AuthContext {
            token_id: Uuid::nil(),
            principal_id: Uuid::nil(),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: Vec::new(),
            grants: vec![
                AuthGrant {
                    id: Uuid::from_u128(1),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_QUERY_RUN[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(2),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_MCP_MEMORY_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(3),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_RUNTIME_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(4),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_LIBRARY_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
            ],
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
        }
    }

    #[test]
    fn ui_agent_tool_defs_match_mcp_answer_surface_descriptors() {
        let auth = auth_with_answer_tool_access();
        let expected_names = tools::visible_tool_names(&auth, McpToolSurface::Answer);
        let tool_defs = answer_surface_tool_defs(&auth);

        assert_eq!(
            tool_defs.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            expected_names.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(tool_defs.iter().any(|tool| tool.name == "grounded_answer"));
        assert!(!tool_defs.iter().any(|tool| tool.name == "upload_documents"));

        for tool in tool_defs {
            let descriptor = tools::descriptor_for(&tool.name).expect("descriptor");
            assert_eq!(tool.description, descriptor.description);
            assert_eq!(tool.parameters, descriptor.input_schema);
        }
    }

    #[test]
    fn extracts_grounded_answer_child_runtime_execution_id_from_top_level_contract() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let value = serde_json::json!({
            "runtimeExecutionId": first,
            "executionDetail": {
                "execution": {
                    "runtimeExecutionId": first
                }
            },
            "items": [
                { "runtimeExecutionId": second },
                { "runtimeExecutionId": "not-a-uuid" }
            ]
        });

        let ids = extract_child_runtime_execution_ids(GROUNDED_ANSWER_TOOL_NAME, &value);

        assert_eq!(ids, vec![first]);
        assert_ne!(ids, vec![second]);
    }

    #[test]
    fn extracts_grounded_answer_child_query_execution_id_from_top_level_contract() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let value = serde_json::json!({
            "executionId": first,
            "executionDetail": {
                "execution": {
                    "id": first
                }
            },
            "items": [
                { "executionId": second },
                { "executionId": "not-a-uuid" }
            ]
        });

        let ids = extract_child_query_execution_ids(GROUNDED_ANSWER_TOOL_NAME, &value);

        assert_eq!(ids, vec![first]);
        assert_ne!(ids, vec![second]);
    }

    #[test]
    fn ignores_execution_ids_from_non_grounded_tool_results() {
        let execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        let value = serde_json::json!({
            "executionId": execution_id,
            "runtimeExecutionId": runtime_execution_id,
            "items": [
                { "executionId": Uuid::now_v7(), "runtimeExecutionId": Uuid::now_v7() }
            ]
        });

        assert!(extract_child_query_execution_ids("list_documents", &value).is_empty());
        assert!(extract_child_runtime_execution_ids("list_documents", &value).is_empty());
    }

    #[test]
    fn final_iteration_disables_tools_after_prior_tool_calls() {
        let started = Instant::now();
        let names = BTreeSet::new();

        assert!(force_final_answer_iteration(5, 5, 1, 1, &names, 0, started, None));
        assert!(force_final_answer_iteration(1, 1, 1, 1, &names, 0, started, None));
        assert!(!force_final_answer_iteration(4, 5, 1, 1, &names, 0, started, None));
        assert!(!force_final_answer_iteration(5, 5, 0, 0, &names, 0, started, None));
    }

    #[test]
    fn soft_deadline_disables_tools_after_sufficient_tool_evidence() {
        let started = Instant::now() - Duration::from_secs(40);
        let names =
            BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string(), "search_entities".to_string()]);

        assert!(force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            &names,
            0,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn soft_deadline_keeps_collecting_when_tool_evidence_has_one_category() {
        let started = Instant::now() - Duration::from_secs(40);
        let names = BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string()]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            &names,
            0,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn soft_deadline_waits_for_enough_successful_tools_or_verified_grounded_answer() {
        let started = Instant::now() - Duration::from_secs(40);
        let names = BTreeSet::new();

        assert!(!force_final_answer_iteration(
            3,
            5,
            3,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS - 1,
            &names,
            0,
            started,
            Some(Duration::from_secs(35)),
        ));
        assert!(force_final_answer_iteration(
            3,
            5,
            1,
            1,
            &names,
            1,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn composite_doc_graph_evidence_disables_tools_before_soft_deadline() {
        let started = Instant::now();
        let names = BTreeSet::from([
            SEARCH_DOCUMENTS_TOOL_NAME.to_string(),
            "search_entities".to_string(),
            READ_DOCUMENT_TOOL_NAME.to_string(),
        ]);

        assert!(force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            &names,
            0,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn soft_deadline_keeps_tools_before_deadline() {
        let started = Instant::now();
        let names = BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string()]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            &names,
            0,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn final_answer_requires_distinct_tool_evidence_until_floor() {
        let tool_defs = [
            "list_workspaces",
            "list_libraries",
            "grounded_answer",
            "search_documents",
            "search_entities",
            "read_document",
        ]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();

        assert!(should_require_tool_call_before_final(false, &tool_defs, &BTreeSet::new()));

        let simple_evidence = BTreeSet::from(["search_documents".to_string()]);
        assert!(!should_require_tool_call_before_final(false, &tool_defs, &simple_evidence));

        let composite_evidence =
            BTreeSet::from(["search_documents".to_string(), "search_entities".to_string()]);
        assert!(should_require_tool_call_before_final(false, &tool_defs, &composite_evidence));

        let complete_composite = BTreeSet::from([
            "search_documents".to_string(),
            "search_entities".to_string(),
            "read_document".to_string(),
        ]);
        assert!(!should_require_tool_call_before_final(false, &tool_defs, &complete_composite));
        assert!(!should_require_tool_call_before_final(true, &tool_defs, &BTreeSet::new()));
        assert!(!should_require_tool_call_before_final(false, &[], &BTreeSet::new()));

        let answer_only = [ChatToolDef {
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }];
        assert!(should_require_tool_call_before_final(false, &answer_only, &BTreeSet::new()));
        assert!(!should_require_tool_call_before_final(
            false,
            &answer_only,
            &BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()])
        ));
    }

    #[test]
    fn agent_iteration_prefers_unused_tools_until_distinct_floor() {
        let tool_defs = [
            "list_workspaces",
            "list_libraries",
            "grounded_answer",
            "search_documents",
            "search_entities",
            "read_document",
        ]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();
        let successful_tool_names = BTreeSet::new();

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
        let next_names = next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>();
        assert_eq!(
            next_names,
            vec![
                "list_workspaces",
                "list_libraries",
                "grounded_answer",
                "search_documents",
                "search_entities",
                "read_document"
            ]
        );

        let mut successful_tool_names = BTreeSet::from(["search_documents".to_string()]);

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
        let next_names = next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>();
        assert_eq!(
            next_names,
            vec![
                "list_workspaces",
                "list_libraries",
                "grounded_answer",
                "search_documents",
                "search_entities",
                "read_document"
            ]
        );

        successful_tool_names.insert("search_entities".to_string());
        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec!["grounded_answer", "read_document"]
        );

        successful_tool_names.insert("read_document".to_string());
        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec![
                "list_workspaces",
                "list_libraries",
                "grounded_answer",
                "search_documents",
                "search_entities",
                "read_document",
            ]
        );

        assert!(tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, true).is_empty());
    }

    #[test]
    fn first_iteration_exposes_available_answer_tools() {
        let tool_defs = ["list_workspaces", "list_libraries", "grounded_answer"]
            .into_iter()
            .map(|name| ChatToolDef {
                name: name.to_string(),
                description: String::new(),
                parameters: serde_json::json!({}),
            })
            .collect::<Vec<_>>();

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &BTreeSet::new(), false);

        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec!["list_workspaces", "list_libraries", "grounded_answer"]
        );
    }

    #[test]
    fn grounded_answer_tool_result_is_compact_for_model_messages() {
        let execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        let structured_content = serde_json::json!({
            "executionId": execution_id,
            "runtimeExecutionId": runtime_execution_id,
            "conversationId": Uuid::now_v7(),
            "libraryId": Uuid::now_v7(),
            "workspaceId": Uuid::now_v7(),
            "lifecycleState": "completed",
            "executionDetail": {
                "execution": {
                    "id": execution_id,
                    "runtimeExecutionId": runtime_execution_id
                },
                "verificationState": "verified",
                "verificationWarnings": [],
                "chunkReferences": (0..12)
                    .map(|index| serde_json::json!({
                        "chunkId": Uuid::now_v7(),
                        "rank": index + 1,
                        "score": 1.0
                    }))
                    .collect::<Vec<_>>(),
                "preparedSegmentReferences": [],
                "technicalFactReferences": [],
                "entityReferences": [],
                "relationReferences": []
            }
        });
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "grounded answer body".to_string(),
            }],
            structured_content,
            is_error: false,
        };

        let message = tool_result_model_message(GROUNDED_ANSWER_TOOL_NAME, &result);

        assert!(message.contains("grounded answer body"));
        assert!(message.contains(&execution_id.to_string()));
        assert!(message.contains(&runtime_execution_id.to_string()));
        assert!(message.contains("\"omittedCount\":4"));
        assert!(!message.contains("\"executionDetail\""));
    }

    #[test]
    fn ui_agent_bounds_grounded_answer_tool_top_k_to_parent_turn() {
        let mut missing = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion"
        });
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut missing,
            8,
            "workspace-a/library-b",
        );
        assert_eq!(missing["topK"], 8);
        assert_eq!(missing["library"], "workspace-a/library-b");

        let mut wider = serde_json::json!({
            "library": "workspace-x/library-y",
            "query": "focused subquestion",
            "topK": 24
        });
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut wider,
            8,
            "workspace-a/library-b",
        );
        assert_eq!(wider["topK"], 8);
        assert_eq!(wider["library"], "workspace-a/library-b");

        let mut narrower = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion",
            "topK": 4
        });
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut narrower,
            8,
            "workspace-a/library-b",
        );
        assert_eq!(narrower["topK"], 4);
    }

    #[test]
    fn ui_agent_bounds_high_fanout_tool_limits() {
        let mut missing = serde_json::json!({
            "library": "workspace-x/library-y",
            "query": "focused probe"
        });
        apply_agent_tool_argument_defaults(
            "search_entities",
            &mut missing,
            8,
            "workspace-a/library-b",
        );
        assert_eq!(missing["limit"], 8);
        assert_eq!(missing["library"], "workspace-a/library-b");

        let mut wider = serde_json::json!({
            "library": "workspace-a/library-b",
            "limit": 200
        });
        apply_agent_tool_argument_defaults(
            "get_graph_topology",
            &mut wider,
            12,
            "workspace-a/library-b",
        );
        assert_eq!(wider["limit"], 12);

        let mut narrower = serde_json::json!({
            "library": "workspace-a/library-b",
            "limit": 4
        });
        apply_agent_tool_argument_defaults(
            "list_relations",
            &mut narrower,
            12,
            "workspace-a/library-b",
        );
        assert_eq!(narrower["limit"], 4);
    }

    #[test]
    fn ui_agent_forces_search_documents_to_session_library_scope() {
        let mut arguments = serde_json::json!({
            "query": "focused probe",
            "limit": 99
        });

        apply_agent_tool_argument_defaults(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &mut arguments,
            8,
            "workspace-a/library-b",
        );

        assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
        assert_eq!(arguments["limit"], 8);
    }

    #[test]
    fn ui_agent_rejects_cross_library_single_scope_arguments() {
        let arguments = serde_json::json!({
            "library": "workspace-x/library-y",
            "query": "focused probe"
        });

        let error = validate_agent_tool_library_scope(
            GROUNDED_ANSWER_TOOL_NAME,
            &arguments,
            "workspace-a/library-b",
        )
        .expect_err("scope mismatch");

        assert!(error.contains("library scope mismatch"));
        assert!(error.contains("workspace-x/library-y"));
        assert!(error.contains("workspace-a/library-b"));
    }

    #[test]
    fn ui_agent_rejects_cross_library_search_scope_arguments() {
        let arguments = serde_json::json!({
            "query": "focused probe",
            "libraries": ["workspace-a/library-b", "workspace-x/library-y"]
        });

        let error = validate_agent_tool_library_scope(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &arguments,
            "workspace-a/library-b",
        )
        .expect_err("scope mismatch");

        assert!(error.contains("library scope mismatch"));
        assert!(error.contains("workspace-x/library-y"));
    }

    #[test]
    fn verified_grounded_answer_short_circuit_requires_single_tool_call() {
        let grounded = ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: "{}".to_string(),
        };
        let graph = ChatToolCall {
            id: "call-2".to_string(),
            name: "search_entities".to_string(),
            arguments_json: "{}".to_string(),
        };

        assert!(can_return_verified_grounded_answer_without_synthesis(std::slice::from_ref(
            &grounded
        )));
        assert!(!can_return_verified_grounded_answer_without_synthesis(&[grounded, graph]));
    }

    #[test]
    fn verified_grounded_answer_text_extracts_full_answer_text() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![
                crate::interfaces::http::mcp::McpContentBlock {
                    content_type: "text",
                    text: "First supported paragraph.".to_string(),
                },
                crate::interfaces::http::mcp::McpContentBlock {
                    content_type: "text",
                    text: "Second supported paragraph.".to_string(),
                },
            ],
            structured_content: serde_json::json!({
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified"
                }
            }),
            is_error: false,
        };

        let answer =
            verified_grounded_answer_text(GROUNDED_ANSWER_TOOL_NAME, &result).expect("answer");

        assert_eq!(answer, "First supported paragraph.\n\nSecond supported paragraph.");
    }

    #[test]
    fn verified_grounded_answer_accepts_nested_completed_lifecycle() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Nested lifecycle answer.".to_string(),
            }],
            structured_content: serde_json::json!({
                "executionDetail": {
                    "verificationState": "verified",
                    "execution": {
                        "lifecycleState": "completed"
                    }
                }
            }),
            is_error: false,
        };

        let answer =
            verified_grounded_answer_text(GROUNDED_ANSWER_TOOL_NAME, &result).expect("answer");

        assert_eq!(answer, "Nested lifecycle answer.");
    }

    #[test]
    fn unverified_grounded_answer_text_is_not_final_evidence() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "unsupported answer".to_string(),
            }],
            structured_content: serde_json::json!({
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "insufficient_evidence"
                }
            }),
            is_error: false,
        };

        assert!(verified_grounded_answer_text(GROUNDED_ANSWER_TOOL_NAME, &result).is_none());
    }

    #[test]
    fn verified_grounded_answer_requires_completed_lifecycle() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "verified but incomplete answer".to_string(),
            }],
            structured_content: serde_json::json!({
                "executionDetail": {
                    "verificationState": "verified"
                }
            }),
            is_error: false,
        };

        assert!(verified_grounded_answer_text(GROUNDED_ANSWER_TOOL_NAME, &result).is_none());
    }

    #[test]
    fn successful_tool_result_becomes_verifier_grounding() {
        let mut grounding = AssistantGroundingEvidence::default();
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Document read completed.".to_string(),
            }],
            structured_content: serde_json::json!({
                "documentTitle": "Alpha overview",
                "content": "The release channel is stable."
            }),
            is_error: false,
        };
        let evidence =
            tool_result_verification_text("read_document", &result).expect("verification text");

        push_tool_grounding_fragment(&mut grounding, "read_document", &evidence);

        assert_eq!(grounding.verification_corpus.len(), 1);
        assert!(grounding.verification_corpus[0].contains("read_document"));
        assert!(grounding.verification_corpus[0].contains("Document read completed."));
        assert!(grounding.verification_corpus[0].contains("The release channel is stable."));
        assert!(!grounding.verification_corpus[0].contains("\\\"content\\\""));
    }

    #[test]
    fn inventory_tool_result_is_not_verifier_grounding() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Documents listed.".to_string(),
            }],
            structured_content: serde_json::json!({
                "items": [{
                    "documentId": Uuid::now_v7(),
                    "title": "Alpha overview",
                    "readabilityState": "readable"
                }]
            }),
            is_error: false,
        };

        assert!(tool_result_verification_text("list_documents", &result).is_none());
    }

    #[test]
    fn search_and_graph_results_can_be_verifier_grounding() {
        let search_result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Found Alpha overview.".to_string(),
            }],
            structured_content: serde_json::json!({
                "items": [{
                    "documentTitle": "Alpha overview",
                    "snippet": "The supported mode is standby."
                }]
            }),
            is_error: false,
        };
        let graph_result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Found related entities.".to_string(),
            }],
            structured_content: serde_json::json!({
                "entities": [{
                    "label": "Alpha controller",
                    "summary": "Controls standby mode."
                }]
            }),
            is_error: false,
        };

        let search_evidence = tool_result_verification_text("search_documents", &search_result)
            .expect("search evidence");
        let graph_evidence = tool_result_verification_text("search_entities", &graph_result)
            .expect("graph evidence");

        assert!(search_evidence.contains("The supported mode is standby."));
        assert!(graph_evidence.contains("Controls standby mode."));
    }

    #[test]
    fn tool_error_result_is_not_verifier_grounding() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "read failed".to_string(),
            }],
            structured_content: serde_json::json!({
                "errorKind": "not_found",
                "message": "document not found"
            }),
            is_error: true,
        };

        assert!(tool_result_verification_text("read_document", &result).is_none());
    }

    #[test]
    fn debug_tool_result_json_is_bounded() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Large result completed.".to_string(),
            }],
            structured_content: serde_json::json!({
                "payload": "x".repeat(TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT + 128)
            }),
            is_error: false,
        };

        let debug_json = debug_tool_result_json(&result);

        assert_eq!(debug_json["isError"], false);
        assert_eq!(debug_json["content"][0]["text"], "Large result completed.");
        assert_eq!(debug_json["structuredContent"]["truncated"], true);
        assert!(
            debug_json["structuredContent"]["originalCharCount"].as_u64().unwrap()
                > TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT as u64
        );
    }

    #[test]
    fn runtime_tool_answer_messages_preserve_chat_history_and_tool_result() {
        let history =
            vec![ChatMessage::user("first question"), ChatMessage::assistant_text("first answer")];

        let messages = build_runtime_tool_answer_messages(
            "system prompt".to_string(),
            &history,
            "continue",
            RUNTIME_RETRIEVED_CONTEXT_TOOL,
            serde_json::json!({ "question": "continue" }),
            "grounded context",
        );

        assert_eq!(
            messages.iter().map(|message| message.role.as_str()).collect::<Vec<_>>(),
            vec!["system", "user", "assistant", "user", "assistant", "tool"]
        );
        assert_eq!(messages[1].content.as_deref(), Some("first question"));
        assert_eq!(messages[2].content.as_deref(), Some("first answer"));
        assert_eq!(messages[4].tool_calls.len(), 1);
        assert_eq!(messages[4].tool_calls[0].name, RUNTIME_RETRIEVED_CONTEXT_TOOL);
        assert_eq!(
            messages[5].tool_call_id,
            Some(format!("call_{RUNTIME_RETRIEVED_CONTEXT_TOOL}"))
        );
        assert_eq!(messages[5].content.as_deref(), Some("grounded context"));
    }
}
