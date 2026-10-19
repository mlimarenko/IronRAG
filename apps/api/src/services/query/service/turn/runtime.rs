use super::*;

/// Shared finalization context for the three turn surfaces.
///
/// Every surface finishes the same way: it finalizes the runtime session,
/// persists the result, updates the async operation and releases the
/// interruption guard. Bundling the invariant inputs keeps each `finalize_*`
/// call to its genuinely varying arguments.
pub(super) struct QueryFinalization<'a> {
    pub(super) state: &'a AppState,
    pub(super) command: &'a ExecuteConversationTurnCommand,
    pub(super) conversation: &'a query_repository::QueryConversationRow,
    pub(super) execution: &'a query_repository::QueryExecutionRow,
    pub(super) async_operation_id: Uuid,
    pub(super) interruption_guard: &'a mut QueryExecutionInterruptionGuard,
}

pub(super) async fn finalize_grounded_retrieve_failure(
    finalization: QueryFinalization<'_>,
    runtime_session: RuntimeExecutionSession,
    failure: QueryAnswerTaskFailure,
) -> Result<ApiError, ApiError> {
    let QueryFinalization {
        state,
        command,
        conversation,
        execution,
        async_operation_id,
        interruption_guard,
    } = finalization;
    let outcome = make_query_terminal_failure_outcome(failure.clone());
    let runtime_result = state
        .agent_runtime
        .executor()
        .finalize_session::<QueryAnswerTask>(runtime_session, outcome)
        .await;
    runtime_persistence::persist_runtime_result(
        &state.persistence.postgres,
        &runtime_result.execution,
        &runtime_result.trace,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let failed = query_repository::get_execution_by_id(&state.persistence.postgres, execution.id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))?;
    if let Err(error) = state
        .application_services
        .ops
        .update_async_operation(
            state,
            crate::services::ops::service::UpdateAsyncOperationCommand {
                operation_id: async_operation_id,
                status: query_async_operation_status(&runtime_result.outcome).to_string(),
                completed_at: runtime_result.execution.completed_at,
                failure_code: runtime_result.execution.failure_code.clone(),
            },
        )
        .await
    {
        tracing::warn!(stage = "query", error = %error, "ops update_async_operation failed");
    }
    append_query_runtime_policy_audit(
        state,
        command.author_principal_id,
        conversation,
        execution.id,
        &runtime_result,
    )
    .await;
    interruption_guard.disarm();
    Ok(map_query_execution_failure(&failed.id, &failed.query_text, &failure))
}

pub(super) async fn finalize_grounded_runtime_execution(
    finalization: QueryFinalization<'_>,
    runtime_session: RuntimeExecutionSession,
    outcome: RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>,
) -> Result<query_repository::QueryExecutionRow, ApiError> {
    let QueryFinalization {
        state,
        command,
        conversation,
        execution,
        async_operation_id,
        interruption_guard,
    } = finalization;
    let runtime_result = state
        .agent_runtime
        .executor()
        .finalize_session::<QueryAnswerTask>(runtime_session, outcome)
        .await;
    runtime_persistence::persist_runtime_result(
        &state.persistence.postgres,
        &runtime_result.execution,
        &runtime_result.trace,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let terminal_execution = load_terminal_query_execution(state, execution).await?;
    match &runtime_result.outcome {
        RuntimeTerminalOutcome::Completed { .. } | RuntimeTerminalOutcome::Recovered { .. } => {
            update_query_async_operation_ready(
                state,
                async_operation_id,
                runtime_result.execution.completed_at,
            )
            .await;
            Ok(terminal_execution)
        }
        RuntimeTerminalOutcome::Failed { failure, summary }
        | RuntimeTerminalOutcome::Canceled { failure, summary } => {
            update_query_async_operation_failed(
                state,
                async_operation_id,
                &runtime_result.outcome,
                runtime_result.execution.completed_at,
                &summary.code,
            )
            .await;
            append_query_runtime_policy_audit(
                state,
                command.author_principal_id,
                conversation,
                terminal_execution.id,
                &runtime_result,
            )
            .await;
            interruption_guard.disarm();
            Err(map_query_execution_failure(
                &terminal_execution.id,
                &terminal_execution.query_text,
                failure,
            ))
        }
    }
}

pub(super) async fn finalize_agent_runtime_execution(
    finalization: QueryFinalization<'_>,
    runtime_session: RuntimeExecutionSession,
    outcome: RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>,
) -> Result<query_repository::QueryExecutionRow, ApiError> {
    let QueryFinalization {
        state,
        command,
        conversation,
        execution,
        async_operation_id,
        interruption_guard,
    } = finalization;
    let runtime_result = state
        .agent_runtime
        .executor()
        .finalize_session::<QueryAnswerTask>(runtime_session, outcome)
        .await;
    runtime_persistence::persist_runtime_result(
        &state.persistence.postgres,
        &runtime_result.execution,
        &runtime_result.trace,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let terminal_execution = load_terminal_query_execution(state, execution).await?;
    match &runtime_result.outcome {
        RuntimeTerminalOutcome::Completed { .. } | RuntimeTerminalOutcome::Recovered { .. } => {
            update_query_async_operation_ready(
                state,
                async_operation_id,
                runtime_result.execution.completed_at,
            )
            .await;
            Ok(terminal_execution)
        }
        RuntimeTerminalOutcome::Failed { failure, summary }
        | RuntimeTerminalOutcome::Canceled { failure, summary } => {
            update_query_async_operation_failed(
                state,
                async_operation_id,
                &runtime_result.outcome,
                runtime_result.execution.completed_at,
                &summary.code,
            )
            .await;
            append_query_runtime_policy_audit(
                state,
                command.author_principal_id,
                conversation,
                terminal_execution.id,
                &runtime_result,
            )
            .await;
            interruption_guard.disarm();
            Err(map_query_execution_failure(
                &terminal_execution.id,
                &terminal_execution.query_text,
                failure,
            ))
        }
    }
}

async fn load_terminal_query_execution(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
) -> Result<query_repository::QueryExecutionRow, ApiError> {
    query_repository::get_execution_by_id(&state.persistence.postgres, execution.id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))
}

async fn update_query_async_operation_ready(
    state: &AppState,
    operation_id: Uuid,
    completed_at: Option<chrono::DateTime<Utc>>,
) {
    if let Err(error) = state
        .application_services
        .ops
        .update_async_operation(
            state,
            crate::services::ops::service::UpdateAsyncOperationCommand {
                operation_id,
                status: "ready".to_string(),
                completed_at,
                failure_code: None,
            },
        )
        .await
    {
        tracing::warn!(stage = "query", error = %error, "ops update_async_operation failed");
    }
}

async fn update_query_async_operation_failed(
    state: &AppState,
    operation_id: Uuid,
    outcome: &RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>,
    completed_at: Option<chrono::DateTime<Utc>>,
    failure_code: &str,
) {
    if let Err(error) = state
        .application_services
        .ops
        .update_async_operation(
            state,
            crate::services::ops::service::UpdateAsyncOperationCommand {
                operation_id,
                status: query_async_operation_status(outcome).to_string(),
                completed_at,
                failure_code: Some(failure_code.to_string()),
            },
        )
        .await
    {
        tracing::warn!(stage = "query", error = %error, "ops update_async_operation failed");
    }
}

pub(super) async fn seed_query_runtime_session(
    state: &AppState,
    query_execution_id: Uuid,
    conversation_context: &ConversationRuntimeContext,
    surface_kind: RuntimeSurfaceKind,
) -> Result<RuntimeExecutionSession, ApiError> {
    let task_spec = QueryAnswerTask::spec();
    let runtime_overrides = bounded_runtime_overrides(state, &task_spec);
    let request = TextRequestBuilder::<QueryAnswerTask>::new(
        QueryAnswerTaskInput {
            query_execution_id,
            question: conversation_context.current_question_text.clone(),
            prompt_history_text: conversation_context.prompt_history_text.clone(),
            grounded_context_text: String::new(),
        },
        RuntimeExecutionOwner::query_execution(query_execution_id),
    )
    .with_surface_kind(surface_kind)
    .with_budget_limits(runtime_overrides.max_turns, runtime_overrides.max_parallel_actions)
    .build();

    state
        .agent_runtime
        .seed_and_persist_session(&state.persistence.postgres, &request)
        .await
        .map_err(|error| map_runtime_execution_error(query_execution_id, error))
}

pub(super) async fn persist_agent_answer(
    state: &AppState,
    conversation_id: Uuid,
    execution_id: Uuid,
    request_turn_id: Uuid,
    answer_text: &str,
) -> Result<String, QueryAnswerTaskFailure> {
    let response_turn = query_repository::create_turn(
        &state.persistence.postgres,
        &query_repository::NewQueryTurn {
            conversation_id,
            turn_kind: "assistant",
            author_principal_id: None,
            content_text: answer_text,
            execution_id: Some(execution_id),
        },
    )
    .await
    .map_err(|error| {
        make_query_answer_failure(
            "query_persist_failed",
            format!("failed to persist assistant response turn: {error}"),
        )
    })?;

    match query_repository::update_execution(
        &state.persistence.postgres,
        execution_id,
        &query_repository::UpdateQueryExecution {
            request_turn_id: Some(request_turn_id),
            response_turn_id: Some(response_turn.id),
        },
    )
    .await
    {
        Ok(Some(_)) => Ok(answer_text.to_string()),
        Ok(None) => Err(make_query_answer_failure(
            "query_execution_not_found",
            format!("query execution {execution_id} not found during persist"),
        )),
        Err(error) => Err(make_query_answer_failure(
            "query_persist_failed",
            format!("failed to update query execution after assistant response: {error}"),
        )),
    }
}

pub(super) async fn persist_failed_agent_debug_snapshot(
    state: &AppState,
    execution_id: Uuid,
    library_id: Uuid,
    question: &str,
    failure: &AgentTurnFailure,
) {
    if failure.debug_iterations.is_empty() && failure.agent_loop.is_none() {
        return;
    }
    if let Err(error) = crate::services::query::llm_context_debug::upsert_snapshot(
        &state.persistence.postgres,
        &crate::services::query::llm_context_debug::LlmContextSnapshot {
            execution_id,
            library_id,
            question: question.to_string(),
            iterations: failure.debug_iterations.clone(),
            total_iterations: failure.debug_iterations.len(),
            final_answer: None,
            captured_at: Utc::now(),
            query_ir: None,
            agent_loop: failure.agent_loop.as_deref().cloned(),
            spans: Vec::new(),
        },
    )
    .await
    {
        warn!(
            error = %error,
            execution_id = %execution_id,
            "failed to persist failed UI agent LLM context snapshot"
        );
    }
}

pub(super) fn map_runtime_execution_error(
    execution_id: Uuid,
    error: RuntimeExecutionError,
) -> ApiError {
    let failure_code = match &error {
        RuntimeExecutionError::InvalidTaskSpec(_) => "invalid_runtime_task_spec",
        RuntimeExecutionError::UnregisteredTask(_) => "unregistered_runtime_task",
        RuntimeExecutionError::TurnBudgetExhausted => "runtime_budget_exhausted",
        RuntimeExecutionError::PolicyBlocked { reason_code, .. } => reason_code,
    };
    tracing::error!(
        execution_id = %execution_id,
        failure_code = %failure_code,
        "query runtime initialization failed"
    );

    match error {
        RuntimeExecutionError::InvalidTaskSpec(_) => {
            ApiError::Conflict("runtime task specification is invalid".to_string())
        }
        RuntimeExecutionError::UnregisteredTask(task_kind) => {
            ApiError::Conflict(format!("runtime task is not registered: {}", task_kind.as_str()))
        }
        RuntimeExecutionError::TurnBudgetExhausted => {
            ApiError::Conflict("runtime execution budget exhausted".to_string())
        }
        RuntimeExecutionError::PolicyBlocked { reason_code, reason_summary_redacted, .. } => {
            ApiError::Conflict(format!("{reason_code}: {reason_summary_redacted}"))
        }
    }
}

pub(super) fn make_query_answer_failure(
    code: &str,
    summary: impl Into<String>,
) -> QueryAnswerTaskFailure {
    QueryAnswerTaskFailure::new(code, summary)
}

pub(super) fn map_runtime_answer_query_failure(
    failure: RuntimeAnswerQueryFailure,
    fallback_code: &'static str,
) -> QueryAnswerTaskFailure {
    let RuntimeAnswerQueryFailure { error, provider_calls } = failure;
    let code = anyhow_failure_code(&error, fallback_code);
    let detail_redacted = redacted_failure_detail(&error);
    make_query_answer_failure(code, format!("{error:#}"))
        .with_detail_redacted(detail_redacted)
        .with_provider_calls(provider_calls)
}

/// Extracts a value-free failure detail from the typed links of the error
/// chain, so the persisted stage record says more than the bare failure code.
/// Sources are enum-shape classifiers only — provider call/budget errors and
/// the static `ServiceUnavailable` kind token — never free-form diagnostics.
/// `QueryServiceError::Internal` hides its payload from `chain()` (the inner
/// `anyhow::Error` is not a `source`), so it is descended explicitly, exactly
/// like `anyhow_failure_code` does for the code.
pub(super) fn redacted_failure_detail(error: &anyhow::Error) -> Option<String> {
    for cause in error.chain() {
        if let Some(provider_error) = cause.downcast_ref::<ProviderCallError>() {
            return Some(provider_error.redacted_detail());
        }
        if let Some(budget_error) = cause.downcast_ref::<ProviderBudgetError>() {
            return Some(budget_error.redacted_detail().to_string());
        }
        if let Some(ApiError::ServiceUnavailable { kind, .. }) = cause.downcast_ref::<ApiError>() {
            return Some((*kind).to_string());
        }
        match cause.downcast_ref::<QueryServiceError>() {
            Some(QueryServiceError::Provider(failure)) => {
                return failure.detail_redacted().map(str::to_string);
            }
            Some(QueryServiceError::Internal(inner)) => {
                return redacted_failure_detail(inner);
            }
            _ => {}
        }
    }
    None
}

pub(super) fn query_service_failure_code(
    error: &QueryServiceError,
    fallback: &'static str,
) -> &'static str {
    match error {
        QueryServiceError::LibraryNotFound { .. } | QueryServiceError::NotFound { .. } => {
            "query_not_found"
        }
        QueryServiceError::BindingNotConfigured { .. } => "query_binding_not_configured",
        QueryServiceError::StateConflict { .. } => "query_state_conflict",
        QueryServiceError::Provider(failure) => match failure.kind() {
            ProviderFailureKind::Unavailable => "query_provider_failed",
            ProviderFailureKind::QuotaExhausted => "query_provider_quota_exhausted",
        },
        QueryServiceError::CacheUnavailable { .. } => "query_dependency_unavailable",
        QueryServiceError::Cancelled => "query_cancelled",
        QueryServiceError::DeadlineExceeded => "query_deadline_exceeded",
        // `Internal` is an opaque `anyhow::Error` that may still carry a
        // specific, already-classified cause. Unwrap one level before giving
        // up so the agent-loop surface preserves that classification.
        QueryServiceError::Internal(inner) => anyhow_failure_code(inner, fallback),
        QueryServiceError::Repository(_) => fallback,
    }
}

fn api_failure_code(error: &ApiError, fallback: &'static str) -> &'static str {
    match error {
        ApiError::ProviderFailure(_) => "query_provider_failed",
        ApiError::GatewayTimeout { .. } => "query_deadline_exceeded",
        ApiError::NotFound(_) => "query_not_found",
        ApiError::ServiceUnavailable { .. } => "query_dependency_unavailable",
        ApiError::BootstrapAlreadyClaimed(_)
        | ApiError::Conflict(_)
        | ApiError::UnreadableDocument(_)
        | ApiError::StaleRevision(_)
        | ApiError::ConflictingMutation(_)
        | ApiError::IdempotencyConflict(_)
        | ApiError::MissingPrice(_)
        | ApiError::KnowledgeNotReady(_)
        | ApiError::GraphWriteContention(_)
        | ApiError::GraphPersistenceIntegrity(_)
        | ApiError::SettlementRefreshFailed(_) => "query_state_conflict",
        _ => fallback,
    }
}

pub(super) fn anyhow_failure_code(error: &anyhow::Error, fallback: &'static str) -> &'static str {
    if let Some(error) = error.downcast_ref::<QueryServiceError>() {
        return query_service_failure_code(error, fallback);
    }
    if let Some(error) = error.downcast_ref::<ApiError>() {
        return api_failure_code(error, fallback);
    }
    if error.downcast_ref::<ProviderCallError>().is_some() {
        return "query_provider_failed";
    }
    fallback
}

fn make_runtime_failure_summary(failure: &QueryAnswerTaskFailure) -> RuntimeFailureSummary {
    RuntimeFailureSummary {
        code: failure.code.clone(),
        summary_redacted: query_failure_summary_redacted(failure),
    }
}

/// Operator-facing redacted summary for a query failure: policy failures keep
/// their typed policy summary; everything else stores the failure code plus
/// the value-free typed detail when one was captured at classification time.
pub(super) fn query_failure_summary_redacted(failure: &QueryAnswerTaskFailure) -> Option<String> {
    if is_runtime_policy_failure_code(&failure.code) {
        bounded_runtime_policy_summary(&failure.summary)
    } else {
        combined_runtime_failure_summary(&failure.code, failure.detail_redacted.as_deref())
    }
}

pub(super) fn make_query_terminal_failure_outcome(
    failure: QueryAnswerTaskFailure,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    let summary = make_runtime_failure_summary(&failure);
    if is_runtime_policy_failure_code(&failure.code) {
        RuntimeTerminalOutcome::Canceled { failure, summary }
    } else {
        RuntimeTerminalOutcome::Failed { failure, summary }
    }
}

fn query_async_operation_status(
    outcome: &RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>,
) -> &'static str {
    match outcome {
        RuntimeTerminalOutcome::Completed { .. } | RuntimeTerminalOutcome::Recovered { .. } => {
            "ready"
        }
        RuntimeTerminalOutcome::Canceled { .. } => "canceled",
        RuntimeTerminalOutcome::Failed { .. } => "failed",
    }
}

fn query_policy_action_kind(failure_code: &str) -> Option<&'static str> {
    match failure_code {
        "runtime_policy_rejected" => Some("query.runtime.policy.rejected"),
        "runtime_policy_terminated" => Some("query.runtime.policy.terminated"),
        "runtime_policy_blocked" => Some("query.runtime.policy.blocked"),
        _ => None,
    }
}

async fn append_query_runtime_policy_audit(
    state: &AppState,
    actor_principal_id: Option<Uuid>,
    conversation: &query_repository::QueryConversationRow,
    query_execution_id: Uuid,
    runtime_result: &crate::agent_runtime::task::RuntimeTaskResult<QueryAnswerTask>,
) {
    let RuntimeTerminalOutcome::Canceled { summary, .. } = &runtime_result.outcome else {
        return;
    };
    let Some(action_kind) = query_policy_action_kind(&summary.code) else {
        return;
    };
    if let Err(error) = state
        .application_services
        .audit
        .append_event(
            state,
            crate::services::iam::audit::AppendAuditEventCommand {
                actor_principal_id,
                surface_kind: runtime_result.execution.surface_kind.as_str().to_string(),
                action_kind: action_kind.to_string(),
                request_id: None,
                trace_id: None,
                result_kind: "rejected".to_string(),
                redacted_message: summary.summary_redacted.clone(),
                internal_message: Some(format!(
                    "runtime policy canceled query execution {} via runtime execution {} with code {}",
                    query_execution_id, runtime_result.execution.id, summary.code
                )),
                subjects: vec![
                    state.application_services.audit.query_session_subject(
                        conversation.id,
                        conversation.workspace_id,
                        conversation.library_id,
                    ),
                    state.application_services.audit.query_execution_subject(
                        query_execution_id,
                        conversation.workspace_id,
                        conversation.library_id,
                    ),
                    state.application_services.audit.runtime_execution_subject(
                        runtime_result.execution.id,
                        Some(conversation.workspace_id),
                        Some(conversation.library_id),
                    ),
                ],
            },
        )
        .await
    {
        tracing::warn!(stage = "query", error = %error, "audit append failed");
    }
}

pub(super) async fn begin_query_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
) -> Result<chrono::DateTime<chrono::Utc>, QueryAnswerTaskFailure> {
    executor.begin_stage(session, stage_kind).await.map_err(|error| match error {
        RuntimeExecutionError::TurnBudgetExhausted => make_query_answer_failure(
            "runtime_budget_exhausted",
            "runtime execution budget exhausted",
        ),
        RuntimeExecutionError::InvalidTaskSpec(message) => {
            make_query_answer_failure("invalid_runtime_task_spec", message)
        }
        RuntimeExecutionError::UnregisteredTask(task_kind) => make_query_answer_failure(
            "unregistered_runtime_task",
            format!("runtime task is not registered: {}", task_kind.as_str()),
        ),
        RuntimeExecutionError::PolicyBlocked {
            decision_kind,
            reason_code,
            reason_summary_redacted,
        } => make_query_answer_failure(
            match decision_kind {
                RuntimeDecisionKind::Reject => "runtime_policy_rejected",
                RuntimeDecisionKind::Terminate => "runtime_policy_terminated",
                RuntimeDecisionKind::Allow => "runtime_policy_blocked",
            },
            format!("{reason_code}: {reason_summary_redacted}"),
        ),
    })
}

pub(super) fn record_query_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
    deterministic: bool,
    failure: Option<&QueryAnswerTaskFailure>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    // `started_at` is the real wall-clock moment the stage began,
    // threaded through from the matching `begin_query_runtime_stage`
    // call. When a stage errors out before we have a `begin` result
    // (e.g. policy rejection at stage entry), callers pass `None` and
    // we stamp `Utc::now()` so the record still has a monotonically
    // correct pair — `started_at == completed_at` in that case, which
    // mirrors the trace viewer's "zero-duration" entry for genuinely
    // atomic policy denials.
    //
    // The stage state is derived, not passed: a recorded failure means the
    // stage failed, its absence means it completed.
    let stage_state =
        if failure.is_some() { RuntimeStageState::Failed } else { RuntimeStageState::Completed };
    let resolved_started_at = started_at.unwrap_or_else(chrono::Utc::now);
    executor.complete_stage(
        session,
        crate::agent_runtime::executor::StageCompletion {
            stage_kind,
            stage_state,
            deterministic,
            failure_code: failure.map(|value| value.code.clone()),
            failure_summary_redacted: failure.and_then(query_failure_summary_redacted),
            started_at: resolved_started_at,
        },
    );
}

pub(crate) fn query_runtime_stage_label(stage_kind: RuntimeStageKind) -> &'static str {
    match stage_kind {
        RuntimeStageKind::Compile => "compile",
        RuntimeStageKind::Plan => "plan",
        RuntimeStageKind::Retrieve => "retrieve",
        RuntimeStageKind::Answer => "answer",
        RuntimeStageKind::Rerank => "rerank",
        RuntimeStageKind::AssembleContext => "assembling_context",
        RuntimeStageKind::Verify => "verify",
        RuntimeStageKind::ExtractGraph => "extract_graph",
        RuntimeStageKind::StructuredPrepare => "structured_preparation",
        RuntimeStageKind::TechnicalFactExtract => "technical_fact_extraction",
        RuntimeStageKind::Recovery => "recovery",
        RuntimeStageKind::Persist => "persist",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueryExecutionApiFailure {
    DeadlineExceeded,
    Conflict,
    ProviderFailure,
    NotFound,
    ServiceUnavailable(&'static str),
    Internal,
}

pub(super) fn query_execution_api_failure(code: &str) -> QueryExecutionApiFailure {
    match code {
        "query_deadline_exceeded" => QueryExecutionApiFailure::DeadlineExceeded,
        "query_binding_not_configured" => {
            QueryExecutionApiFailure::ServiceUnavailable("query_binding_not_configured")
        }
        "query_retrieval_unavailable" => {
            QueryExecutionApiFailure::ServiceUnavailable("query_retrieval_unavailable")
        }
        "query_dependency_unavailable" => {
            QueryExecutionApiFailure::ServiceUnavailable("query_dependency_unavailable")
        }
        "query_provider_failed" => QueryExecutionApiFailure::ProviderFailure,
        "query_not_found" => QueryExecutionApiFailure::NotFound,
        "query_state_conflict" | "query_cancelled" => QueryExecutionApiFailure::Conflict,
        code if is_runtime_policy_failure_code(code) => QueryExecutionApiFailure::Conflict,
        _ => QueryExecutionApiFailure::Internal,
    }
}

fn log_query_execution_failure(execution_id: &Uuid, failure: &QueryAnswerTaskFailure) {
    tracing::error!(
        execution_id = %execution_id,
        failure_code = %failure.code,
        failure_summary = %failure.summary,
        "query execution failed"
    );
}

fn log_query_deadline_exceeded(execution_id: &Uuid) {
    tracing::warn!(execution_id = %execution_id, "query answer exceeded its execution deadline");
}

pub(super) fn map_query_execution_failure(
    execution_id: &Uuid,
    _query_text: &str,
    failure: &QueryAnswerTaskFailure,
) -> ApiError {
    log_query_execution_failure(execution_id, failure);

    match query_execution_api_failure(&failure.code) {
        QueryExecutionApiFailure::DeadlineExceeded => {
            log_query_deadline_exceeded(execution_id);
            ApiError::query_deadline_exceeded()
        }
        QueryExecutionApiFailure::Conflict => {
            ApiError::Conflict("query execution did not complete".to_string())
        }
        QueryExecutionApiFailure::ProviderFailure => {
            ApiError::ProviderFailure("query provider request failed".to_string())
        }
        QueryExecutionApiFailure::NotFound => {
            ApiError::NotFound("query result was not found".to_string())
        }
        QueryExecutionApiFailure::ServiceUnavailable(kind) => {
            ApiError::service_unavailable("query dependency is unavailable", kind)
        }
        // Unknown failure codes fail closed and never expose provider diagnostics
        // or the user's question through the public API.
        QueryExecutionApiFailure::Internal => ApiError::Internal,
    }
}
