use super::*;

pub(super) fn finish_mcp_agent_loop(
    context: &McpAgentLoopContext<'_>,
    mut state: McpAgentLoopState,
) -> Result<AgentTurnResult, Box<AgentTurnFailure>> {
    normalize_agent_loop_stop_reason(&mut state);
    if failed_focused_follow_up_answer(&state).is_some() {
        let answer = mark_failed_follow_up_answer(&state);
        return Ok(state.take_result(
            context,
            answer,
            AgentAnswerProvenance::Composed,
            None,
            AgentStopReason::ToolError,
        ));
    }
    if let Some(answer) = iteration_cap_grounded_answer(&state) {
        return Ok(state.take_result(
            context,
            answer,
            AgentAnswerProvenance::Composed,
            None,
            AgentStopReason::IterationCap,
        ));
    }
    Err(Box::new(terminal_agent_loop_failure(context, state)))
}

pub(super) fn normalize_agent_loop_stop_reason(state: &mut McpAgentLoopState) {
    if matches!(state.stopped_reason, AgentStopReason::IterationCap)
        && state.successful_tool_call_count == 0
        && state.total_tool_call_count > 0
    {
        state.stopped_reason = AgentStopReason::ToolError;
    }
}

pub(super) fn failed_focused_follow_up_answer(state: &McpAgentLoopState) -> Option<&str> {
    let failed_follow_up = state.incomplete_grounded_answer_needs_follow_up
        && state.focused_grounded_follow_up.is_unresolved_after_attempt();
    let verified_partial = failed_follow_up
        .then_some(state.last_verified_partial_grounded_answer.as_deref())
        .flatten()
        .filter(|answer| !answer.trim().is_empty());
    verified_partial.or_else(|| {
        completed_grounded_answer_after_failed_focused_follow_up(
            state.incomplete_grounded_answer_needs_follow_up,
            state.focused_grounded_follow_up.was_attempted(),
            state.focused_grounded_follow_up.is_unresolved_after_attempt(),
            state
                .last_completed_nonterminal_grounded_answer
                .as_deref()
                .filter(|answer| !answer.trim().is_empty())
                .or(state.last_completed_grounded_answer.as_deref()),
        )
    })
}

pub(super) fn mark_failed_follow_up_answer(state: &McpAgentLoopState) -> String {
    if let Some(answer) = state
        .last_verified_partial_grounded_answer
        .as_deref()
        .filter(|answer| !answer.trim().is_empty())
    {
        return explicitly_mark_partial_grounded_answer(
            answer,
            state.focused_grounded_follow_up.language(),
        );
    }
    let answer = state
        .last_completed_nonterminal_grounded_answer
        .as_deref()
        .or(state.last_completed_grounded_answer.as_deref())
        .unwrap_or_default();
    explicitly_mark_unverified_completed_grounded_answer(
        answer,
        state.focused_grounded_follow_up.language(),
    )
}

pub(super) fn iteration_cap_grounded_answer(state: &McpAgentLoopState) -> Option<String> {
    current_turn_grounded_answer_on_iteration_cap(IterationCapAnswerInput {
        stopped_reason: state.stopped_reason,
        incomplete_grounded_answer_needs_follow_up: state
            .incomplete_grounded_answer_needs_follow_up,
        focused_grounded_follow_up_attempted: state.focused_grounded_follow_up.was_attempted(),
        successful_tool_call_count: state.successful_tool_call_count,
        verified_grounded_answer_count: state.verified_grounded_answer_count,
        last_verified_grounded_answer: state.last_verified_grounded_answer.as_deref(),
        last_completed_grounded_answer: state.last_completed_grounded_answer.as_deref(),
    })
    .map(ToOwned::to_owned)
}

/// Advance the no-progress streak counter after one tool-running iteration.
///
/// A *new* successful tool result (`iteration_made_progress`) resets the
/// streak; otherwise it grows by one. Returns the updated streak and whether
/// it has reached [`NO_PROGRESS_ITERATION_LIMIT`], at which point the next
/// iteration must be forced into the final-answer path instead of burning the
/// remaining budget on a stuck model.
pub(super) fn next_no_progress_state(
    no_progress_iterations: usize,
    iteration_made_progress: bool,
) -> (usize, bool) {
    if iteration_made_progress {
        return (0, false);
    }
    let updated = no_progress_iterations.saturating_add(1);
    (updated, updated >= NO_PROGRESS_ITERATION_LIMIT)
}

/// Stop reason for a model-emitted final answer: `NoProgress` when the loop
/// forced this final answer because of a no-progress streak, otherwise the
/// ordinary `FinalAnswer`.
pub(super) fn final_answer_stop_reason(no_progress_force_final: bool) -> AgentStopReason {
    if no_progress_force_final { AgentStopReason::NoProgress } else { AgentStopReason::FinalAnswer }
}

pub(super) fn agent_loop_metadata(
    iteration_cap: usize,
    deadline: Duration,
    stopped_reason: AgentStopReason,
    total_tool_call_count: usize,
) -> AgentLoopMetadata {
    let provider_retry = crate::integrations::retry::current_provider_retry_telemetry();
    AgentLoopMetadata {
        iteration_cap,
        deadline_ms: deadline.as_millis().try_into().unwrap_or(u64::MAX),
        stopped_reason,
        tool_call_count: total_tool_call_count,
        provider_429_count: provider_retry.provider_429_count,
        provider_retry_count: provider_retry.provider_retry_count,
        degraded_completion_count: provider_retry.degraded_completion_count,
    }
}

/// Inputs for [`force_final_answer_iteration`]: the current iteration against
/// its cap, the turn's accumulated tool progress, and the soft-deadline window.
pub(super) struct FinalAnswerForcingInput<'a> {
    pub(super) iteration: usize,
    pub(super) iteration_cap: usize,
    pub(super) total_tool_call_count: usize,
    pub(super) successful_tool_call_count: usize,
    pub(super) verified_grounded_answer_count: usize,
    pub(super) successful_tool_names: &'a BTreeSet<String>,
    pub(super) incomplete_grounded_answer_needs_follow_up: bool,
    pub(super) started: Instant,
    pub(super) soft_final_answer_deadline: Option<Duration>,
}

pub(super) fn force_final_answer_iteration(input: FinalAnswerForcingInput<'_>) -> bool {
    let FinalAnswerForcingInput {
        iteration,
        iteration_cap,
        total_tool_call_count,
        successful_tool_call_count,
        verified_grounded_answer_count,
        successful_tool_names,
        incomplete_grounded_answer_needs_follow_up,
        started,
        soft_final_answer_deadline,
    } = input;
    if iteration == iteration_cap && total_tool_call_count > 0 {
        return true;
    }
    if incomplete_grounded_answer_needs_follow_up {
        return false;
    }
    if verified_grounded_answer_count > 0
        && successful_tool_call_count >= SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS
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
        && (successful_tool_call_count >= SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS
            || has_composite_tool_signal(successful_tool_names))
}

pub(super) fn should_require_tool_call_before_final(
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

pub(super) fn should_inject_required_grounded_answer_tool_call(
    response_tool_calls_empty: bool,
    require_tool_call: bool,
    force_final_answer: bool,
    tool_defs: &[ChatToolDef],
) -> bool {
    response_tool_calls_empty
        && require_tool_call
        && !force_final_answer
        && tool_defs.iter().any(|tool| tool.name == GROUNDED_ANSWER_TOOL_NAME)
}

pub(super) fn required_grounded_answer_tool_call(
    user_question: &str,
    grounded_top_k: usize,
) -> ChatToolCall {
    ChatToolCall {
        id: format!("call_{GROUNDED_ANSWER_TOOL_NAME}_fallback"),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({
            "query": user_question,
            "topK": initial_grounded_answer_top_k(grounded_top_k),
            "responseProfile": "compact",
            "maxReferences": crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT,
        })
        .to_string(),
    }
}

pub(super) fn initial_grounded_answer_top_k(grounded_top_k: usize) -> usize {
    let repair_ceiling = MAX_TOP_K.saturating_sub(GROUNDED_ANSWER_REPAIR_TOP_K_HEADROOM).max(1);
    grounded_top_k.max(1).min(repair_ceiling)
}

pub(super) fn initial_grounded_answer_tool_call(
    iteration: usize,
    total_tool_call_count: usize,
    user_question: &str,
    grounded_top_k: usize,
) -> Option<ChatToolCall> {
    (iteration == 1 && total_tool_call_count == 0)
        .then(|| required_grounded_answer_tool_call(user_question, grounded_top_k))
}

pub(super) fn runtime_enforced_tool_response(
    call: ChatToolCall,
    provider_kind: &str,
    model_name: &str,
) -> ToolUseResponse {
    ToolUseResponse {
        provider_kind: provider_kind.to_string(),
        model_name: model_name.to_string(),
        output_text: String::new(),
        tool_calls: vec![call],
        finish_reason: Some("runtime_enforced_tool_call".to_string()),
        usage_json: serde_json::json!({ "runtimeEnforced": true }),
        reasoning_content: None,
    }
}

pub(super) fn focused_grounded_answer_follow_up_call(
    user_question: &str,
    grounded_top_k: usize,
    metadata: RuntimeGroundedRepairMetadata,
) -> ChatToolCall {
    ChatToolCall {
        id: format!("call_{GROUNDED_ANSWER_TOOL_NAME}_focused_follow_up"),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({
            "query": user_question,
            "topK": focused_grounded_answer_repair_top_k(grounded_top_k),
            "responseProfile": "compact",
            "maxReferences": crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT,
            (RUNTIME_REPAIR_ARGUMENT_FIELD): metadata,
        })
        .to_string(),
    }
}

pub(super) fn focused_grounded_answer_repair_top_k(grounded_top_k: usize) -> usize {
    grounded_top_k.max(1).saturating_add(GROUNDED_ANSWER_REPAIR_TOP_K_INCREMENT).min(MAX_TOP_K)
}

#[cfg(test)]
pub(super) fn next_incomplete_grounded_answer_follow_up_required(
    previous_required: bool,
    iteration_had_incomplete_grounded_answer: bool,
    iteration_had_follow_up_after_incomplete_grounded_answer: bool,
) -> bool {
    iteration_had_incomplete_grounded_answer
        || (previous_required && !iteration_had_follow_up_after_incomplete_grounded_answer)
}

#[cfg(test)]
pub(super) fn tool_result_satisfies_incomplete_grounded_follow_up(
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

pub(super) fn tool_defs_for_agent_iteration(
    tool_defs: &[ChatToolDef],
    _successful_tool_names: &BTreeSet<String>,
    force_final_answer: bool,
) -> Vec<ChatToolDef> {
    if force_final_answer || tool_defs.is_empty() {
        return Vec::new();
    }
    tool_defs.to_vec()
}

pub(super) fn has_composite_tool_signal(successful_tool_names: &BTreeSet<String>) -> bool {
    let categories = [
        successful_tool_names.iter().any(|name| is_document_content_tool(name)),
        successful_tool_names.iter().any(|name| is_graph_content_tool(name)),
        successful_tool_names.iter().any(|name| is_runtime_content_tool(name)),
        successful_tool_names.contains(GROUNDED_ANSWER_TOOL_NAME),
    ];
    categories.into_iter().filter(|present| *present).count() >= 2
}

pub(super) fn is_document_content_tool(tool_name: &str) -> bool {
    matches!(tool_name, SEARCH_DOCUMENTS_TOOL_NAME | READ_DOCUMENT_TOOL_NAME)
}

pub(super) fn is_graph_content_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "search_entities" | "get_graph_topology" | "list_relations" | "get_communities"
    )
}

pub(super) fn is_runtime_content_tool(tool_name: &str) -> bool {
    matches!(tool_name, "get_runtime_execution" | "get_runtime_execution_trace")
}

pub(super) fn tool_requirement_reminder() -> String {
    "Before writing the final answer, call at least one MCP tool and inspect the result. Use any relevant visible tool, and do not repeat an identical argument payload.".to_string()
}

pub(super) fn final_answer_request_messages(
    messages: &[ChatMessage],
    force_final_answer: bool,
) -> Vec<ChatMessage> {
    let mut request_messages = messages.to_vec();
    if force_final_answer {
        request_messages.push(ChatMessage::system(final_answer_required_reminder()));
    }
    request_messages
}

pub(super) fn final_answer_required_reminder() -> String {
    "No more MCP tool calls are available in this turn. Write the final answer now from the tool results already in the transcript. If the evidence is partial, answer only the supported parts and mark missing parts plainly.".to_string()
}

pub(super) fn final_answer_retry_messages(
    request_messages: &[ChatMessage],
    reasoning_content: Option<String>,
    tool_calls: &[ChatToolCall],
) -> Vec<ChatMessage> {
    let mut retry_messages = request_messages.to_vec();
    retry_messages.push(ChatMessage::assistant_with_reasoning_and_tool_calls(
        reasoning_content,
        tool_calls.to_vec(),
    ));
    for call in tool_calls {
        retry_messages.push(ChatMessage::tool_result(
            call.id.clone(),
            call.name.clone(),
            final_answer_tool_unavailable_result(),
        ));
    }
    retry_messages.push(ChatMessage::system(final_answer_retry_reminder()));
    retry_messages
}

pub(super) fn final_answer_tool_unavailable_result() -> String {
    "This tool request was not executed because no more MCP tool calls are available in this turn. Produce the final answer from the prior tool results already in the transcript.".to_string()
}

pub(super) fn final_answer_retry_reminder() -> String {
    "The previous response requested another tool, but this is the final answer step and tools are unavailable. Do not request tools. Produce the answer from the existing tool results now.".to_string()
}
