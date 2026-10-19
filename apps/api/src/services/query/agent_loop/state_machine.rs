use super::*;

/// Build the LLM-facing tool definitions from the MCP
/// descriptors. MCP JSON-RPC and in-process UI agent calls therefore
/// share one schema source of truth, and — given the same
/// [`ToolVisibilityCapabilities`] — the same visible tool set. This is
/// what keeps the UI agent at 1:1 tool parity with external MCP clients,
/// including `view_document_image` when the running agent model is
/// vision-capable.
pub(crate) fn answer_surface_tool_defs(
    auth: &AuthContext,
    capabilities: ToolVisibilityCapabilities,
) -> Result<Vec<ChatToolDef>, tools::McpToolContractError> {
    Ok(tools::visible_tool_contract_with_capabilities(auth, McpToolSurface::Answer, capabilities)?
        .descriptors
        .into_iter()
        .map(|descriptor| ChatToolDef {
            name: descriptor.name.to_string(),
            description: descriptor.description.to_string(),
            parameters: descriptor.input_schema,
        })
        .collect())
}

/// Run the web UI assistant over the answer MCP surface. Every turn starts
/// with the `grounded_answer` tool and may execute one deterministic
/// focused repair. Complete grounded-answer results bypass model synthesis; the
/// model loop remains only for completed results that cannot be returned
/// directly under the answer contract.
pub(crate) async fn run_mcp_tool_agent_turn(
    input: McpToolAgentTurnInput<'_>,
) -> Result<AgentTurnResult, AgentTurnFailure> {
    let context = initialize_mcp_agent_loop(input).await?;
    let deadline = context.input.deadline;
    Box::pin(crate::integrations::retry::with_provider_retry_execution(
        deadline,
        run_initialized_mcp_tool_agent_turn(context),
    ))
    .await
}

async fn run_initialized_mcp_tool_agent_turn(
    context: McpAgentLoopContext<'_>,
) -> Result<AgentTurnResult, AgentTurnFailure> {
    let mut state = McpAgentLoopState::new(&context);
    for iteration in 1..=context.iteration_cap {
        match run_mcp_agent_iteration(&context, &mut state, iteration)
            .await
            .map_err(|failure| *failure)?
        {
            AgentIterationControl::Continue => {}
            AgentIterationControl::Stop(reason) => {
                state.stopped_reason = reason;
                break;
            }
            AgentIterationControl::Complete(result) => return Ok(*result),
        }
    }
    finish_mcp_agent_loop(&context, state).map_err(|failure| *failure)
}

pub(super) struct McpAgentLoopContext<'a> {
    pub(super) input: McpToolAgentTurnInput<'a>,
    pub(super) binding: ResolvedRuntimeBinding,
    pub(super) tool_defs: Vec<ChatToolDef>,
    pub(super) allowed_tool_names: BTreeSet<String>,
    pub(super) iteration_cap: usize,
    pub(super) max_parallel_actions: usize,
    pub(super) deadline_started: Instant,
}

pub(super) struct McpAgentLoopState {
    pub(super) messages: Vec<ChatMessage>,
    pub(super) usage_json: Value,
    pub(super) provider_calls: Vec<QueryProviderCall>,
    pub(super) debug_iterations: Vec<LlmIterationDebug>,
    pub(super) total_tool_call_count: usize,
    pub(super) successful_tool_call_count: usize,
    pub(super) successful_tool_names: BTreeSet<String>,
    pub(super) seen_effective_tool_payloads: BTreeMap<String, EffectiveToolPayloadEntry>,
    pub(super) assistant_grounding: AssistantGroundingEvidence,
    pub(super) child_query_execution_ids: Vec<Uuid>,
    pub(super) stopped_reason: AgentStopReason,
    pub(super) last_required_tool_refusal_answer: Option<String>,
    pub(super) verified_grounded_answer_count: usize,
    pub(super) last_verified_grounded_answer: Option<String>,
    pub(super) verified_grounded_answer_guard_text: Option<String>,
    pub(super) last_completed_grounded_answer: Option<String>,
    pub(super) last_completed_nonterminal_grounded_answer: Option<String>,
    pub(super) last_verified_partial_grounded_answer: Option<String>,
    pub(super) grounded_answer_evidence_ledger: GroundedAnswerEvidenceLedger,
    pub(super) focused_grounded_follow_up: FocusedGroundedFollowUpState,
    pub(super) incomplete_grounded_answer_needs_follow_up: bool,
    pub(super) no_progress_iterations: usize,
    pub(super) no_progress_force_final: bool,
    /// Classification of the most recent tool-call failure that the tool
    /// surface itself already marked retryable (for example the
    /// `grounded_answer` tool hitting a transient document-update condition).
    /// A later successful tool call clears it. Consulted only
    /// when the loop ends without a final answer, so the terminal failure
    /// preserves this classification instead of collapsing to a generic
    /// internal error and losing the retryable/503 signal.
    pub(super) last_retryable_tool_failure: Option<RetryableToolFailure>,
    /// Redacted argument-shape detail for the most recent model-generated MCP
    /// validation error. It is cleared by a later successful model tool call
    /// and used only if the bounded loop exhausts without an answer.
    pub(super) last_model_tool_validation_failure: Option<ModelToolValidationFailure>,
}

/// A tool-call failure the tool surface already classified as retryable,
/// carried through to `terminal_agent_loop_failure` so the overall turn
/// failure can be reported with the same classification instead of an opaque
/// internal error.
#[derive(Debug, Clone)]
pub(super) struct RetryableToolFailure {
    pub(super) kind: &'static str,
    pub(super) message: String,
}

impl McpAgentLoopState {
    pub(super) fn new(context: &McpAgentLoopContext<'_>) -> Self {
        let input = &context.input;
        let mut messages = Vec::with_capacity(
            input
                .conversation_history
                .len()
                .saturating_add(input.follow_up_context_messages.len())
                .saturating_add(context.iteration_cap * 3 + 2),
        );
        messages.push(ChatMessage::system(super::super::assistant_prompt::render(
            input.library_ref,
            None,
        )));
        messages.extend(compact_agent_prompt_history(input.conversation_history));
        messages.push(ChatMessage::user(input.user_question.to_string()));
        messages.extend(input.follow_up_context_messages.iter().cloned());
        Self {
            messages,
            usage_json: serde_json::json!({}),
            provider_calls: Vec::new(),
            debug_iterations: Vec::new(),
            total_tool_call_count: 0,
            successful_tool_call_count: 0,
            successful_tool_names: BTreeSet::new(),
            seen_effective_tool_payloads: BTreeMap::new(),
            assistant_grounding: AssistantGroundingEvidence::default(),
            child_query_execution_ids: Vec::new(),
            stopped_reason: AgentStopReason::IterationCap,
            last_required_tool_refusal_answer: None,
            verified_grounded_answer_count: 0,
            last_verified_grounded_answer: None,
            verified_grounded_answer_guard_text: None,
            last_completed_grounded_answer: None,
            last_completed_nonterminal_grounded_answer: None,
            last_verified_partial_grounded_answer: None,
            grounded_answer_evidence_ledger: GroundedAnswerEvidenceLedger::default(),
            focused_grounded_follow_up: FocusedGroundedFollowUpState::default(),
            incomplete_grounded_answer_needs_follow_up: false,
            no_progress_iterations: 0,
            no_progress_force_final: false,
            last_retryable_tool_failure: None,
            last_model_tool_validation_failure: None,
        }
    }

    pub(super) fn take_result(
        &mut self,
        context: &McpAgentLoopContext<'_>,
        answer: String,
        answer_provenance: AgentAnswerProvenance,
        grounded_answer_outcome: Option<AgentGroundedAnswerOutcome>,
        stopped_reason: AgentStopReason,
    ) -> AgentTurnResult {
        let iterations = self.debug_iterations.len();
        AgentTurnResult {
            answer,
            answer_provenance,
            grounded_answer_outcome,
            usage_json: std::mem::replace(&mut self.usage_json, serde_json::json!({})),
            provider_calls: std::mem::take(&mut self.provider_calls),
            iterations,
            assistant_grounding: std::mem::take(&mut self.assistant_grounding),
            child_query_execution_ids: std::mem::take(&mut self.child_query_execution_ids),
            debug_iterations: std::mem::take(&mut self.debug_iterations),
            agent_loop: Some(agent_loop_metadata(
                context.iteration_cap,
                context.input.deadline,
                stopped_reason,
                self.total_tool_call_count,
            )),
        }
    }
}

pub(super) async fn initialize_mcp_agent_loop(
    input: McpToolAgentTurnInput<'_>,
) -> Result<McpAgentLoopContext<'_>, AgentTurnFailure> {
    let binding = input
        .state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(input.state, input.library_id, AiBindingPurpose::Agent)
        .await
        .map_err(AgentTurnFailure::empty)?
        .ok_or_else(|| missing_agent_binding(input.library_id))?;
    let agent_vision_available = resolve_agent_vision_available(&input, &binding).await;
    let tool_defs =
        answer_surface_tool_defs(input.auth, ToolVisibilityCapabilities { agent_vision_available })
            .map_err(|error| AgentTurnFailure::empty(anyhow::Error::new(error)))?;
    if let Some(failure) = validate_agent_tool_definitions(&tool_defs) {
        return Err(failure);
    }
    let allowed_tool_names = tool_defs.iter().map(|tool| tool.name.clone()).collect();
    Ok(McpAgentLoopContext {
        iteration_cap: input.iteration_cap.max(1),
        max_parallel_actions: input.max_parallel_actions.max(1),
        input,
        binding,
        tool_defs,
        allowed_tool_names,
        deadline_started: Instant::now(),
    })
}

pub(super) fn missing_agent_binding(library_id: Uuid) -> AgentTurnFailure {
    AgentTurnFailure::empty(QueryServiceError::BindingNotConfigured {
        message: format!("no active agent binding configured for library {library_id}"),
    })
}

pub(super) async fn resolve_agent_vision_available(
    input: &McpToolAgentTurnInput<'_>,
    binding: &ResolvedRuntimeBinding,
) -> bool {
    input
        .state
        .application_services
        .ai_catalog
        .get_model_catalog(input.state, binding.model_catalog_id)
        .await
        .is_ok_and(|model| model.allowed_binding_purposes.contains(&AiBindingPurpose::ExtractText))
}

pub(super) fn validate_agent_tool_definitions(
    tool_defs: &[ChatToolDef],
) -> Option<AgentTurnFailure> {
    if tool_defs.is_empty() {
        return Some(AgentTurnFailure::empty(QueryServiceError::StateConflict {
            message: "no MCP answer tools are visible for the current caller".to_string(),
        }));
    }
    (!tool_defs.iter().any(|tool| tool.name == GROUNDED_ANSWER_TOOL_NAME)).then(|| {
        AgentTurnFailure::empty(QueryServiceError::StateConflict {
            message: format!(
                "required MCP answer tool '{GROUNDED_ANSWER_TOOL_NAME}' is not visible for the current caller"
            ),
        })
    })
}

pub(super) enum AgentIterationControl {
    Continue,
    Stop(AgentStopReason),
    Complete(Box<AgentTurnResult>),
}

pub(super) struct AgentIterationPlan {
    pub(super) deadline_budget: Duration,
    pub(super) initial_grounded_answer_iteration: bool,
    pub(super) focused_follow_up_iteration: bool,
    pub(super) runtime_enforced_call: Option<ChatToolCall>,
    pub(super) force_final_answer: bool,
    pub(super) require_tool_call: bool,
    pub(super) tools_for_iteration: Vec<ChatToolDef>,
    pub(super) request_messages: Vec<ChatMessage>,
}

pub(super) struct AgentIterationResponse {
    pub(super) response: ToolUseResponse,
    pub(super) request_messages: Vec<ChatMessage>,
    pub(super) model_call_duration_ms: u64,
    pub(super) tool_call_origin: AgentToolCallOrigin,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum AgentToolCallOrigin {
    RuntimeEnforced,
    Model,
    RuntimeFallback,
}

impl AgentToolCallOrigin {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeEnforced => "runtime_enforced",
            Self::Model => "model",
            Self::RuntimeFallback => "runtime_fallback",
        }
    }
}

pub(super) async fn run_mcp_agent_iteration(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
) -> Result<AgentIterationControl, Box<AgentTurnFailure>> {
    let plan = match prepare_agent_iteration(context, state, iteration) {
        Ok(plan) => plan,
        Err(reason) => return Ok(AgentIterationControl::Stop(reason)),
    };
    let mut iteration_response =
        request_agent_iteration(context, state, iteration, &plan).await.map_err(Box::new)?;
    if enforce_forced_final_response(
        context,
        state,
        iteration,
        plan.force_final_answer,
        &mut iteration_response,
    )
    .await
    .map_err(Box::new)?
    {
        return Ok(AgentIterationControl::Stop(AgentStopReason::Deadline));
    }
    if inject_required_grounded_answer_call(
        context,
        iteration,
        &plan,
        &mut iteration_response.response,
    ) {
        iteration_response.tool_call_origin = AgentToolCallOrigin::RuntimeFallback;
    }
    if iteration_response.response.tool_calls.is_empty() {
        return handle_agent_final_response(context, state, iteration, &plan, iteration_response);
    }
    handle_agent_tool_response(context, state, iteration, &plan, iteration_response).await
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum UnresolvedFocusedFollowUpAction {
    ReturnSavedFallback,
    SynthesizeFromEvidence,
    FailClosed,
}

pub(super) fn unresolved_focused_follow_up_action(
    has_saved_fallback: bool,
    has_grounded_evidence: bool,
) -> UnresolvedFocusedFollowUpAction {
    if has_saved_fallback {
        UnresolvedFocusedFollowUpAction::ReturnSavedFallback
    } else if has_grounded_evidence {
        UnresolvedFocusedFollowUpAction::SynthesizeFromEvidence
    } else {
        UnresolvedFocusedFollowUpAction::FailClosed
    }
}

pub(super) fn prepare_agent_iteration(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
) -> Result<AgentIterationPlan, AgentStopReason> {
    let Some(deadline_budget) =
        deadline_remaining(context.deadline_started, context.input.deadline)
    else {
        return Err(AgentStopReason::Deadline);
    };
    let initial_call = initial_grounded_answer_tool_call(
        iteration,
        state.total_tool_call_count,
        context.input.user_question,
        context.input.grounded_answer_top_k,
    );
    let initial_grounded_answer_iteration = initial_call.is_some();
    let focused_call = initial_call
        .is_none()
        .then(|| {
            state
                .focused_grounded_follow_up
                .take_call(context.input.user_question, context.input.grounded_answer_top_k)
        })
        .flatten();
    let focused_follow_up_iteration = focused_call.is_some();
    if !focused_follow_up_iteration
        && state.focused_grounded_follow_up.is_unresolved_after_attempt()
    {
        log_unresolved_focused_follow_up(context, iteration);
        return match unresolved_focused_follow_up_action(
            failed_focused_follow_up_answer(state).is_some(),
            state.grounded_answer_evidence_ledger.has_guardable_evidence(),
        ) {
            UnresolvedFocusedFollowUpAction::ReturnSavedFallback => {
                Err(AgentStopReason::IterationCap)
            }
            UnresolvedFocusedFollowUpAction::SynthesizeFromEvidence => Ok(AgentIterationPlan {
                deadline_budget,
                initial_grounded_answer_iteration,
                focused_follow_up_iteration,
                runtime_enforced_call: None,
                force_final_answer: true,
                require_tool_call: false,
                tools_for_iteration: Vec::new(),
                request_messages: final_answer_request_messages(&state.messages, true),
            }),
            UnresolvedFocusedFollowUpAction::FailClosed => Err(AgentStopReason::ToolError),
        };
    }
    let runtime_enforced_call = initial_call.or(focused_call);
    let force_final_answer =
        should_force_agent_final_answer(context, state, iteration, focused_follow_up_iteration);
    let require_tool_call = focused_follow_up_iteration
        || should_require_tool_call_before_final(
            force_final_answer,
            &context.tool_defs,
            &state.successful_tool_names,
            state.incomplete_grounded_answer_needs_follow_up,
        );
    let tools_for_iteration =
        agent_iteration_tool_defs(context, state, focused_follow_up_iteration, force_final_answer);
    Ok(AgentIterationPlan {
        deadline_budget,
        initial_grounded_answer_iteration,
        focused_follow_up_iteration,
        runtime_enforced_call,
        force_final_answer,
        require_tool_call,
        tools_for_iteration,
        request_messages: final_answer_request_messages(&state.messages, force_final_answer),
    })
}

pub(super) fn log_unresolved_focused_follow_up(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
) {
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        "query.agent_loop.focused_grounded_follow_up_unresolved"
    );
}

pub(super) fn should_force_agent_final_answer(
    context: &McpAgentLoopContext<'_>,
    state: &McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
) -> bool {
    !focused_follow_up_iteration
        && (state.no_progress_force_final
            || force_final_answer_iteration(FinalAnswerForcingInput {
                iteration,
                iteration_cap: context.iteration_cap,
                total_tool_call_count: state.total_tool_call_count,
                successful_tool_call_count: state.successful_tool_call_count,
                verified_grounded_answer_count: state.verified_grounded_answer_count,
                successful_tool_names: &state.successful_tool_names,
                incomplete_grounded_answer_needs_follow_up: state
                    .incomplete_grounded_answer_needs_follow_up,
                started: context.deadline_started,
                soft_final_answer_deadline: context.input.soft_final_answer_deadline,
            }))
}

pub(super) fn agent_iteration_tool_defs(
    context: &McpAgentLoopContext<'_>,
    state: &McpAgentLoopState,
    focused_follow_up_iteration: bool,
    force_final_answer: bool,
) -> Vec<ChatToolDef> {
    if focused_follow_up_iteration {
        return context
            .tool_defs
            .iter()
            .filter(|tool| tool.name == GROUNDED_ANSWER_TOOL_NAME)
            .cloned()
            .collect();
    }
    tool_defs_for_agent_iteration(
        &context.tool_defs,
        &state.successful_tool_names,
        force_final_answer,
    )
}

pub(super) async fn request_agent_iteration(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    plan: &AgentIterationPlan,
) -> Result<AgentIterationResponse, AgentTurnFailure> {
    if let Some(call) = plan.runtime_enforced_call.clone() {
        log_runtime_enforced_agent_call(context, iteration, plan.initial_grounded_answer_iteration);
        return Ok(AgentIterationResponse {
            response: runtime_enforced_tool_response(
                call,
                &context.binding.provider_kind,
                &context.binding.model_name,
            ),
            request_messages: plan.request_messages.clone(),
            model_call_duration_ms: 0,
            tool_call_origin: AgentToolCallOrigin::RuntimeEnforced,
        });
    }
    let (response, duration_ms) = request_agent_model_response(
        context,
        state,
        iteration,
        plan.deadline_budget,
        AgentModelRequest {
            messages: plan.request_messages.clone(),
            tools: plan.tools_for_iteration.clone(),
            require_tool_call: plan.require_tool_call,
            kind: AgentModelRequestKind::Iteration,
        },
    )
    .await?;
    Ok(AgentIterationResponse {
        response,
        request_messages: plan.request_messages.clone(),
        model_call_duration_ms: duration_ms,
        tool_call_origin: AgentToolCallOrigin::Model,
    })
}

pub(super) fn log_runtime_enforced_agent_call(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    initial_grounded_answer_iteration: bool,
) {
    if initial_grounded_answer_iteration {
        log_runtime_enforced_initial_grounded_answer(context, iteration);
        return;
    }
    log_runtime_enforced_focused_grounded_follow_up(context, iteration);
}

fn log_runtime_enforced_initial_grounded_answer(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
) {
    tracing::debug!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        "query.agent_loop.runtime_enforced_initial_grounded_answer"
    );
}

fn log_runtime_enforced_focused_grounded_follow_up(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
) {
    tracing::debug!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        "query.agent_loop.runtime_enforced_focused_grounded_follow_up"
    );
}

#[derive(Clone, Copy)]
pub(super) enum AgentModelRequestKind {
    Iteration,
    ForcedFinalRetry,
}

impl AgentModelRequestKind {
    pub(super) const fn error_context(self) -> &'static str {
        match self {
            Self::Iteration => "MCP-backed assistant agent LLM call failed",
            Self::ForcedFinalRetry => "MCP-backed assistant agent forced-final retry failed",
        }
    }
}
