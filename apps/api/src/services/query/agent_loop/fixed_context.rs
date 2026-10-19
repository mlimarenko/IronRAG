use super::*;

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
/// Inputs for [`run_single_shot_turn`]: the library, question, transcript,
/// assembled grounded context, and the answer language the prompt renders in.
pub(crate) struct SingleShotTurnRequest<'a> {
    pub(crate) library_id: Uuid,
    pub(crate) user_question: &'a str,
    pub(crate) conversation_history: &'a [ChatMessage],
    pub(crate) grounded_context: &'a str,
    pub(crate) answer_language: QueryLanguage,
}

pub(crate) async fn run_single_shot_turn(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    request: SingleShotTurnRequest<'_>,
) -> Result<AgentTurnResult, QueryServiceError> {
    let SingleShotTurnRequest {
        library_id,
        user_question,
        conversation_history,
        grounded_context,
        answer_language,
    } = request;
    // The runtime has already performed retrieval. Model-visible
    // context is represented as the same chat transcript shape a
    // tool-using agent would see: prior messages, current user, an
    // assistant tool-call record, and the matching tool result.
    run_fixed_context_turn(
        state,
        execution_context,
        FixedContextTurnRequest {
            library_id,
            user_question,
            conversation_history,
            context: grounded_context,
            system_prompt: super::super::assistant_prompt::render_single_shot(answer_language),
            tool_name: RUNTIME_RETRIEVED_CONTEXT_TOOL,
            tool_arguments: serde_json::json!({ "question": user_question }),
            call_kind: QueryProviderCallKind::QueryAnswer,
            failure_context: "single-shot grounded-answer LLM call failed",
        },
    )
    .await
}

pub(crate) enum LiteralRevisionRequest<'a> {
    Fidelity {
        library_id: Uuid,
        user_question: &'a str,
        conversation_history: &'a [ChatMessage],
        original_answer: &'a str,
        unsupported_literals: &'a [String],
        grounded_context: &'a str,
    },
    InventoryCoverage {
        library_id: Uuid,
        user_question: &'a str,
        conversation_history: &'a [ChatMessage],
        original_answer: &'a str,
        required_literals: &'a [String],
        revision_context: &'a str,
    },
}

pub(crate) async fn run_literal_revision_turn(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    request: LiteralRevisionRequest<'_>,
) -> Result<AgentTurnResult, QueryServiceError> {
    let (
        library_id,
        user_question,
        conversation_history,
        revision_context,
        call_kind,
        system_prompt,
        tool_arguments,
        failure_context,
    ) = match request {
        LiteralRevisionRequest::Fidelity {
            library_id,
            user_question,
            conversation_history,
            original_answer,
            unsupported_literals,
            grounded_context,
        } => (
            library_id,
            user_question,
            conversation_history,
            grounded_context,
            QueryProviderCallKind::QueryAnswerLiteralRevision,
            super::super::assistant_prompt::render_literal_fidelity_revision(
                "Provided in the `ironrag_literal_revision_context` runtime tool result.",
                original_answer,
                unsupported_literals,
                None,
            ),
            serde_json::json!({
                "question": user_question,
                "unsupportedLiterals": unsupported_literals,
            }),
            "literal-fidelity revision LLM call failed",
        ),
        LiteralRevisionRequest::InventoryCoverage {
            library_id,
            user_question,
            conversation_history,
            original_answer,
            required_literals,
            revision_context,
        } => (
            library_id,
            user_question,
            conversation_history,
            revision_context,
            QueryProviderCallKind::QueryAnswerInventoryRevision,
            super::super::assistant_prompt::render_literal_inventory_coverage_revision(
                original_answer,
                required_literals,
                "Provided in the `ironrag_literal_revision_context` runtime tool result.",
            ),
            serde_json::json!({
                "question": user_question,
                "requiredLiterals": required_literals,
            }),
            "literal-inventory coverage revision LLM call failed",
        ),
    };

    run_fixed_context_turn(
        state,
        execution_context,
        FixedContextTurnRequest {
            library_id,
            user_question,
            conversation_history,
            context: revision_context,
            system_prompt,
            tool_name: RUNTIME_LITERAL_REVISION_CONTEXT_TOOL,
            tool_arguments,
            call_kind,
            failure_context,
        },
    )
    .await
}

pub(super) struct FixedContextTurnRequest<'a> {
    pub(super) library_id: Uuid,
    pub(super) user_question: &'a str,
    pub(super) conversation_history: &'a [ChatMessage],
    pub(super) context: &'a str,
    pub(super) system_prompt: String,
    pub(super) tool_name: &'static str,
    pub(super) tool_arguments: Value,
    pub(super) call_kind: QueryProviderCallKind,
    pub(super) failure_context: &'static str,
}

pub(super) async fn run_fixed_context_turn(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    request: FixedContextTurnRequest<'_>,
) -> Result<AgentTurnResult, QueryServiceError> {
    let binding = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, request.library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|error| anyhow::anyhow!("failed to resolve query_answer binding: {error}"))?
        .ok_or_else(|| missing_query_answer_binding_error(request.library_id))?;

    let (provider_call_attribution, mut provider_call_reservation) =
        reserve_attributed_provider_call(state, execution_context, &binding, request.call_kind)
            .await?;

    // Endpoints that reject tool calls inserted mid-conversation (DeepSeek
    // documents that restriction for its Chat Completions API) get the same
    // evidence as a named block inside the last user message instead of the
    // synthetic assistant tool-call pair.
    let inline_context = crate::integrations::llm::binding_request_policy(
        &binding.provider_kind,
        &binding.extra_parameters_json,
    )
    .inlines_synthetic_tool_history();

    let messages = if inline_context {
        build_inline_context_answer_messages(
            with_inline_delivery_note(request.system_prompt.clone(), request.tool_name),
            request.conversation_history,
            request.user_question,
            request.tool_name,
            request.context,
        )
    } else {
        build_runtime_tool_answer_messages(
            request.system_prompt,
            request.conversation_history,
            request.user_question,
            request.tool_name,
            request.tool_arguments,
            request.context,
        )
    };

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
    let response = match crate::observability::with_provider_call_id(
        provider_call_reservation.provider_call_id(),
        state.llm_gateway.generate_with_tools(tool_use_request),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            fail_provider_call(&mut provider_call_reservation).await;
            return Err(error.context(request.failure_context).into());
        }
    };
    let provider_call = complete_attributed_provider_call(
        &provider_call_attribution,
        &mut provider_call_reservation,
        response.usage_json.clone(),
    )
    .await?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::super::llm_context_debug::LlmIterationDebug {
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
    let provider_calls = vec![provider_call];

    Ok(AgentTurnResult {
        answer,
        answer_provenance: AgentAnswerProvenance::Composed,
        grounded_answer_outcome: None,
        usage_json: response.usage_json,
        provider_calls,
        iterations: 1,
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![debug_iteration],
        agent_loop: None,
    })
}

pub(super) fn missing_query_answer_binding_error(library_id: Uuid) -> QueryServiceError {
    QueryServiceError::BindingNotConfigured {
        message: format!("no active query_answer binding configured for library {library_id}"),
    }
}

pub(super) fn build_runtime_tool_answer_messages(
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

/// Build the fixed-context request for endpoints whose Chat Completions API
/// rejects tool calls inserted mid-conversation: the question and the evidence
/// block travel together in the last user message, named exactly as the tool
/// the instructions refer to, and no tool roles appear on the wire.
pub(super) fn build_inline_context_answer_messages(
    system_prompt: String,
    conversation_history: &[ChatMessage],
    user_question: &str,
    tool_name: &str,
    tool_result: &str,
) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(conversation_history.len().saturating_add(2));
    messages.push(ChatMessage::system(system_prompt));
    messages.extend(conversation_history.iter().cloned());
    messages.push(ChatMessage::user(format!(
        "{question}\n\n<{tool}>\n{result}\n</{tool}>",
        question = user_question.trim(),
        tool = tool_name,
        result = tool_result.trim(),
    )));
    messages
}

/// The system-prompt delivery note for the inline shape: the instructions
/// above still describe a runtime tool result, and this note tells the model
/// where that evidence actually arrived.
fn with_inline_delivery_note(mut system_prompt: String, tool_name: &str) -> String {
    system_prompt.push_str(&format!(
        "\n\n* Delivery note: this endpoint rejects tool calls inserted into the conversation, so the `{tool_name}` block appears at the end of the user's message. Read it exactly as the runtime tool result these instructions describe."
    ));
    system_prompt
}

/// Accumulate one iteration's `usage_json` into the diagnostic turn summary.
/// Billing never consumes this aggregate: it persists the typed
/// `provider_calls` ledger one response at a time. We still normalize common
/// provider protocol fields here so the debug summary remains useful.
///
/// Numbers are summed, and per-iteration counters (`iteration_count`,
/// `provider_call_count`) expose the round-trip volume separately from
/// raw tokens so an operator reading the debug snapshot can tell a single-shot
/// call apart from a multi-iteration escalation without cross-referencing
/// `debug_iterations`.
pub(crate) fn merge_usage_into(accumulator: &mut serde_json::Value, iteration: &serde_json::Value) {
    fn sum_key(
        accumulator: &mut serde_json::Map<String, serde_json::Value>,
        identity_key: &str,
        source: &serde_json::Value,
        aliases: &[&str],
    ) {
        let value =
            aliases.iter().find_map(|alias| source.get(*alias)).and_then(serde_json::Value::as_i64);
        let Some(delta) = value else {
            return;
        };
        let existing =
            accumulator.get(identity_key).and_then(serde_json::Value::as_i64).unwrap_or(0);
        accumulator.insert(identity_key.to_string(), serde_json::json!(existing + delta));
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
    let flat_cache_aliases =
        ["cached_input_tokens", "cache_read_input_tokens", "input_cached_tokens"];
    if flat_cache_aliases.iter().any(|alias| iteration.get(*alias).is_some()) {
        sum_key(obj, "cached_input_tokens", iteration, &flat_cache_aliases);
    } else {
        // Nested `{"prompt_tokens_details": {"cached_tokens": N}}` shape some
        // providers emit instead of the flat alias — same quantity, so only
        // merge it when no flat alias was present (mirrors
        // provider_usage_semantics_for_payload's mutual exclusivity).
        let nested_cached = iteration
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .or_else(|| {
                iteration
                    .get("input_tokens_details")
                    .and_then(|details| details.get("cached_tokens"))
            })
            .and_then(serde_json::Value::as_i64);
        if let Some(delta) = nested_cached {
            let existing =
                obj.get("cached_input_tokens").and_then(serde_json::Value::as_i64).unwrap_or(0);
            obj.insert("cached_input_tokens".to_string(), serde_json::json!(existing + delta));
        }
    }

    let existing_iterations =
        obj.get("iteration_count").and_then(serde_json::Value::as_i64).unwrap_or(0);
    obj.insert("iteration_count".to_string(), serde_json::json!(existing_iterations + 1));
}
