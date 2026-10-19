use super::*;

pub(super) mod tool_failure_diagnostics;

pub(super) use tool_failure_diagnostics::ModelToolValidationFailure;
use tool_failure_diagnostics::{
    log_runtime_initial_grounded_answer_failure, observe_model_tool_validation_outcomes,
    tool_iteration_stop_reason,
};

/// Grouped inputs for [`request_agent_model_response`]: the transcript and tool
/// surface the model sees, plus which request kind this call is.
pub(super) struct AgentModelRequest {
    pub(super) messages: Vec<ChatMessage>,
    pub(super) tools: Vec<ChatToolDef>,
    pub(super) require_tool_call: bool,
    pub(super) kind: AgentModelRequestKind,
}

pub(super) async fn request_agent_model_response(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    deadline_budget: Duration,
    request: AgentModelRequest,
) -> Result<(ToolUseResponse, u64), AgentTurnFailure> {
    let AgentModelRequest { messages, tools, require_tool_call, kind: request_kind } = request;
    let (attribution, mut reservation) = reserve_attributed_provider_call(
        context.input.state,
        context.input.execution_context,
        &context.binding,
        QueryProviderCallKind::QueryAgent,
    )
    .await
    .map_err(AgentTurnFailure::empty)?;
    emit_model_request_activity(context, iteration);
    let started = Instant::now();
    let request = ToolUseRequest {
        provider_kind: context.binding.provider_kind.clone(),
        model_name: context.binding.model_name.clone(),
        api_key_override: context.binding.api_key.clone(),
        base_url_override: context.binding.provider_base_url.clone(),
        temperature: context.binding.temperature,
        top_p: context.binding.top_p,
        max_output_tokens_override: context.binding.max_output_tokens_override,
        messages,
        tools,
        extra_parameters_json: context.binding.extra_parameters_json.clone(),
        require_tool_call,
    };
    let provider_call = crate::observability::with_provider_call_id(
        reservation.provider_call_id(),
        crate::integrations::retry::with_provider_retry_execution(
            deadline_budget,
            context.input.state.llm_gateway.generate_with_tools(request),
        ),
    );
    let response = match tokio::time::timeout(deadline_budget, provider_call).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            fail_provider_call(&mut reservation).await;
            log_agent_provider_error(context, iteration, &error, request_kind);
            return Err(agent_loop_failure(
                state,
                context,
                error.context(request_kind.error_context()),
                AgentStopReason::ProviderError,
            ));
        }
        Err(_) => {
            return Err(agent_loop_failure(
                state,
                context,
                QueryServiceError::DeadlineExceeded,
                AgentStopReason::Deadline,
            ));
        }
    };
    let provider_call = complete_attributed_provider_call(
        &attribution,
        &mut reservation,
        response.usage_json.clone(),
    )
    .await
    .map_err(|error| agent_loop_failure(state, context, error, AgentStopReason::ProviderError))?;
    state.provider_calls.push(provider_call);
    merge_usage_into(&mut state.usage_json, &response.usage_json);
    let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    Ok((response, duration_ms))
}

pub(super) fn log_agent_provider_error(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    error: &anyhow::Error,
    request_kind: AgentModelRequestKind,
) {
    match request_kind {
        AgentModelRequestKind::Iteration => {
            log_agent_iteration_provider_error(context, iteration, error);
        }
        AgentModelRequestKind::ForcedFinalRetry => {
            log_agent_forced_final_retry_provider_error(context, iteration, error);
        }
    }
}

fn log_agent_iteration_provider_error(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    error: &anyhow::Error,
) {
    tracing::warn!(
        provider = %context.binding.provider_kind,
        model = %context.binding.model_name,
        iteration,
        max_output_tokens_override = ?context.binding.max_output_tokens_override,
        error = %error,
        "MCP-backed assistant agent provider call failed"
    );
}

fn log_agent_forced_final_retry_provider_error(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    error: &anyhow::Error,
) {
    tracing::warn!(
        provider = %context.binding.provider_kind,
        model = %context.binding.model_name,
        iteration,
        max_output_tokens_override = ?context.binding.max_output_tokens_override,
        error = %error,
        "MCP-backed assistant agent forced-final retry failed"
    );
}

pub(super) fn emit_model_request_activity(context: &McpAgentLoopContext<'_>, iteration: usize) {
    emit_activity(
        &context.input.activity_tx,
        AgentLoopActivityEvent::ModelRequest {
            iteration,
            provider_kind: context.binding.provider_kind.clone(),
            model_name: context.binding.model_name.clone(),
        },
    );
}

pub(super) fn agent_loop_failure(
    state: &McpAgentLoopState,
    context: &McpAgentLoopContext<'_>,
    error: impl Into<QueryServiceError>,
    stop_reason: AgentStopReason,
) -> AgentTurnFailure {
    AgentTurnFailure::with_loop(
        error,
        state.provider_calls.clone(),
        state.debug_iterations.clone(),
        agent_loop_metadata(
            context.iteration_cap,
            context.input.deadline,
            stop_reason,
            state.total_tool_call_count,
        ),
    )
}

pub(super) async fn enforce_forced_final_response(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    force_final_answer: bool,
    iteration_response: &mut AgentIterationResponse,
) -> Result<bool, AgentTurnFailure> {
    if !force_final_answer || iteration_response.response.tool_calls.is_empty() {
        return Ok(false);
    }
    log_forced_final_tool_calls(context, iteration, &iteration_response.response);
    if !iteration_response.response.output_text.trim().is_empty() {
        iteration_response.response.tool_calls.clear();
        return Ok(false);
    }
    let Some(deadline_budget) =
        deadline_remaining(context.deadline_started, context.input.deadline)
    else {
        return Ok(true);
    };
    let retry_messages = final_answer_retry_messages(
        &iteration_response.request_messages,
        iteration_response.response.reasoning_content.clone(),
        &iteration_response.response.tool_calls,
    );
    let (retry_response, retry_duration_ms) = request_agent_model_response(
        context,
        state,
        iteration,
        deadline_budget,
        AgentModelRequest {
            messages: retry_messages.clone(),
            tools: Vec::new(),
            require_tool_call: false,
            kind: AgentModelRequestKind::ForcedFinalRetry,
        },
    )
    .await?;
    iteration_response.request_messages = retry_messages;
    iteration_response.model_call_duration_ms =
        iteration_response.model_call_duration_ms.saturating_add(retry_duration_ms);
    iteration_response.response = retry_response;
    discard_forced_final_retry_tool_calls(context, iteration, &mut iteration_response.response);
    Ok(false)
}

pub(super) fn log_forced_final_tool_calls(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    response: &ToolUseResponse,
) {
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        tool_call_count = response.tool_calls.len(),
        has_output_text = !response.output_text.trim().is_empty(),
        "assistant agent provider returned tool calls during forced-final iteration"
    );
}

pub(super) fn discard_forced_final_retry_tool_calls(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    response: &mut ToolUseResponse,
) {
    if response.tool_calls.is_empty() {
        return;
    }
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        tool_call_count = response.tool_calls.len(),
        "assistant agent forced-final retry still returned tool calls; discarding them"
    );
    response.tool_calls.clear();
}

pub(super) fn inject_required_grounded_answer_call(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    plan: &AgentIterationPlan,
    response: &mut ToolUseResponse,
) -> bool {
    if !should_inject_required_grounded_answer_tool_call(
        response.tool_calls.is_empty(),
        plan.require_tool_call,
        plan.force_final_answer,
        &context.tool_defs,
    ) {
        return false;
    }
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        provider = %context.binding.provider_kind,
        model = %context.binding.model_name,
        has_output_text = !response.output_text.trim().is_empty(),
        "assistant agent omitted a required MCP tool call; executing grounded_answer fallback"
    );
    response.tool_calls.push(required_grounded_answer_tool_call(
        context.input.user_question,
        context.input.grounded_answer_top_k,
    ));
    true
}

pub(super) fn handle_agent_final_response(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    plan: &AgentIterationPlan,
    iteration_response: AgentIterationResponse,
) -> Result<AgentIterationControl, Box<AgentTurnFailure>> {
    let answer = iteration_response.response.output_text.trim().to_string();
    emit_model_response_activity(context, iteration, 0, !answer.is_empty());
    if answer.is_empty() {
        return Err(Box::new(agent_loop_failure(
            state,
            context,
            ProviderCallError::protocol("assistant agent returned an empty final answer"),
            AgentStopReason::ProviderError,
        )));
    }
    state.debug_iterations.push(LlmIterationDebug {
        iteration,
        provider_kind: context.binding.provider_kind.clone(),
        model_name: context.binding.model_name.clone(),
        request_messages: iteration_response.request_messages,
        response_text: Some(answer.clone()),
        response_tool_calls: Vec::new(),
        usage: iteration_response.response.usage_json,
        duration_ms: Some(iteration_response.model_call_duration_ms),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    });
    if plan.require_tool_call
        && state.last_required_tool_refusal_answer.is_none()
        && iteration < context.iteration_cap
    {
        state.last_required_tool_refusal_answer = Some(answer.clone());
        // DeepSeek requires the previous turn's `reasoning_content` to be
        // echoed back on every later request that carries tools, including a
        // turn where the model answered without calling one.
        state.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: Some(answer.clone()),
            reasoning_content: iteration_response.response.reasoning_content.clone(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        });
        state.messages.push(ChatMessage::system(tool_requirement_reminder()));
        return Ok(AgentIterationControl::Continue);
    }
    let stop_reason = final_answer_stop_reason(state.no_progress_force_final);
    let answer = finalize_composed_agent_answer(context, state, answer);
    let answer = mark_unresolved_repair_synthesis(&state.focused_grounded_follow_up, answer);
    Ok(AgentIterationControl::Complete(Box::new(state.take_result(
        context,
        answer,
        AgentAnswerProvenance::Composed,
        None,
        stop_reason,
    ))))
}

pub(super) fn emit_model_response_activity(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    tool_call_count: usize,
    has_final_answer: bool,
) {
    emit_activity(
        &context.input.activity_tx,
        AgentLoopActivityEvent::ModelResponse {
            iteration,
            provider_kind: context.binding.provider_kind.clone(),
            model_name: context.binding.model_name.clone(),
            tool_call_count,
            has_final_answer,
        },
    );
}

pub(super) fn mark_unresolved_repair_synthesis(
    focused_follow_up: &FocusedGroundedFollowUpState,
    answer: String,
) -> String {
    if focused_follow_up.is_unresolved_after_attempt() {
        explicitly_mark_unverified_completed_grounded_answer(&answer, focused_follow_up.language())
    } else {
        answer
    }
}

pub(super) fn finalize_composed_agent_answer(
    context: &McpAgentLoopContext<'_>,
    state: &McpAgentLoopState,
    answer: String,
) -> String {
    let ledger_guard = state.grounded_answer_evidence_ledger.guard_candidate_for_answer(&answer);
    let grounded_guard = ledger_guard.as_deref().or_else(|| {
        verified_grounded_answer_guard_candidate(
            state.last_verified_grounded_answer.as_deref(),
            state.verified_grounded_answer_guard_text.as_deref(),
            state.verified_grounded_answer_count,
            state.successful_tool_call_count,
        )
    });
    finalize_agent_loop_answer(
        answer,
        context.input.user_question,
        grounded_guard,
        context.input.request_id,
        context.input.library_id,
    )
}

pub(super) async fn handle_agent_tool_response(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    plan: &AgentIterationPlan,
    iteration_response: AgentIterationResponse,
) -> Result<AgentIterationControl, Box<AgentTurnFailure>> {
    let tool_calls = iteration_response.response.tool_calls.clone();
    if iteration_response.tool_call_origin != AgentToolCallOrigin::RuntimeEnforced {
        emit_model_response_activity(context, iteration, tool_calls.len(), false);
    }
    push_agent_tool_request_message(state, &iteration_response.response, &tool_calls);
    let outcomes = execute_tool_calls(
        context.input.clone(),
        AgentToolDispatch {
            iteration,
            tool_calls: &tool_calls,
            max_parallel_actions: context.max_parallel_actions,
            deadline_started: context.deadline_started,
            focused_grounded_follow_up: plan.focused_follow_up_iteration,
            allowed_tool_names: &context.allowed_tool_names,
        },
        &mut state.seen_effective_tool_payloads,
    )
    .await;
    state.total_tool_call_count = state.total_tool_call_count.saturating_add(tool_calls.len());
    let observed = observe_agent_tool_outcomes(
        context,
        state,
        iteration,
        plan.focused_follow_up_iteration,
        &tool_calls,
        &outcomes,
    );
    observe_model_tool_validation_outcomes(
        context,
        state,
        iteration,
        iteration_response.tool_call_origin,
        &tool_calls,
        &outcomes,
    );
    let grounded_passthrough = select_grounded_answer_passthrough_candidate(
        plan.focused_follow_up_iteration,
        &tool_calls,
        &outcomes,
        context.input.user_question,
    );
    push_grounded_evidence_ledger_message(state);
    state.debug_iterations.push(LlmIterationDebug {
        iteration,
        provider_kind: context.binding.provider_kind.clone(),
        model_name: context.binding.model_name.clone(),
        request_messages: iteration_response.request_messages,
        response_text: (!iteration_response.response.output_text.trim().is_empty())
            .then(|| iteration_response.response.output_text.trim().to_string()),
        response_tool_calls: observed.response_tool_calls,
        usage: iteration_response.response.usage_json,
        duration_ms: Some(iteration_response.model_call_duration_ms),
        child_runtime_execution_ids: observed.child_runtime_execution_ids,
        child_query_execution_ids: observed.child_query_execution_ids,
    });
    if let Some(stop_reason) = tool_iteration_stop_reason(
        iteration_response.tool_call_origin,
        plan.initial_grounded_answer_iteration,
        &tool_calls,
        &outcomes,
        state.focused_grounded_follow_up.requires_resolution(),
    ) {
        log_runtime_initial_grounded_answer_failure(context, iteration, &tool_calls, &outcomes);
        return Ok(AgentIterationControl::Stop(stop_reason));
    }
    if let Some(passthrough) = grounded_passthrough {
        return Ok(complete_grounded_answer_passthrough(context, state, iteration, passthrough));
    }
    update_focused_follow_up_requirement(
        context,
        state,
        iteration,
        plan.focused_follow_up_iteration,
    );
    update_agent_no_progress(context, state, iteration, observed.iteration_made_progress);
    Ok(AgentIterationControl::Continue)
}

pub(super) fn push_agent_tool_request_message(
    state: &mut McpAgentLoopState,
    response: &ToolUseResponse,
    tool_calls: &[ChatToolCall],
) {
    state.messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: (!response.output_text.trim().is_empty())
            .then(|| response.output_text.trim().to_string()),
        reasoning_content: response.reasoning_content.clone(),
        tool_calls: tool_calls.to_vec(),
        tool_call_id: None,
        name: None,
    });
}

pub(super) struct ObservedAgentToolOutcomes {
    pub(super) response_tool_calls: Vec<ResponseToolCallDebug>,
    pub(super) child_runtime_execution_ids: Vec<Uuid>,
    pub(super) child_query_execution_ids: Vec<Uuid>,
    pub(super) iteration_made_progress: bool,
}

pub(super) fn observe_agent_tool_outcomes(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
) -> ObservedAgentToolOutcomes {
    let mut observed = ObservedAgentToolOutcomes {
        response_tool_calls: Vec::with_capacity(tool_calls.len()),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
        iteration_made_progress: false,
    };
    for (call, outcome) in tool_calls.iter().zip(outcomes) {
        observe_agent_tool_outcome(
            context,
            state,
            ObservedToolCall { iteration, focused_follow_up_iteration, call, outcome },
            &mut observed,
        );
    }
    observed
}

/// One model tool call paired with the outcome it produced, as observed by
/// [`observe_agent_tool_outcome`].
pub(super) struct ObservedToolCall<'a> {
    pub(super) iteration: usize,
    pub(super) focused_follow_up_iteration: bool,
    pub(super) call: &'a ChatToolCall,
    pub(super) outcome: &'a ToolExecutionOutcome,
}

pub(super) fn observe_agent_tool_outcome(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    tool_call: ObservedToolCall<'_>,
    observed: &mut ObservedAgentToolOutcomes,
) {
    let ObservedToolCall { iteration, focused_follow_up_iteration, call, outcome } = tool_call;
    observe_focused_follow_up_outcome(
        context,
        state,
        iteration,
        focused_follow_up_iteration,
        call,
        outcome,
    );
    state.child_query_execution_ids.extend(outcome.child_query_execution_ids.iter().copied());
    observed.child_query_execution_ids.extend(outcome.child_query_execution_ids.iter().copied());
    observed
        .child_runtime_execution_ids
        .extend(outcome.child_runtime_execution_ids.iter().copied());
    if !outcome.is_error {
        observe_successful_agent_tool_outcome(state, call, outcome, observed);
        state.last_retryable_tool_failure = None;
    } else {
        state.last_retryable_tool_failure =
            retryable_tool_failure_from_result_json(outcome.result_json.as_ref());
    }
    observed.response_tool_calls.push(ResponseToolCallDebug {
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
    state.messages.push(ChatMessage::tool_result(
        call.id.clone(),
        call.name.clone(),
        outcome.message_content.clone(),
    ));
}

pub(super) fn observe_focused_follow_up_outcome(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
    call: &ChatToolCall,
    outcome: &ToolExecutionOutcome,
) {
    if !focused_follow_up_iteration || call.name != GROUNDED_ANSWER_TOOL_NAME {
        return;
    }
    state.focused_grounded_follow_up.observe_attempt_outcome(outcome);
    tracing::info!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        duration_ms = outcome.duration_ms,
        succeeded = !outcome.is_error && outcome.grounded_answer_ready,
        unresolved = state.focused_grounded_follow_up.requires_resolution(),
        "query.agent_loop.focused_grounded_follow_up_outcome"
    );
}

pub(super) fn observe_successful_agent_tool_outcome(
    state: &mut McpAgentLoopState,
    call: &ChatToolCall,
    outcome: &ToolExecutionOutcome,
    observed: &mut ObservedAgentToolOutcomes,
) {
    state.successful_tool_call_count = state.successful_tool_call_count.saturating_add(1);
    state.successful_tool_names.insert(call.name.clone());
    observed.iteration_made_progress |= !outcome.is_replay;
    remember_grounded_answer_outcome(state, call, outcome);
    schedule_grounded_answer_repair(state, outcome);
    state.grounded_answer_evidence_ledger.remember(&call.name, outcome);
    if let Some(grounding_text) = &outcome.grounding_text {
        push_tool_grounding_fragment(&mut state.assistant_grounding, &call.name, grounding_text);
    }
}

pub(super) fn remember_grounded_answer_outcome(
    state: &mut McpAgentLoopState,
    call: &ChatToolCall,
    outcome: &ToolExecutionOutcome,
) {
    if outcome.grounded_answer_ready {
        state.verified_grounded_answer_count =
            state.verified_grounded_answer_count.saturating_add(1);
        state.last_verified_grounded_answer = remember_verified_grounded_answer(
            state.last_verified_grounded_answer.take(),
            &call.name,
            outcome,
        );
        state.verified_grounded_answer_guard_text = remember_verified_grounded_answer_guard_text(
            state.verified_grounded_answer_guard_text.take(),
            &call.name,
            outcome,
        );
    }
    if !outcome.grounded_answer_completed {
        return;
    }
    state.last_completed_grounded_answer = remember_completed_grounded_answer(
        state.last_completed_grounded_answer.take(),
        &call.name,
        outcome,
    );
    state.last_completed_nonterminal_grounded_answer =
        remember_completed_nonterminal_grounded_answer(
            state.last_completed_nonterminal_grounded_answer.take(),
            &call.name,
            outcome,
        );
    state.last_verified_partial_grounded_answer = remember_verified_partial_grounded_answer(
        state.last_verified_partial_grounded_answer.take(),
        &call.name,
        outcome,
    );
}

pub(super) fn schedule_grounded_answer_repair(
    state: &mut McpAgentLoopState,
    outcome: &ToolExecutionOutcome,
) {
    if outcome.grounded_answer_clarification_required {
        return;
    }
    if let Some(reason) = outcome.grounded_answer_repair_reason {
        state.focused_grounded_follow_up.schedule(reason, outcome.grounded_answer_language);
    }
}

pub(super) fn select_grounded_answer_passthrough_candidate(
    focused_follow_up_iteration: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    user_question: &str,
) -> Option<TerminalGroundedAnswerPassthrough> {
    terminal_grounded_answer_nonfactual_candidate(tool_calls, outcomes, user_question)
        .or_else(|| {
            focused_grounded_answer_passthrough_candidate(
                focused_follow_up_iteration,
                tool_calls,
                outcomes,
            )
        })
        .or_else(|| grounded_answer_passthrough_candidate(tool_calls, outcomes, user_question))
}

pub(super) fn push_grounded_evidence_ledger_message(state: &mut McpAgentLoopState) {
    if let Some(message) = state.grounded_answer_evidence_ledger.system_message() {
        state.messages.push(ChatMessage::system(message));
    }
}

pub(super) fn complete_grounded_answer_passthrough(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    passthrough: TerminalGroundedAnswerPassthrough,
) -> AgentIterationControl {
    let TerminalGroundedAnswerPassthrough { answer, stop_reason, grounded_answer_outcome } =
        passthrough;
    tracing::info!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        answer_chars = answer.chars().count(),
        "query.agent_loop.\u{63}anonical_grounded_answer_passthrough"
    );
    AgentIterationControl::Complete(Box::new(state.take_result(
        context,
        answer,
        AgentAnswerProvenance::GroundedAnswerPassthrough,
        Some(grounded_answer_outcome),
        stop_reason,
    )))
}

pub(super) fn update_focused_follow_up_requirement(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
) {
    let next = state.focused_grounded_follow_up.requires_resolution();
    if next != state.incomplete_grounded_answer_needs_follow_up {
        tracing::debug!(
            request_id = context.input.request_id,
            library_id = %context.input.library_id,
            iteration,
            previous = state.incomplete_grounded_answer_needs_follow_up,
            focused_follow_up_iteration,
            next,
            "query.agent_loop.grounded_answer_follow_up_state"
        );
    }
    state.incomplete_grounded_answer_needs_follow_up = next;
}

pub(super) fn update_agent_no_progress(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    iteration_made_progress: bool,
) {
    let force_final;
    (state.no_progress_iterations, force_final) =
        next_no_progress_state(state.no_progress_iterations, iteration_made_progress);
    if !force_final {
        return;
    }
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        no_progress_iterations = state.no_progress_iterations,
        "query.agent_loop.no_progress_forced_final"
    );
    state.no_progress_force_final = true;
}
