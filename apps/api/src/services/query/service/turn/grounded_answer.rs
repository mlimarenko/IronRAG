use super::*;

pub(super) struct GroundedAnswerStageInput<'a> {
    pub(super) state: &'a AppState,
    pub(super) library: &'a CatalogLibrary,
    pub(super) conversation: &'a query_repository::QueryConversationRow,
    pub(super) conversation_context: &'a ConversationRuntimeContext,
    pub(super) request_turn: &'a query_repository::QueryTurnRow,
    pub(super) execution: &'a query_repository::QueryExecutionRow,
    pub(super) execution_context_bundle_id: Uuid,
    pub(super) runtime_execution_id: Uuid,
    pub(super) provider_execution_context: QueryProviderExecutionContext,
    pub(super) content_text: &'a str,
    pub(super) top_k: usize,
    pub(super) include_debug: bool,
}

pub(super) async fn run_grounded_retrieve_stage(
    input: &GroundedAnswerStageInput<'_>,
    runtime_session: &mut RuntimeExecutionSession,
) -> Result<crate::services::query::execution::PreparedAnswerQueryResult, QueryAnswerTaskFailure> {
    if let Err(failure) = begin_query_runtime_stage(
        input.state.agent_runtime.executor(),
        runtime_session,
        RuntimeStageKind::Retrieve,
    )
    .await
    {
        record_query_runtime_stage(
            input.state.agent_runtime.executor(),
            runtime_session,
            RuntimeStageKind::Retrieve,
            true,
            Some(&failure),
            None,
        );
        return Err(failure);
    }
    let retrieve_started = Utc::now();
    let result = Box::pin(prepare_answer_query(PrepareAnswerQueryRequest {
        state: input.state,
        library_id: input.library.id,
        semantic_rerank_context: SemanticRerankExecutionContext {
            workspace_id: input.conversation.workspace_id,
            query_execution_id: input.execution.id,
            runtime_execution_id: input.runtime_execution_id,
        },
        question: input.conversation_context.current_question_text.clone(),
        conversation_history: &input.conversation_context.query_compiler_history,
        mode: GROUNDED_ANSWER_QUERY_MODE,
        top_k: input.top_k,
        include_debug: input.include_debug,
    }))
    .await;
    match result {
        Ok(prepared) => {
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Retrieve,
                true,
                None,
                Some(retrieve_started),
            );
            append_prepared_chunk_references(input, &prepared).await;
            Ok(prepared)
        }
        Err(error) => {
            let failure = make_query_answer_failure(
                anyhow_failure_code(&error, "query_embedding_failed"),
                format!("{error:#}"),
            );
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Retrieve,
                true,
                Some(&failure),
                Some(retrieve_started),
            );
            Err(failure)
        }
    }
}

async fn append_prepared_chunk_references(
    input: &GroundedAnswerStageInput<'_>,
    prepared: &crate::services::query::execution::PreparedAnswerQueryResult,
) {
    let references = prepared
        .structured
        .chunk_references
        .iter()
        .map(|reference| query_repository::NewQueryChunkReference {
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect::<Vec<_>>();
    if let Err(error) = query_repository::append_chunk_references(
        &input.state.persistence.postgres,
        input.execution.id,
        &references,
    )
    .await
    {
        tracing::warn!(
            %error,
            execution_id = %input.execution.id,
            chunk_count = references.len(),
            "failed to persist query_chunk_reference rows"
        );
    }
}

pub(super) async fn run_grounded_answer_stages(
    input: &GroundedAnswerStageInput<'_>,
    runtime_session: &mut RuntimeExecutionSession,
    prepared: crate::services::query::execution::PreparedAnswerQueryResult,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    if let Err(failure) = run_grounded_assemble_stage(input, runtime_session, &prepared).await {
        return make_query_terminal_failure_outcome(failure);
    }
    let answer_result = match run_grounded_generate_stage(input, runtime_session, prepared).await {
        Ok(result) => result,
        Err(failure) => return make_query_terminal_failure_outcome(failure),
    };
    run_grounded_persist_stage(input, runtime_session, answer_result).await
}

async fn run_grounded_assemble_stage(
    input: &GroundedAnswerStageInput<'_>,
    runtime_session: &mut RuntimeExecutionSession,
    prepared: &crate::services::query::execution::PreparedAnswerQueryResult,
) -> Result<(), QueryAnswerTaskFailure> {
    if let Err(failure) = begin_query_runtime_stage(
        input.state.agent_runtime.executor(),
        runtime_session,
        RuntimeStageKind::AssembleContext,
    )
    .await
    {
        record_query_runtime_stage(
            input.state.agent_runtime.executor(),
            runtime_session,
            RuntimeStageKind::AssembleContext,
            true,
            Some(&failure),
            None,
        );
        return Err(failure);
    }
    let started = Utc::now();
    let result = assemble_context_bundle(AssembleContextBundleRequest {
        state: input.state,
        conversation: input.conversation,
        execution_id: input.execution.id,
        bundle_id: input.execution_context_bundle_id,
        query_text: &input.conversation_context.current_question_text,
        query_ir: &prepared.query_ir,
        requested_mode: GROUNDED_ANSWER_QUERY_MODE,
        top_k: input.top_k,
        include_debug: input.include_debug,
        resolved_mode: prepared.structured.planned_mode,
        answer_chunk_references: &prepared.structured.chunk_references,
        answer_entity_references: &prepared.structured.graph_entity_references,
        answer_relation_references: &prepared.structured.graph_relation_references,
    })
    .await;
    match result {
        Ok(()) => {
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::AssembleContext,
                true,
                None,
                Some(started),
            );
            Ok(())
        }
        Err(error) => {
            let failure_code = anyhow_failure_code(&error, "query_context_assembly_failed");
            tracing::error!(
                execution_id = %input.execution.id,
                failure_code = %failure_code,
                "failed to assemble knowledge context bundle"
            );
            let failure = make_query_answer_failure(
                failure_code,
                format!("failed to assemble knowledge context bundle: {error}"),
            );
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::AssembleContext,
                true,
                Some(&failure),
                Some(started),
            );
            Err(failure)
        }
    }
}

async fn run_grounded_generate_stage(
    input: &GroundedAnswerStageInput<'_>,
    runtime_session: &mut RuntimeExecutionSession,
    prepared: crate::services::query::execution::PreparedAnswerQueryResult,
) -> Result<RuntimeAnswerQueryResult, QueryAnswerTaskFailure> {
    if let Err(failure) = begin_query_runtime_stage(
        input.state.agent_runtime.executor(),
        runtime_session,
        RuntimeStageKind::Answer,
    )
    .await
    {
        record_query_runtime_stage(
            input.state.agent_runtime.executor(),
            runtime_session,
            RuntimeStageKind::Answer,
            false,
            Some(&failure),
            None,
        );
        return Err(failure);
    }
    let started = Utc::now();
    let (history_text, history_messages) = grounded_answer_history(input, &prepared.query_ir);
    let result = Box::pin(generate_answer_query(
        AnswerQueryRequest {
            state: input.state,
            execution_context: input.provider_execution_context,
            effective_question: &input.conversation_context.current_question_text,
            user_question: input.content_text,
            conversation_history: history_text,
            conversation_history_messages: history_messages,
        },
        prepared,
    ))
    .await;
    match result {
        Ok(answer) => {
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Answer,
                false,
                None,
                Some(started),
            );
            Ok(answer)
        }
        Err(error) => {
            let failure = map_runtime_answer_query_failure(error, "query_answer_failed");
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Answer,
                false,
                Some(&failure),
                Some(started),
            );
            Err(failure)
        }
    }
}

fn grounded_answer_history<'a>(
    input: &'a GroundedAnswerStageInput<'_>,
    query_ir: &QueryIR,
) -> (Option<&'a str>, &'a [crate::integrations::llm::ChatMessage]) {
    if !compiled_query_uses_answer_history(
        &input.conversation_context.current_question_text,
        query_ir,
    ) {
        return (None, &[]);
    }
    (
        input.conversation_context.prompt_history_text.as_deref(),
        input.conversation_context.prompt_history_messages.as_slice(),
    )
}

async fn run_grounded_persist_stage(
    input: &GroundedAnswerStageInput<'_>,
    runtime_session: &mut RuntimeExecutionSession,
    answer_result: RuntimeAnswerQueryResult,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    let RuntimeAnswerQueryResult { answer, provider_calls } = answer_result;
    if let Err(failure) = begin_query_runtime_stage(
        input.state.agent_runtime.executor(),
        runtime_session,
        RuntimeStageKind::Persist,
    )
    .await
    {
        let failure = failure.with_provider_calls(provider_calls);
        record_query_runtime_stage(
            input.state.agent_runtime.executor(),
            runtime_session,
            RuntimeStageKind::Persist,
            true,
            Some(&failure),
            None,
        );
        return make_query_terminal_failure_outcome(failure);
    }
    let started = Utc::now();
    let persist_result = persist_grounded_answer_rows(input, &answer).await;
    match persist_result {
        Ok(()) => {
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Persist,
                true,
                None,
                Some(started),
            );
            RuntimeTerminalOutcome::Completed {
                success: QueryAnswerTaskSuccess { answer_text: answer, provider_calls },
            }
        }
        Err(failure) => {
            let failure = failure.with_provider_calls(provider_calls);
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Persist,
                true,
                Some(&failure),
                Some(started),
            );
            make_query_terminal_failure_outcome(failure)
        }
    }
}

async fn persist_grounded_answer_rows(
    input: &GroundedAnswerStageInput<'_>,
    answer: &str,
) -> Result<(), QueryAnswerTaskFailure> {
    let response_turn = query_repository::create_turn(
        &input.state.persistence.postgres,
        &query_repository::NewQueryTurn {
            conversation_id: input.conversation.id,
            turn_kind: "assistant",
            author_principal_id: None,
            content_text: answer,
            execution_id: Some(input.execution.id),
        },
    )
    .await
    .map_err(|error| {
        make_query_answer_failure(
            "query_persist_failed",
            format!("failed to persist assistant response turn: {error}"),
        )
    })?;
    let updated = query_repository::update_execution(
        &input.state.persistence.postgres,
        input.execution.id,
        &query_repository::UpdateQueryExecution {
            request_turn_id: Some(input.request_turn.id),
            response_turn_id: Some(response_turn.id),
        },
    )
    .await
    .map_err(|error| {
        make_query_answer_failure(
            "query_persist_failed",
            format!("failed to update query execution after assistant response: {error}"),
        )
    })?;
    if updated.is_some() {
        return Ok(());
    }
    Err(make_query_answer_failure(
        "query_execution_not_found",
        format!("query execution {} not found during persist", input.execution.id),
    ))
}
