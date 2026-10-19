use super::*;

/// One grounded-answer tool call under the workspace-a scope every
/// effective-payload test shares; only the id and raw query vary.
fn grounded_tool_call(id: &str, query: &str) -> ChatToolCall {
    ChatToolCall {
        id: id.to_string(),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({
            "library": "workspace-a/library-b",
            "query": query,
            "topK": 8
        })
        .to_string(),
    }
}

/// The single-turn argument context shared by the effective-payload tests.
fn agent_tool_argument_context(user_question: &'static str) -> AgentToolArgumentContext<'static> {
    AgentToolArgumentContext {
        user_question,
        contextual_follow_up: false,
        grounded_top_k: 8,
        library_ref: "workspace-a/library-b",
        grounded_answer_tool_history: &[],
    }
}

fn focused_agent_tool_argument_context() -> AgentToolArgumentContext<'static> {
    agent_tool_argument_context("focused subquestion")
}

/// Prepare one tool-call batch under the shared focused context, returning the
/// pending calls plus the batch's outcome slots.
fn prepare_focused_turn(
    tool_calls: &[ChatToolCall],
    seen: &mut BTreeMap<String, EffectiveToolPayloadEntry>,
) -> (Vec<PendingAgentToolCall>, Vec<Option<ToolExecutionOutcome>>) {
    let mut outcomes = vec![None; tool_calls.len()];
    let pending = prepare_agent_tool_calls(
        tool_calls,
        focused_agent_tool_argument_context(),
        seen,
        &mut outcomes,
    );
    (pending, outcomes)
}

// GUARD 1 — `per_tool_call_wait` is bounded by the smaller of the remaining
// turn deadline and the configured per-tool-call max.
#[test]
fn per_tool_call_wait_is_bounded_by_remaining_and_soft_target() {
    // Synthetic soft target below the hard max, standing in for the caller's
    // tool-collection target threaded into the turn.
    let soft = Duration::from_secs(20);
    assert!(soft < PER_TOOL_CALL_MAX_WAIT);
    // Plenty of turn deadline left: the soft tool-collection target caps it.
    assert_eq!(per_tool_call_wait(Duration::from_secs(120), Some(soft)), soft);
    // Little turn deadline left: the remaining deadline caps it.
    assert_eq!(per_tool_call_wait(Duration::from_secs(2), Some(soft)), Duration::from_secs(2));
    // No soft target supplied: the hard per-call max applies.
    assert_eq!(per_tool_call_wait(Duration::from_secs(120), None), PER_TOOL_CALL_MAX_WAIT);
    // A misconfigured oversized soft target can never exceed the hard max.
    assert_eq!(
        per_tool_call_wait(Duration::from_secs(600), Some(Duration::from_secs(300))),
        PER_TOOL_CALL_MAX_WAIT
    );
}

#[test]
fn grounded_answer_wait_uses_remaining_turn_deadline_for_initial_authoritative_call() {
    let remaining = Duration::from_secs(150);
    let soft = Duration::from_secs(35);

    let wait = per_tool_call_wait_for_tool(GROUNDED_ANSWER_TOOL_NAME, remaining, Some(soft), false);

    assert_eq!(wait, remaining);
    assert_eq!(
        per_tool_call_wait_for_tool(
            GROUNDED_ANSWER_TOOL_NAME,
            Duration::from_secs(45),
            Some(soft),
            false,
        ),
        Duration::from_secs(45),
    );
    assert_eq!(per_tool_call_wait_for_tool("search_documents", remaining, Some(soft), false), soft,);
    assert_eq!(
        per_tool_call_wait_for_tool(GROUNDED_ANSWER_TOOL_NAME, remaining, Some(soft), true,),
        GROUNDED_ANSWER_REPAIR_MAX_WAIT,
    );
    assert_eq!(
        per_tool_call_wait_for_tool(
            GROUNDED_ANSWER_TOOL_NAME,
            Duration::from_secs(45),
            Some(soft),
            true,
        ),
        Duration::from_secs(45),
    );
    assert_eq!(
        per_tool_call_wait_for_tool(
            GROUNDED_ANSWER_TOOL_NAME,
            Duration::from_secs(20),
            Some(soft),
            true,
        ),
        Duration::from_secs(20),
    );
}

// GUARD 1 — per-tool-call timeout.
#[tokio::test]
async fn hanging_tool_future_times_out_within_budget_with_structured_error() {
    // A future that never resolves must yield a structured timeout error
    // within the (small, real-time) budget rather than hang the loop.
    let wait = Duration::from_millis(20);
    let hanging = async {
        std::future::pending::<()>().await;
        synthetic_success_outcome()
    };
    let outcome = run_tool_call_within_budget(hanging, wait, "search_documents", 3).await;
    assert!(outcome.is_error);
    assert!(!outcome.is_replay);
    let message = outcome.result_text.unwrap_or_default();
    assert!(
        message.contains("timed out") && message.contains("search_documents"),
        "expected a structured timeout error, got: {message}"
    );
    assert_eq!(
        outcome
            .result_json
            .as_ref()
            .and_then(|json| json.get("structuredContent"))
            .and_then(|structured| structured.get("errorKind"))
            .and_then(serde_json::Value::as_str),
        Some("agent_tool_timeout")
    );
    assert_eq!(
        outcome
            .result_json
            .as_ref()
            .and_then(|json| json.get("structuredContent"))
            .and_then(|structured| structured.get("retryable"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn tool_call_within_budget_returns_real_outcome() {
    // A future that resolves before the budget passes its outcome through
    // untouched.
    let wait = Duration::from_secs(5);
    let quick = async { synthetic_success_outcome() };
    let outcome = run_tool_call_within_budget(quick, wait, "search_documents", 1).await;
    assert!(!outcome.is_error);
    assert_eq!(outcome.result_text.as_deref(), Some("ok"));
}

// GUARD 1 — a timed-out call's error outcome clears the dedup fingerprint so
// a retry of the same call is not suppressed.
#[tokio::test]
async fn timed_out_tool_call_clears_dedup_fingerprint() {
    let call = grounded_tool_call("call-1", "how to configure the gateway?");
    let mut seen = BTreeMap::new();
    let mut outcomes = vec![None; 1];
    let pending = prepare_agent_tool_calls(
        std::slice::from_ref(&call),
        agent_tool_argument_context("how to configure the gateway?"),
        &mut seen,
        &mut outcomes,
    );
    let fingerprint = pending[0].fingerprint.clone().expect("fingerprint");
    let raw_args_key = raw_tool_call_argument_key(&call.arguments_json);

    // Simulate the hung call: the timeout wrapper produces an error outcome.
    let wait = Duration::from_millis(20);
    let hanging = async {
        std::future::pending::<()>().await;
        synthetic_success_outcome()
    };
    let outcome = run_tool_call_within_budget(hanging, wait, &call.name, 1).await;
    assert!(outcome.is_error);
    record_effective_tool_payload_outcome(&mut seen, fingerprint, raw_args_key, &outcome);

    // The identical call must run again, not be suppressed as a duplicate.
    let mut retry_outcomes = vec![None; 1];
    let retry_pending = prepare_agent_tool_calls(
        std::slice::from_ref(&call),
        agent_tool_argument_context("how to configure the gateway?"),
        &mut seen,
        &mut retry_outcomes,
    );
    assert_eq!(retry_pending.len(), 1);
    assert!(retry_outcomes[0].is_none(), "retry must not be suppressed after a timeout");
}

// GUARD 2 — no-progress early-stop.
#[test]
fn no_progress_iterations_force_final_after_limit() {
    let mut streak = 0usize;
    let mut force_final = false;
    // `NO_PROGRESS_ITERATION_LIMIT` consecutive error/replay-only iterations
    // route into the forced-final path.
    for _ in 0..NO_PROGRESS_ITERATION_LIMIT {
        let (next, force) = next_no_progress_state(streak, false);
        streak = next;
        force_final = force_final || force;
    }
    assert!(force_final);
    assert_eq!(streak, NO_PROGRESS_ITERATION_LIMIT);
    // The forced final answer carries the observable `NoProgress` stop
    // reason, distinct from deadline/iteration-cap.
    assert_eq!(final_answer_stop_reason(true), AgentStopReason::NoProgress);
}

#[test]
fn replayed_outcome_is_flagged_as_replay() {
    // A replay is a cache hit, not a fresh successful tool result; the
    // no-progress guard relies on `is_replay` to avoid counting it as
    // progress.
    let payload = CompletedToolPayload {
        message_content: "{}".to_string(),
        result_text: Some("cached".to_string()),
        result_json: None,
        grounding_text: None,
        grounded_answer_body: None,
        grounded_answer_outcome: None,
        grounded_answer_ready: false,
        grounded_answer_completed: false,
        grounded_answer_needs_follow_up: false,
        grounded_answer_repair_reason: None,
        grounded_answer_language: QueryLanguage::Auto,
        grounded_answer_clarification_required: false,
    };
    let outcome = replayed_tool_execution_outcome(payload);
    assert!(!outcome.is_error);
    assert!(outcome.is_replay);
}

#[test]
fn one_no_progress_iteration_below_limit_does_not_force_final() {
    // A single miss must not trip the guard when the limit is above one.
    let (streak, force_final) = next_no_progress_state(0, false);
    assert_eq!(streak, 1);
    assert_eq!(force_final, NO_PROGRESS_ITERATION_LIMIT <= 1);
}

// GUARD 2 — a successful NEW tool result resets the no-progress counter.
#[test]
fn successful_new_tool_result_resets_no_progress_counter() {
    // Build a streak just under the limit, then a progress iteration resets
    // it so the loop is given a fresh window.
    let mut streak = NO_PROGRESS_ITERATION_LIMIT.saturating_sub(1);
    let (reset_streak, force_after_progress) = next_no_progress_state(streak, true);
    assert_eq!(reset_streak, 0);
    assert!(!force_after_progress);
    streak = reset_streak;
    // After the reset, a single fresh miss must not immediately force final
    // (proving the counter restarted rather than carrying over).
    let (after_miss, force_after_miss) = next_no_progress_state(streak, false);
    assert_eq!(after_miss, 1);
    assert_eq!(force_after_miss, NO_PROGRESS_ITERATION_LIMIT <= 1);
}

#[test]
fn completed_effective_tool_payload_replays_cached_result() {
    // A duplicate of a COMPLETED call replays the stored data instead of a
    // refusal, so the agent gets the answer it asked for.
    let tool_calls = vec![grounded_tool_call("call-1", "focused subquestion")];
    let mut seen = BTreeMap::new();
    let (first_pending, _first_outcomes) = prepare_focused_turn(&tool_calls, &mut seen);
    let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
    let raw_args_key = raw_tool_call_argument_key(&tool_calls[0].arguments_json);

    // The original call completed successfully with a real answer body.
    let mut success = tool_execution_error("placeholder");
    success.is_error = false;
    success.message_content = "{\"answer\":\"edge service upgrade steps\"}".to_string();
    success.result_text = Some("edge service upgrade steps".to_string());
    success.grounded_answer_ready = true;
    success.grounded_answer_completed = true;
    record_effective_tool_payload_outcome(&mut seen, fingerprint, raw_args_key, &success);

    let (second_pending, second_outcomes) = prepare_focused_turn(&tool_calls, &mut seen);

    assert!(second_pending.is_empty());
    let replay = second_outcomes[0].as_ref().expect("replayed outcome");
    assert!(!replay.is_error);
    assert!(replay.grounded_answer_ready);
    assert!(replay.grounded_answer_completed);
    assert_eq!(replay.result_text.as_deref(), Some("edge service upgrade steps"));
    assert!(
        replay
            .result_text
            .as_deref()
            .is_some_and(|text| !text.contains("duplicate MCP tool call suppressed"))
    );
}

#[test]
fn completed_effective_tool_payload_tracking_is_turn_scoped() {
    let tool_calls = vec![grounded_tool_call("call-1", "focused subquestion")];
    let mut seen = BTreeMap::new();
    let (first_pending, first_outcomes) = prepare_focused_turn(&tool_calls, &mut seen);
    let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
    let raw_args_key = raw_tool_call_argument_key(&tool_calls[0].arguments_json);
    seen.insert(
        fingerprint,
        EffectiveToolPayloadEntry {
            raw_args_key,
            state: EffectiveToolPayloadState::Completed(Box::new(CompletedToolPayload {
                message_content: "{\"answer\":\"cached body\"}".to_string(),
                result_text: Some("cached body".to_string()),
                result_json: None,
                grounding_text: None,
                grounded_answer_body: Some("cached body".to_string()),
                grounded_answer_outcome: Some(AgentGroundedAnswerOutcome {
                    disposition: QueryAnswerDisposition::FactualReady,
                    clarification: QueryClarification::default(),
                }),
                grounded_answer_ready: true,
                grounded_answer_completed: true,
                grounded_answer_needs_follow_up: false,
                grounded_answer_repair_reason: None,
                grounded_answer_language: QueryLanguage::Auto,
                grounded_answer_clarification_required: false,
            })),
        },
    );
    let (second_pending, second_outcomes) = prepare_focused_turn(&tool_calls, &mut seen);

    // The dedup entry is turn-scoped and persists across prepare rounds, so
    // the second identical call is not re-dispatched; it replays the cached
    // successful result instead of dead-ending.
    assert_eq!(first_pending.len(), 1);
    assert!(second_pending.is_empty());
    assert!(first_outcomes[0].is_none());
    let replay = second_outcomes[0].as_ref().expect("replayed outcome");
    assert!(!replay.is_error);
    assert_eq!(replay.result_text.as_deref(), Some("cached body"));
}

#[test]
fn duplicate_effective_tool_payload_can_retry_after_actual_error() {
    let tool_calls = vec![grounded_tool_call("call-1", "focused subquestion")];
    let mut seen = BTreeMap::new();
    let (first_pending, first_outcomes) = prepare_focused_turn(&tool_calls, &mut seen);
    let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
    let raw_args_key = raw_tool_call_argument_key(&tool_calls[0].arguments_json);
    let error = tool_execution_error("upstream timeout");
    record_effective_tool_payload_outcome(&mut seen, fingerprint, raw_args_key, &error);
    let (second_pending, second_outcomes) = prepare_focused_turn(&tool_calls, &mut seen);

    assert_eq!(first_pending.len(), 1);
    assert_eq!(second_pending.len(), 1);
    assert!(first_outcomes[0].is_none());
    assert!(second_outcomes[0].is_none());
}

#[test]
fn distinct_effective_tool_payloads_remain_allowed_in_one_turn() {
    let tool_calls = vec![grounded_tool_call("call-1", "focused subquestion B")];
    let mut seen = BTreeMap::new();
    let (first_pending, first_outcomes) =
        prepare_focused_turn(&[grounded_tool_call("call-0", "focused subquestion A")], &mut seen);
    let (second_pending, second_outcomes) = prepare_focused_turn(&tool_calls, &mut seen);

    assert_eq!(first_pending.len(), 1);
    assert_eq!(second_pending.len(), 1);
    assert!(first_outcomes[0].is_none());
    assert!(second_outcomes[0].is_none());
}
