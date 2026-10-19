use super::*;

/// Per-iteration dispatch inputs for [`execute_tool_calls`]: the model's tool
/// calls plus the budget, scope and dedup state for this batch.
pub(super) struct AgentToolDispatch<'a> {
    pub(super) iteration: usize,
    pub(super) tool_calls: &'a [ChatToolCall],
    pub(super) max_parallel_actions: usize,
    pub(super) deadline_started: Instant,
    pub(super) focused_grounded_follow_up: bool,
    pub(super) allowed_tool_names: &'a BTreeSet<String>,
}

pub(super) async fn execute_tool_calls(
    input: McpToolAgentTurnInput<'_>,
    dispatch: AgentToolDispatch<'_>,
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadEntry>,
) -> Vec<ToolExecutionOutcome> {
    let AgentToolDispatch {
        iteration,
        tool_calls,
        max_parallel_actions,
        deadline_started,
        focused_grounded_follow_up,
        allowed_tool_names,
    } = dispatch;
    let mut outcomes: Vec<Option<ToolExecutionOutcome>> = vec![None; tool_calls.len()];
    let pending_calls = prepare_agent_tool_calls(
        tool_calls,
        AgentToolArgumentContext {
            user_question: input.user_question,
            contextual_follow_up: input.contextual_follow_up,
            grounded_top_k: input.grounded_answer_top_k,
            library_ref: input.library_ref,
            grounded_answer_tool_history: input.grounded_answer_tool_history,
        },
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
                    Some(remaining) => {
                        // Bound the per-tool-call wait by the smaller of the
                        // remaining turn deadline and the configured per-tool-call
                        // max, so one hung tool future can never run past every
                        // budget. On timeout produce a structured error outcome;
                        // the error path clears the dedup fingerprint so a retry
                        // of the same call is not suppressed.
                        let per_call_wait = per_tool_call_wait_for_tool(
                            &pending.call.name,
                            remaining,
                            input.soft_final_answer_deadline,
                            focused_grounded_follow_up,
                        );
                        Box::pin(run_tool_call_within_budget(
                            execute_one_tool_call(&input, &pending.call, allowed_tool_names),
                            per_call_wait,
                            &pending.call.name,
                            iteration,
                        ))
                        .await
                    }
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
                let raw_args_key = raw_tool_call_argument_key(&pending.call.arguments_json);
                (pending.index, pending.fingerprint, raw_args_key, outcome)
            }
        })
        .buffer_unordered(max_parallel_actions)
        .collect::<Vec<_>>()
        .await;

    for (pending_index, fingerprint, raw_args_key, outcome) in pending_results {
        if let Some(fingerprint) = fingerprint {
            record_effective_tool_payload_outcome(
                seen_effective_payloads,
                fingerprint,
                raw_args_key,
                &outcome,
            );
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

/// Replay payload cached for a successfully completed tool call so a later
/// effective-duplicate of the same call replays the real data instead of
/// dead-ending the agent with a refusal message.
#[derive(Debug, Clone)]
pub(super) struct CompletedToolPayload {
    pub(super) message_content: String,
    pub(super) result_text: Option<String>,
    pub(super) result_json: Option<Value>,
    pub(super) grounding_text: Option<String>,
    pub(super) grounded_answer_body: Option<String>,
    pub(super) grounded_answer_outcome: Option<AgentGroundedAnswerOutcome>,
    pub(super) grounded_answer_ready: bool,
    pub(super) grounded_answer_completed: bool,
    pub(super) grounded_answer_needs_follow_up: bool,
    pub(super) grounded_answer_repair_reason: Option<GroundedAnswerRepairReason>,
    pub(super) grounded_answer_language: QueryLanguage,
    pub(super) grounded_answer_clarification_required: bool,
}

#[derive(Debug, Clone)]
pub(super) enum EffectiveToolPayloadState {
    InFlight,
    Completed(Box<CompletedToolPayload>),
}

/// One entry in the cross-iteration dedup map keyed by the *effective*
/// (post-normalization) fingerprint. `raw_args_key` is the stable hash of
/// the *raw* arguments the model actually sent, so two distinct raw queries
/// that normalize to the same effective fingerprint are not confused for each
/// other: only a genuine raw duplicate is suppressed, while a distinct raw call
/// replays a prior successful result (or runs on its own when none exists yet).
#[derive(Debug, Clone)]
pub(super) struct EffectiveToolPayloadEntry {
    pub(super) raw_args_key: String,
    pub(super) state: EffectiveToolPayloadState,
}

/// How a pending call is dispatched after dedup classification.
pub(super) enum PreparedToolCallDisposition {
    /// Execute the call; record its outcome under `fingerprint` afterwards.
    Execute,
    /// Suppress as a genuine same-raw in-flight duplicate.
    SuppressDuplicate,
    /// Replay a prior successful call's cached result instead of executing.
    Replay(Box<CompletedToolPayload>),
}

#[derive(Debug, Clone)]
pub(super) struct PendingAgentToolCall {
    pub(super) index: usize,
    pub(super) call: ChatToolCall,
    pub(super) fingerprint: Option<String>,
}

pub(super) fn prepare_agent_tool_calls(
    tool_calls: &[ChatToolCall],
    context: AgentToolArgumentContext<'_>,
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadEntry>,
    outcomes: &mut [Option<ToolExecutionOutcome>],
) -> Vec<PendingAgentToolCall> {
    tool_calls
        .iter()
        .cloned()
        .enumerate()
        .filter_map(|(pending_index, call)| {
            let fingerprint = effective_tool_call_fingerprint_with_context(
                &call.name,
                &call.arguments_json,
                context,
            );
            if let Some(fingerprint) = &fingerprint {
                let raw_args_key = raw_tool_call_argument_key(&call.arguments_json);
                match classify_effective_tool_call(
                    seen_effective_payloads,
                    fingerprint,
                    &raw_args_key,
                ) {
                    PreparedToolCallDisposition::SuppressDuplicate => {
                        outcomes[pending_index] =
                            Some(tool_execution_error(duplicate_tool_call_message(&call.name)));
                        return None;
                    }
                    PreparedToolCallDisposition::Replay(payload) => {
                        outcomes[pending_index] = Some(replayed_tool_execution_outcome(*payload));
                        return None;
                    }
                    PreparedToolCallDisposition::Execute => {
                        seen_effective_payloads.insert(
                            fingerprint.clone(),
                            EffectiveToolPayloadEntry {
                                raw_args_key,
                                state: EffectiveToolPayloadState::InFlight,
                            },
                        );
                    }
                }
            }
            Some(PendingAgentToolCall { index: pending_index, call, fingerprint })
        })
        .collect::<Vec<_>>()
}

/// Decide how a pending tool call is dispatched given the dedup map.
///
/// The dedup map exists to curb genuine same-arguments spam, not to dead-end a
/// distinct raw call that merely *normalizes* to the same effective fingerprint
/// as an earlier call. Therefore:
/// - a genuine same-raw in-flight duplicate is suppressed (true spam);
/// - a same-raw completed duplicate replays the cached successful result;
/// - a distinct-raw call replays a prior *completed* result (effective args are
///   identical, so the answer is the same), or, when the colliding entry is
///   still in-flight (same iteration batch, no result yet), is allowed to run so
///   the model's distinct intent is never dead-ended.
pub(super) fn classify_effective_tool_call(
    seen_effective_payloads: &BTreeMap<String, EffectiveToolPayloadEntry>,
    fingerprint: &str,
    raw_args_key: &str,
) -> PreparedToolCallDisposition {
    let Some(entry) = seen_effective_payloads.get(fingerprint) else {
        return PreparedToolCallDisposition::Execute;
    };
    match &entry.state {
        EffectiveToolPayloadState::Completed(payload) => {
            PreparedToolCallDisposition::Replay(payload.clone())
        }
        EffectiveToolPayloadState::InFlight => {
            if entry.raw_args_key == raw_args_key {
                PreparedToolCallDisposition::SuppressDuplicate
            } else {
                PreparedToolCallDisposition::Execute
            }
        }
    }
}

pub(super) fn record_effective_tool_payload_outcome(
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadEntry>,
    fingerprint: String,
    raw_args_key: String,
    outcome: &ToolExecutionOutcome,
) {
    if outcome.is_error {
        seen_effective_payloads.remove(&fingerprint);
        return;
    }
    seen_effective_payloads.insert(
        fingerprint,
        EffectiveToolPayloadEntry {
            raw_args_key,
            state: EffectiveToolPayloadState::Completed(Box::new(CompletedToolPayload {
                message_content: outcome.message_content.clone(),
                result_text: outcome.result_text.clone(),
                result_json: outcome.result_json.clone(),
                grounding_text: outcome.grounding_text.clone(),
                grounded_answer_body: outcome.grounded_answer_body.clone(),
                grounded_answer_outcome: outcome.grounded_answer_outcome.clone(),
                grounded_answer_ready: outcome.grounded_answer_ready,
                grounded_answer_completed: outcome.grounded_answer_completed,
                grounded_answer_needs_follow_up: outcome.grounded_answer_needs_follow_up,
                grounded_answer_repair_reason: outcome.grounded_answer_repair_reason,
                grounded_answer_language: outcome.grounded_answer_language,
                grounded_answer_clarification_required: outcome
                    .grounded_answer_clarification_required,
            })),
        },
    );
}

/// Stable hash of the *raw* (pre-normalization) tool-call arguments, used to
/// tell genuine same-arguments duplicates apart from distinct raw calls that
/// happen to normalize to the same effective fingerprint. Unparseable JSON
/// falls back to the verbatim string so it still keys deterministically.
pub(super) fn raw_tool_call_argument_key(arguments_json: &str) -> String {
    match serde_json::from_str::<Value>(arguments_json) {
        Ok(value) => serde_json::to_string(&normalized_json_value(&value))
            .unwrap_or_else(|_| arguments_json.to_string()),
        Err(_) => arguments_json.to_string(),
    }
}

/// Rebuild a successful tool-execution outcome from a cached replay payload so a
/// later effective-duplicate call receives the same data the original produced
/// instead of a refusal. Replays carry no child execution ids: the authoritative result's
/// child trace belongs to the original call, not the replay.
pub(super) fn replayed_tool_execution_outcome(
    payload: CompletedToolPayload,
) -> ToolExecutionOutcome {
    ToolExecutionOutcome {
        arguments_json: None,
        requested_arguments_json: None,
        message_content: payload.message_content,
        result_text: payload.result_text,
        result_json: payload.result_json,
        grounding_text: payload.grounding_text,
        grounded_answer_body: payload.grounded_answer_body,
        grounded_answer_outcome: payload.grounded_answer_outcome,
        grounded_answer_ready: payload.grounded_answer_ready,
        grounded_answer_completed: payload.grounded_answer_completed,
        grounded_answer_needs_follow_up: payload.grounded_answer_needs_follow_up,
        grounded_answer_repair_reason: payload.grounded_answer_repair_reason,
        grounded_answer_language: payload.grounded_answer_language,
        grounded_answer_clarification_required: payload.grounded_answer_clarification_required,
        is_error: false,
        is_replay: true,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    }
}

pub(super) fn duplicate_tool_call_message(tool_name: &str) -> String {
    format!(
        "duplicate MCP tool call suppressed: `{tool_name}` already used the same effective arguments in this model iteration; vary the sub-question, change arguments, or choose another MCP tool"
    )
}

pub(super) fn emit_activity(
    sender: &Option<Sender<AgentLoopActivityEvent>>,
    event: AgentLoopActivityEvent,
) {
    if let Some(sender) = sender {
        // Diagnostic activity must not back-pressure tool execution; the
        // authoritative transcript is still persisted in debug_iterations.
        let _ = sender.try_send(event);
    }
}

pub(super) async fn execute_one_tool_call(
    input: &McpToolAgentTurnInput<'_>,
    call: &ChatToolCall,
    allowed_tool_names: &BTreeSet<String>,
) -> ToolExecutionOutcome {
    if let Err(message) = validate_ui_agent_tool_allowed(&call.name, allowed_tool_names) {
        return tool_execution_error(message);
    }
    let mut arguments = match serde_json::from_str::<Value>(&call.arguments_json) {
        Ok(arguments) => arguments,
        Err(error) => {
            return invalid_mcp_tool_call_outcome(format!("invalid tool arguments JSON: {error}"));
        }
    };
    let requested_arguments = arguments.clone();
    normalize_agent_tool_argument_types(&call.name, &mut arguments);
    if let Err(message) =
        validate_agent_tool_library_scope(&call.name, &arguments, input.library_ref)
    {
        return tool_execution_error(message);
    }
    apply_agent_tool_argument_defaults_with_context(
        &call.name,
        &mut arguments,
        AgentToolArgumentContext {
            user_question: input.user_question,
            contextual_follow_up: input.contextual_follow_up,
            grounded_top_k: input.grounded_answer_top_k,
            library_ref: input.library_ref,
            grounded_answer_tool_history: input.grounded_answer_tool_history,
        },
    );

    let context = ToolCallContext {
        auth: input.auth,
        state: input.state,
        request_id: input.request_id,
        surface_kind: RuntimeSurfaceKind::Ui,
    };
    let Some(result) = Box::pin(tools::call_named_tool(&call.name, context, &arguments)).await
    else {
        return tool_execution_error(format!("unsupported MCP answer tool '{}'", call.name));
    };

    let result_text = tool_result_answer_text(&call.name, &result.content);
    let child_query_execution_ids =
        extract_child_query_execution_ids(&call.name, &result.structured_content);
    let child_runtime_execution_ids =
        extract_child_runtime_execution_ids(&call.name, &result.structured_content);
    let is_error = result.is_error;
    let message_content = tool_result_model_message(&call.name, &result);
    let grounding_text = tool_result_verification_text(&call.name, &result);
    let grounded_answer_body = grounded_answer_verbatim_body(&call.name, &result);
    let grounded_answer_outcome = grounded_answer_typed_outcome(&call.name, &result);
    let grounded_answer_repair_reason =
        grounded_answer_repair_reason(&call.name, input.user_question, &result);
    let grounded_answer_ready =
        grounded_answer_ready_for_question(&call.name, input.user_question, &result);
    let grounded_answer_completed = grounded_answer_completed(&call.name, &result);
    let grounded_answer_needs_follow_up = grounded_answer_repair_reason.is_some()
        || grounded_answer_needs_follow_up(&call.name, &result);
    let grounded_answer_language = grounded_answer_query_language(&call.name, &result);
    let grounded_answer_clarification_required =
        grounded_answer_outcome.as_ref().is_some_and(|outcome| {
            matches!(outcome.disposition, QueryAnswerDisposition::Clarification)
        });
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
        grounded_answer_body,
        grounded_answer_outcome,
        grounded_answer_ready,
        grounded_answer_completed,
        grounded_answer_needs_follow_up,
        grounded_answer_repair_reason,
        grounded_answer_language,
        grounded_answer_clarification_required,
        is_error,
        is_replay: false,
        duration_ms: 0,
        child_query_execution_ids,
        child_runtime_execution_ids,
    }
}
