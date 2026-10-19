use super::*;

pub(super) fn finalize_agent_loop_answer(
    answer: String,
    user_question: &str,
    verified_grounded_answer: Option<&str>,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let answer = prefer_verified_grounded_answer_on_unverified_procedure_rewrite(
        answer,
        verified_grounded_answer,
        request_id,
        library_id,
    );
    let answer = prefer_verified_grounded_answer_on_ordered_source_loss(
        answer,
        verified_grounded_answer,
        request_id,
        library_id,
    );
    prefer_verified_grounded_answer_on_literal_drift(
        answer,
        user_question,
        verified_grounded_answer,
        request_id,
        library_id,
    )
}

pub(super) fn verified_grounded_answer_fallback_candidate(
    last_verified_grounded_answer: Option<&str>,
    verified_grounded_answer_count: usize,
    successful_tool_call_count: usize,
) -> Option<&str> {
    if verified_grounded_answer_count == 1 && successful_tool_call_count == 1 {
        return last_verified_grounded_answer;
    }
    None
}

pub(super) fn terminal_grounded_answer_nonfactual_candidate(
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    user_question: &str,
) -> Option<TerminalGroundedAnswerPassthrough> {
    let ([call], [outcome]) = (tool_calls, outcomes) else {
        return None;
    };
    if call.name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || outcome.is_replay
        || !outcome.grounded_answer_completed
        || !grounded_answer_outcome_is_terminal_nonfactual(outcome)
        || !grounded_answer_executed_query_matches_user_question(outcome, user_question)
    {
        return None;
    }
    let answer =
        outcome.grounded_answer_body.as_ref().filter(|answer| !answer.trim().is_empty())?.clone();
    let grounded_answer_outcome = outcome.grounded_answer_outcome.clone()?;
    Some(TerminalGroundedAnswerPassthrough::final_answer(answer, grounded_answer_outcome))
}

pub(super) fn grounded_answer_outcome_is_terminal_nonfactual(
    outcome: &ToolExecutionOutcome,
) -> bool {
    outcome.grounded_answer_outcome.as_ref().is_some_and(|typed| {
        matches!(
            typed.disposition,
            QueryAnswerDisposition::SafeFallback | QueryAnswerDisposition::Clarification
        )
    })
}

pub(super) fn grounded_answer_passthrough_candidate(
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    user_question: &str,
) -> Option<TerminalGroundedAnswerPassthrough> {
    let ([call], [outcome]) = (tool_calls, outcomes) else {
        return None;
    };
    if call.name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || outcome.is_replay
        || !outcome.grounded_answer_ready
        || !outcome.grounded_answer_completed
        || outcome.grounded_answer_needs_follow_up
        || outcome.grounded_answer_repair_reason.is_some()
        || !grounded_answer_executed_query_matches_user_question(outcome, user_question)
    {
        return None;
    }
    let answer =
        outcome.grounded_answer_body.as_ref().filter(|answer| !answer.trim().is_empty())?.clone();
    let grounded_answer_outcome = outcome
        .grounded_answer_outcome
        .as_ref()
        .filter(|typed| matches!(typed.disposition, QueryAnswerDisposition::FactualReady))?
        .clone();
    Some(TerminalGroundedAnswerPassthrough::final_answer(answer, grounded_answer_outcome))
}

pub(super) fn focused_grounded_answer_passthrough_candidate(
    focused_grounded_follow_up: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
) -> Option<TerminalGroundedAnswerPassthrough> {
    if !focused_grounded_follow_up {
        return None;
    }
    let ([call], [outcome]) = (tool_calls, outcomes) else {
        return None;
    };
    if call.name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || outcome.is_replay
        || !outcome.grounded_answer_ready
        || !outcome.grounded_answer_completed
        || outcome.grounded_answer_needs_follow_up
        || outcome.grounded_answer_repair_reason.is_some()
    {
        return None;
    }
    let answer =
        outcome.grounded_answer_body.as_ref().filter(|answer| !answer.trim().is_empty())?.clone();
    let grounded_answer_outcome = outcome
        .grounded_answer_outcome
        .as_ref()
        .filter(|typed| matches!(typed.disposition, QueryAnswerDisposition::FactualReady))?
        .clone();
    Some(TerminalGroundedAnswerPassthrough::final_answer(answer, grounded_answer_outcome))
}

pub(super) fn runtime_initial_grounded_answer_failed(
    initial_grounded_answer_iteration: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    focused_follow_up_scheduled: bool,
) -> bool {
    if !initial_grounded_answer_iteration || focused_follow_up_scheduled {
        return false;
    }
    let ([call], [outcome]) = (tool_calls, outcomes) else {
        return true;
    };
    call.name != GROUNDED_ANSWER_TOOL_NAME || outcome.is_error || !outcome.grounded_answer_completed
}

pub(super) fn grounded_answer_executed_query_matches_user_question(
    outcome: &ToolExecutionOutcome,
    user_question: &str,
) -> bool {
    let Some(arguments_json) = outcome.arguments_json.as_deref() else {
        return false;
    };
    let Ok(arguments) = serde_json::from_str::<Value>(arguments_json) else {
        return false;
    };
    let Some(executed_query) = arguments.get("query").and_then(Value::as_str) else {
        return false;
    };

    let current_user_question = grounded_answer_passthrough_question(user_question);
    !current_user_question.is_empty()
        && grounded_answer_passthrough_question(executed_query) == current_user_question
}
