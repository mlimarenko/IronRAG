use chrono::Utc;
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;
use uuid::Uuid;

use crate::{
    agent_runtime::{
        builder::TextRequestBuilder,
        executor::{RuntimeExecutionError, RuntimeExecutionSession},
        persistence as runtime_persistence,
        response::{RuntimeFailureSummary, RuntimeTerminalOutcome},
        task::RuntimeTask,
        tasks::query_answer::{
            QueryAnswerTask, QueryAnswerTaskFailure, QueryAnswerTaskInput, QueryAnswerTaskSuccess,
        },
    },
    app::state::AppState,
    domains::agent_runtime::{
        RuntimeDecisionKind, RuntimeExecutionOwner, RuntimeExecutionSummary, RuntimeStageKind,
        RuntimeStageState, RuntimeSurfaceKind,
    },
    domains::catalog::CatalogLifecycleState,
    domains::query::{QueryConversationState, QueryExecutionDetail, QueryVerificationState},
    infra::repositories::{ai_repository, query_repository, runtime_repository},
    interfaces::http::router_support::ApiError,
    services::{
        ingest::runtime::bounded_runtime_overrides,
        ops::billing::CaptureQueryExecutionBillingCommand,
        ops::service::CreateAsyncOperationCommand,
        query::execution::{RuntimeAnswerQueryResult, generate_answer_query, prepare_answer_query},
    },
};

use super::{
    CANONICAL_QUERY_MODE, ConversationRuntimeContext, ExecuteConversationTurnCommand, QueryService,
    QueryTurnExecutionResult, QueryTurnProgressEvent,
    context::{assemble_context_bundle, load_execution_prepared_reference_context},
    emit_query_runtime_summary,
    formatting::{
        append_answer_source_links, build_assistant_document_references,
        build_prepared_segment_references, build_technical_fact_references, map_chunk_references,
        map_entity_references, map_execution_runtime_stage_summaries,
        map_execution_runtime_summary, map_relation_references, parse_query_verification_state,
        parse_query_verification_warnings, search_pg_entity_references,
    },
    session::{
        build_conversation_runtime_context, derive_conversation_title,
        enrich_query_with_coreference_entities, map_conversation_row, map_execution_row,
        map_turn_row, normalize_required_text, should_refresh_conversation_title,
    },
};

impl QueryService {
    pub async fn execute_turn(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        self.execute_turn_with_progress(state, command, None).await
    }

    pub async fn execute_turn_stream(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
        progress: UnboundedSender<QueryTurnProgressEvent>,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        self.execute_turn_with_progress(state, command, Some(progress)).await
    }

    async fn execute_turn_with_progress(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
        progress: Option<UnboundedSender<QueryTurnProgressEvent>>,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        let mut conversation = query_repository::get_conversation_by_id(
            &state.persistence.postgres,
            command.conversation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("conversation", command.conversation_id))?;
        if conversation.conversation_state != QueryConversationState::Active {
            return Err(ApiError::Conflict(format!(
                "conversation {} is not active",
                conversation.id
            )));
        }
        let library =
            state.canonical_services.catalog.get_library(state, conversation.library_id).await?;
        if library.workspace_id != conversation.workspace_id {
            return Err(ApiError::Conflict(format!(
                "conversation {} has library {} outside workspace {}",
                conversation.id, library.id, conversation.workspace_id
            )));
        }
        if library.lifecycle_state != CatalogLifecycleState::Active {
            return Err(ApiError::Conflict(format!("library {} is not active", library.id)));
        }

        let content_text = normalize_required_text(&command.content_text, "contentText")?;
        let request_turn = query_repository::create_turn(
            &state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: conversation.id,
                turn_kind: "user",
                author_principal_id: command.author_principal_id,
                content_text: &content_text,
                execution_id: None,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        if let Some(derived_title) = derive_conversation_title(&content_text) {
            if should_refresh_conversation_title(conversation.title.as_deref(), &derived_title) {
                conversation = query_repository::update_conversation_title(
                    &state.persistence.postgres,
                    conversation.id,
                    &derived_title,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            }
        }
        let conversation_turns = query_repository::list_turns_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let conversation_context =
            build_conversation_runtime_context(&conversation_turns, request_turn.id);

        let binding_id = ai_repository::get_effective_binding_assignment_by_purpose(
            &state.persistence.postgres,
            conversation.library_id,
            "query_answer",
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .map(|binding| binding.id);

        let execution_id = Uuid::now_v7();
        let execution_context_bundle_id = Uuid::now_v7();
        let runtime_surface_kind =
            if progress.is_some() { RuntimeSurfaceKind::Stream } else { RuntimeSurfaceKind::Rest };
        let mut runtime_session =
            seed_query_runtime_session(state, execution_id, &conversation_context).await?;
        runtime_session.execution.surface_kind = runtime_surface_kind;
        let runtime_execution_id = runtime_session.execution.id;
        emit_query_runtime_summary(
            progress.as_ref(),
            RuntimeExecutionSummary::from(&runtime_session.execution),
        );
        let execution = query_repository::create_execution(
            &state.persistence.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: execution_context_bundle_id,
                workspace_id: conversation.workspace_id,
                library_id: conversation.library_id,
                conversation_id: conversation.id,
                request_turn_id: Some(request_turn.id),
                response_turn_id: None,
                binding_id,
                runtime_execution_id,
                query_text: &content_text,
                failure_code: None,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let async_operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: conversation.workspace_id,
                    library_id: conversation.library_id,
                    operation_kind: "query_execution".to_string(),
                    surface_kind: "rest".to_string(),
                    requested_by_principal_id: command.author_principal_id,
                    status: "accepted".to_string(),
                    subject_kind: "query_execution".to_string(),
                    subject_id: Some(execution.id),
                    parent_async_operation_id: None,
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;
        let async_operation = state
            .canonical_services
            .ops
            .update_async_operation(
                state,
                crate::services::ops::service::UpdateAsyncOperationCommand {
                    operation_id: async_operation.id,
                    status: "processing".to_string(),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;

        let top_k = command.top_k.clamp(1, 32);
        let mut query_embedding_usage = None;
        // Bridge the assistant agent's structured progress events into
        // the turn-level SSE stream. One central place so the handler
        // and the frontend both see one canonical set of frames.
        let mut stream_agent_progress =
            |event: crate::services::query::agent_loop::AgentProgressEvent| {
                use crate::services::query::agent_loop::AgentProgressEvent;
                let Some(progress) = progress.as_ref() else {
                    return;
                };
                let frame = match event {
                    AgentProgressEvent::AnswerDelta(delta) => {
                        QueryTurnProgressEvent::AnswerDelta(delta)
                    }
                    AgentProgressEvent::ToolCallStarted {
                        iteration,
                        call_id,
                        name,
                        arguments_preview,
                    } => QueryTurnProgressEvent::AssistantToolCallStarted {
                        iteration,
                        call_id,
                        name,
                        arguments_preview,
                    },
                    AgentProgressEvent::ToolCallCompleted {
                        iteration,
                        call_id,
                        name,
                        is_error,
                        result_preview,
                    } => QueryTurnProgressEvent::AssistantToolCallCompleted {
                        iteration,
                        call_id,
                        name,
                        is_error,
                        result_preview,
                    },
                };
                let _ = progress.send(frame);
            };
        let outcome: RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> = {
            if let Err(failure) = begin_query_runtime_stage(
                state.agent_runtime.executor(),
                &mut runtime_session,
                RuntimeStageKind::Retrieve,
            )
            .await
            {
                emit_query_runtime_summary(
                    progress.as_ref(),
                    RuntimeExecutionSummary::from(&runtime_session.execution),
                );
                record_query_runtime_stage(
                    state.agent_runtime.executor(),
                    &mut runtime_session,
                    RuntimeStageKind::Retrieve,
                    RuntimeStageState::Failed,
                    true,
                    Some(&failure),
                );
                emit_query_runtime_summary(
                    progress.as_ref(),
                    RuntimeExecutionSummary::from(&runtime_session.execution),
                );
                make_query_terminal_failure_outcome(failure.clone())
            } else {
                let enriched_query_text = enrich_query_with_coreference_entities(
                    &conversation_context.effective_query_text,
                    &conversation_context.coreference_entities,
                );
                let prepared = match prepare_answer_query(
                    state,
                    library.id,
                    enriched_query_text,
                    CANONICAL_QUERY_MODE,
                    top_k,
                    command.include_debug,
                )
                .await
                {
                    Ok(result) => {
                        record_query_runtime_stage(
                            state.agent_runtime.executor(),
                            &mut runtime_session,
                            RuntimeStageKind::Retrieve,
                            RuntimeStageState::Completed,
                            true,
                            None,
                        );
                        emit_query_runtime_summary(
                            progress.as_ref(),
                            RuntimeExecutionSummary::from(&runtime_session.execution),
                        );
                        query_embedding_usage = result.embedding_usage.clone();
                        result
                    }
                    Err(error) => {
                        let failure =
                            make_query_answer_failure("query_retrieve_failed", error.to_string());
                        record_query_runtime_stage(
                            state.agent_runtime.executor(),
                            &mut runtime_session,
                            RuntimeStageKind::Retrieve,
                            RuntimeStageState::Failed,
                            true,
                            Some(&failure),
                        );
                        emit_query_runtime_summary(
                            progress.as_ref(),
                            RuntimeExecutionSummary::from(&runtime_session.execution),
                        );
                        let outcome: RuntimeTerminalOutcome<
                            QueryAnswerTaskSuccess,
                            QueryAnswerTaskFailure,
                        > = make_query_terminal_failure_outcome(failure.clone());
                        let runtime_result = state
                            .agent_runtime
                            .executor()
                            .finalize_session::<QueryAnswerTask>(runtime_session, outcome)
                            .await;
                        emit_query_runtime_summary(
                            progress.as_ref(),
                            RuntimeExecutionSummary::from(&runtime_result.execution),
                        );
                        runtime_persistence::persist_runtime_result(
                            &state.persistence.postgres,
                            &runtime_result.execution,
                            &runtime_result.trace,
                        )
                        .await
                        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
                        let failed = query_repository::update_execution(
                            &state.persistence.postgres,
                            execution.id,
                            &query_repository::UpdateQueryExecution {
                                request_turn_id: Some(request_turn.id),
                                response_turn_id: None,
                                failure_code: Some(
                                    runtime_result
                                        .execution
                                        .failure_code
                                        .as_deref()
                                        .unwrap_or("query_retrieve_failed"),
                                ),
                                completed_at: runtime_result.execution.completed_at,
                            },
                        )
                        .await
                        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                        .ok_or_else(|| {
                            ApiError::resource_not_found("query_execution", execution.id)
                        })?;
                        if let Err(error) = state
                            .canonical_services
                            .ops
                            .update_async_operation(
                                state,
                                crate::services::ops::service::UpdateAsyncOperationCommand {
                                    operation_id: async_operation.id,
                                    status: query_async_operation_status(&runtime_result.outcome)
                                        .to_string(),
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
                            &conversation,
                            execution.id,
                            &runtime_result,
                        )
                        .await;
                        return Err(map_query_execution_error_message(
                            state,
                            &failed.id,
                            &failed.query_text,
                            runtime_result
                                .execution
                                .failure_summary_redacted
                                .unwrap_or_else(|| "query retrieve failed".to_string()),
                        ));
                    }
                };

                if let Err(failure) = begin_query_runtime_stage(
                    state.agent_runtime.executor(),
                    &mut runtime_session,
                    RuntimeStageKind::AssembleContext,
                )
                .await
                {
                    emit_query_runtime_summary(
                        progress.as_ref(),
                        RuntimeExecutionSummary::from(&runtime_session.execution),
                    );
                    record_query_runtime_stage(
                        state.agent_runtime.executor(),
                        &mut runtime_session,
                        RuntimeStageKind::AssembleContext,
                        RuntimeStageState::Failed,
                        true,
                        Some(&failure),
                    );
                    emit_query_runtime_summary(
                        progress.as_ref(),
                        RuntimeExecutionSummary::from(&runtime_session.execution),
                    );
                    make_query_terminal_failure_outcome(failure.clone())
                } else {
                    match assemble_context_bundle(
                        state,
                        &conversation,
                        execution.id,
                        execution_context_bundle_id,
                        &conversation_context.effective_query_text,
                        CANONICAL_QUERY_MODE,
                        top_k,
                        command.include_debug,
                        prepared.structured.planned_mode,
                    )
                    .await
                    {
                        Ok(()) => {
                            record_query_runtime_stage(
                                state.agent_runtime.executor(),
                                &mut runtime_session,
                                RuntimeStageKind::AssembleContext,
                                RuntimeStageState::Completed,
                                true,
                                None,
                            );
                            emit_query_runtime_summary(
                                progress.as_ref(),
                                RuntimeExecutionSummary::from(&runtime_session.execution),
                            );

                            if let Err(failure) = begin_query_runtime_stage(
                                state.agent_runtime.executor(),
                                &mut runtime_session,
                                RuntimeStageKind::Answer,
                            )
                            .await
                            {
                                emit_query_runtime_summary(
                                    progress.as_ref(),
                                    RuntimeExecutionSummary::from(&runtime_session.execution),
                                );
                                record_query_runtime_stage(
                                    state.agent_runtime.executor(),
                                    &mut runtime_session,
                                    RuntimeStageKind::Answer,
                                    RuntimeStageState::Failed,
                                    false,
                                    Some(&failure),
                                );
                                emit_query_runtime_summary(
                                    progress.as_ref(),
                                    RuntimeExecutionSummary::from(&runtime_session.execution),
                                );
                                make_query_terminal_failure_outcome(failure.clone())
                            } else {
                                match generate_answer_query(
                                    state,
                                    library.id,
                                    execution.id,
                                    &conversation_context.effective_query_text,
                                    &content_text,
                                    conversation_context.prompt_history_text.as_deref(),
                                    None,
                                    prepared,
                                    progress.as_ref().map(|_| {
                                        &mut stream_agent_progress
                                            as &mut (dyn FnMut(
                                                crate::services::query::agent_loop::AgentProgressEvent,
                                            ) + Send)
                                    }),
                                    &command.auth,
                                )
                                .await
                                {
                                    Ok(result) => {
                                        let RuntimeAnswerQueryResult {
                                            answer,
                                            provider,
                                            usage_json,
                                        } = result;
                                        record_query_runtime_stage(
                                            state.agent_runtime.executor(),
                                            &mut runtime_session,
                                            RuntimeStageKind::Answer,
                                            RuntimeStageState::Completed,
                                            false,
                                            None,
                                        );
                                        emit_query_runtime_summary(
                                            progress.as_ref(),
                                            RuntimeExecutionSummary::from(
                                                &runtime_session.execution,
                                            ),
                                        );
                                        let answer_text = self
                                            .decorate_answer_with_source_links_if_enabled(
                                                state,
                                                execution.id,
                                                &content_text,
                                                answer,
                                            )
                                            .await;

                                        if let Err(failure) = begin_query_runtime_stage(
                                            state.agent_runtime.executor(),
                                            &mut runtime_session,
                                            RuntimeStageKind::Persist,
                                        )
                                        .await
                                        {
                                            emit_query_runtime_summary(
                                                progress.as_ref(),
                                                RuntimeExecutionSummary::from(
                                                    &runtime_session.execution,
                                                ),
                                            );
                                            record_query_runtime_stage(
                                                state.agent_runtime.executor(),
                                                &mut runtime_session,
                                                RuntimeStageKind::Persist,
                                                RuntimeStageState::Failed,
                                                true,
                                                Some(&failure),
                                            );
                                            emit_query_runtime_summary(
                                                progress.as_ref(),
                                                RuntimeExecutionSummary::from(
                                                    &runtime_session.execution,
                                                ),
                                            );
                                            make_query_terminal_failure_outcome(failure.clone())
                                        } else {
                                            match query_repository::create_turn(
                                                &state.persistence.postgres,
                                                &query_repository::NewQueryTurn {
                                                    conversation_id: conversation.id,
                                                    turn_kind: "assistant",
                                                    author_principal_id: None,
                                                    content_text: &answer_text,
                                                    execution_id: Some(execution.id),
                                                },
                                            )
                                            .await
                                            {
                                                Ok(response_turn) => {
                                                    match query_repository::update_execution(
                                                        &state.persistence.postgres,
                                                        execution.id,
                                                        &query_repository::UpdateQueryExecution {
                                                            request_turn_id: Some(request_turn.id),
                                                            response_turn_id: Some(
                                                                response_turn.id,
                                                            ),
                                                            failure_code: None,
                                                            completed_at: Some(Utc::now()),
                                                        },
                                                    )
                                                    .await
                                                    {
                                                        Ok(Some(_)) => {
                                                            record_query_runtime_stage(
                                                                state.agent_runtime.executor(),
                                                                &mut runtime_session,
                                                                RuntimeStageKind::Persist,
                                                                RuntimeStageState::Completed,
                                                                true,
                                                                None,
                                                            );
                                                            emit_query_runtime_summary(
                                                                progress.as_ref(),
                                                                RuntimeExecutionSummary::from(
                                                                    &runtime_session.execution,
                                                                ),
                                                            );
                                                            RuntimeTerminalOutcome::Completed {
                                                                success: QueryAnswerTaskSuccess {
                                                                    answer_text,
                                                                    provider,
                                                                    usage_json,
                                                                },
                                                            }
                                                        }
                                                        Ok(None) => {
                                                            let failure = make_query_answer_failure(
                                                                "query_execution_not_found",
                                                                format!(
                                                                    "query execution {} not found during persist",
                                                                    execution.id
                                                                ),
                                                            );
                                                            record_query_runtime_stage(
                                                                state.agent_runtime.executor(),
                                                                &mut runtime_session,
                                                                RuntimeStageKind::Persist,
                                                                RuntimeStageState::Failed,
                                                                true,
                                                                Some(&failure),
                                                            );
                                                            emit_query_runtime_summary(
                                                                progress.as_ref(),
                                                                RuntimeExecutionSummary::from(
                                                                    &runtime_session.execution,
                                                                ),
                                                            );
                                                            make_query_terminal_failure_outcome(
                                                                failure.clone(),
                                                            )
                                                        }
                                                        Err(error) => {
                                                            let failure = make_query_answer_failure(
                                                                "query_persist_failed",
                                                                format!(
                                                                    "failed to update query execution after assistant response: {error}"
                                                                ),
                                                            );
                                                            record_query_runtime_stage(
                                                                state.agent_runtime.executor(),
                                                                &mut runtime_session,
                                                                RuntimeStageKind::Persist,
                                                                RuntimeStageState::Failed,
                                                                true,
                                                                Some(&failure),
                                                            );
                                                            emit_query_runtime_summary(
                                                                progress.as_ref(),
                                                                RuntimeExecutionSummary::from(
                                                                    &runtime_session.execution,
                                                                ),
                                                            );
                                                            make_query_terminal_failure_outcome(
                                                                failure.clone(),
                                                            )
                                                        }
                                                    }
                                                }
                                                Err(error) => {
                                                    let failure = make_query_answer_failure(
                                                        "query_persist_failed",
                                                        format!(
                                                            "failed to persist assistant response turn: {error}"
                                                        ),
                                                    );
                                                    record_query_runtime_stage(
                                                        state.agent_runtime.executor(),
                                                        &mut runtime_session,
                                                        RuntimeStageKind::Persist,
                                                        RuntimeStageState::Failed,
                                                        true,
                                                        Some(&failure),
                                                    );
                                                    emit_query_runtime_summary(
                                                        progress.as_ref(),
                                                        RuntimeExecutionSummary::from(
                                                            &runtime_session.execution,
                                                        ),
                                                    );
                                                    make_query_terminal_failure_outcome(
                                                        failure.clone(),
                                                    )
                                                }
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        let failure = make_query_answer_failure(
                                            "query_answer_failed",
                                            error.to_string(),
                                        );
                                        record_query_runtime_stage(
                                            state.agent_runtime.executor(),
                                            &mut runtime_session,
                                            RuntimeStageKind::Answer,
                                            RuntimeStageState::Failed,
                                            false,
                                            Some(&failure),
                                        );
                                        emit_query_runtime_summary(
                                            progress.as_ref(),
                                            RuntimeExecutionSummary::from(
                                                &runtime_session.execution,
                                            ),
                                        );
                                        make_query_terminal_failure_outcome(failure.clone())
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            tracing::error!(
                                execution_id = %execution.id,
                                conversation_id = %conversation.id,
                                library_id = %conversation.library_id,
                                error = ?error,
                                "failed to assemble knowledge context bundle"
                            );
                            let failure = make_query_answer_failure(
                                "query_context_assembly_failed",
                                format!("failed to assemble knowledge context bundle: {error}"),
                            );
                            record_query_runtime_stage(
                                state.agent_runtime.executor(),
                                &mut runtime_session,
                                RuntimeStageKind::AssembleContext,
                                RuntimeStageState::Failed,
                                true,
                                Some(&failure),
                            );
                            emit_query_runtime_summary(
                                progress.as_ref(),
                                RuntimeExecutionSummary::from(&runtime_session.execution),
                            );
                            make_query_terminal_failure_outcome(failure.clone())
                        }
                    }
                }
            }
        };

        let runtime_result = state
            .agent_runtime
            .executor()
            .finalize_session::<QueryAnswerTask>(runtime_session, outcome)
            .await;
        emit_query_runtime_summary(
            progress.as_ref(),
            RuntimeExecutionSummary::from(&runtime_result.execution),
        );
        runtime_persistence::persist_runtime_result(
            &state.persistence.postgres,
            &runtime_result.execution,
            &runtime_result.trace,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let terminal_execution = match &runtime_result.outcome {
            RuntimeTerminalOutcome::Completed { .. } | RuntimeTerminalOutcome::Recovered { .. } => {
                query_repository::get_execution_by_id(&state.persistence.postgres, execution.id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))?
            }
            RuntimeTerminalOutcome::Failed { .. } | RuntimeTerminalOutcome::Canceled { .. } => {
                query_repository::update_execution(
                    &state.persistence.postgres,
                    execution.id,
                    &query_repository::UpdateQueryExecution {
                        request_turn_id: Some(request_turn.id),
                        response_turn_id: None,
                        failure_code: runtime_result.execution.failure_code.as_deref(),
                        completed_at: runtime_result.execution.completed_at,
                    },
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))?
            }
        };

        match &runtime_result.outcome {
            RuntimeTerminalOutcome::Completed { success } => {
                if let Err(error) = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        crate::services::ops::service::UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: "ready".to_string(),
                            completed_at: runtime_result.execution.completed_at,
                            failure_code: None,
                        },
                    )
                    .await
                {
                    tracing::warn!(stage = "query", error = %error, "ops update_async_operation failed");
                }

                if let Err(error) = state
                    .canonical_services
                    .billing
                    .capture_query_execution(
                        state,
                        CaptureQueryExecutionBillingCommand {
                            workspace_id: conversation.workspace_id,
                            library_id: conversation.library_id,
                            execution_id: terminal_execution.id,
                            runtime_execution_id: runtime_result.execution.id,
                            binding_id: terminal_execution.binding_id,
                            provider_kind: success.provider.provider_kind.as_str().to_string(),
                            model_name: success.provider.model_name.clone(),
                            usage_json: success.usage_json.clone(),
                        },
                    )
                    .await
                {
                    warn!(error = %error, execution_id = %terminal_execution.id, "canonical query billing capture failed");
                }
                if let Some(embed_usage) = &query_embedding_usage {
                    if let Err(error) = state
                        .canonical_services
                        .billing
                        .capture_query_execution(
                            state,
                            CaptureQueryExecutionBillingCommand {
                                workspace_id: conversation.workspace_id,
                                library_id: conversation.library_id,
                                execution_id: terminal_execution.id,
                                runtime_execution_id: runtime_result.execution.id,
                                binding_id: None,
                                provider_kind: embed_usage.provider_kind.clone(),
                                model_name: embed_usage.model_name.clone(),
                                usage_json: embed_usage.usage_json.clone(),
                            },
                        )
                        .await
                    {
                        warn!(error = %error, execution_id = %terminal_execution.id, "query embedding billing capture failed");
                    }
                }
            }
            RuntimeTerminalOutcome::Recovered { success, .. } => {
                if let Err(error) = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        crate::services::ops::service::UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: "ready".to_string(),
                            completed_at: runtime_result.execution.completed_at,
                            failure_code: None,
                        },
                    )
                    .await
                {
                    tracing::warn!(stage = "query", error = %error, "ops update_async_operation failed");
                }

                if let Err(error) = state
                    .canonical_services
                    .billing
                    .capture_query_execution(
                        state,
                        CaptureQueryExecutionBillingCommand {
                            workspace_id: conversation.workspace_id,
                            library_id: conversation.library_id,
                            execution_id: terminal_execution.id,
                            runtime_execution_id: runtime_result.execution.id,
                            binding_id: terminal_execution.binding_id,
                            provider_kind: success.provider.provider_kind.as_str().to_string(),
                            model_name: success.provider.model_name.clone(),
                            usage_json: success.usage_json.clone(),
                        },
                    )
                    .await
                {
                    warn!(error = %error, execution_id = %terminal_execution.id, "canonical query billing capture failed");
                }
                if let Some(embed_usage) = &query_embedding_usage {
                    if let Err(error) = state
                        .canonical_services
                        .billing
                        .capture_query_execution(
                            state,
                            CaptureQueryExecutionBillingCommand {
                                workspace_id: conversation.workspace_id,
                                library_id: conversation.library_id,
                                execution_id: terminal_execution.id,
                                runtime_execution_id: runtime_result.execution.id,
                                binding_id: None,
                                provider_kind: embed_usage.provider_kind.clone(),
                                model_name: embed_usage.model_name.clone(),
                                usage_json: embed_usage.usage_json.clone(),
                            },
                        )
                        .await
                    {
                        warn!(error = %error, execution_id = %terminal_execution.id, "query embedding billing capture failed");
                    }
                }
            }
            RuntimeTerminalOutcome::Failed { summary, .. }
            | RuntimeTerminalOutcome::Canceled { summary, .. } => {
                if let Err(error) = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        crate::services::ops::service::UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: query_async_operation_status(&runtime_result.outcome)
                                .to_string(),
                            completed_at: runtime_result.execution.completed_at,
                            failure_code: Some(summary.code.clone()),
                        },
                    )
                    .await
                {
                    tracing::warn!(stage = "query", error = %error, "ops update_async_operation failed");
                }
                append_query_runtime_policy_audit(
                    state,
                    command.author_principal_id,
                    &conversation,
                    terminal_execution.id,
                    &runtime_result,
                )
                .await;
                return Err(map_query_execution_error_message(
                    state,
                    &terminal_execution.id,
                    &terminal_execution.query_text,
                    summary.summary_redacted.clone().unwrap_or_else(|| summary.code.clone()),
                ));
            }
        }

        let detail = self.get_execution(state, terminal_execution.id).await?;
        let request_turn = detail.request_turn.ok_or(ApiError::Internal)?;
        Ok(QueryTurnExecutionResult {
            conversation: map_conversation_row(conversation),
            request_turn,
            response_turn: detail.response_turn,
            execution: detail.execution,
            runtime_summary: detail.runtime_summary,
            runtime_stage_summaries: detail.runtime_stage_summaries,
            context_bundle_id: execution_context_bundle_id,
            chunk_references: detail.chunk_references,
            prepared_segment_references: detail.prepared_segment_references,
            technical_fact_references: detail.technical_fact_references,
            graph_node_references: detail.graph_node_references,
            graph_edge_references: detail.graph_edge_references,
            verification_state: detail.verification_state,
            verification_warnings: detail.verification_warnings,
        })
    }

    pub async fn get_execution(
        &self,
        state: &AppState,
        execution_id: Uuid,
    ) -> Result<QueryExecutionDetail, ApiError> {
        let execution =
            query_repository::get_execution_by_id(&state.persistence.postgres, execution_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution_id))?;
        let request_turn = match execution.request_turn_id {
            Some(turn_id) => query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .map(map_turn_row),
            None => None,
        };
        let response_turn = match execution.response_turn_id {
            Some(turn_id) => query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .map(map_turn_row),
            None => None,
        };
        let runtime_stage_records = runtime_repository::list_runtime_stage_records(
            &state.persistence.postgres,
            execution.runtime_execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let runtime_policy_rows = runtime_repository::list_runtime_policy_decisions(
            &state.persistence.postgres,
            execution.runtime_execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let prepared_reference_context =
            load_execution_prepared_reference_context(state, execution.id).await?;

        let query_text = execution.query_text.clone();
        let mut graph_node_references = prepared_reference_context
            .bundle_refs
            .as_ref()
            .map_or_else(Vec::new, map_entity_references);

        // Fallback: if ArangoDB returned no entity references, search PostgreSQL
        // runtime_graph_node by keyword overlap with the query text.
        if graph_node_references.is_empty() {
            graph_node_references = search_pg_entity_references(
                &state.persistence.postgres,
                execution.library_id,
                execution.id,
                &query_text,
            )
            .await;
        }

        Ok(QueryExecutionDetail {
            execution: map_execution_row(execution.clone()),
            runtime_summary: map_execution_runtime_summary(&execution, &runtime_policy_rows),
            runtime_stage_summaries: map_execution_runtime_stage_summaries(
                &execution,
                &runtime_stage_records,
            ),
            request_turn,
            response_turn,
            chunk_references: prepared_reference_context
                .bundle_refs
                .as_ref()
                .map_or_else(Vec::new, map_chunk_references),
            prepared_segment_references: {
                let mut references = build_prepared_segment_references(
                    prepared_reference_context.bundle_refs.as_ref(),
                    &prepared_reference_context.structured_block_rows,
                    &prepared_reference_context.block_rank_refs,
                    &query_text,
                    &prepared_reference_context.segment_revision_info,
                );
                references.extend(build_assistant_document_references(
                    execution.id,
                    &prepared_reference_context.assistant_document_references,
                ));
                references
            },
            technical_fact_references: build_technical_fact_references(
                prepared_reference_context.bundle_refs.as_ref(),
                &prepared_reference_context.technical_fact_rows,
                &prepared_reference_context.fact_rank_refs,
            ),
            graph_node_references,
            graph_edge_references: prepared_reference_context
                .bundle_refs
                .as_ref()
                .map_or_else(Vec::new, map_relation_references),
            verification_state: prepared_reference_context
                .bundle_refs
                .as_ref()
                .map_or(QueryVerificationState::NotRun, |bundle| {
                    parse_query_verification_state(&bundle.bundle.verification_state)
                }),
            verification_warnings: prepared_reference_context
                .bundle_refs
                .as_ref()
                .map_or_else(Vec::new, |bundle| {
                    parse_query_verification_warnings(&bundle.bundle.verification_warnings)
                }),
        })
    }

    async fn decorate_answer_with_source_links_if_enabled(
        &self,
        state: &AppState,
        execution_id: Uuid,
        query_text: &str,
        answer: String,
    ) -> String {
        if !state.settings.query_answer_source_links_enabled {
            return answer;
        }

        let reference_context =
            match load_execution_prepared_reference_context(state, execution_id).await {
                Ok(reference_context) => reference_context,
                Err(error) => {
                    warn!(
                        execution_id = %execution_id,
                        error = %error,
                        "failed to resolve prepared-segment source links for assistant answer"
                    );
                    return answer;
                }
            };
        let mut prepared_segment_references = build_prepared_segment_references(
            reference_context.bundle_refs.as_ref(),
            &reference_context.structured_block_rows,
            &reference_context.block_rank_refs,
            query_text,
            &reference_context.segment_revision_info,
        );
        prepared_segment_references.extend(build_assistant_document_references(
            execution_id,
            &reference_context.assistant_document_references,
        ));

        append_answer_source_links(answer, &prepared_segment_references)
    }
}

async fn seed_query_runtime_session(
    state: &AppState,
    query_execution_id: Uuid,
    conversation_context: &ConversationRuntimeContext,
) -> Result<RuntimeExecutionSession, ApiError> {
    let task_spec = QueryAnswerTask::spec();
    let runtime_overrides = bounded_runtime_overrides(state, &task_spec);
    let request = TextRequestBuilder::<QueryAnswerTask>::new(
        QueryAnswerTaskInput {
            query_execution_id,
            question: conversation_context.effective_query_text.clone(),
            prompt_history_text: conversation_context.prompt_history_text.clone(),
            grounded_context_text: String::new(),
        },
        RuntimeExecutionOwner::query_execution(query_execution_id),
    )
    .with_budget_limits(runtime_overrides.max_turns, runtime_overrides.max_parallel_actions)
    .build();

    state
        .agent_runtime
        .seed_and_persist_session(&state.persistence.postgres, &request)
        .await
        .map_err(map_runtime_execution_error)
}

fn map_runtime_execution_error(error: RuntimeExecutionError) -> ApiError {
    match error {
        RuntimeExecutionError::InvalidTaskSpec(message) => ApiError::Conflict(message),
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

fn make_query_answer_failure(code: &str, summary: impl Into<String>) -> QueryAnswerTaskFailure {
    QueryAnswerTaskFailure { code: code.to_string(), summary: summary.into() }
}

fn make_runtime_failure_summary(code: &str, summary: &str) -> RuntimeFailureSummary {
    RuntimeFailureSummary {
        code: code.to_string(),
        summary_redacted: Some(truncate_failure_code(summary).to_string()),
    }
}

fn is_runtime_policy_failure_code(code: &str) -> bool {
    matches!(
        code,
        "runtime_policy_rejected" | "runtime_policy_terminated" | "runtime_policy_blocked"
    )
}

fn make_query_terminal_failure_outcome(
    failure: QueryAnswerTaskFailure,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    let summary = make_runtime_failure_summary(&failure.code, &failure.summary);
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
        .canonical_services
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
                    state.canonical_services.audit.query_session_subject(
                        conversation.id,
                        conversation.workspace_id,
                        conversation.library_id,
                    ),
                    state.canonical_services.audit.query_execution_subject(
                        query_execution_id,
                        conversation.workspace_id,
                        conversation.library_id,
                    ),
                    state.canonical_services.audit.runtime_execution_subject(
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

async fn begin_query_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
) -> Result<(), QueryAnswerTaskFailure> {
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

fn record_query_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
    stage_state: RuntimeStageState,
    deterministic: bool,
    failure: Option<&QueryAnswerTaskFailure>,
) {
    executor.complete_stage(
        session,
        stage_kind,
        stage_state,
        deterministic,
        failure.map(|value| value.code.clone()),
        failure.map(|value| truncate_failure_code(&value.summary).to_string()),
    );
}

pub(crate) fn query_runtime_stage_label(stage_kind: RuntimeStageKind) -> &'static str {
    match stage_kind {
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

fn truncate_failure_code(message: &str) -> &str {
    const LIMIT: usize = 120;
    let truncated = message.trim();
    if truncated.len() <= LIMIT {
        truncated
    } else {
        let cutoff =
            truncated.char_indices().nth(LIMIT).map_or(truncated.len(), |(index, _)| index);
        &truncated[..cutoff]
    }
}

fn map_query_execution_error_message(
    state: &AppState,
    execution_id: &Uuid,
    query_text: &str,
    message: String,
) -> ApiError {
    let normalized = message.to_ascii_lowercase();
    let formatted = format!("query execution {execution_id} for '{query_text}' failed: {message}");
    let provider_failure = &state
        .resolve_settle_blockers_services
        .provider_failure_classification
        .classify_error_message(&message);

    if normalized.contains("active answer binding is not configured")
        || normalized.contains("active embedding binding is not configured")
        || normalized.contains("missing provider api key")
        || normalized.contains("missing openai api key")
        || normalized.contains("missing deepseek api key")
        || normalized.contains("missing qwen api key")
        || normalized.contains("unsupported provider kind")
        || normalized.contains("runtime_policy_rejected")
        || normalized.contains("runtime_policy_terminated")
        || normalized.contains("runtime_policy_blocked")
        || normalized.contains("runtime policy")
    {
        return ApiError::Conflict(formatted);
    }

    if provider_failure.is_some()
        || normalized.contains("provider request failed")
        || normalized.contains("embedding request failed")
        || normalized.contains("failed to generate grounded answer")
        || normalized.contains("failed to embed runtime query")
    {
        return ApiError::ProviderFailure(formatted);
    }

    ApiError::Internal
}
