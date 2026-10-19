use super::*;

pub(super) struct AgentAnswerStageInput<'a> {
    pub(super) state: &'a AppState,
    pub(super) auth: &'a AuthContext,
    pub(super) library: &'a CatalogLibrary,
    pub(super) library_ref: &'a str,
    pub(super) content_text: &'a str,
    pub(super) conversation: &'a query_repository::QueryConversationRow,
    pub(super) conversation_context: &'a ConversationRuntimeContext,
    pub(super) request_turn: &'a query_repository::QueryTurnRow,
    pub(super) execution: &'a query_repository::QueryExecutionRow,
    pub(super) execution_context_bundle_id: Uuid,
    pub(super) provider_execution_context: QueryProviderExecutionContext,
    pub(super) prior_grounded_answer_context_messages:
        &'a [crate::integrations::llm::ChatMessage],
    pub(super) agent_conversation_history: &'a [crate::integrations::llm::ChatMessage],
    pub(super) agent_grounded_answer_tool_history: &'a [ExternalConversationTurn],
    pub(super) agent_request_id: &'a str,
    pub(super) activity_tx: Option<Sender<AgentLoopActivityEvent>>,
    pub(super) top_k: usize,
    pub(super) runtime_session: &'a mut RuntimeExecutionSession,
}

pub(super) async fn execute_agent_answer_stages(
    mut input: AgentAnswerStageInput<'_>,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    if let Err(failure) = begin_agent_answer_stage(&mut input).await {
        return *failure;
    }
    let answer_started = Utc::now();
    match run_mcp_tool_agent_turn(agent_turn_input(&input)).await {
        Ok(agent_result) => {
            complete_agent_answer_stage(&mut input, agent_result, answer_started).await
        }
        Err(agent_failure) => {
            fail_agent_answer_stage(&mut input, agent_failure, answer_started).await
        }
    }
}

fn agent_turn_input<'a>(input: &'a AgentAnswerStageInput<'a>) -> McpToolAgentTurnInput<'a> {
    McpToolAgentTurnInput {
        state: input.state,
        execution_context: input.provider_execution_context,
        auth: input.auth,
        library_id: input.library.id,
        library_ref: input.library_ref,
        user_question: input.content_text,
        contextual_follow_up: input.conversation_context.has_prior_conversation,
        conversation_history: input.agent_conversation_history,
        follow_up_context_messages: input.prior_grounded_answer_context_messages,
        grounded_answer_tool_history: input.agent_grounded_answer_tool_history,
        request_id: input.agent_request_id,
        grounded_answer_top_k: input.top_k,
        iteration_cap: ui_agent_iteration_cap(),
        max_parallel_actions: usize::from(QueryAnswerTask::spec().max_parallel_actions),
        deadline: Duration::from_millis(ASSISTANT_AGENT_LOOP_DEADLINE_MS),
        soft_final_answer_deadline: Some(Duration::from_millis(
            ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS,
        )),
        activity_tx: input.activity_tx.clone(),
    }
}

// The `Err` variant is itself the terminal outcome; boxing it here (rather
// than in the generic `RuntimeTerminalOutcome` used across the runtime layer)
// keeps `clippy::result_large_err` quiet without changing the serialized
// `utoipa::ToSchema` shape of that protocol type.
async fn begin_agent_answer_stage(
    input: &mut AgentAnswerStageInput<'_>,
) -> Result<(), Box<RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>>> {
    let Err(failure) = begin_query_runtime_stage(
        input.state.agent_runtime.executor(),
        input.runtime_session,
        RuntimeStageKind::Answer,
    )
    .await
    else {
        return Ok(());
    };
    record_query_runtime_stage(
        input.state.agent_runtime.executor(),
        input.runtime_session,
        RuntimeStageKind::Answer,
        false,
        Some(&failure),
        None,
    );
    Err(Box::new(make_query_terminal_failure_outcome(failure)))
}

async fn complete_agent_answer_stage(
    input: &mut AgentAnswerStageInput<'_>,
    mut agent_result: AgentTurnResult,
    answer_started: chrono::DateTime<Utc>,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    record_query_runtime_stage(
        input.state.agent_runtime.executor(),
        input.runtime_session,
        RuntimeStageKind::Answer,
        false,
        None,
        Some(answer_started),
    );
    persist_agent_debug_snapshot(input, &agent_result, false).await;
    let verification = verify_agent_result(input, &mut agent_result).await;
    if verification.revised_answer {
        persist_agent_debug_snapshot(input, &agent_result, true).await;
    }
    if let Some(outcome) = verification.outcome {
        return outcome;
    }
    persist_verified_agent_result(input, agent_result).await
}

struct AgentVerificationStageResult {
    outcome: Option<RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>>,
    revised_answer: bool,
}

async fn verify_agent_result(
    input: &mut AgentAnswerStageInput<'_>,
    agent_result: &mut AgentTurnResult,
) -> AgentVerificationStageResult {
    let Err(begin_failure) = begin_query_runtime_stage(
        input.state.agent_runtime.executor(),
        input.runtime_session,
        RuntimeStageKind::Verify,
    )
    .await
    else {
        return run_agent_evidence_verification(input, agent_result).await;
    };
    let failure = begin_failure.with_provider_calls(agent_result.provider_calls.clone());
    record_query_runtime_stage(
        input.state.agent_runtime.executor(),
        input.runtime_session,
        RuntimeStageKind::Verify,
        false,
        Some(&failure),
        None,
    );
    AgentVerificationStageResult {
        outcome: Some(make_query_terminal_failure_outcome(failure)),
        revised_answer: false,
    }
}

async fn run_agent_evidence_verification(
    input: &mut AgentAnswerStageInput<'_>,
    agent_result: &mut AgentTurnResult,
) -> AgentVerificationStageResult {
    let verify_started = Utc::now();
    let result = verify_agent_evidence(input, agent_result).await;
    match result {
        Ok(revised_answer) => {
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                input.runtime_session,
                RuntimeStageKind::Verify,
                false,
                None,
                Some(verify_started),
            );
            AgentVerificationStageResult { outcome: None, revised_answer }
        }
        Err(failure) => {
            let failure = failure.with_provider_calls(agent_result.provider_calls.clone());
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                input.runtime_session,
                RuntimeStageKind::Verify,
                false,
                Some(&failure),
                Some(verify_started),
            );
            AgentVerificationStageResult {
                outcome: Some(make_query_terminal_failure_outcome(failure)),
                revised_answer: false,
            }
        }
    }
}

async fn verify_agent_evidence(
    input: &AgentAnswerStageInput<'_>,
    agent_result: &mut AgentTurnResult,
) -> Result<bool, QueryAnswerTaskFailure> {
    let child_execution_ids = agent_result.child_query_execution_ids.clone();
    if agent_result.answer_provenance == AgentAnswerProvenance::GroundedAnswerPassthrough {
        let child_answer_kind =
            system_statement_kind_from_child_executions(input.state, &child_execution_ids)
                .await
                .map_err(|error| {
                    make_query_answer_failure(
                        "query_agent_verify_failed",
                        format!(
                            "failed to resolve the child system-statement kind for the UI agent answer: {error}"
                        ),
                    )
                })?;
        if is_system_statement_passthrough(agent_result.answer_provenance, child_answer_kind) {
            // The parent body is the child's deterministic system statement
            // about the library, not a corpus-grounded claim: parent
            // verification neither applies nor may attach warnings or
            // retrieval sources. Persist it neutral with the child's answer
            // kind and skip model revision.
            finalize_agent_system_statement(
                input.state,
                input.execution,
                input.execution_context_bundle_id,
                &agent_result.assistant_grounding,
                AnswerKind::MissingExplicitDocument,
            )
            .await
            .map_err(|error| {
                make_query_answer_failure(
                    "query_agent_verify_failed",
                    format!("failed to finalize the UI agent system statement: {error}"),
                )
            })?;
            return Ok(false);
        }
    }
    let has_evidence =
        agent_has_verifiable_tool_evidence(&child_execution_ids, &agent_result.assistant_grounding);
    ensure_agent_grounding_available(input, agent_result, &child_execution_ids, has_evidence)
        .await?;
    if !agent_answer_needs_parent_verification(agent_result, has_evidence) {
        return Ok(false);
    }
    let verification = verify_agent_answer_against_tool_evidence(AgentAnswerVerificationRequest {
        state: input.state,
        execution: input.execution,
        context_bundle_id: input.execution_context_bundle_id,
        answer_text: &agent_result.answer,
        assistant_grounding: &agent_result.assistant_grounding,
        answer_provenance: agent_result.answer_provenance,
        grounded_answer_outcome: agent_result.grounded_answer_outcome.as_ref(),
    })
    .await
    .map_err(|error| {
        make_query_answer_failure(
            "query_agent_verify_failed",
            format!("failed to verify UI agent answer against MCP tool evidence: {error}"),
        )
    })?;
    let fidelity_revised = apply_agent_fidelity_revision(input, agent_result, &verification).await;
    let inventory_revised = apply_agent_inventory_revision(input, agent_result).await;
    Ok(fidelity_revised || inventory_revised)
}

fn agent_answer_needs_parent_verification(
    agent_result: &AgentTurnResult,
    has_verifiable_tool_evidence: bool,
) -> bool {
    agent_answer_requires_parent_tool_evidence_verification(has_verifiable_tool_evidence)
        || matches!(
            agent_result.answer_provenance,
            AgentAnswerProvenance::GroundedAnswerPassthrough
        )
}

async fn ensure_agent_grounding_available(
    input: &AgentAnswerStageInput<'_>,
    agent_result: &AgentTurnResult,
    child_execution_ids: &[Uuid],
    has_verifiable_tool_evidence: bool,
) -> Result<(), QueryAnswerTaskFailure> {
    if !has_verifiable_tool_evidence {
        return mark_agent_answer_unverifiable(input, agent_result).await;
    }
    if child_execution_ids.is_empty() {
        return Ok(());
    }
    match materialize_agent_grounding_from_child_execution(
        input.state,
        input.execution,
        input.execution_context_bundle_id,
        child_execution_ids,
    )
    .await
    {
        Ok(Some(materialized)) => {
            tracing::info!(
                execution_id = %input.execution.id,
                source_execution_id = %materialized.source_execution_id,
                primary_execution_id = %materialized.primary_execution_id,
                "attached child grounded-answer evidence to UI agent execution"
            );
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => Err(make_query_answer_failure(
            "query_agent_grounding_failed",
            format!("failed to attach MCP tool evidence to UI agent execution: {error}"),
        )),
    }
}

async fn mark_agent_answer_unverifiable(
    input: &AgentAnswerStageInput<'_>,
    agent_result: &AgentTurnResult,
) -> Result<(), QueryAnswerTaskFailure> {
    let called_any_tool =
        agent_result.agent_loop.as_ref().is_some_and(|metadata| metadata.tool_call_count > 0);
    let warnings = if called_any_tool {
        no_verifiable_tool_evidence_warnings()
    } else {
        no_agent_tool_evidence_warnings()
    };
    ensure_agent_tool_context_bundle(
        input.state,
        input.execution,
        input.execution_context_bundle_id,
        &agent_result.assistant_grounding,
        QueryVerificationState::InsufficientEvidence,
        warnings,
    )
    .await
    .map_err(|error| {
        make_query_answer_failure(
            "query_agent_verify_failed",
            format!("failed to mark UI agent answer as unverifiable: {error}"),
        )
    })
}

async fn apply_agent_fidelity_revision(
    input: &AgentAnswerStageInput<'_>,
    agent_result: &mut AgentTurnResult,
    verification: &RuntimeAnswerVerification,
) -> bool {
    if !agent_answer_allows_parent_model_revision(agent_result.answer_provenance)
        || !agent_verification_needs_literal_revision(verification)
    {
        return false;
    }
    let revision_targets =
        literal_revision_targets(&agent_result.answer, &verification.unsupported_literals);
    if revision_targets.is_empty() {
        return false;
    }
    let prompt_context = agent_result.assistant_grounding.verification_corpus.join("\n\n");
    let revision = match run_literal_revision_turn(
        input.state,
        input.provider_execution_context,
        LiteralRevisionRequest::Fidelity {
            library_id: input.library.id,
            user_question: input.content_text,
            conversation_history: input.agent_conversation_history,
            original_answer: &agent_result.answer,
            unsupported_literals: &revision_targets,
            grounded_context: &prompt_context,
        },
    )
    .await
    {
        Ok(revision) => revision,
        Err(error) => {
            warn!(
                error = %error,
                execution_id = %input.execution.id,
                "literal-fidelity revision failed for UI agent answer"
            );
            return false;
        }
    };
    if fidelity_revision_drops_history_literals(input, agent_result, &revision.answer) {
        agent_result.provider_calls.extend(revision.provider_calls);
        return false;
    }
    verify_and_accept_agent_revision(input, agent_result, revision, "literal-fidelity").await
}

fn fidelity_revision_drops_history_literals(
    input: &AgentAnswerStageInput<'_>,
    agent_result: &AgentTurnResult,
    revised_answer: &str,
) -> bool {
    let Some((before, after, total)) = literal_revision_history_literal_coverage(
        &agent_result.answer,
        revised_answer,
        input.agent_grounded_answer_tool_history,
    )
    .filter(|(before, after, _)| after < before) else {
        return false;
    };
    tracing::info!(
        execution_id = %input.execution.id,
        history_literal_count = total,
        draft_visible_literals = before,
        revised_visible_literals = after,
        "rejected literal-fidelity revision that dropped prior literal anchors"
    );
    true
}

fn log_inventory_revision_failure<E: std::fmt::Display>(error: &E, execution_id: &Uuid) {
    tracing::warn!(
        stage = "literal_inventory_coverage_revision",
        error = %error,
        execution_id = %execution_id,
        "literal-inventory coverage revision failed for UI agent answer"
    );
}

fn log_inventory_revision_timeout(execution_id: &Uuid, elapsed_ms: u128) {
    tracing::warn!(
        stage = "literal_inventory_coverage_revision",
        execution_id = %execution_id,
        timeout_ms = ASSISTANT_LITERAL_INVENTORY_REVISION_TIMEOUT.as_millis(),
        elapsed_ms,
        "literal-inventory coverage revision timed out for UI agent answer"
    );
}

fn log_inventory_revision_rejected(
    execution_id: &Uuid,
    required_literal_count: usize,
    elapsed_ms: u128,
) {
    tracing::info!(
        stage = "literal_inventory_coverage_revision",
        execution_id = %execution_id,
        required_literal_count,
        elapsed_ms,
        "rejected literal-inventory coverage revision missing required anchors"
    );
}

fn log_accepted_revision(execution_id: &Uuid, revised_answer_chars: usize, revision_kind: &str) {
    tracing::info!(
        execution_id = %execution_id,
        revised_answer_chars,
        revision_kind,
        "accepted revised UI agent answer"
    );
}

fn log_unverified_revision(
    execution_id: &Uuid,
    state: QueryVerificationState,
    revised_warnings: usize,
    revision_kind: &str,
) {
    tracing::info!(
        execution_id = %execution_id,
        revised_state = ?state,
        revised_warnings,
        revision_kind,
        "revised UI agent answer was not verified"
    );
}

fn log_revision_verification_failure(
    execution_id: &Uuid,
    error: &anyhow::Error,
    revision_kind: &str,
) {
    tracing::warn!(
        error = %error,
        execution_id = %execution_id,
        revision_kind,
        "failed to verify revised UI agent answer"
    );
}

async fn apply_agent_inventory_revision(
    input: &AgentAnswerStageInput<'_>,
    agent_result: &mut AgentTurnResult,
) -> bool {
    if !agent_answer_allows_parent_model_revision(agent_result.answer_provenance) {
        return false;
    }
    let targets = literal_inventory_coverage_revision_targets(
        &agent_result.answer,
        input.agent_grounded_answer_tool_history,
        &agent_result.assistant_grounding,
    );
    if targets.is_empty() {
        return false;
    }
    let revision_context = literal_inventory_revision_context(&agent_result.assistant_grounding);
    let revision_started = Instant::now();
    let revision = tokio::time::timeout(
        ASSISTANT_LITERAL_INVENTORY_REVISION_TIMEOUT,
        run_literal_revision_turn(
            input.state,
            input.provider_execution_context,
            LiteralRevisionRequest::InventoryCoverage {
                library_id: input.library.id,
                user_question: input.content_text,
                conversation_history: input.agent_conversation_history,
                original_answer: &agent_result.answer,
                required_literals: &targets,
                revision_context: &revision_context,
            },
        ),
    )
    .await;
    let revision = match revision {
        Ok(Ok(revision)) => revision,
        Ok(Err(error)) => {
            log_inventory_revision_failure(&error, &input.execution.id);
            return false;
        }
        Err(_) => {
            log_inventory_revision_timeout(
                &input.execution.id,
                revision_started.elapsed().as_millis(),
            );
            return false;
        }
    };
    if !literal_revision_covers_required_literals(&revision.answer, &targets) {
        agent_result.provider_calls.extend(revision.provider_calls);
        log_inventory_revision_rejected(
            &input.execution.id,
            targets.len(),
            revision_started.elapsed().as_millis(),
        );
        return false;
    }
    verify_and_accept_agent_revision(input, agent_result, revision, "literal-inventory coverage")
        .await
}

async fn verify_and_accept_agent_revision(
    input: &AgentAnswerStageInput<'_>,
    agent_result: &mut AgentTurnResult,
    mut revision: AgentTurnResult,
    revision_kind: &str,
) -> bool {
    agent_result.provider_calls.append(&mut revision.provider_calls);
    let verification = verify_agent_answer_against_tool_evidence(AgentAnswerVerificationRequest {
        state: input.state,
        execution: input.execution,
        context_bundle_id: input.execution_context_bundle_id,
        answer_text: &revision.answer,
        assistant_grounding: &agent_result.assistant_grounding,
        answer_provenance: agent_result.answer_provenance,
        grounded_answer_outcome: agent_result.grounded_answer_outcome.as_ref(),
    })
    .await;
    match verification {
        Ok(verification) if verification.state == QueryVerificationState::Verified => {
            log_accepted_revision(
                &input.execution.id,
                revision.answer.chars().count(),
                revision_kind,
            );
            crate::services::query::agent_loop::merge_usage_into(
                &mut agent_result.usage_json,
                &revision.usage_json,
            );
            agent_result.debug_iterations.extend(revision.debug_iterations);
            agent_result.answer = revision.answer;
            true
        }
        Ok(verification) => {
            log_unverified_revision(
                &input.execution.id,
                verification.state,
                verification.warnings.len(),
                revision_kind,
            );
            false
        }
        Err(error) => {
            log_revision_verification_failure(&input.execution.id, &error, revision_kind);
            false
        }
    }
}

async fn persist_verified_agent_result(
    input: &mut AgentAnswerStageInput<'_>,
    agent_result: AgentTurnResult,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    let Err(begin_failure) = begin_query_runtime_stage(
        input.state.agent_runtime.executor(),
        input.runtime_session,
        RuntimeStageKind::Persist,
    )
    .await
    else {
        return persist_agent_result_body(input, agent_result).await;
    };
    let failure = begin_failure.with_provider_calls(agent_result.provider_calls);
    record_query_runtime_stage(
        input.state.agent_runtime.executor(),
        input.runtime_session,
        RuntimeStageKind::Persist,
        true,
        Some(&failure),
        None,
    );
    make_query_terminal_failure_outcome(failure)
}

async fn persist_agent_result_body(
    input: &mut AgentAnswerStageInput<'_>,
    agent_result: AgentTurnResult,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    let persist_started = Utc::now();
    match persist_agent_answer(
        input.state,
        input.conversation.id,
        input.execution.id,
        input.request_turn.id,
        &agent_result.answer,
    )
    .await
    {
        Ok(answer_text) => {
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                input.runtime_session,
                RuntimeStageKind::Persist,
                true,
                None,
                Some(persist_started),
            );
            RuntimeTerminalOutcome::Completed {
                success: QueryAnswerTaskSuccess {
                    answer_text,
                    provider_calls: agent_result.provider_calls,
                },
            }
        }
        Err(failure) => {
            let failure = failure.with_provider_calls(agent_result.provider_calls);
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                input.runtime_session,
                RuntimeStageKind::Persist,
                true,
                Some(&failure),
                Some(persist_started),
            );
            make_query_terminal_failure_outcome(failure)
        }
    }
}

/// Writes this turn's debug snapshot row.
///
/// Called twice for one `execution_id` when verification revises the answer,
/// and each write replaces the row whole. Every field must therefore be derived
/// from `agent_result` at write time, as it is here, so the later write is a
/// superset of the earlier one. `query_ir` and `spans` are empty because the
/// parent agent turn compiles no IR and records no retrieval spans — those
/// belong to the child `grounded_answer` executions, which own their own rows.
async fn persist_agent_debug_snapshot(
    input: &AgentAnswerStageInput<'_>,
    agent_result: &AgentTurnResult,
    revised: bool,
) {
    if let Err(error) = crate::services::query::llm_context_debug::upsert_snapshot(
        &input.state.persistence.postgres,
        &crate::services::query::llm_context_debug::LlmContextSnapshot {
            execution_id: input.execution.id,
            library_id: input.library.id,
            question: input.content_text.to_string(),
            iterations: agent_result.debug_iterations.clone(),
            total_iterations: agent_result.debug_iterations.len(),
            final_answer: Some(agent_result.answer.clone()),
            captured_at: Utc::now(),
            query_ir: None,
            agent_loop: agent_result.agent_loop.clone(),
            spans: Vec::new(),
        },
    )
    .await
    {
        warn!(error = %error, execution_id = %input.execution.id, revised, "failed to persist UI agent LLM context snapshot");
    }
}

async fn fail_agent_answer_stage(
    input: &mut AgentAnswerStageInput<'_>,
    agent_failure: AgentTurnFailure,
    answer_started: chrono::DateTime<Utc>,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    persist_failed_agent_debug_snapshot(
        input.state,
        input.execution.id,
        input.library.id,
        input.content_text,
        &agent_failure,
    )
    .await;
    let failure = make_query_answer_failure(
        query_service_failure_code(&agent_failure.error, "query_agent_loop_failed"),
        agent_failure.to_string(),
    )
    .with_provider_calls(agent_failure.provider_calls);
    record_query_runtime_stage(
        input.state.agent_runtime.executor(),
        input.runtime_session,
        RuntimeStageKind::Answer,
        false,
        Some(&failure),
        Some(answer_started),
    );
    make_query_terminal_failure_outcome(failure)
}
