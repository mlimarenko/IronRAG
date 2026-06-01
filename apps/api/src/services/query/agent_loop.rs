//! LLM turn helpers used by the assistant answer surfaces.
//!
//! The in-app UI assistant runs as the same kind of tool-using MCP
//! client agent an external chat client would run: the model sees the
//! answer-tool registry, chooses tool calls, receives tool
//! results, and then writes the final reply.

use std::{
    collections::{BTreeMap, BTreeSet},
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
    domains::query::{QueryTurnKind, resolve_contextual_grounded_answer_top_k},
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
        service::ExternalConversationTurn,
        text_match::normalized_alnum_token_sequence,
    },
};

const RUNTIME_RETRIEVED_CONTEXT_TOOL: &str = "ironrag_retrieved_context";
const RUNTIME_LITERAL_REVISION_CONTEXT_TOOL: &str = "ironrag_literal_revision_context";
const RUNTIME_CLARIFY_VARIANTS_TOOL: &str = "ironrag_clarify_variants";
const GROUNDED_ANSWER_TOOL_NAME: &str = "grounded_answer";
const GROUNDED_ANSWER_LIFECYCLE_COMPLETED: &str = "completed";
const GROUNDED_ANSWER_VERIFICATION_NOT_RUN: &str = "not_run";
const GROUNDED_ANSWER_VERIFICATION_VERIFIED: &str = "verified";
const TOOL_MODEL_DEFAULT_CONTENT_CHAR_LIMIT: usize = 3_000;
const TOOL_MODEL_GROUNDED_ANSWER_CONTENT_CHAR_LIMIT: usize = 12_000;
const TOOL_MODEL_READ_DOCUMENT_CONTENT_CHAR_LIMIT: usize = 5_000;
const TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT: usize = 8_000;
const TOOL_VERIFICATION_CONTENT_CHAR_LIMIT: usize = 8_000;
const TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT: usize = 16_000;
const TOOL_MODEL_GROUNDED_REFERENCE_LIMIT: usize = 8;
const TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT: usize = 96_000;
const TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT: usize = 20_000;
const TOOL_GROUNDING_TOTAL_CHAR_LIMIT: usize = 80_000;
const SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS: usize = 4;
const VERBATIM_USER_FRAGMENT_LIMIT: usize = 6;
const VERBATIM_USER_FRAGMENT_MIN_CHARS: usize = 4;
const VERBATIM_USER_FRAGMENT_MAX_CHARS: usize = 400;
const VERBATIM_USER_FRAGMENT_TOTAL_CHARS: usize = 1_200;
const USER_FRAGMENT_QUOTE_PAIRS: [(char, char); 8] = [
    ('«', '»'),
    ('“', '”'),
    ('„', '“'),
    ('「', '」'),
    ('『', '』'),
    ('‹', '›'),
    ('"', '"'),
    ('`', '`'),
];

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
    pub follow_up_context_messages: &'a [ChatMessage],
    pub grounded_answer_tool_history: &'a [ExternalConversationTurn],
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
    arguments_json: Option<String>,
    requested_arguments_json: Option<String>,
    message_content: String,
    result_text: Option<String>,
    result_json: Option<Value>,
    grounding_text: Option<String>,
    grounded_answer_ready: bool,
    grounded_answer_needs_follow_up: bool,
    is_error: bool,
    /// Wall-clock the tool ran. Set centrally in `execute_tool_calls`;
    /// constructors default to 0.
    duration_ms: u64,
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
        .resolve_active_runtime_binding(input.state, input.library_id, AiBindingPurpose::Agent)
        .await
        .map_err(|e| {
            AgentTurnFailure::empty(anyhow::anyhow!("failed to resolve agent binding: {e}"))
        })?
        .ok_or_else(|| {
            AgentTurnFailure::empty(anyhow::anyhow!(
                "no active agent binding configured for library {}",
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
    let mut messages = Vec::with_capacity(
        input
            .conversation_history
            .len()
            .saturating_add(input.follow_up_context_messages.len())
            .saturating_add(iteration_cap * 3 + 2),
    );
    messages.push(ChatMessage::system(super::assistant_prompt::render(input.library_ref, None)));
    messages.extend(input.conversation_history.iter().cloned());
    messages.push(ChatMessage::user(input.user_question.to_string()));
    messages.extend(input.follow_up_context_messages.iter().cloned());
    if let Some(reminder) = latest_user_verbatim_fragment_reminder(input.user_question) {
        messages.push(ChatMessage::system(reminder));
    }

    let mut usage_json = serde_json::json!({});
    let mut debug_iterations = Vec::new();
    let mut total_tool_call_count = 0usize;
    let mut successful_tool_call_count = 0usize;
    let mut successful_tool_names = BTreeSet::new();
    let mut seen_effective_tool_payloads = BTreeMap::new();
    let mut assistant_grounding = AssistantGroundingEvidence::default();
    let mut child_query_execution_ids = Vec::new();
    let mut stopped_reason = AgentStopReason::IterationCap;
    let mut last_required_tool_refusal_answer: Option<String> = None;
    let mut verified_grounded_answer_count = 0usize;
    let mut incomplete_grounded_answer_needs_follow_up = false;

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
            verified_grounded_answer_count,
            &successful_tool_names,
            deadline_started,
            input.soft_final_answer_deadline,
        );
        let require_tool_call = should_require_tool_call_before_final(
            force_final_answer,
            &tool_defs,
            &successful_tool_names,
            incomplete_grounded_answer_needs_follow_up,
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
        let model_call_started = std::time::Instant::now();
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
                tracing::warn!(
                    provider = %binding.provider_kind,
                    model = %binding.model_name,
                    iteration,
                    max_output_tokens_override = ?binding.max_output_tokens_override,
                    error = %error,
                    "MCP-backed assistant agent provider call failed"
                );
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
        let model_call_duration_ms =
            model_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

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
                duration_ms: Some(model_call_duration_ms),
                child_runtime_execution_ids: Vec::new(),
                child_query_execution_ids: Vec::new(),
            });
            if require_tool_call {
                if last_required_tool_refusal_answer.is_some() || iteration == iteration_cap {
                    stopped_reason = AgentStopReason::FinalAnswer;
                    let answer = ensure_user_fragments_visible(answer, input.user_question);
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
                messages.push(ChatMessage::system(tool_requirement_reminder()));
                continue;
            }
            stopped_reason = AgentStopReason::FinalAnswer;
            let answer = ensure_user_fragments_visible(answer, input.user_question);
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
            &mut seen_effective_tool_payloads,
        )
        .await;
        total_tool_call_count = total_tool_call_count.saturating_add(tool_calls.len());

        let mut response_tool_calls = Vec::with_capacity(tool_calls.len());
        let mut child_runtime_execution_ids = Vec::new();
        let mut iteration_child_query_execution_ids = Vec::new();
        let mut iteration_had_incomplete_grounded_answer = false;
        let mut iteration_had_follow_up_after_incomplete_grounded_answer = false;
        for (call, outcome) in tool_calls.iter().zip(outcomes.iter()) {
            child_query_execution_ids.extend(outcome.child_query_execution_ids.iter().copied());
            iteration_child_query_execution_ids
                .extend(outcome.child_query_execution_ids.iter().copied());
            child_runtime_execution_ids.extend(outcome.child_runtime_execution_ids.iter().copied());
            if !outcome.is_error {
                successful_tool_call_count = successful_tool_call_count.saturating_add(1);
                successful_tool_names.insert(call.name.clone());
                if incomplete_grounded_answer_needs_follow_up
                    && tool_outcome_satisfies_incomplete_grounded_follow_up(&call.name, outcome)
                {
                    iteration_had_follow_up_after_incomplete_grounded_answer = true;
                }
                if outcome.grounded_answer_ready {
                    verified_grounded_answer_count =
                        verified_grounded_answer_count.saturating_add(1);
                }
                if outcome.grounded_answer_needs_follow_up {
                    iteration_had_incomplete_grounded_answer = true;
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
                arguments_json: outcome
                    .arguments_json
                    .clone()
                    .unwrap_or_else(|| call.arguments_json.clone()),
                requested_arguments_json: outcome.requested_arguments_json.clone(),
                result_text: outcome.result_text.clone(),
                result_json: outcome.result_json.clone(),
                is_error: outcome.is_error,
                duration_ms: Some(outcome.duration_ms),
            });
            messages.push(ChatMessage::tool_result(
                call.id.clone(),
                call.name.clone(),
                outcome.message_content.clone(),
            ));
        }
        if let Some(reminder) = latest_user_verbatim_fragment_reminder(input.user_question) {
            messages.push(ChatMessage::system(reminder));
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
            duration_ms: Some(model_call_duration_ms),
            child_runtime_execution_ids,
            child_query_execution_ids: iteration_child_query_execution_ids,
        });
        let next_incomplete_grounded_answer_needs_follow_up =
            next_incomplete_grounded_answer_follow_up_required(
                incomplete_grounded_answer_needs_follow_up,
                iteration_had_incomplete_grounded_answer,
                iteration_had_follow_up_after_incomplete_grounded_answer,
            );
        if next_incomplete_grounded_answer_needs_follow_up
            != incomplete_grounded_answer_needs_follow_up
        {
            tracing::debug!(
                request_id = input.request_id,
                library_id = %input.library_id,
                iteration,
                previous = incomplete_grounded_answer_needs_follow_up,
                iteration_had_incomplete_grounded_answer,
                iteration_had_follow_up_after_incomplete_grounded_answer,
                next = next_incomplete_grounded_answer_needs_follow_up,
                "query.agent_loop.grounded_answer_follow_up_state"
            );
        }
        incomplete_grounded_answer_needs_follow_up =
            next_incomplete_grounded_answer_needs_follow_up;
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
    verified_grounded_answer_count: usize,
    successful_tool_names: &BTreeSet<String>,
    started: Instant,
    soft_final_answer_deadline: Option<Duration>,
) -> bool {
    if iteration == iteration_cap && total_tool_call_count > 0 {
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

    verified_grounded_answer_count > 0
        || (successful_tool_call_count >= SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS
            && has_composite_tool_signal(successful_tool_names))
}

fn should_require_tool_call_before_final(
    force_final_answer: bool,
    tool_defs: &[ChatToolDef],
    successful_tool_names: &BTreeSet<String>,
    incomplete_grounded_answer_needs_follow_up: bool,
) -> bool {
    if force_final_answer || tool_defs.is_empty() {
        return false;
    }
    if incomplete_grounded_answer_needs_follow_up {
        return true;
    }
    successful_tool_names.is_empty()
}

fn next_incomplete_grounded_answer_follow_up_required(
    previous_required: bool,
    iteration_had_incomplete_grounded_answer: bool,
    iteration_had_follow_up_after_incomplete_grounded_answer: bool,
) -> bool {
    iteration_had_incomplete_grounded_answer
        || (previous_required && !iteration_had_follow_up_after_incomplete_grounded_answer)
}

fn tool_outcome_satisfies_incomplete_grounded_follow_up(
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> bool {
    tool_result_satisfies_incomplete_grounded_follow_up(
        tool_name,
        outcome.is_error,
        outcome.grounded_answer_ready,
        outcome.grounding_text.as_deref(),
    )
}

fn tool_result_satisfies_incomplete_grounded_follow_up(
    tool_name: &str,
    is_error: bool,
    grounded_answer_ready: bool,
    grounding_text: Option<&str>,
) -> bool {
    if is_error {
        return false;
    }
    if grounded_answer_ready {
        return true;
    }
    if tool_name == GROUNDED_ANSWER_TOOL_NAME || !tool_result_can_ground_final_answer(tool_name) {
        return false;
    }
    grounding_text.is_some_and(|text| !text.trim().is_empty())
}

fn tool_defs_for_agent_iteration(
    tool_defs: &[ChatToolDef],
    _successful_tool_names: &BTreeSet<String>,
    force_final_answer: bool,
) -> Vec<ChatToolDef> {
    if force_final_answer || tool_defs.is_empty() {
        return Vec::new();
    }
    tool_defs.to_vec()
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

fn tool_requirement_reminder() -> String {
    "Before writing the final answer, call at least one MCP tool and inspect the result. Use any relevant visible tool, and do not repeat an identical argument payload.".to_string()
}

fn latest_user_verbatim_fragment_reminder(user_question: &str) -> Option<String> {
    let fragments = extract_verbatim_user_fragments(user_question);
    if fragments.is_empty() {
        return None;
    }

    let mut reminder = String::from(
        "Latest-user verbatim fragments that may identify a user-visible value, label, code, quoted phrase, requested output slot, or diagnostic message. These fragments come from the user; use them only to identify what the user asked about, not as evidence for external facts. If the final answer explains one of these items, keep the exact fragment visible before explaining it instead of replacing it with a paraphrase. If a fragment names requested answer slots, keep those slot labels visible when you cover them.",
    );
    for (index, fragment) in fragments.iter().enumerate() {
        reminder.push_str("\n\nFragment ");
        reminder.push_str(&(index + 1).to_string());
        reminder.push_str(":\n```text\n");
        reminder.push_str(fragment);
        reminder.push_str("\n```");
    }
    Some(reminder)
}

fn extract_verbatim_user_fragments(user_question: &str) -> Vec<String> {
    let mut fragments = extract_quoted_verbatim_user_fragments(user_question);
    extend_delimited_user_fragments(user_question, &mut fragments);
    extend_identifier_user_fragments(user_question, &mut fragments);
    fragments
}

fn extract_quoted_verbatim_user_fragments(user_question: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    for (open, close) in USER_FRAGMENT_QUOTE_PAIRS {
        extract_quoted_user_fragments(user_question, open, close, &mut fragments);
        if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
            break;
        }
    }
    fragments
}

fn ensure_user_fragments_visible(answer: String, user_question: &str) -> String {
    let missing = extract_required_visible_user_fragments(user_question)
        .into_iter()
        .filter(|fragment| !fragment.is_visible_in(&answer))
        .map(|fragment| fragment.text)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return answer;
    }

    let mut prefixed = missing.join("\n");
    prefixed.push_str("\n\n");
    prefixed.push_str(answer.trim_start());
    prefixed
}

fn contains_casefolded(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RequiredVisibleUserFragment {
    text: String,
    match_kind: RequiredVisibleMatchKind,
}

impl RequiredVisibleUserFragment {
    fn casefolded(text: String) -> Self {
        Self { text, match_kind: RequiredVisibleMatchKind::CaseFolded }
    }

    fn exact(text: String) -> Self {
        Self { text, match_kind: RequiredVisibleMatchKind::Exact }
    }

    fn is_visible_in(&self, answer: &str) -> bool {
        match self.match_kind {
            RequiredVisibleMatchKind::CaseFolded => contains_casefolded(answer, &self.text),
            RequiredVisibleMatchKind::Exact => answer.contains(&self.text),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RequiredVisibleMatchKind {
    CaseFolded,
    Exact,
}

fn extract_required_visible_user_fragments(
    user_question: &str,
) -> Vec<RequiredVisibleUserFragment> {
    let mut raw_fragments = extract_quoted_verbatim_user_fragments(user_question);
    for fragment in extract_delimited_verbatim_user_fragments(user_question) {
        let ends_with_terminal =
            fragment.chars().last().is_some_and(|ch| matches!(ch, '?' | '.' | '!'));
        if fragment.chars().count() >= 16 && !ends_with_terminal {
            push_verbatim_fragment(&fragment, &mut raw_fragments);
        }
    }
    let mut required =
        raw_fragments.into_iter().map(RequiredVisibleUserFragment::casefolded).collect::<Vec<_>>();
    let mut identifier_fragments = Vec::new();
    extend_identifier_user_fragments(user_question, &mut identifier_fragments);
    for fragment in identifier_fragments {
        if required.iter().any(|existing| existing.text == fragment) {
            continue;
        }
        required.push(RequiredVisibleUserFragment::exact(fragment));
        if required.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
            break;
        }
    }
    required
}

fn extract_delimited_verbatim_user_fragments(user_question: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    extend_delimited_user_fragments(user_question, &mut fragments);
    fragments
}

fn extract_quoted_user_fragments(
    user_question: &str,
    open: char,
    close: char,
    fragments: &mut Vec<String>,
) {
    if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
        return;
    }

    if open == close {
        let mut open_at: Option<usize> = None;
        for (index, ch) in user_question.char_indices() {
            if ch != open {
                continue;
            }
            if let Some(start) = open_at.take() {
                push_verbatim_fragment(&user_question[start + open.len_utf8()..index], fragments);
                if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
                    return;
                }
            } else {
                open_at = Some(index);
            }
        }
        return;
    }

    let mut search_from = 0usize;
    while search_from < user_question.len() && fragments.len() < VERBATIM_USER_FRAGMENT_LIMIT {
        let Some(open_offset) = user_question[search_from..].find(open) else {
            return;
        };
        let content_start = search_from + open_offset + open.len_utf8();
        let Some(close_offset) = user_question[content_start..].find(close) else {
            return;
        };
        let close_index = content_start + close_offset;
        push_verbatim_fragment(&user_question[content_start..close_index], fragments);
        search_from = close_index + close.len_utf8();
    }
}

fn push_verbatim_fragment(candidate: &str, fragments: &mut Vec<String>) {
    if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
        return;
    }

    let candidate = candidate.trim();
    let char_count = candidate.chars().count();
    if !(VERBATIM_USER_FRAGMENT_MIN_CHARS..=VERBATIM_USER_FRAGMENT_MAX_CHARS).contains(&char_count)
    {
        return;
    }
    if fragments.iter().any(|existing| existing == candidate) {
        return;
    }
    let total_chars: usize =
        fragments.iter().map(|fragment: &String| fragment.chars().count()).sum();
    if total_chars.saturating_add(char_count) > VERBATIM_USER_FRAGMENT_TOTAL_CHARS {
        return;
    }
    fragments.push(candidate.to_string());
}

fn extend_delimited_user_fragments(user_question: &str, fragments: &mut Vec<String>) {
    if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
        return;
    }

    let mut segment_start = 0usize;
    let mut saw_delimiter = false;
    let mut active_quote_close: Option<char> = None;
    for (index, ch) in user_question.char_indices() {
        if let Some(close) = active_quote_close {
            if ch == close {
                active_quote_close = None;
            }
            continue;
        }
        if let Some(close) = quote_close_for_open(ch) {
            active_quote_close = Some(close);
            continue;
        }
        if !is_verbatim_fragment_delimiter(ch) {
            continue;
        }
        if saw_delimiter {
            push_verbatim_clause(&user_question[segment_start..index], fragments);
            if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
                return;
            }
        }
        saw_delimiter = true;
        segment_start = index + ch.len_utf8();
    }
    if saw_delimiter && segment_start < user_question.len() {
        push_verbatim_clause(&user_question[segment_start..], fragments);
    }
}

fn extend_identifier_user_fragments(user_question: &str, fragments: &mut Vec<String>) {
    if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
        return;
    }

    let mut token_start: Option<usize> = None;
    for (index, ch) in user_question.char_indices() {
        if is_identifier_fragment_char(ch) {
            token_start.get_or_insert(index);
            continue;
        }
        if let Some(start) = token_start.take() {
            push_identifier_fragment(&user_question[start..index], fragments);
            if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
                return;
            }
        }
    }
    if let Some(start) = token_start {
        push_identifier_fragment(&user_question[start..], fragments);
    }
}

fn is_identifier_fragment_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/')
}

fn push_identifier_fragment(candidate: &str, fragments: &mut Vec<String>) {
    let candidate = candidate.trim_matches(|ch: char| matches!(ch, '.' | '-' | '_' | '/'));
    if !is_identifier_shaped_fragment(candidate) {
        return;
    }
    if fragments.iter().any(|existing| existing.contains(candidate)) {
        return;
    }
    push_verbatim_fragment(candidate, fragments);
}

fn is_identifier_shaped_fragment(candidate: &str) -> bool {
    let char_count = candidate.chars().count();
    if !(VERBATIM_USER_FRAGMENT_MIN_CHARS..=VERBATIM_USER_FRAGMENT_MAX_CHARS).contains(&char_count)
    {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alnum_count < 2 {
        return false;
    }
    let has_underscore = candidate.contains('_');
    let has_hyphen = candidate.contains('-');
    let has_dot = candidate.contains('.');
    let has_slash = candidate.contains('/');
    if !(has_underscore || has_hyphen || has_dot || has_slash) {
        return false;
    }
    if has_underscore {
        return true;
    }
    if has_slash {
        return candidate.starts_with('/')
            || has_dot
            || candidate.chars().any(|ch| ch.is_numeric());
    }
    let has_digit = candidate.chars().any(|ch| ch.is_numeric());
    let has_upper = candidate.chars().any(|ch| ch.is_uppercase());
    let has_lower = candidate.chars().any(|ch| ch.is_lowercase());
    let all_cased_upper = has_upper && !has_lower;
    if has_hyphen {
        return has_digit || all_cased_upper;
    }
    has_dot && (has_digit || all_cased_upper)
}

fn is_verbatim_fragment_delimiter(ch: char) -> bool {
    matches!(ch, ',' | ';' | ':' | '(' | ')' | '[' | ']' | '\n')
}

fn quote_close_for_open(ch: char) -> Option<char> {
    match ch {
        '«' => Some('»'),
        '“' => Some('”'),
        '„' => Some('“'),
        '「' => Some('」'),
        '『' => Some('』'),
        '‹' => Some('›'),
        '"' => Some('"'),
        '`' => Some('`'),
        _ => None,
    }
}

fn push_verbatim_clause(candidate: &str, fragments: &mut Vec<String>) {
    let candidate = candidate.trim();
    if candidate.chars().all(|ch| !ch.is_alphanumeric()) {
        return;
    }
    push_verbatim_fragment(candidate, fragments);
}

async fn execute_tool_calls(
    input: McpToolAgentTurnInput<'_>,
    iteration: usize,
    tool_calls: &[ChatToolCall],
    max_parallel_actions: usize,
    deadline_started: Instant,
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadState>,
) -> Vec<ToolExecutionOutcome> {
    let mut outcomes: Vec<Option<ToolExecutionOutcome>> = vec![None; tool_calls.len()];
    let pending_calls = prepare_agent_tool_calls(
        tool_calls,
        input.user_question,
        input.grounded_answer_top_k,
        input.library_ref,
        input.grounded_answer_tool_history,
        seen_effective_payloads,
        &mut outcomes,
    );

    let pending_results = stream::iter(pending_calls)
        .map(|pending| {
            let input = input.clone();
            async move {
                let started_at = Instant::now();
                emit_activity(
                    &input.activity_tx,
                    AgentLoopActivityEvent::ToolCallStarted {
                        iteration,
                        tool_name: pending.call.name.clone(),
                    },
                );
                let mut outcome = match deadline_remaining(deadline_started, input.deadline) {
                    Some(_) => execute_one_tool_call(&input, &pending.call).await,
                    None => tool_execution_error(format!(
                        "tool '{}' was not started because the assistant turn deadline expired",
                        pending.call.name
                    )),
                };
                let elapsed_ms = started_at.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                outcome.duration_ms = elapsed_ms;
                emit_activity(
                    &input.activity_tx,
                    AgentLoopActivityEvent::ToolCallFinished {
                        iteration,
                        tool_name: pending.call.name.clone(),
                        elapsed_ms,
                        is_error: outcome.is_error,
                        child_execution_id: outcome.child_query_execution_ids.first().copied(),
                        result_preview: outcome.result_text.as_deref().map(activity_result_preview),
                    },
                );
                (pending.index, pending.fingerprint, outcome)
            }
        })
        .buffer_unordered(max_parallel_actions)
        .collect::<Vec<_>>()
        .await;

    for (pending_index, fingerprint, outcome) in pending_results {
        if let Some(fingerprint) = fingerprint {
            record_effective_tool_payload_outcome(seen_effective_payloads, fingerprint, &outcome);
        }
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum EffectiveToolPayloadState {
    InFlight,
    Completed,
}

#[derive(Debug, Clone)]
struct PendingAgentToolCall {
    index: usize,
    call: ChatToolCall,
    fingerprint: Option<String>,
}

fn prepare_agent_tool_calls(
    tool_calls: &[ChatToolCall],
    user_question: &str,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadState>,
    outcomes: &mut [Option<ToolExecutionOutcome>],
) -> Vec<PendingAgentToolCall> {
    tool_calls
        .iter()
        .cloned()
        .enumerate()
        .filter_map(|(pending_index, call)| {
            let fingerprint = effective_tool_call_fingerprint(
                &call.name,
                &call.arguments_json,
                user_question,
                grounded_top_k,
                library_ref,
                grounded_answer_tool_history,
            );
            if let Some(fingerprint) = &fingerprint {
                if seen_effective_payloads.contains_key(fingerprint) {
                    outcomes[pending_index] =
                        Some(tool_execution_error(duplicate_tool_call_message(&call.name)));
                    return None;
                }
                seen_effective_payloads
                    .insert(fingerprint.clone(), EffectiveToolPayloadState::InFlight);
            }
            Some(PendingAgentToolCall { index: pending_index, call, fingerprint })
        })
        .collect::<Vec<_>>()
}

fn record_effective_tool_payload_outcome(
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadState>,
    fingerprint: String,
    outcome: &ToolExecutionOutcome,
) {
    if outcome.is_error {
        seen_effective_payloads.remove(&fingerprint);
        return;
    }
    seen_effective_payloads.insert(fingerprint, EffectiveToolPayloadState::Completed);
}

fn duplicate_tool_call_message(tool_name: &str) -> String {
    format!(
        "duplicate MCP tool call suppressed: `{tool_name}` already used the same effective arguments in this model iteration; vary the sub-question, change arguments, or choose another MCP tool"
    )
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
    let requested_arguments = arguments.clone();
    apply_agent_tool_argument_defaults(
        &call.name,
        &mut arguments,
        input.user_question,
        input.grounded_answer_top_k,
        input.library_ref,
        input.grounded_answer_tool_history,
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
    let grounded_answer_ready = grounded_answer_ready(&call.name, &result);
    let grounded_answer_needs_follow_up = grounded_answer_needs_follow_up(&call.name, &result);
    let result_json = Some(debug_tool_result_json(&result));
    let arguments_json = Some(arguments.to_string());
    let requested_arguments_json =
        (requested_arguments != arguments).then(|| call.arguments_json.clone());

    ToolExecutionOutcome {
        arguments_json,
        requested_arguments_json,
        message_content,
        result_text,
        result_json,
        grounding_text,
        grounded_answer_ready,
        grounded_answer_needs_follow_up,
        is_error,
        duration_ms: 0,
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
    user_question: &str,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
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
        if let Some(query) = object.get("query").and_then(Value::as_str)
            && should_replace_history_padded_grounded_answer_query(
                query,
                user_question,
                grounded_answer_tool_history,
            )
        {
            object.insert("query".to_string(), serde_json::json!(user_question.trim()));
        }
        let requested_top_k = object
            .get("topK")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        let turns = grounded_answer_conversation_turn_defaults(grounded_answer_tool_history);
        object.insert("conversationTurns".to_string(), Value::Array(turns));
        let has_contextual_turns = !grounded_answer_tool_history.is_empty();
        let effective_top_k = resolve_contextual_grounded_answer_top_k(
            requested_top_k,
            has_contextual_turns,
            bounded_top_k,
        );
        if requested_top_k != Some(effective_top_k) {
            object.insert("topK".to_string(), serde_json::json!(effective_top_k));
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

fn effective_tool_call_fingerprint(
    tool_name: &str,
    arguments_json: &str,
    user_question: &str,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> Option<String> {
    let mut arguments = serde_json::from_str::<Value>(arguments_json).ok()?;
    validate_agent_tool_library_scope(tool_name, &arguments, library_ref).ok()?;
    apply_agent_tool_argument_defaults(
        tool_name,
        &mut arguments,
        user_question,
        grounded_top_k,
        library_ref,
        grounded_answer_tool_history,
    );
    let canonical = canonical_json_value(&arguments);
    let serialized = serde_json::to_string(&canonical).ok()?;
    Some(format!("{tool_name}:{serialized}"))
}

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json_value).collect()),
        Value::Object(object) => {
            let mut sorted = serde_json::Map::new();
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if let Some(item) = object.get(key) {
                    sorted.insert(key.clone(), canonical_json_value(item));
                }
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

fn should_replace_history_padded_grounded_answer_query(
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> bool {
    if grounded_answer_tool_history.is_empty() {
        return false;
    }
    let query = query.trim();
    let user_question = user_question.trim();
    if query.is_empty() || user_question.is_empty() || query == user_question {
        return false;
    }
    if !query_mentions_user_question(query, user_question) {
        return false;
    }
    let query_chars = query.chars().count();
    let user_chars = user_question.chars().count();
    if query_chars <= user_chars.saturating_add(80) {
        return false;
    }
    let query_markers = technical_surface_marker_count(query);
    let user_markers = technical_surface_marker_count(user_question);
    if query_markers < user_markers.saturating_add(8) {
        return false;
    }
    history_added_query_token_overlap(query, user_question, grounded_answer_tool_history) >= 3
}

fn query_mentions_user_question(query: &str, user_question: &str) -> bool {
    if query.to_lowercase().contains(&user_question.to_lowercase()) {
        return true;
    }
    let query_tokens = normalized_alnum_token_sequence(query, 3);
    let user_tokens = normalized_alnum_token_sequence(user_question, 3);
    if user_tokens.is_empty() || query_tokens.is_empty() {
        return false;
    }
    let overlap = user_tokens.iter().filter(|token| query_tokens.contains(token)).count();
    overlap >= user_tokens.len().min(3)
}

fn history_added_query_token_overlap(
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> usize {
    let user_tokens =
        normalized_alnum_token_sequence(user_question, 3).into_iter().collect::<BTreeSet<_>>();
    let history_tokens = grounded_answer_tool_history
        .iter()
        .flat_map(|turn| normalized_alnum_token_sequence(&turn.content_text, 3))
        .collect::<BTreeSet<_>>();

    normalized_alnum_token_sequence(query, 3)
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|token| !user_tokens.contains(token) && history_tokens.contains(token))
        .count()
}

fn technical_surface_marker_count(value: &str) -> usize {
    value
        .chars()
        .filter(|ch| {
            matches!(ch, '`' | '/' | '\\' | '[' | ']' | '_' | ':' | '=' | '.' | '<' | '>' | '"')
        })
        .count()
}

fn grounded_answer_conversation_turn_defaults(
    conversation_turns: &[ExternalConversationTurn],
) -> Vec<Value> {
    conversation_turns
        .iter()
        .filter_map(|turn| {
            let role = match turn.turn_kind {
                QueryTurnKind::User => "user",
                QueryTurnKind::Assistant => "assistant",
                QueryTurnKind::System | QueryTurnKind::Tool => return None,
            };
            let content = turn.content_text.trim();
            if content.is_empty() {
                return None;
            }
            Some(serde_json::json!({
                "role": role,
                "content": content,
            }))
        })
        .collect()
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
        arguments_json: None,
        requested_arguments_json: None,
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
        grounded_answer_ready: false,
        grounded_answer_needs_follow_up: false,
        is_error: true,
        duration_ms: 0,
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

fn grounded_answer_ready(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    if result.structured_content.get("finalAnswerReady").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    if grounded_answer_lifecycle_state(result) != Some(GROUNDED_ANSWER_LIFECYCLE_COMPLETED)
        || grounded_answer_verification_state(result) != Some(GROUNDED_ANSWER_VERIFICATION_VERIFIED)
    {
        return false;
    }
    true
}

fn grounded_answer_needs_follow_up(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    if grounded_answer_ready(tool_name, result) {
        return false;
    }
    if grounded_answer_lifecycle_state(result) != Some(GROUNDED_ANSWER_LIFECYCLE_COMPLETED) {
        return false;
    }
    match grounded_answer_verification_state(result) {
        Some(GROUNDED_ANSWER_VERIFICATION_VERIFIED | GROUNDED_ANSWER_VERIFICATION_NOT_RUN) => false,
        Some(_) | None => true,
    }
}

#[cfg(test)]
fn grounded_answer_completed(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    tool_name == GROUNDED_ANSWER_TOOL_NAME
        && !result.is_error
        && grounded_answer_lifecycle_state(result) == Some(GROUNDED_ANSWER_LIFECYCLE_COMPLETED)
}

fn grounded_answer_lifecycle_state(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<&str> {
    result.structured_content.get("lifecycleState").and_then(Value::as_str).or_else(|| {
        result
            .structured_content
            .pointer("/executionDetail/execution/lifecycleState")
            .and_then(Value::as_str)
    })
}

fn grounded_answer_verification_state(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<&str> {
    result.structured_content.pointer("/executionDetail/verificationState").and_then(Value::as_str)
}

#[cfg(test)]
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
        "finalAnswerReady": value.get("finalAnswerReady").cloned().unwrap_or(Value::Bool(false)),
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

    let llm_call_started = std::time::Instant::now();
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
        duration_ms: Some(llm_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
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

    let llm_call_started = std::time::Instant::now();
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
        duration_ms: Some(llm_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
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

pub async fn run_literal_inventory_coverage_revision_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: &[ChatMessage],
    original_answer: &str,
    required_literals: &[String],
    revision_context: &str,
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

    let system_prompt = super::assistant_prompt::render_literal_inventory_coverage_revision(
        original_answer,
        required_literals,
        "Provided in the `ironrag_literal_revision_context` runtime tool result.",
    );
    let messages = build_runtime_tool_answer_messages(
        system_prompt,
        conversation_history,
        user_question,
        RUNTIME_LITERAL_REVISION_CONTEXT_TOOL,
        serde_json::json!({
            "question": user_question,
            "requiredLiterals": required_literals,
        }),
        revision_context,
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

    let llm_call_started = std::time::Instant::now();
    let response = state
        .llm_gateway
        .generate_with_tools(tool_use_request)
        .await
        .with_context(|| "literal-inventory coverage revision LLM call failed")?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::llm_context_debug::LlmIterationDebug {
        iteration: 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: response.usage_json.clone(),
        duration_ms: Some(llm_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
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

    let llm_call_started = std::time::Instant::now();
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
        duration_ms: Some(llm_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
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

        assert!(force_final_answer_iteration(5, 5, 1, 1, 0, &names, started, None));
        assert!(force_final_answer_iteration(1, 1, 1, 1, 0, &names, started, None));
        assert!(!force_final_answer_iteration(4, 5, 1, 1, 0, &names, started, None));
        assert!(!force_final_answer_iteration(5, 5, 0, 0, 0, &names, started, None));
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
            0,
            &names,
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
            0,
            &names,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn soft_deadline_waits_for_enough_successful_tools() {
        let started = Instant::now() - Duration::from_secs(40);
        let names = BTreeSet::new();

        assert!(!force_final_answer_iteration(
            3,
            5,
            3,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS - 1,
            0,
            &names,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn grounded_answer_success_alone_waits_until_soft_deadline() {
        let before_deadline = Instant::now();
        let after_deadline = Instant::now() - Duration::from_secs(40);
        let names = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            5,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            1,
            &names,
            before_deadline,
            Some(Duration::from_secs(35)),
        ));
        assert!(force_final_answer_iteration(
            3,
            5,
            5,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            1,
            &names,
            after_deadline,
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
            0,
            &names,
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
            0,
            &names,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn final_answer_requires_at_least_one_tool_result() {
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

        assert!(should_require_tool_call_before_final(false, &tool_defs, &BTreeSet::new(), false));

        let simple_evidence = BTreeSet::from(["search_documents".to_string()]);
        assert!(
            !should_require_tool_call_before_final(false, &tool_defs, &simple_evidence, false,)
        );

        let composite_evidence =
            BTreeSet::from(["search_documents".to_string(), "search_entities".to_string()]);
        assert!(!should_require_tool_call_before_final(
            false,
            &tool_defs,
            &composite_evidence,
            false,
        ));

        let complete_composite = BTreeSet::from([
            "search_documents".to_string(),
            "search_entities".to_string(),
            "read_document".to_string(),
        ]);
        assert!(!should_require_tool_call_before_final(
            false,
            &tool_defs,
            &complete_composite,
            false,
        ));
        assert!(!should_require_tool_call_before_final(true, &tool_defs, &BTreeSet::new(), true,));
        assert!(!should_require_tool_call_before_final(false, &[], &BTreeSet::new(), true,));

        let answer_only = [ChatToolDef {
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }];
        assert!(should_require_tool_call_before_final(
            false,
            &answer_only,
            &BTreeSet::new(),
            false
        ));
        assert!(!should_require_tool_call_before_final(
            false,
            &answer_only,
            &BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]),
            false,
        ));
    }

    #[test]
    fn final_answer_requires_follow_up_after_incomplete_grounded_answer() {
        let tool_defs =
            [GROUNDED_ANSWER_TOOL_NAME, SEARCH_DOCUMENTS_TOOL_NAME, READ_DOCUMENT_TOOL_NAME]
                .into_iter()
                .map(|name| ChatToolDef {
                    name: name.to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                })
                .collect::<Vec<_>>();
        let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

        assert!(should_require_tool_call_before_final(false, &tool_defs, &grounded, true));
    }

    #[test]
    fn complete_grounded_answer_allows_final_synthesis() {
        let tool_defs = [GROUNDED_ANSWER_TOOL_NAME]
            .into_iter()
            .map(|name| ChatToolDef {
                name: name.to_string(),
                description: String::new(),
                parameters: serde_json::json!({}),
            })
            .collect::<Vec<_>>();
        let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

        assert!(!should_require_tool_call_before_final(false, &tool_defs, &grounded, false));
    }

    #[test]
    fn incomplete_grounded_follow_up_state_stays_required_after_repeated_incomplete_result() {
        assert!(next_incomplete_grounded_answer_follow_up_required(false, true, false));
        assert!(next_incomplete_grounded_answer_follow_up_required(true, true, true));
        assert!(!next_incomplete_grounded_answer_follow_up_required(true, false, true));
        assert!(next_incomplete_grounded_answer_follow_up_required(true, false, false));
    }

    #[test]
    fn incomplete_grounded_follow_up_requires_grounding_evidence() {
        assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
            "list_documents",
            false,
            false,
            Some("document names only"),
        ));
        assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
            SEARCH_DOCUMENTS_TOOL_NAME,
            false,
            false,
            Some("   "),
        ));
        assert!(tool_result_satisfies_incomplete_grounded_follow_up(
            SEARCH_DOCUMENTS_TOOL_NAME,
            false,
            false,
            Some("content evidence"),
        ));
        assert!(tool_result_satisfies_incomplete_grounded_follow_up(
            GROUNDED_ANSWER_TOOL_NAME,
            false,
            true,
            None,
        ));
        assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
            GROUNDED_ANSWER_TOOL_NAME,
            false,
            false,
            Some("clarification without final evidence"),
        ));
    }

    #[test]
    fn extracts_latest_user_verbatim_fragments_from_common_quote_pairs() {
        let fragments = extract_verbatim_user_fragments(
            "Explain «Status: 未確認?» and \"ERR-42: retry?\" without changing the labels.",
        );

        assert_eq!(fragments, vec!["Status: 未確認?", "ERR-42: retry?"]);
    }

    #[test]
    fn extracts_latest_user_verbatim_fragments_from_delimited_clauses() {
        let fragments = extract_verbatim_user_fragments(
            "What should happen if the form shows, item MARK-17 was already accepted?",
        );

        assert!(fragments.iter().any(|fragment| fragment == "item MARK-17 was already accepted?"));
    }

    #[test]
    fn extracts_latest_user_verbatim_fragments_from_identifier_tokens() {
        let fragments = extract_verbatim_user_fragments(
            "Describe SYNC-AGENT topology and compare it with API_V2.",
        );

        assert!(fragments.iter().any(|fragment| fragment == "SYNC-AGENT"));
        assert!(fragments.iter().any(|fragment| fragment == "API_V2"));
    }

    #[test]
    fn ignores_natural_language_punctuation_as_identifier_tokens() {
        let fragments = extract_verbatim_user_fragments(
            "Explain set-up flow, ad-hoc about sync-agent and graph.example.com.",
        );

        assert!(!fragments.iter().any(|fragment| fragment == "set-up"));
        assert!(!fragments.iter().any(|fragment| fragment == "ad-hoc"));
        assert!(!fragments.iter().any(|fragment| fragment == "sync-agent"));
        assert!(!fragments.iter().any(|fragment| fragment == "graph.example.com"));
    }

    #[test]
    fn latest_user_verbatim_fragment_reminder_marks_fragments_as_identifiers_not_evidence() {
        let reminder =
            latest_user_verbatim_fragment_reminder("What does «Result: timeout?» mean?").unwrap();

        assert!(reminder.contains("Result: timeout?"));
        assert!(reminder.contains("not as evidence for external facts"));
        assert!(reminder.contains("requested answer slots"));
        assert!(reminder.contains("instead of replacing it with a paraphrase"));
    }

    #[test]
    fn final_answer_keeps_missing_quoted_user_fragment_visible() {
        let answer = ensure_user_fragments_visible(
            "It means the workstation cannot reach the account servers.".to_string(),
            "Explain «Cash register access to account servers: no».",
        );

        assert!(answer.starts_with("Cash register access to account servers: no\n\n"));
    }

    #[test]
    fn final_answer_does_not_duplicate_visible_quoted_user_fragment() {
        let answer = ensure_user_fragments_visible(
            "cash register access to account servers: no means network access is disabled."
                .to_string(),
            "Explain «Cash register access to account servers: no».",
        );

        assert_eq!(
            answer,
            "cash register access to account servers: no means network access is disabled."
        );
    }

    #[test]
    fn final_answer_keeps_missing_requested_slot_fragment_visible() {
        let answer = ensure_user_fragments_visible(
            "Package: alpha-plugin.\nConfig path: /opt/app/config.ini.".to_string(),
            "Configure Alpha: name package, config path and parameters, then explain defaults.",
        );

        assert!(answer.starts_with("config path and parameters\n\n"));
    }

    #[test]
    fn final_answer_keeps_missing_identifier_fragment_with_exact_case_visible() {
        let answer = ensure_user_fragments_visible(
            "sync-agent exchanges data between the register and central server.".to_string(),
            "Describe SYNC-AGENT topology.",
        );

        assert!(answer.starts_with("SYNC-AGENT\n\n"));
    }

    #[test]
    fn agent_iteration_keeps_full_mcp_surface_until_final_answer() {
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
            vec![
                "list_workspaces",
                "list_libraries",
                "grounded_answer",
                "search_documents",
                "search_entities",
                "read_document"
            ]
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
    fn wildcard_scope_keeps_full_mcp_surface() {
        let tool_defs =
            ["list_documents", "grounded_answer", "search_documents", "search_entities"]
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
            vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
        );

        let listing_only = BTreeSet::from(["list_documents".to_string()]);
        assert!(!should_require_tool_call_before_final(false, &tool_defs, &listing_only, false,));
        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &listing_only, false);
        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
        );

        let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);
        assert!(!should_require_tool_call_before_final(false, &tool_defs, &grounded, false));
    }

    #[test]
    fn atomic_content_question_keeps_full_mcp_surface() {
        let tool_defs =
            ["list_documents", "grounded_answer", "search_documents", "search_entities"]
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
            vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
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
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
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
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
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
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        );
        assert_eq!(narrower["topK"], 4);
    }

    #[test]
    fn ui_agent_defaults_non_contextual_grounded_answer_top_k_to_canonical_default() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion"
        });

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "focused subquestion",
            32,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["topK"], 24);
    }

    #[test]
    fn effective_tool_call_fingerprint_normalizes_defaulted_arguments() {
        let left = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"query":"focused subquestion"}"#,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        )
        .expect("fingerprint");
        let right = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"topK":8,"library":"workspace-a/library-b","query":"focused subquestion"}"#,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        )
        .expect("fingerprint");

        assert_eq!(left, right);
    }

    #[test]
    fn effective_tool_call_fingerprint_keeps_scope_errors_authoritative() {
        let fingerprint = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"query":"focused subquestion","library":"workspace-x/library-y"}"#,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        );

        assert!(fingerprint.is_none());
    }

    #[test]
    fn effective_tool_call_fingerprint_detects_contextual_history_defaults() {
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "original question".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "original answer".to_string(),
            },
        ];
        let first = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"library":"workspace-a/library-b","query":"follow-up"}"#,
            "follow-up",
            32,
            "workspace-a/library-b",
            &history,
        )
        .expect("fingerprint");
        let second = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"conversationTurns":[],"topK":24,"query":"follow-up","library":"workspace-a/library-b"}"#,
            "follow-up",
            32,
            "workspace-a/library-b",
            &history,
        )
        .expect("fingerprint");

        assert_eq!(first, second);
    }

    #[test]
    fn duplicate_effective_tool_payload_is_suppressed_before_dispatch() {
        let tool_calls = vec![
            ChatToolCall {
                id: "call-1".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "library": "workspace-a/library-b",
                    "query": "focused subquestion",
                    "topK": 8
                })
                .to_string(),
            },
            ChatToolCall {
                id: "call-2".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "query": "focused subquestion"
                })
                .to_string(),
            },
        ];
        let mut outcomes = vec![None; tool_calls.len()];
        let mut seen = BTreeMap::new();

        let pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut outcomes,
        );

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].index, 0);
        assert!(outcomes[0].is_none());
        let duplicate = outcomes[1].as_ref().expect("duplicate outcome");
        assert!(duplicate.is_error);
        assert!(
            duplicate
                .result_text
                .as_deref()
                .is_some_and(|text| text.contains("duplicate MCP tool call suppressed"))
        );
    }

    #[test]
    fn completed_effective_tool_payload_tracking_is_turn_scoped() {
        let tool_calls = vec![ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "focused subquestion",
                "topK": 8
            })
            .to_string(),
        }];
        let mut first_outcomes = vec![None; tool_calls.len()];
        let mut seen = BTreeMap::new();
        let first_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut first_outcomes,
        );
        let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
        seen.insert(fingerprint, EffectiveToolPayloadState::Completed);
        let mut second_outcomes = vec![None; tool_calls.len()];
        let second_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut second_outcomes,
        );

        assert_eq!(first_pending.len(), 1);
        assert!(second_pending.is_empty());
        assert!(first_outcomes[0].is_none());
        let duplicate = second_outcomes[0].as_ref().expect("duplicate outcome");
        assert!(duplicate.is_error);
    }

    #[test]
    fn duplicate_effective_tool_payload_can_retry_after_actual_error() {
        let tool_calls = vec![ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "focused subquestion",
                "topK": 8
            })
            .to_string(),
        }];
        let mut seen = BTreeMap::new();
        let mut first_outcomes = vec![None; tool_calls.len()];
        let first_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut first_outcomes,
        );
        let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
        let error = tool_execution_error("upstream timeout");
        record_effective_tool_payload_outcome(&mut seen, fingerprint, &error);
        let mut second_outcomes = vec![None; tool_calls.len()];
        let second_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut second_outcomes,
        );

        assert_eq!(first_pending.len(), 1);
        assert_eq!(second_pending.len(), 1);
        assert!(first_outcomes[0].is_none());
        assert!(second_outcomes[0].is_none());
    }

    #[test]
    fn distinct_effective_tool_payloads_remain_allowed_in_one_turn() {
        let tool_calls = vec![ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "focused subquestion B",
                "topK": 8
            })
            .to_string(),
        }];
        let mut seen = BTreeMap::new();
        let mut first_outcomes = vec![None; tool_calls.len()];
        let first_pending = prepare_agent_tool_calls(
            &[ChatToolCall {
                id: "call-0".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "library": "workspace-a/library-b",
                    "query": "focused subquestion A",
                    "topK": 8
                })
                .to_string(),
            }],
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut first_outcomes,
        );
        let mut second_outcomes = vec![None; tool_calls.len()];
        let second_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut second_outcomes,
        );

        assert_eq!(first_pending.len(), 1);
        assert_eq!(second_pending.len(), 1);
        assert!(first_outcomes[0].is_none());
        assert!(second_outcomes[0].is_none());
    }

    #[test]
    fn ui_agent_raises_contextual_grounded_answer_tool_top_k_floor() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up subquestion",
            "topK": 4,
            "conversationTurns": [
                {"role": "user", "content": "original question"},
                {"role": "assistant", "content": "original answer"}
            ]
        });

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up subquestion",
            32,
            "workspace-a/library-b",
            &[
                ExternalConversationTurn {
                    turn_kind: QueryTurnKind::User,
                    content_text: "original question".to_string(),
                },
                ExternalConversationTurn {
                    turn_kind: QueryTurnKind::Assistant,
                    content_text: "original answer".to_string(),
                },
            ],
        );

        assert_eq!(arguments["topK"], 24);
    }

    #[test]
    fn ui_agent_raises_injected_contextual_grounded_answer_tool_top_k_floor() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up subquestion",
            "topK": 4
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "original question".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "original answer".to_string(),
            },
        ];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up subquestion",
            32,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["topK"], 24);
        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "original question"},
                {"role": "assistant", "content": "original answer"}
            ])
        );
    }

    #[test]
    fn ui_agent_overrides_explicit_empty_context_with_typed_history_top_k_floor() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "new standalone topic",
            "topK": 4,
            "conversationTurns": []
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "previous topic".to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "new standalone topic",
            32,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["topK"], 24);
        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "previous topic"}
            ])
        );
    }

    #[test]
    fn ui_agent_defaults_grounded_answer_conversation_turns_from_typed_history() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up question"
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "original user question".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "original assistant answer".to_string(),
            },
        ];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up question",
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "original user question"},
                {"role": "assistant", "content": "original assistant answer"}
            ])
        );
    }

    #[test]
    fn ui_agent_drops_explicit_grounded_answer_conversation_turns_without_server_history() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up question",
            "conversationTurns": [
                {"role": "user", "content": "model supplied context"}
            ]
        });
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up question",
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
    }

    #[test]
    fn ui_agent_replaces_stale_explicit_grounded_answer_conversation_turns_with_server_history() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "show full ready config",
            "conversationTurns": [
                {"role": "user", "content": "how do I configure connector alpha"},
                {"role": "assistant", "content": "Connector Alpha uses `alphaFlag`."},
                {"role": "user", "content": "configure it"}
            ]
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "configure it".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text:
                    "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                        .to_string(),
            },
        ];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "show full ready config",
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "configure it"},
                {
                    "role": "assistant",
                    "content": "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                }
            ])
        );
    }

    #[test]
    fn ui_agent_ignores_llm_supplied_literal_padding_without_server_history() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion",
            "topK": 4,
            "conversationTurns": [
                {
                    "role": "assistant",
                    "content": "`fake-a` `fake-b` `fake-c` `fake-d` `fake-e` `fake-f` `fake-g` `fake-h`"
                }
            ]
        });

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "focused subquestion",
            24,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["topK"], 4);
        assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
    }

    #[test]
    fn ui_agent_overrides_explicit_empty_grounded_answer_conversation_turns() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "new standalone topic",
            "conversationTurns": []
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "previous topic".to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "new standalone topic",
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "previous topic"}
            ])
        );
    }

    #[test]
    fn ui_agent_overrides_explicit_empty_grounded_answer_conversation_turns_for_follow_up() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up question",
            "conversationTurns": []
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "original user question".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "original assistant answer".to_string(),
            },
        ];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up question",
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "original user question"},
                {"role": "assistant", "content": "original assistant answer"}
            ])
        );
    }

    #[test]
    fn ui_agent_replaces_history_padded_grounded_answer_query_with_current_turn() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "explain all settings: [Main], url, https://localhost/api, [UI.AlphaForm.alphaCode], http://localhost, timeout, alphaMerchantId, alphaSecret, alpha-provider-module, staticAlphaId, staticAlphaPayload, alphaCodeLifetime, /opt/alpha/alpha.conf, /var/log/alpha.log",
            "conversationTurns": []
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "how do I configure Provider Alpha".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text:
                    "Install `alpha-provider-module`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                        .to_string(),
            },
        ];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "explain all settings",
            24,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["query"], "explain all settings");
        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "how do I configure Provider Alpha"},
                {
                    "role": "assistant",
                    "content": "Install `alpha-provider-module`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                }
            ])
        );
    }

    #[test]
    fn ui_agent_keeps_user_supplied_long_grounded_answer_query() {
        let user_question = "explain all settings: [Main], url, https://localhost/api, [UI.AlphaForm.alphaCode], http://localhost, timeout, alphaMerchantId, alphaSecret, alpha-provider-module, staticAlphaId, staticAlphaPayload, alphaCodeLifetime, /opt/alpha/alpha.conf, /var/log/alpha.log";
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": user_question
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Previous grounded answer.".to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            user_question,
            24,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["query"], user_question);
    }

    #[test]
    fn ui_agent_keeps_current_turn_rewrite_when_literals_are_not_from_history() {
        let user_question = "show setup steps";
        let rewritten_query = "show setup steps: configure `/opt/beta/beta.conf`, set `[Main]`, `betaSecret`, `betaToken`, `https://beta.local/api`, `timeout = 30`, `beta-module`";
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": rewritten_query
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Previous grounded answer about Provider Alpha.".to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            user_question,
            24,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["query"], rewritten_query);
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
            "focused probe",
            8,
            "workspace-a/library-b",
            &[],
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
            "",
            12,
            "workspace-a/library-b",
            &[],
        );
        assert_eq!(wider["limit"], 12);

        let mut narrower = serde_json::json!({
            "library": "workspace-a/library-b",
            "limit": 4
        });
        apply_agent_tool_argument_defaults(
            "list_relations",
            &mut narrower,
            "",
            12,
            "workspace-a/library-b",
            &[],
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
            "focused probe",
            8,
            "workspace-a/library-b",
            &[],
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
    fn tool_result_full_text_extracts_all_content_blocks() {
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
                "finalAnswerReady": true,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified"
                }
            }),
            is_error: false,
        };

        let answer = tool_result_full_text(&result.content).expect("answer");

        assert_eq!(answer, "First supported paragraph.\n\nSecond supported paragraph.");
    }

    #[test]
    fn final_grounded_answer_requires_ready_flag() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Verified but not explicitly final.".to_string(),
            }],
            structured_content: serde_json::json!({
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified",
                    "execution": {
                        "lifecycleState": "completed"
                    }
                }
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn unverified_grounded_answer_is_not_final_evidence() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "unsupported answer".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "insufficient_evidence"
                }
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn clarification_grounded_answer_does_not_force_follow_up() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Several matching documents were found. Please choose one.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "not_run"
                }
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(!grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn completed_grounded_answer_without_verification_state_requires_follow_up() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Completed answer with no verifier state.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed"
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_readiness_rejects_unverified_completed_answer() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Completed answer with warnings.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "lenient"
                }
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_completed(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_ready_flag_controls_final_readiness() {
        let ready_without_execution_detail = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Flag-only answer.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": true,
            }),
            is_error: false,
        };
        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &ready_without_execution_detail));

        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Ready answer.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": true,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified"
                }
            }),
            is_error: false,
        };

        assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_with_verifier_warnings_is_ready_when_verified() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Warning-bearing but verified answer.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": true,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": [
                        {
                            "code": "partial_coverage",
                            "warning": "Only part of the requested evidence was grounded."
                        }
                    ]
                }
            }),
            is_error: false,
        };

        assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
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
