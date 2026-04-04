use std::{
    collections::{BTreeSet, HashMap},
    time::Instant,
};

use anyhow::Context;
use chrono::Utc;
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, warn};
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
        trace::build_policy_summary,
    },
    app::state::AppState,
    domains::agent_runtime::{
        RuntimeExecutionOwner, RuntimeExecutionSummary, RuntimePolicyDecision,
        RuntimePolicySummary, RuntimeStageKind, RuntimeStageState, RuntimeSurfaceKind,
    },
    domains::ai::AiBindingPurpose,
    domains::catalog::CatalogLifecycleState,
    domains::query::{
        PreparedSegmentReference, QueryChunkReference, QueryConversation, QueryConversationDetail,
        QueryConversationState, QueryExecution, QueryExecutionDetail, QueryGraphEdgeReference,
        QueryGraphNodeReference, QueryRuntimeStageSummary, QueryTurn, QueryTurnKind,
        QueryVerificationState, RuntimeQueryMode, TechnicalFactReference,
    },
    infra::{
        arangodb::{
            collections::{
                KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                KNOWLEDGE_ENTITY_COLLECTION, KNOWLEDGE_EVIDENCE_COLLECTION,
                KNOWLEDGE_RELATION_COLLECTION,
            },
            context_store::{
                KnowledgeBundleChunkEdgeRow, KnowledgeBundleEntityEdgeRow,
                KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleRelationEdgeRow,
                KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow,
                KnowledgeRetrievalTraceRow,
            },
            document_store::{
                KnowledgeChunkRow, KnowledgeLibraryGenerationRow, KnowledgeStructuredBlockRow,
                KnowledgeTechnicalFactRow,
            },
            graph_store::{KnowledgeEvidenceRow, KnowledgeGraphTraversalRow},
        },
        repositories::{ai_repository, query_repository, runtime_repository},
    },
    integrations::llm::EmbeddingRequest,
    interfaces::http::router_support::ApiError,
    services::{
        billing_service::CaptureQueryExecutionBillingCommand,
        graph_identity::normalize_graph_identity_component,
        ops_service::CreateAsyncOperationCommand,
        query_execution::{generate_answer_query, prepare_answer_query},
        runtime_ingestion::bounded_runtime_overrides,
    },
};

const MAX_LIBRARY_CONVERSATIONS: usize = 5;
const QUERY_CONVERSATION_TITLE_LIMIT: usize = 72;
const MAX_PROMPT_HISTORY_TURNS: usize = 6;
const MAX_PROMPT_HISTORY_TURN_CHARS: usize = 360;
const MAX_EFFECTIVE_QUERY_HISTORY_TURNS: usize = 3;
const MAX_EFFECTIVE_QUERY_TURN_CHARS: usize = 220;
const CANONICAL_QUERY_MODE: RuntimeQueryMode = RuntimeQueryMode::Mix;
const MAX_DETAIL_TECHNICAL_FACT_REFERENCES: usize = 24;
const MAX_DETAIL_PREPARED_SEGMENT_REFERENCES: usize = 48;
const MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION: usize = 8;
const PREPARED_SEGMENT_FOCUS_STOPWORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "for",
    "how",
    "in",
    "is",
    "of",
    "the",
    "to",
    "what",
    "как",
    "какая",
    "какие",
    "какой",
    "по",
    "про",
    "такое",
    "этой",
    "этот",
    "это",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConversationRuntimeContext {
    effective_query_text: String,
    prompt_history_text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateConversationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecuteConversationTurnCommand {
    pub conversation_id: Uuid,
    pub author_principal_id: Option<Uuid>,
    pub content_text: String,
    pub top_k: usize,
    pub include_debug: bool,
}

#[derive(Debug, Clone)]
pub struct QueryTurnExecutionResult {
    pub conversation: QueryConversation,
    pub request_turn: QueryTurn,
    pub response_turn: Option<QueryTurn>,
    pub execution: QueryExecution,
    pub runtime_summary: RuntimeExecutionSummary,
    pub runtime_stage_summaries: Vec<QueryRuntimeStageSummary>,
    pub context_bundle_id: Uuid,
    pub chunk_references: Vec<QueryChunkReference>,
    pub prepared_segment_references: Vec<PreparedSegmentReference>,
    pub technical_fact_references: Vec<TechnicalFactReference>,
    pub graph_node_references: Vec<QueryGraphNodeReference>,
    pub graph_edge_references: Vec<QueryGraphEdgeReference>,
    pub verification_state: QueryVerificationState,
    pub verification_warnings: Vec<crate::domains::query::QueryVerificationWarning>,
}

#[derive(Debug, Clone)]
pub enum QueryTurnProgressEvent {
    Runtime(RuntimeExecutionSummary),
    AnswerDelta(String),
}

#[derive(Clone, Default)]
pub struct QueryService;

impl QueryService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn list_conversations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<QueryConversation>, ApiError> {
        let rows = query_repository::list_conversations_by_library(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_conversation_row).collect())
    }

    pub async fn get_conversation(
        &self,
        state: &AppState,
        conversation_id: Uuid,
    ) -> Result<QueryConversationDetail, ApiError> {
        let conversation =
            query_repository::get_conversation_by_id(&state.persistence.postgres, conversation_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("conversation", conversation_id))?;
        let turns = query_repository::list_turns_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let executions = query_repository::list_executions_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(QueryConversationDetail {
            conversation: map_conversation_row(conversation),
            turns: turns.into_iter().map(map_turn_row).collect(),
            executions: executions.into_iter().map(map_execution_row).collect(),
        })
    }

    pub async fn create_conversation(
        &self,
        state: &AppState,
        command: CreateConversationCommand,
    ) -> Result<QueryConversation, ApiError> {
        let title = normalize_optional_text(command.title.as_deref());
        let library =
            state.canonical_services.catalog.get_library(state, command.library_id).await?;
        if library.workspace_id != command.workspace_id {
            return Err(ApiError::Conflict(format!(
                "library {} does not belong to workspace {}",
                library.id, command.workspace_id
            )));
        }
        if library.lifecycle_state != CatalogLifecycleState::Active {
            return Err(ApiError::Conflict(format!("library {} is not active", library.id)));
        }
        let row = query_repository::create_conversation(
            &state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: library.workspace_id,
                library_id: library.id,
                created_by_principal_id: command.created_by_principal_id,
                title: title.as_deref(),
                conversation_state: "active",
            },
            MAX_LIBRARY_CONVERSATIONS,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_conversation_row(row))
    }

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
        .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?;

        if let Some(derived_title) = derive_conversation_title(&content_text) {
            if should_refresh_conversation_title(conversation.title.as_deref(), &derived_title) {
                conversation = query_repository::update_conversation_title(
                    &state.persistence.postgres,
                    conversation.id,
                    &derived_title,
                )
                .await
                .map_err(|_| ApiError::Internal)?;
            }
        }
        let conversation_turns = query_repository::list_turns_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let conversation_context =
            build_conversation_runtime_context(&conversation_turns, request_turn.id);

        let binding_id = ai_repository::get_active_library_binding_by_purpose(
            &state.persistence.postgres,
            conversation.library_id,
            "query_answer",
        )
        .await
        .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?;
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
                crate::services::ops_service::UpdateAsyncOperationCommand {
                    operation_id: async_operation.id,
                    status: "processing".to_string(),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;

        let top_k = command.top_k.clamp(1, 32);
        let mut stream_answer_delta = |delta: String| {
            if let Some(progress) = progress.as_ref() {
                let _ = progress.send(QueryTurnProgressEvent::AnswerDelta(delta));
            }
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
                let prepared = match prepare_answer_query(
                    state,
                    library.id,
                    conversation_context.effective_query_text.clone(),
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
                        .map_err(|_| ApiError::Internal)?;
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
                        .map_err(|_| ApiError::Internal)?
                        .ok_or_else(|| {
                            ApiError::resource_not_found("query_execution", execution.id)
                        })?;
                        let _ = state
                            .canonical_services
                            .ops
                            .update_async_operation(
                                state,
                                crate::services::ops_service::UpdateAsyncOperationCommand {
                                    operation_id: async_operation.id,
                                    status: query_async_operation_status(&runtime_result.outcome)
                                        .to_string(),
                                    completed_at: runtime_result.execution.completed_at,
                                    failure_code: runtime_result.execution.failure_code.clone(),
                                },
                            )
                            .await;
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
                            failed.id,
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
                                        &mut stream_answer_delta as &mut (dyn FnMut(String) + Send)
                                    }),
                                )
                                .await
                                {
                                    Ok(result) => {
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
                                                    content_text: &result.answer,
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
                                                                    answer_text: result.answer,
                                                                    provider: result.provider,
                                                                    usage_json: result.usage_json,
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
                            error!(
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
        .map_err(|_| ApiError::Internal)?;

        let terminal_execution = match &runtime_result.outcome {
            RuntimeTerminalOutcome::Completed { .. } | RuntimeTerminalOutcome::Recovered { .. } => {
                query_repository::get_execution_by_id(&state.persistence.postgres, execution.id)
                    .await
                    .map_err(|_| ApiError::Internal)?
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
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))?
            }
        };

        match &runtime_result.outcome {
            RuntimeTerminalOutcome::Completed { success } => {
                let _ = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        crate::services::ops_service::UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: "ready".to_string(),
                            completed_at: runtime_result.execution.completed_at,
                            failure_code: None,
                        },
                    )
                    .await;

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
            }
            RuntimeTerminalOutcome::Recovered { success, .. } => {
                let _ = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        crate::services::ops_service::UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: "ready".to_string(),
                            completed_at: runtime_result.execution.completed_at,
                            failure_code: None,
                        },
                    )
                    .await;

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
            }
            RuntimeTerminalOutcome::Failed { summary, .. }
            | RuntimeTerminalOutcome::Canceled { summary, .. } => {
                let _ = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        crate::services::ops_service::UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: query_async_operation_status(&runtime_result.outcome)
                                .to_string(),
                            completed_at: runtime_result.execution.completed_at,
                            failure_code: Some(summary.code.clone()),
                        },
                    )
                    .await;
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
                    terminal_execution.id,
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
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution_id))?;
        let request_turn = match execution.request_turn_id {
            Some(turn_id) => query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .map(map_turn_row),
            None => None,
        };
        let response_turn = match execution.response_turn_id {
            Some(turn_id) => query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .map(map_turn_row),
            None => None,
        };
        let runtime_stage_records = runtime_repository::list_runtime_stage_records(
            &state.persistence.postgres,
            execution.runtime_execution_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let runtime_policy_rows = runtime_repository::list_runtime_policy_decisions(
            &state.persistence.postgres,
            execution.runtime_execution_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let bundle_refs = state
            .arango_context_store
            .get_bundle_reference_set_by_query_execution(execution.id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let chunk_rows = match bundle_refs.as_ref() {
            Some(bundle) => state
                .arango_document_store
                .list_chunks_by_ids(
                    &bundle
                        .chunk_references
                        .iter()
                        .map(|reference| reference.chunk_id)
                        .collect::<Vec<_>>(),
                )
                .await
                .map_err(|_| ApiError::Internal)?,
            None => Vec::new(),
        };
        let evidence_rows = match bundle_refs.as_ref() {
            Some(bundle) => state
                .arango_graph_store
                .list_evidence_by_ids(
                    &bundle
                        .evidence_references
                        .iter()
                        .map(|reference| reference.evidence_id)
                        .collect::<Vec<_>>(),
                )
                .await
                .map_err(|_| ApiError::Internal)?,
            None => Vec::new(),
        };
        let mut fact_rank_refs = bundle_refs
            .as_ref()
            .map_or_else(HashMap::new, |bundle| derive_fact_rank_refs(bundle, &evidence_rows));
        let chunk_supported_fact_rows = if chunk_rows.is_empty() {
            Vec::new()
        } else {
            state
                .arango_document_store
                .list_technical_facts_by_chunk_ids(
                    &chunk_rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
                )
                .await
                .map_err(|_| ApiError::Internal)?
        };
        augment_fact_rank_refs_from_chunk_support(
            bundle_refs.as_ref(),
            &chunk_supported_fact_rows,
            &mut fact_rank_refs,
        );
        let technical_fact_rows = if fact_rank_refs.is_empty()
            && bundle_refs.as_ref().is_none_or(|bundle| bundle.bundle.selected_fact_ids.is_empty())
        {
            Vec::new()
        } else {
            state
                .arango_document_store
                .list_technical_facts_by_ids(
                    &bundle_refs
                        .as_ref()
                        .map(|bundle| selected_fact_ids_for_detail(bundle, &fact_rank_refs))
                        .unwrap_or_default(),
                )
                .await
                .map_err(|_| ApiError::Internal)?
        };
        let block_rank_refs = bundle_refs.as_ref().map_or_else(HashMap::new, |bundle| {
            derive_block_rank_refs(bundle, &evidence_rows, &technical_fact_rows, &chunk_rows)
        });
        let structured_block_rows = if block_rank_refs.is_empty() {
            Vec::new()
        } else {
            state
                .arango_document_store
                .list_structured_blocks_by_ids(&block_rank_refs.keys().copied().collect::<Vec<_>>())
                .await
                .map_err(|_| ApiError::Internal)?
        };

        let query_text = execution.query_text.clone();
        Ok(QueryExecutionDetail {
            execution: map_execution_row(execution.clone()),
            runtime_summary: map_execution_runtime_summary(&execution, &runtime_policy_rows),
            runtime_stage_summaries: map_execution_runtime_stage_summaries(
                &execution,
                &runtime_stage_records,
            ),
            request_turn,
            response_turn,
            chunk_references: bundle_refs.as_ref().map_or_else(Vec::new, map_chunk_references),
            prepared_segment_references: build_prepared_segment_references(
                bundle_refs.as_ref(),
                &structured_block_rows,
                &block_rank_refs,
                &query_text,
            ),
            technical_fact_references: build_technical_fact_references(
                bundle_refs.as_ref(),
                &technical_fact_rows,
                &fact_rank_refs,
            ),
            graph_node_references: bundle_refs
                .as_ref()
                .map_or_else(Vec::new, map_entity_references),
            graph_edge_references: bundle_refs
                .as_ref()
                .map_or_else(Vec::new, map_relation_references),
            verification_state: bundle_refs
                .as_ref()
                .map_or(QueryVerificationState::NotRun, |bundle| {
                    parse_query_verification_state(&bundle.bundle.verification_state)
                }),
            verification_warnings: bundle_refs.as_ref().map_or_else(Vec::new, |bundle| {
                parse_query_verification_warnings(&bundle.bundle.verification_warnings)
            }),
        })
    }
}

fn emit_query_runtime_summary(
    progress: Option<&UnboundedSender<QueryTurnProgressEvent>>,
    runtime: RuntimeExecutionSummary,
) {
    if let Some(progress) = progress {
        let _ = progress.send(QueryTurnProgressEvent::Runtime(runtime));
    }
}

#[derive(Debug, Clone)]
struct QueryEmbeddingContext {
    model_catalog_id: Uuid,
    freshness_generation: i64,
    query_vector: Vec<f32>,
}

#[derive(Debug, Clone, Default)]
struct RankedBundleReference {
    rank: i32,
    score: f64,
    reasons: BTreeSet<String>,
}

async fn assemble_context_bundle(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    execution_id: Uuid,
    bundle_id: Uuid,
    query_text: &str,
    requested_mode: RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
    resolved_mode: RuntimeQueryMode,
) -> anyhow::Result<()> {
    let started_at = Instant::now();
    let candidate_limit = top_k.saturating_mul(3).max(6);
    let lexical_search = state
        .canonical_services
        .search
        .search_query_evidence(state, conversation.library_id, query_text, candidate_limit)
        .await
        .context(
            "failed canonical lexical evidence search while assembling query context bundle",
        )?;
    let lexical_chunk_hits = lexical_search.chunk_hits;
    let lexical_entity_hits = lexical_search.entity_hits;
    let lexical_relation_hits = lexical_search.relation_hits;
    let lexical_fact_hits = lexical_search.technical_fact_hits;
    let exact_literal_bias = lexical_search.exact_literal_bias;

    let embedding_context =
        match resolve_query_embedding_context(state, conversation.library_id, query_text).await {
            Ok(context) => context,
            Err(error) => {
                warn!(
                    error = %error,
                    library_id = %conversation.library_id,
                    execution_id = %execution_id,
                    "canonical query bundle fell back to lexical retrieval"
                );
                None
            }
        };

    let vector_limit = candidate_limit.saturating_mul(2).max(8);
    let vector_chunk_hits = if let Some(context) = embedding_context.as_ref() {
        state
            .arango_search_store
            .search_chunk_vectors_by_similarity(
                conversation.library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                &context.query_vector,
                vector_limit,
                Some(16),
            )
            .await
            .context("failed vector chunk search while assembling query context bundle")?
    } else {
        Vec::new()
    };
    let vector_entity_hits = if let Some(context) = embedding_context.as_ref() {
        state
            .arango_search_store
            .search_entity_vectors_by_similarity(
                conversation.library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                &context.query_vector,
                vector_limit,
                Some(16),
            )
            .await
            .context("failed vector entity search while assembling query context bundle")?
    } else {
        Vec::new()
    };

    let mut chunk_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut fact_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut entity_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut relation_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut evidence_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();

    for (index, hit) in lexical_chunk_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut chunk_refs,
            hit.chunk_id,
            saturating_rank(index),
            hit.score,
            "lexical_chunk",
        );
    }
    for (index, hit) in lexical_fact_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut fact_refs,
            hit.fact_id,
            saturating_rank(index),
            hit.score,
            if hit.exact_match { "lexical_fact_exact" } else { "lexical_fact" },
        );
    }
    for (index, hit) in vector_chunk_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut chunk_refs,
            hit.chunk_id,
            saturating_rank(index),
            hit.score,
            "vector_chunk",
        );
    }
    for (index, hit) in lexical_entity_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut entity_refs,
            hit.entity_id,
            saturating_rank(index),
            hit.score,
            "lexical_entity",
        );
    }
    for (index, hit) in vector_entity_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut entity_refs,
            hit.entity_id,
            saturating_rank(index),
            hit.score,
            "vector_entity",
        );
    }
    for (index, hit) in lexical_relation_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut relation_refs,
            hit.relation_id,
            saturating_rank(index),
            hit.score,
            "lexical_relation",
        );
    }

    let entity_seed_ids = top_ranked_ids(&entity_refs, top_k.max(3));
    let mut entity_neighborhood_rows = 0usize;
    for entity_id in entity_seed_ids {
        let neighborhood = state
            .arango_graph_store
            .list_entity_neighborhood(entity_id, conversation.library_id, 2, candidate_limit * 4)
            .await
            .with_context(|| {
                format!(
                    "failed to load entity neighborhood while assembling query context bundle for entity {entity_id}"
                )
            })?;
        entity_neighborhood_rows = entity_neighborhood_rows.saturating_add(neighborhood.len());
        for row in neighborhood {
            absorb_traversal_row(
                &row,
                &mut chunk_refs,
                &mut entity_refs,
                &mut relation_refs,
                &mut evidence_refs,
                "entity_neighborhood",
            );
        }
    }

    let relation_seed_ids = top_ranked_ids(&relation_refs, top_k.max(3));
    let mut relation_traversal_rows = 0usize;
    let mut relation_evidence_rows = 0usize;
    for relation_id in relation_seed_ids {
        let traversal = state
            .arango_graph_store
            .expand_relation_centric(relation_id, conversation.library_id, 2, candidate_limit * 4)
            .await
            .with_context(|| {
                format!(
                    "failed to expand relation-centric neighborhood while assembling query context bundle for relation {relation_id}"
                )
            })?;
        relation_traversal_rows = relation_traversal_rows.saturating_add(traversal.len());
        for row in traversal {
            absorb_traversal_row(
                &row,
                &mut chunk_refs,
                &mut entity_refs,
                &mut relation_refs,
                &mut evidence_refs,
                "relation_traversal",
            );
        }

        let evidence_lookup = state
            .arango_graph_store
            .list_relation_evidence_lookup(relation_id, conversation.library_id, candidate_limit)
            .await
            .with_context(|| {
                format!(
                    "failed to load relation evidence lookup while assembling query context bundle for relation {relation_id}"
                )
            })?;
        relation_evidence_rows = relation_evidence_rows.saturating_add(evidence_lookup.len());
        for (index, row) in evidence_lookup.into_iter().enumerate() {
            merge_ranked_reference(
                &mut relation_refs,
                row.relation.relation_id,
                saturating_rank(index),
                row.support_edge_score.unwrap_or_default(),
                "relation_provenance",
            );
            merge_ranked_reference(
                &mut evidence_refs,
                row.evidence.evidence_id,
                saturating_rank(index),
                row.support_edge_score.unwrap_or_default(),
                "relation_evidence",
            );
            if let Some(chunk) = row.source_chunk {
                merge_ranked_reference(
                    &mut chunk_refs,
                    chunk.chunk_id,
                    saturating_rank(index),
                    row.support_edge_score.unwrap_or_default(),
                    "evidence_source",
                );
            }
        }
    }

    let evidence_rows = state
        .arango_graph_store
        .list_evidence_by_ids(&top_ranked_ids(&evidence_refs, candidate_limit * 4))
        .await
        .context("failed to load evidence rows while assembling query context bundle")?;
    for evidence in &evidence_rows {
        if let Some(fact_id) = evidence.fact_id {
            merge_ranked_reference(
                &mut fact_refs,
                fact_id,
                evidence_rank_for_bundle(&evidence_refs, evidence.evidence_id),
                evidence_score_for_bundle(&evidence_refs, evidence.evidence_id),
                "evidence_fact",
            );
        }
    }

    let mut fact_rows = state
        .arango_document_store
        .list_technical_facts_by_ids(&top_ranked_ids(&fact_refs, candidate_limit * 3))
        .await
        .context("failed to load technical facts while assembling query context bundle")?;
    for fact in &fact_rows {
        let rank = fact_rank_for_bundle(&fact_refs, fact.fact_id);
        let score = fact_score_for_bundle(&fact_refs, fact.fact_id);
        for chunk_id in &fact.support_chunk_ids {
            merge_ranked_reference(
                &mut chunk_refs,
                *chunk_id,
                rank,
                score,
                "technical_fact_support",
            );
        }
    }

    let now = Utc::now();
    let generations = state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, conversation.library_id)
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to derive library generations while assembling query context bundle: {error}"
            )
        })?;
    let generation = generations.first().cloned();
    let freshness_snapshot =
        generation.as_ref().map(freshness_snapshot_json).unwrap_or_else(|| json!({}));
    let retrieval_strategy =
        if embedding_context.is_some() { "hybrid".to_string() } else { "lexical".to_string() };
    let chunk_edges = build_chunk_bundle_edges(bundle_id, &chunk_refs, now);
    let entity_edges = build_entity_bundle_edges(bundle_id, &entity_refs, now);
    let relation_edges = build_relation_bundle_edges(bundle_id, &relation_refs, now);
    let evidence_edges = build_evidence_bundle_edges(bundle_id, &evidence_refs, now);
    let selected_chunk_rows = if chunk_refs.is_empty() {
        Vec::new()
    } else {
        state
            .arango_document_store
            .list_chunks_by_ids(&top_ranked_ids(&chunk_refs, candidate_limit * 3))
            .await
            .context("failed to load chunk rows while assembling query context bundle")?
    };
    let mut fact_refs = fact_refs;
    let chunk_supported_fact_rows = if selected_chunk_rows.is_empty() {
        Vec::new()
    } else {
        state
            .arango_document_store
            .list_technical_facts_by_chunk_ids(
                &selected_chunk_rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            )
            .await
            .context("failed to load technical facts by chunk support while assembling query context bundle")?
    };
    let provisional_bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
            bundle_id,
            workspace_id: conversation.workspace_id,
            library_id: conversation.library_id,
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: retrieval_strategy.clone(),
            requested_mode: runtime_mode_label(requested_mode).to_string(),
            resolved_mode: runtime_mode_label(resolved_mode).to_string(),
            selected_fact_ids: Vec::new(),
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: freshness_snapshot.clone(),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: now,
            updated_at: now,
        },
        chunk_references: chunk_edges
            .iter()
            .map(|edge| crate::infra::arangodb::context_store::KnowledgeBundleChunkReferenceRow {
                key: edge.key.clone(),
                bundle_id: edge.bundle_id,
                chunk_id: edge.chunk_id,
                rank: edge.rank,
                score: edge.score,
                inclusion_reason: edge.inclusion_reason.clone(),
                created_at: edge.created_at,
            })
            .collect(),
        entity_references: entity_edges
            .iter()
            .map(|edge| crate::infra::arangodb::context_store::KnowledgeBundleEntityReferenceRow {
                key: edge.key.clone(),
                bundle_id: edge.bundle_id,
                entity_id: edge.entity_id,
                rank: edge.rank,
                score: edge.score,
                inclusion_reason: edge.inclusion_reason.clone(),
                created_at: edge.created_at,
            })
            .collect(),
        relation_references: relation_edges
            .iter()
            .map(|edge| {
                crate::infra::arangodb::context_store::KnowledgeBundleRelationReferenceRow {
                    key: edge.key.clone(),
                    bundle_id: edge.bundle_id,
                    relation_id: edge.relation_id,
                    rank: edge.rank,
                    score: edge.score,
                    inclusion_reason: edge.inclusion_reason.clone(),
                    created_at: edge.created_at,
                }
            })
            .collect(),
        evidence_references: evidence_edges
            .iter()
            .map(|edge| {
                crate::infra::arangodb::context_store::KnowledgeBundleEvidenceReferenceRow {
                    key: edge.key.clone(),
                    bundle_id: edge.bundle_id,
                    evidence_id: edge.evidence_id,
                    rank: edge.rank,
                    score: edge.score,
                    inclusion_reason: edge.inclusion_reason.clone(),
                    created_at: edge.created_at,
                }
            })
            .collect(),
    };
    augment_fact_rank_refs_from_chunk_support(
        Some(&provisional_bundle),
        &chunk_supported_fact_rows,
        &mut fact_refs,
    );
    merge_technical_fact_rows(&mut fact_rows, &chunk_supported_fact_rows);
    let selected_fact_ids = top_ranked_ids(&fact_refs, top_k.max(6));
    let block_rank_refs = derive_block_rank_refs(
        &KnowledgeContextBundleReferenceSetRow {
            bundle: KnowledgeContextBundleRow {
                selected_fact_ids: selected_fact_ids.clone(),
                ..provisional_bundle.bundle.clone()
            },
            ..provisional_bundle
        },
        &evidence_rows,
        &fact_rows,
        &selected_chunk_rows,
    );

    let candidate_summary = json!({
        "lexicalChunkHits": lexical_chunk_hits.len(),
        "lexicalFactHits": lexical_fact_hits.len(),
        "vectorChunkHits": vector_chunk_hits.len(),
        "lexicalEntityHits": lexical_entity_hits.len(),
        "vectorEntityHits": vector_entity_hits.len(),
        "lexicalRelationHits": lexical_relation_hits.len(),
        "exactLiteralBias": exact_literal_bias,
        "entityNeighborhoodRows": entity_neighborhood_rows,
        "relationTraversalRows": relation_traversal_rows,
        "relationEvidenceRows": relation_evidence_rows,
        "evidenceRows": evidence_rows.len(),
        "factRows": fact_rows.len(),
        "finalChunkReferences": chunk_edges.len(),
        "finalPreparedSegmentReferences": block_rank_refs.len(),
        "finalTechnicalFactReferences": selected_fact_ids.len(),
        "finalEntityReferences": entity_edges.len(),
        "finalRelationReferences": relation_edges.len(),
        "finalEvidenceReferences": evidence_edges.len(),
    });
    let assembly_diagnostics = json!({
        "requestedMode": runtime_mode_label(requested_mode),
        "resolvedMode": runtime_mode_label(resolved_mode),
        "candidateLimit": candidate_limit,
        "vectorCandidateLimit": vector_limit,
        "vectorEnabled": embedding_context.is_some(),
        "exactLiteralBias": exact_literal_bias,
        "bundleId": bundle_id,
        "queryExecutionId": execution_id,
    });

    let bundle_row = KnowledgeContextBundleRow {
        key: bundle_id.to_string(),
        arango_id: None,
        arango_rev: None,
        bundle_id,
        workspace_id: conversation.workspace_id,
        library_id: conversation.library_id,
        query_execution_id: Some(execution_id),
        bundle_state: "ready".to_string(),
        bundle_strategy: retrieval_strategy.clone(),
        requested_mode: runtime_mode_label(requested_mode).to_string(),
        resolved_mode: runtime_mode_label(resolved_mode).to_string(),
        selected_fact_ids,
        verification_state: "not_run".to_string(),
        verification_warnings: json!([]),
        freshness_snapshot: freshness_snapshot.clone(),
        candidate_summary: candidate_summary.clone(),
        assembly_diagnostics: assembly_diagnostics.clone(),
        created_at: now,
        updated_at: now,
    };
    state
        .arango_context_store
        .upsert_bundle(&bundle_row)
        .await
        .context("failed to upsert knowledge context bundle document")?;
    state
        .arango_context_store
        .replace_bundle_chunk_edges(bundle_id, &chunk_edges)
        .await
        .context("failed to replace bundle chunk edges")?;
    state
        .arango_context_store
        .replace_bundle_entity_edges(bundle_id, &entity_edges)
        .await
        .context("failed to replace bundle entity edges")?;
    state
        .arango_context_store
        .replace_bundle_relation_edges(bundle_id, &relation_edges)
        .await
        .context("failed to replace bundle relation edges")?;
    state
        .arango_context_store
        .replace_bundle_evidence_edges(bundle_id, &evidence_edges)
        .await
        .context("failed to replace bundle evidence edges")?;

    if include_debug {
        let trace = KnowledgeRetrievalTraceRow {
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
            trace_id: bundle_id,
            workspace_id: conversation.workspace_id,
            library_id: conversation.library_id,
            query_execution_id: Some(execution_id),
            bundle_id,
            trace_state: "ready".to_string(),
            retrieval_strategy,
            candidate_counts: candidate_summary,
            dropped_reasons: json!([]),
            timing_breakdown: json!({
                "bundleAssemblyMs": started_at.elapsed().as_millis(),
            }),
            diagnostics_json: assembly_diagnostics,
            created_at: now,
            updated_at: now,
        };
        state
            .arango_context_store
            .upsert_trace(&trace)
            .await
            .context("failed to upsert knowledge retrieval trace")?;
    }

    Ok(())
}

async fn resolve_query_embedding_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<Option<QueryEmbeddingContext>, ApiError> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?;
    let Some(binding) = binding else {
        return Ok(None);
    };

    let generations = state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some(generation) = generations.first() else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    let embedding = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: query_text.to_string(),
            api_key_override: Some(binding.api_key),
            base_url_override: binding.provider_base_url,
        })
        .await
        .map_err(|error| {
            ApiError::ProviderFailure(format!("failed to embed query bundle request: {error}"))
        })?;

    Ok(Some(QueryEmbeddingContext {
        model_catalog_id: binding.model_catalog_id,
        freshness_generation: generation.active_vector_generation,
        query_vector: embedding.embedding,
    }))
}

fn merge_ranked_reference(
    refs: &mut HashMap<Uuid, RankedBundleReference>,
    target_id: Uuid,
    rank: i32,
    score: f64,
    reason: &str,
) {
    let entry = refs.entry(target_id).or_insert_with(|| RankedBundleReference {
        rank,
        score,
        reasons: BTreeSet::new(),
    });
    entry.rank = entry.rank.min(rank);
    if score > entry.score {
        entry.score = score;
    }
    entry.reasons.insert(reason.to_string());
}

fn top_ranked_ids(refs: &HashMap<Uuid, RankedBundleReference>, limit: usize) -> Vec<Uuid> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, rank)| (*id, rank)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items.into_iter().take(limit).map(|(id, _)| id).collect()
}

fn absorb_traversal_row(
    row: &KnowledgeGraphTraversalRow,
    chunk_refs: &mut HashMap<Uuid, RankedBundleReference>,
    entity_refs: &mut HashMap<Uuid, RankedBundleReference>,
    relation_refs: &mut HashMap<Uuid, RankedBundleReference>,
    evidence_refs: &mut HashMap<Uuid, RankedBundleReference>,
    reason: &str,
) {
    let rank = traversal_rank(row.path_length);
    let score = row.edge_score.unwrap_or_else(|| traversal_score(row.path_length));
    match row.vertex_kind.as_str() {
        KNOWLEDGE_CHUNK_COLLECTION => {
            merge_ranked_reference(chunk_refs, row.vertex_id, rank, score, reason);
        }
        KNOWLEDGE_ENTITY_COLLECTION => {
            merge_ranked_reference(entity_refs, row.vertex_id, rank, score, reason);
        }
        KNOWLEDGE_RELATION_COLLECTION => {
            merge_ranked_reference(relation_refs, row.vertex_id, rank, score, reason);
        }
        KNOWLEDGE_EVIDENCE_COLLECTION => {
            merge_ranked_reference(evidence_refs, row.vertex_id, rank, score, reason);
        }
        _ => {}
    }
}

fn traversal_rank(path_length: i64) -> i32 {
    i32::try_from(path_length.saturating_add(1)).unwrap_or(i32::MAX)
}

fn traversal_score(path_length: i64) -> f64 {
    match path_length {
        0 => 1.0,
        1 => 0.8,
        2 => 0.6,
        3 => 0.4,
        _ => 0.2,
    }
}

fn build_chunk_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleChunkEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(chunk_id, reference)| KnowledgeBundleChunkEdgeRow {
            key: format!("{bundle_id}:{chunk_id}"),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_CHUNK_COLLECTION}/{chunk_id}"),
            bundle_id,
            chunk_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

fn build_entity_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleEntityEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(entity_id, reference)| KnowledgeBundleEntityEdgeRow {
            key: format!("{bundle_id}:{entity_id}"),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_ENTITY_COLLECTION}/{entity_id}"),
            bundle_id,
            entity_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

fn build_relation_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleRelationEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(relation_id, reference)| KnowledgeBundleRelationEdgeRow {
            key: format!("{bundle_id}:{relation_id}"),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_RELATION_COLLECTION}/{relation_id}"),
            bundle_id,
            relation_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

fn build_evidence_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleEvidenceEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(evidence_id, reference)| KnowledgeBundleEvidenceEdgeRow {
            key: format!("{bundle_id}:{evidence_id}"),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_EVIDENCE_COLLECTION}/{evidence_id}"),
            bundle_id,
            evidence_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

fn evidence_rank_for_bundle(refs: &HashMap<Uuid, RankedBundleReference>, evidence_id: Uuid) -> i32 {
    refs.get(&evidence_id).map_or(i32::MAX, |reference| reference.rank)
}

fn evidence_score_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    evidence_id: Uuid,
) -> f64 {
    refs.get(&evidence_id).map_or(0.0, |reference| reference.score)
}

fn fact_rank_for_bundle(refs: &HashMap<Uuid, RankedBundleReference>, fact_id: Uuid) -> i32 {
    refs.get(&fact_id).map_or(i32::MAX, |reference| reference.rank)
}

fn fact_score_for_bundle(refs: &HashMap<Uuid, RankedBundleReference>, fact_id: Uuid) -> f64 {
    refs.get(&fact_id).map_or(0.0, |reference| reference.score)
}

fn derive_chunk_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> HashMap<Uuid, RankedBundleReference> {
    let mut chunk_refs = HashMap::<Uuid, RankedBundleReference>::new();
    for reference in &bundle.chunk_references {
        merge_ranked_reference(
            &mut chunk_refs,
            reference.chunk_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_chunk"),
        );
    }
    chunk_refs
}

fn augment_fact_rank_refs_from_chunk_support(
    bundle: Option<&KnowledgeContextBundleReferenceSetRow>,
    technical_fact_rows: &[KnowledgeTechnicalFactRow],
    fact_rank_refs: &mut HashMap<Uuid, RankedBundleReference>,
) {
    let Some(bundle) = bundle else {
        return;
    };
    let chunk_rank_refs = derive_chunk_rank_refs(bundle);
    if chunk_rank_refs.is_empty() {
        return;
    }
    for fact in technical_fact_rows {
        let mut best_rank = None::<i32>;
        let mut best_score = 0.0_f64;
        for chunk_id in &fact.support_chunk_ids {
            let Some(reference) = chunk_rank_refs.get(chunk_id) else {
                continue;
            };
            best_rank = Some(best_rank.map_or(reference.rank, |rank| rank.min(reference.rank)));
            if reference.score > best_score {
                best_score = reference.score;
            }
        }
        let Some(rank) = best_rank else {
            continue;
        };
        merge_ranked_reference(
            fact_rank_refs,
            fact.fact_id,
            rank,
            best_score.max(1.0),
            "selected_chunk_support",
        );
    }
}

fn merge_technical_fact_rows(
    target: &mut Vec<KnowledgeTechnicalFactRow>,
    additional: &[KnowledgeTechnicalFactRow],
) {
    let mut seen = target.iter().map(|row| row.fact_id).collect::<BTreeSet<_>>();
    for row in additional {
        if seen.insert(row.fact_id) {
            target.push(row.clone());
        }
    }
    target.sort_by(|left, right| {
        left.fact_kind.cmp(&right.fact_kind).then_with(|| left.fact_id.cmp(&right.fact_id))
    });
}

fn derive_fact_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    evidence_rows: &[KnowledgeEvidenceRow],
) -> HashMap<Uuid, RankedBundleReference> {
    let mut fact_refs = HashMap::<Uuid, RankedBundleReference>::new();
    let evidence_by_id = evidence_rows
        .iter()
        .map(|evidence| (evidence.evidence_id, evidence))
        .collect::<HashMap<_, _>>();
    for reference in &bundle.evidence_references {
        let Some(evidence) = evidence_by_id.get(&reference.evidence_id) else {
            continue;
        };
        let Some(fact_id) = evidence.fact_id else {
            continue;
        };
        merge_ranked_reference(
            &mut fact_refs,
            fact_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_evidence"),
        );
    }
    for (index, fact_id) in bundle.bundle.selected_fact_ids.iter().copied().enumerate() {
        let score = fact_refs.get(&fact_id).map_or(1.0, |reference| reference.score.max(1.0));
        merge_ranked_reference(
            &mut fact_refs,
            fact_id,
            saturating_rank(index),
            score,
            "bundle_selected_fact",
        );
    }
    fact_refs
}

fn derive_block_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    evidence_rows: &[KnowledgeEvidenceRow],
    technical_fact_rows: &[KnowledgeTechnicalFactRow],
    chunk_rows: &[KnowledgeChunkRow],
) -> HashMap<Uuid, RankedBundleReference> {
    let mut block_refs = HashMap::<Uuid, RankedBundleReference>::new();
    let evidence_by_id = evidence_rows
        .iter()
        .map(|evidence| (evidence.evidence_id, evidence))
        .collect::<HashMap<_, _>>();
    for reference in &bundle.evidence_references {
        let Some(evidence) = evidence_by_id.get(&reference.evidence_id) else {
            continue;
        };
        let Some(block_id) = evidence.block_id else {
            continue;
        };
        merge_ranked_reference(
            &mut block_refs,
            block_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_evidence"),
        );
    }
    let fact_rank_refs = derive_fact_rank_refs(bundle, evidence_rows);
    for fact in technical_fact_rows {
        let rank = fact_rank_for_bundle(&fact_rank_refs, fact.fact_id);
        let score = fact_score_for_bundle(&fact_rank_refs, fact.fact_id).max(1.0);
        for block_id in &fact.support_block_ids {
            merge_ranked_reference(
                &mut block_refs,
                *block_id,
                rank,
                score,
                "technical_fact_support",
            );
        }
    }
    let chunk_rank_refs = derive_chunk_rank_refs(bundle);
    for chunk in chunk_rows {
        let Some(reference) = chunk_rank_refs.get(&chunk.chunk_id) else {
            continue;
        };
        for block_id in &chunk.support_block_ids {
            merge_ranked_reference(
                &mut block_refs,
                *block_id,
                reference.rank,
                reference.score.max(1.0),
                "selected_chunk_support",
            );
        }
    }
    block_refs
}

fn selected_fact_ids_for_detail(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    fact_rank_refs: &HashMap<Uuid, RankedBundleReference>,
) -> Vec<Uuid> {
    let mut fact_ids = bundle.bundle.selected_fact_ids.clone();
    for fact_id in top_ranked_ids(fact_rank_refs, MAX_DETAIL_TECHNICAL_FACT_REFERENCES) {
        if fact_ids.len() >= MAX_DETAIL_TECHNICAL_FACT_REFERENCES {
            break;
        }
        if !fact_ids.contains(&fact_id) {
            fact_ids.push(fact_id);
        }
    }
    fact_ids.truncate(MAX_DETAIL_TECHNICAL_FACT_REFERENCES);
    fact_ids
}

fn build_prepared_segment_references(
    bundle: Option<&KnowledgeContextBundleReferenceSetRow>,
    blocks: &[KnowledgeStructuredBlockRow],
    block_rank_refs: &HashMap<Uuid, RankedBundleReference>,
    query_text: &str,
) -> Vec<PreparedSegmentReference> {
    let Some(bundle) = bundle else {
        return Vec::new();
    };
    let execution_id = bundle
        .bundle
        .query_execution_id
        .expect("query context bundle must carry query_execution_id");
    let query_focus_tokens = prepared_segment_focus_tokens(query_text);
    let mut revision_focus_scores = HashMap::<Uuid, usize>::new();
    for block in blocks {
        if !block_rank_refs.contains_key(&block.block_id) {
            continue;
        }
        let focus_score = prepared_segment_focus_score(&query_focus_tokens, block);
        if focus_score == 0 {
            continue;
        }
        revision_focus_scores
            .entry(block.revision_id)
            .and_modify(|current| *current = (*current).max(focus_score))
            .or_insert(focus_score);
    }
    let max_revision_focus_score = revision_focus_scores.values().copied().max().unwrap_or(0);
    let mut items = blocks
        .iter()
        .filter_map(|block| {
            let reference = block_rank_refs.get(&block.block_id)?;
            if max_revision_focus_score >= 2
                && revision_focus_scores.get(&block.revision_id).copied().unwrap_or(0)
                    < max_revision_focus_score
            {
                return None;
            }
            let block_kind = block.block_kind.parse().ok()?;
            let reference = PreparedSegmentReference {
                execution_id,
                segment_id: block.block_id,
                revision_id: block.revision_id,
                block_kind,
                rank: reference.rank,
                score: reference.score,
                heading_trail: block.heading_trail.clone(),
                section_path: block.section_path.clone(),
            };
            Some((
                reference,
                prepared_segment_focus_score(&query_focus_tokens, block),
                prepared_segment_kind_priority(&block.block_kind),
                block.ordinal,
            ))
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.rank.cmp(&right.0.rank))
            .then_with(|| right.0.score.total_cmp(&left.0.score))
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.segment_id.cmp(&right.0.segment_id))
    });
    let mut per_revision_counts = HashMap::<Uuid, usize>::new();
    let mut limited = Vec::with_capacity(items.len().min(MAX_DETAIL_PREPARED_SEGMENT_REFERENCES));
    for (reference, _, _, _) in items {
        let per_revision = per_revision_counts.entry(reference.revision_id).or_insert(0);
        if *per_revision >= MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION {
            continue;
        }
        limited.push(reference);
        *per_revision += 1;
        if limited.len() >= MAX_DETAIL_PREPARED_SEGMENT_REFERENCES {
            break;
        }
    }
    limited
}

fn prepared_segment_focus_tokens(query_text: &str) -> BTreeSet<String> {
    normalize_graph_identity_component(query_text)
        .split('_')
        .filter(|token| token.len() >= 3)
        .filter(|token| !PREPARED_SEGMENT_FOCUS_STOPWORDS.contains(token))
        .map(str::to_string)
        .collect()
}

fn prepared_segment_focus_score(
    query_focus_tokens: &BTreeSet<String>,
    block: &KnowledgeStructuredBlockRow,
) -> usize {
    if query_focus_tokens.is_empty() {
        return 0;
    }
    let mut focus_haystack = String::new();
    if !block.heading_trail.is_empty() {
        focus_haystack.push_str(&block.heading_trail.join(" "));
        focus_haystack.push(' ');
    }
    if !block.section_path.is_empty() {
        focus_haystack.push_str(&block.section_path.join(" "));
    }
    let normalized_focus_haystack = normalize_graph_identity_component(&focus_haystack);
    let block_tokens = normalized_focus_haystack
        .split('_')
        .filter(|token| !token.is_empty())
        .collect::<BTreeSet<_>>();
    query_focus_tokens.iter().filter(|token| block_tokens.contains(token.as_str())).count()
}

fn prepared_segment_kind_priority(block_kind: &str) -> u8 {
    match block_kind {
        "heading" | "endpoint_block" => 4,
        "paragraph" | "code_block" | "table_row" => 3,
        "list_item" | "table" => 2,
        "quote_block" | "metadata_block" => 1,
        _ => 0,
    }
}

fn build_technical_fact_references(
    bundle: Option<&KnowledgeContextBundleReferenceSetRow>,
    facts: &[KnowledgeTechnicalFactRow],
    fact_rank_refs: &HashMap<Uuid, RankedBundleReference>,
) -> Vec<TechnicalFactReference> {
    let Some(bundle) = bundle else {
        return Vec::new();
    };
    let execution_id = bundle
        .bundle
        .query_execution_id
        .expect("query context bundle must carry query_execution_id");
    let mut items = facts
        .iter()
        .filter_map(|fact| {
            let reference = fact_rank_refs.get(&fact.fact_id)?;
            Some(TechnicalFactReference {
                execution_id,
                fact_id: fact.fact_id,
                revision_id: fact.revision_id,
                fact_kind: fact.fact_kind.parse().ok()?,
                canonical_value: fact.canonical_value_text.clone(),
                display_value: fact.display_value.clone(),
                rank: reference.rank,
                score: reference.score,
            })
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    items
}

fn parse_query_verification_state(value: &str) -> QueryVerificationState {
    match value.trim().to_ascii_lowercase().as_str() {
        "verified" => QueryVerificationState::Verified,
        "partially_supported" => QueryVerificationState::PartiallySupported,
        "conflicting_evidence" | "conflicting" => QueryVerificationState::Conflicting,
        "insufficient_evidence" => QueryVerificationState::InsufficientEvidence,
        "failed" => QueryVerificationState::Failed,
        _ => QueryVerificationState::NotRun,
    }
}

fn parse_query_verification_warnings(
    value: &serde_json::Value,
) -> Vec<crate::domains::query::QueryVerificationWarning> {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

fn freshness_snapshot_json(row: &KnowledgeLibraryGenerationRow) -> serde_json::Value {
    json!({
        "generationId": row.generation_id,
        "activeTextGeneration": row.active_text_generation,
        "activeVectorGeneration": row.active_vector_generation,
        "activeGraphGeneration": row.active_graph_generation,
        "degradedState": row.degraded_state,
        "updatedAt": row.updated_at,
    })
}

fn runtime_mode_label(mode: RuntimeQueryMode) -> &'static str {
    match mode {
        RuntimeQueryMode::Document => "document",
        RuntimeQueryMode::Local => "local",
        RuntimeQueryMode::Global => "global",
        RuntimeQueryMode::Hybrid => "hybrid",
        RuntimeQueryMode::Mix => "mix",
    }
}

fn map_chunk_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryChunkReference> {
    let execution_id = bundle
        .bundle
        .query_execution_id
        .expect("query context bundle must carry query_execution_id");
    bundle
        .chunk_references
        .iter()
        .map(|reference| QueryChunkReference {
            execution_id,
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

fn map_entity_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryGraphNodeReference> {
    let execution_id = bundle
        .bundle
        .query_execution_id
        .expect("query context bundle must carry query_execution_id");
    bundle
        .entity_references
        .iter()
        .map(|reference| QueryGraphNodeReference {
            execution_id,
            node_id: reference.entity_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

fn map_relation_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryGraphEdgeReference> {
    let execution_id = bundle
        .bundle
        .query_execution_id
        .expect("query context bundle must carry query_execution_id");
    bundle
        .relation_references
        .iter()
        .map(|reference| QueryGraphEdgeReference {
            execution_id,
            edge_id: reference.relation_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

fn map_conversation_row(row: query_repository::QueryConversationRow) -> QueryConversation {
    QueryConversation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        created_by_principal_id: row.created_by_principal_id,
        title: row.title,
        conversation_state: row.conversation_state,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_turn_row(row: query_repository::QueryTurnRow) -> QueryTurn {
    QueryTurn {
        id: row.id,
        conversation_id: row.conversation_id,
        turn_index: row.turn_index,
        turn_kind: row.turn_kind,
        author_principal_id: row.author_principal_id,
        content_text: row.content_text,
        execution_id: row.execution_id,
        created_at: row.created_at,
    }
}

fn map_execution_row(row: query_repository::QueryExecutionRow) -> QueryExecution {
    QueryExecution {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        conversation_id: row.conversation_id,
        context_bundle_id: row.context_bundle_id,
        request_turn_id: row.request_turn_id,
        response_turn_id: row.response_turn_id,
        binding_id: row.binding_id,
        runtime_execution_id: Some(row.runtime_execution_id),
        lifecycle_state: row.runtime_lifecycle_state,
        active_stage: row.runtime_active_stage,
        query_text: row.query_text,
        failure_code: row.failure_code,
        started_at: row.started_at,
        completed_at: row.completed_at,
    }
}

fn map_execution_runtime_summary(
    row: &query_repository::QueryExecutionRow,
    runtime_policy_rows: &[runtime_repository::RuntimePolicyDecisionRow],
) -> RuntimeExecutionSummary {
    RuntimeExecutionSummary {
        runtime_execution_id: row.runtime_execution_id,
        lifecycle_state: row.runtime_lifecycle_state,
        active_stage: row.runtime_active_stage,
        turn_budget: row.turn_budget,
        turn_count: row.turn_count,
        parallel_action_limit: row.parallel_action_limit,
        failure_code: row.failure_code.clone(),
        failure_summary_redacted: row.failure_summary_redacted.clone().or(row.failure_code.clone()),
        policy_summary: map_runtime_policy_summary(runtime_policy_rows),
        accepted_at: row.started_at,
        completed_at: row.completed_at,
    }
}

fn map_runtime_policy_summary(
    rows: &[runtime_repository::RuntimePolicyDecisionRow],
) -> RuntimePolicySummary {
    build_policy_summary(
        &rows
            .iter()
            .map(|row| RuntimePolicyDecision {
                id: row.id,
                runtime_execution_id: row.runtime_execution_id,
                stage_record_id: row.stage_record_id,
                action_record_id: row.action_record_id,
                target_kind: row.target_kind,
                decision_kind: row.decision_kind,
                reason_code: row.reason_code.clone(),
                reason_summary_redacted: row.reason_summary_redacted.clone(),
                created_at: row.created_at,
            })
            .collect::<Vec<_>>(),
    )
}

fn map_execution_runtime_stage_summaries(
    row: &query_repository::QueryExecutionRow,
    runtime_stage_records: &[runtime_repository::RuntimeStageRecordRow],
) -> Vec<QueryRuntimeStageSummary> {
    if !runtime_stage_records.is_empty() {
        let mut seen = BTreeSet::new();
        return runtime_stage_records
            .iter()
            .map(|record| record.stage_kind)
            .filter(|stage_kind| seen.insert(*stage_kind))
            .map(|stage_kind| QueryRuntimeStageSummary {
                stage_kind,
                stage_label: query_runtime_stage_label(stage_kind).to_string(),
            })
            .collect();
    }

    row.runtime_active_stage
        .map(|stage_kind| {
            vec![QueryRuntimeStageSummary {
                stage_kind,
                stage_label: query_runtime_stage_label(stage_kind).to_string(),
            }]
        })
        .unwrap_or_default()
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToString::to_string)
}

fn normalize_required_text(value: &str, field: &str) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest(format!("{field} is required")));
    }
    Ok(normalized.to_string())
}

fn derive_conversation_title(value: &str) -> Option<String> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }

    let truncated = if collapsed.chars().count() <= QUERY_CONVERSATION_TITLE_LIMIT {
        collapsed
    } else {
        let cutoff = collapsed
            .char_indices()
            .nth(QUERY_CONVERSATION_TITLE_LIMIT)
            .map_or(collapsed.len(), |(index, _)| index);
        format!("{}…", collapsed[..cutoff].trim_end())
    };

    Some(truncated)
}

fn should_refresh_conversation_title(current: Option<&str>, candidate: &str) -> bool {
    match current {
        None => true,
        Some(current) => {
            is_weak_conversation_title(current) && !is_weak_conversation_title(candidate)
        }
    }
}

fn is_weak_conversation_title(value: &str) -> bool {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return true;
    }
    let chars = collapsed.chars().count();
    let words = collapsed.split_whitespace().count();
    chars <= 6 || (words <= 1 && chars <= 14)
}

fn build_conversation_runtime_context(
    turns: &[query_repository::QueryTurnRow],
    current_turn_id: Uuid,
) -> ConversationRuntimeContext {
    if turns.is_empty() {
        return ConversationRuntimeContext {
            effective_query_text: String::new(),
            prompt_history_text: None,
        };
    }
    let current_index = turns
        .iter()
        .position(|turn| turn.id == current_turn_id)
        .unwrap_or_else(|| turns.len().saturating_sub(1));
    let relevant_turns = &turns[..=current_index.min(turns.len().saturating_sub(1))];
    let current_turn = relevant_turns.last();
    let current_text = current_turn
        .map(|turn| turn.content_text.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let previous_turns =
        relevant_turns[..relevant_turns.len().saturating_sub(1)].iter().collect::<Vec<_>>();
    let prompt_history_text = render_turn_history(
        &previous_turns,
        MAX_PROMPT_HISTORY_TURNS,
        MAX_PROMPT_HISTORY_TURN_CHARS,
    );

    let effective_query_text = if is_context_dependent_follow_up(&current_text) {
        render_effective_query_text(&previous_turns, &current_text)
            .unwrap_or_else(|| current_text.clone())
    } else {
        current_text.clone()
    };

    ConversationRuntimeContext { effective_query_text, prompt_history_text }
}

fn render_effective_query_text(
    previous_turns: &[&query_repository::QueryTurnRow],
    current_text: &str,
) -> Option<String> {
    let mut lines = previous_turns
        .iter()
        .rev()
        .filter_map(|turn| {
            let text =
                compact_conversation_turn_text(&turn.content_text, MAX_EFFECTIVE_QUERY_TURN_CHARS);
            (!text.is_empty()).then_some(text)
        })
        .take(MAX_EFFECTIVE_QUERY_HISTORY_TURNS)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    lines.push(current_text.to_string());
    Some(lines.join("\n"))
}

fn render_turn_history(
    turns: &[&query_repository::QueryTurnRow],
    limit: usize,
    max_chars_per_turn: usize,
) -> Option<String> {
    let selected = turns
        .iter()
        .rev()
        .filter_map(|turn| {
            let text = compact_conversation_turn_text(&turn.content_text, max_chars_per_turn);
            (!text.is_empty())
                .then(|| format!("{}: {}", conversation_turn_speaker(&turn.turn_kind), text))
        })
        .take(limit)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        None
    } else {
        Some(selected.into_iter().rev().collect::<Vec<_>>().join("\n"))
    }
}

fn compact_conversation_turn_text(value: &str, max_chars: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let cutoff =
        collapsed.char_indices().nth(max_chars).map_or(collapsed.len(), |(index, _)| index);
    format!("{}…", collapsed[..cutoff].trim_end())
}

fn conversation_turn_speaker(turn_kind: &QueryTurnKind) -> &'static str {
    match turn_kind {
        QueryTurnKind::Assistant => "Assistant",
        _ => "User",
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
    let _ = state
        .canonical_services
        .audit
        .append_event(
            state,
            crate::services::audit_service::AppendAuditEventCommand {
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
        .await;
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
                crate::domains::agent_runtime::RuntimeDecisionKind::Reject => {
                    "runtime_policy_rejected"
                }
                crate::domains::agent_runtime::RuntimeDecisionKind::Terminate => {
                    "runtime_policy_terminated"
                }
                crate::domains::agent_runtime::RuntimeDecisionKind::Allow => {
                    "runtime_policy_blocked"
                }
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

fn query_runtime_stage_label(stage_kind: RuntimeStageKind) -> &'static str {
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

fn is_context_dependent_follow_up(value: &str) -> bool {
    const EXPLICIT_FOLLOW_UP_MARKERS: &[&str] = &[
        "да",
        "давай",
        "ага",
        "угу",
        "ок",
        "okay",
        "ok",
        "хорошо",
        "продолжай",
        "продолжи",
        "дальше",
        "ещё",
        "еще",
        "подробнее",
        "детальнее",
        "распиши",
        "пошагово",
        "покажи",
        "поясни",
        "continue",
        "go on",
        "more",
        "show me",
        "walk me through",
    ];
    const CONTEXT_WORDS: &[&str] = &[
        "это",
        "этот",
        "эта",
        "эту",
        "этом",
        "этим",
        "эти",
        "того",
        "такое",
        "такой",
        "так",
        "там",
        "тут",
        "сюда",
        "туда",
        "дальше",
        "потом",
        "здесь",
        "here",
        "there",
        "this",
        "that",
        "it",
        "them",
        "those",
        "same",
        "again",
        "further",
    ];
    const LOW_SIGNAL_WORDS: &[&str] =
        &["а", "и", "ну", "же", "ли", "бы", "please", "just", "the", "this", "that", "it"];

    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    let tokens = normalized
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return false;
    }
    if EXPLICIT_FOLLOW_UP_MARKERS.iter().any(|marker| {
        marker
            .contains(' ')
            .then_some(normalized.contains(marker))
            .unwrap_or_else(|| tokens.iter().any(|token| token == marker))
    }) {
        return true;
    }
    let informative_tokens = tokens
        .iter()
        .filter(|token| token.chars().count() >= 4 && !LOW_SIGNAL_WORDS.contains(token))
        .count();
    tokens.len() <= 6
        && (informative_tokens <= 1 || tokens.iter().any(|token| CONTEXT_WORDS.contains(token)))
}

fn saturating_rank(index: usize) -> i32 {
    i32::try_from(index.saturating_add(1)).unwrap_or(i32::MAX)
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
    execution_id: Uuid,
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn derive_fact_rank_refs_merges_evidence_and_selected_fact_ids() {
        let bundle_id = Uuid::now_v7();
        let execution_id = Uuid::now_v7();
        let fact_id = Uuid::now_v7();
        let evidence_id = Uuid::now_v7();
        let bundle = KnowledgeContextBundleReferenceSetRow {
            bundle: KnowledgeContextBundleRow {
                key: bundle_id.to_string(),
                arango_id: None,
                arango_rev: None,
                bundle_id,
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                query_execution_id: Some(execution_id),
                bundle_state: "ready".to_string(),
                bundle_strategy: "hybrid".to_string(),
                requested_mode: "mix".to_string(),
                resolved_mode: "mix".to_string(),
                selected_fact_ids: vec![fact_id],
                verification_state: "not_run".to_string(),
                verification_warnings: json!([]),
                freshness_snapshot: json!({}),
                candidate_summary: json!({}),
                assembly_diagnostics: json!({}),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            chunk_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            evidence_references: vec![
                crate::infra::arangodb::context_store::KnowledgeBundleEvidenceReferenceRow {
                    key: format!("{bundle_id}:{evidence_id}"),
                    bundle_id,
                    evidence_id,
                    rank: 2,
                    score: 42.0,
                    inclusion_reason: Some("relation_evidence".to_string()),
                    created_at: Utc::now(),
                },
            ],
        };
        let evidence_rows = vec![KnowledgeEvidenceRow {
            key: evidence_id.to_string(),
            arango_id: None,
            arango_rev: None,
            evidence_id,
            workspace_id: bundle.bundle.workspace_id,
            library_id: bundle.bundle.library_id,
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_id: None,
            block_id: Some(Uuid::now_v7()),
            fact_id: Some(fact_id),
            span_start: None,
            span_end: None,
            quote_text: "GET /api/status".to_string(),
            literal_spans_json: json!([]),
            evidence_kind: "relation_fact_support".to_string(),
            extraction_method: "graph_extract".to_string(),
            confidence: Some(0.9),
            evidence_state: "active".to_string(),
            freshness_generation: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];

        let fact_refs = derive_fact_rank_refs(&bundle, &evidence_rows);
        let reference = fact_refs.get(&fact_id).expect("fact reference");
        assert_eq!(reference.rank, 1);
        assert!(reference.score >= 42.0);
    }

    #[test]
    fn selected_fact_ids_for_detail_stays_bounded_to_canonical_limit() {
        let bundle_id = Uuid::now_v7();
        let execution_id = Uuid::now_v7();
        let selected_fact_id = Uuid::now_v7();
        let bundle = KnowledgeContextBundleReferenceSetRow {
            bundle: KnowledgeContextBundleRow {
                key: bundle_id.to_string(),
                arango_id: None,
                arango_rev: None,
                bundle_id,
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                query_execution_id: Some(execution_id),
                bundle_state: "ready".to_string(),
                bundle_strategy: "hybrid".to_string(),
                requested_mode: "mix".to_string(),
                resolved_mode: "mix".to_string(),
                selected_fact_ids: vec![selected_fact_id],
                verification_state: "not_run".to_string(),
                verification_warnings: json!([]),
                freshness_snapshot: json!({}),
                candidate_summary: json!({}),
                assembly_diagnostics: json!({}),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            chunk_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            evidence_references: Vec::new(),
        };
        let fact_rank_refs = (0..40)
            .map(|index| {
                (
                    Uuid::now_v7(),
                    RankedBundleReference {
                        rank: index + 1,
                        score: 100.0 - index as f64,
                        reasons: BTreeSet::from(["test".to_string()]),
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        let fact_ids = selected_fact_ids_for_detail(&bundle, &fact_rank_refs);
        assert_eq!(fact_ids.len(), MAX_DETAIL_TECHNICAL_FACT_REFERENCES);
        assert_eq!(fact_ids.first().copied(), Some(selected_fact_id));
    }

    #[test]
    fn build_prepared_segment_references_prioritizes_query_matching_headings_and_limits_revision_fanout()
     {
        let bundle_id = Uuid::now_v7();
        let execution_id = Uuid::now_v7();
        let telegram_revision_id = Uuid::now_v7();
        let control_revision_id = Uuid::now_v7();
        let bundle = KnowledgeContextBundleReferenceSetRow {
            bundle: KnowledgeContextBundleRow {
                key: bundle_id.to_string(),
                arango_id: None,
                arango_rev: None,
                bundle_id,
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                query_execution_id: Some(execution_id),
                bundle_state: "ready".to_string(),
                bundle_strategy: "hybrid".to_string(),
                requested_mode: "mix".to_string(),
                resolved_mode: "mix".to_string(),
                selected_fact_ids: Vec::new(),
                verification_state: "not_run".to_string(),
                verification_warnings: json!([]),
                freshness_snapshot: json!({}),
                candidate_summary: json!({}),
                assembly_diagnostics: json!({}),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            chunk_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            evidence_references: Vec::new(),
        };
        let mut block_rank_refs = HashMap::new();
        let mut blocks = Vec::new();
        for ordinal in 0..12_i32 {
            let block_id = Uuid::now_v7();
            blocks.push(KnowledgeStructuredBlockRow {
                key: block_id.to_string(),
                arango_id: None,
                arango_rev: None,
                block_id,
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                revision_id: telegram_revision_id,
                ordinal,
                block_kind: if ordinal == 0 {
                    "heading".to_string()
                } else {
                    "list_item".to_string()
                },
                text: "telegram".to_string(),
                normalized_text: "telegram".to_string(),
                heading_trail: vec!["Acme Telegram Bot - Example".to_string()],
                section_path: vec!["acme-telegram-bot-example".to_string()],
                page_number: None,
                span_start: None,
                span_end: None,
                parent_block_id: None,
                table_coordinates_json: None,
                code_language: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            });
            block_rank_refs.insert(
                block_id,
                RankedBundleReference {
                    rank: 1,
                    score: 100.0 - ordinal as f64,
                    reasons: BTreeSet::from(["test".to_string()]),
                },
            );
        }
        let control_heading_id = Uuid::now_v7();
        blocks.push(KnowledgeStructuredBlockRow {
            key: control_heading_id.to_string(),
            arango_id: None,
            arango_rev: None,
            block_id: control_heading_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: control_revision_id,
            ordinal: 0,
            block_kind: "heading".to_string(),
            text: "control center".to_string(),
            normalized_text: "control center".to_string(),
            heading_trail: vec!["Acme Control Center - Example".to_string()],
            section_path: vec!["acme-control-center-example".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });
        block_rank_refs.insert(
            control_heading_id,
            RankedBundleReference {
                rank: 2,
                score: 90.0,
                reasons: BTreeSet::from(["test".to_string()]),
            },
        );

        let references = build_prepared_segment_references(
            Some(&bundle),
            &blocks,
            &block_rank_refs,
            "Что такое Acme Control Center?",
        );

        assert_eq!(
            references.first().map(|reference| reference.heading_trail.first().cloned()).flatten(),
            Some("Acme Control Center - Example".to_string())
        );
        assert!(
            references.iter().all(|reference| reference.revision_id == control_revision_id),
            "focused query should retain only the best matching revision when focus is explicit"
        );
        assert!(references.len() <= MAX_DETAIL_PREPARED_SEGMENT_REFERENCES);
        assert!(
            references
                .iter()
                .filter(|reference| reference.revision_id == telegram_revision_id)
                .count()
                <= MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION
        );
    }

    #[test]
    fn parse_query_verification_state_maps_canonical_values() {
        assert_eq!(parse_query_verification_state("verified"), QueryVerificationState::Verified);
        assert_eq!(
            parse_query_verification_state("insufficient_evidence"),
            QueryVerificationState::InsufficientEvidence
        );
        assert_eq!(parse_query_verification_state("unknown"), QueryVerificationState::NotRun);
    }

    #[test]
    fn build_conversation_runtime_context_rewrites_short_follow_up_from_history() {
        let conversation_id = Uuid::now_v7();
        let first_user_turn = query_repository::QueryTurnRow {
            id: Uuid::now_v7(),
            conversation_id,
            turn_index: 1,
            turn_kind: QueryTurnKind::User,
            author_principal_id: None,
            content_text: "как в далионе перемещение сделать скажи".to_string(),
            execution_id: None,
            created_at: Utc::now(),
        };
        let assistant_turn = query_repository::QueryTurnRow {
            id: Uuid::now_v7(),
            conversation_id,
            turn_index: 2,
            turn_kind: QueryTurnKind::Assistant,
            author_principal_id: None,
            content_text: "Могу сразу расписать это пошагово для Далиона.".to_string(),
            execution_id: Some(Uuid::now_v7()),
            created_at: Utc::now(),
        };
        let follow_up_turn = query_repository::QueryTurnRow {
            id: Uuid::now_v7(),
            conversation_id,
            turn_index: 3,
            turn_kind: QueryTurnKind::User,
            author_principal_id: None,
            content_text: "давай".to_string(),
            execution_id: None,
            created_at: Utc::now(),
        };

        let context = build_conversation_runtime_context(
            &[first_user_turn, assistant_turn, follow_up_turn.clone()],
            follow_up_turn.id,
        );

        assert!(context.effective_query_text.contains("как в далионе перемещение сделать скажи"));
        assert!(
            context.effective_query_text.contains("Могу сразу расписать это пошагово для Далиона.")
        );
        assert!(context.effective_query_text.ends_with("давай"));
        assert_eq!(
            context.prompt_history_text.as_deref(),
            Some(
                "User: как в далионе перемещение сделать скажи\nAssistant: Могу сразу расписать это пошагово для Далиона."
            )
        );
    }

    #[test]
    fn build_conversation_runtime_context_keeps_standalone_question_without_rewrite() {
        let conversation_id = Uuid::now_v7();
        let first_turn = query_repository::QueryTurnRow {
            id: Uuid::now_v7(),
            conversation_id,
            turn_index: 1,
            turn_kind: QueryTurnKind::User,
            author_principal_id: None,
            content_text: "как перемещение оформить".to_string(),
            execution_id: None,
            created_at: Utc::now(),
        };
        let second_turn = query_repository::QueryTurnRow {
            id: Uuid::now_v7(),
            conversation_id,
            turn_index: 2,
            turn_kind: QueryTurnKind::User,
            author_principal_id: None,
            content_text: "как в далионе перемещение сделать скажи".to_string(),
            execution_id: None,
            created_at: Utc::now(),
        };

        let context =
            build_conversation_runtime_context(&[first_turn, second_turn.clone()], second_turn.id);

        assert_eq!(context.effective_query_text, "как в далионе перемещение сделать скажи");
        assert_eq!(context.prompt_history_text.as_deref(), Some("User: как перемещение оформить"));
    }
}
