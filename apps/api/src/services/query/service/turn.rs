use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use chrono::Utc;
use tokio::sync::mpsc::Sender;
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
    domains::catalog::CatalogLifecycleState,
    domains::query::{
        QueryConversationState, QueryExecutionDetail, QueryTurnKind, QueryVerificationState,
        QueryVerificationWarning, resolve_top_k,
    },
    domains::query_ir::literal_text_is_identifier_shaped,
    domains::{
        agent_runtime::{
            RuntimeDecisionKind, RuntimeExecutionOwner, RuntimeStageKind, RuntimeStageState,
            RuntimeSurfaceKind, RuntimeTaskKind,
        },
        ai::AiBindingPurpose,
    },
    infra::{
        arangodb::{
            collections::{
                KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                KNOWLEDGE_ENTITY_COLLECTION, KNOWLEDGE_EVIDENCE_COLLECTION,
                KNOWLEDGE_RELATION_COLLECTION,
            },
            context_store::{
                KnowledgeBundleChunkEdgeRow, KnowledgeBundleChunkReferenceRow,
                KnowledgeBundleEntityEdgeRow, KnowledgeBundleEntityReferenceRow,
                KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleEvidenceReferenceRow,
                KnowledgeBundleRelationEdgeRow, KnowledgeBundleRelationReferenceRow,
                KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow,
            },
        },
        repositories::{
            ai_repository, catalog_repository, query_repository, query_result_cache_repository,
            runtime_repository,
        },
    },
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    services::{
        ingest::runtime::bounded_runtime_overrides,
        mcp::access::library_catalog_ref,
        ops::billing::{CaptureExecutionBillingCommand, CaptureQueryExecutionBillingCommand},
        ops::service::CreateAsyncOperationCommand,
        query::{
            agent_loop::{
                AgentLoopActivityEvent, AgentTurnFailure, McpToolAgentTurnInput,
                run_literal_fidelity_revision_turn, run_literal_inventory_coverage_revision_turn,
                run_mcp_tool_agent_turn,
            },
            assistant_grounding::AssistantGroundingEvidence,
            execution::{
                CanonicalAnswerEvidence, RuntimeAnswerQueryResult, RuntimeAnswerVerification,
                generate_answer_query, literal_revision_targets, persist_query_verification,
                prepare_answer_query, verify_answer_against_canonical_evidence,
            },
            planner::QueryIntentProfile,
            result_cache,
        },
    },
};

use super::{
    CANONICAL_QUERY_MODE, ConversationRuntimeContext, ExecuteConversationTurnCommand,
    ExternalConversationTurn, QueryService, QueryTurnExecutionResult,
    context::{assemble_context_bundle, load_execution_prepared_reference_context},
    formatting::{
        build_assistant_document_references, build_prepared_segment_references,
        build_technical_fact_references, hydrate_entity_references, hydrate_relation_references,
        map_chunk_references, map_entity_references, map_execution_runtime_stage_summaries,
        map_execution_runtime_summary, map_relation_references, parse_query_verification_state,
        parse_query_verification_warnings, search_runtime_graph_entity_references,
    },
    session::{
        build_conversation_runtime_context,
        build_conversation_runtime_context_with_external_history, derive_conversation_title,
        enrich_query_with_coreference_entities, load_prior_grounded_answer_context_messages,
        map_conversation_row, map_execution_row, map_turn_row, normalize_required_text,
        should_refresh_conversation_title,
    },
};

const REFERENCE_CONTEXT_HYDRATION_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const ASSISTANT_AGENT_LOOP_DEADLINE_MS: u64 = 180_000;
pub(crate) const ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS: u64 = 35_000;
const ASSISTANT_LITERAL_INVENTORY_REVISION_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone)]
struct QueryResultCacheContext {
    cache_key: String,
    readable_content_fingerprint: String,
    graph_projection_version: i64,
    graph_topology_generation: i64,
    binding_fingerprint: String,
}

impl QueryService {
    pub async fn execute_grounded_answer_turn(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        self.execute_grounded_answer_pipeline(state, command).await
    }

    pub async fn execute_assistant_agent_turn(
        &self,
        state: &AppState,
        auth: &AuthContext,
        command: ExecuteConversationTurnCommand,
        activity_tx: Option<Sender<AgentLoopActivityEvent>>,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        let turn_started_at = std::time::Instant::now();
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
        let conversation_context = if command.external_prior_turns.is_empty() {
            build_conversation_runtime_context(&conversation_turns, request_turn.id)
        } else {
            build_conversation_runtime_context_with_external_history(
                &conversation_turns,
                request_turn.id,
                &command.external_prior_turns,
            )
        };

        let binding_id = ai_repository::get_effective_binding_assignment_by_purpose(
            &state.persistence.postgres,
            conversation.library_id,
            AiBindingPurpose::QueryAnswer.as_str(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .map(|binding| binding.id);
        let top_k = resolve_top_k(Some(command.top_k));

        let workspace = catalog_repository::get_workspace_by_id(
            &state.persistence.postgres,
            library.workspace_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("workspace", library.workspace_id))?;
        let library_ref = library_catalog_ref(&workspace.slug, &library.slug);
        let prior_grounded_answer_context_messages = if conversation_context
            .prompt_history_messages
            .is_empty()
        {
            Vec::new()
        } else {
            match load_prior_grounded_answer_context_messages(state, conversation.id, library.id)
                .await
            {
                Ok(messages) => messages,
                Err(error) => {
                    warn!(
                        error = %error,
                        conversation_id = %conversation.id,
                        "failed to build prior grounded-answer context replay; continuing with text history only"
                    );
                    Vec::new()
                }
            }
        };
        let agent_conversation_history = conversation_context.prompt_history_messages.clone();

        let execution_id = Uuid::now_v7();
        let execution_context_bundle_id = Uuid::now_v7();
        let mut runtime_session = seed_query_runtime_session(
            state,
            execution_id,
            &conversation_context,
            command.surface_kind,
        )
        .await?;
        let runtime_execution_id = runtime_session.execution.id;
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
        let agent_request_id = execution.id.to_string();
        let async_operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: conversation.workspace_id,
                    library_id: Some(conversation.library_id),
                    operation_kind: "query_execution".to_string(),
                    surface_kind: command.surface_kind.as_str().to_string(),
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

        let outcome: RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> =
            if let Err(failure) = begin_query_runtime_stage(
                state.agent_runtime.executor(),
                &mut runtime_session,
                RuntimeStageKind::Answer,
            )
            .await
            {
                record_query_runtime_stage(
                    state.agent_runtime.executor(),
                    &mut runtime_session,
                    RuntimeStageKind::Answer,
                    RuntimeStageState::Failed,
                    false,
                    Some(&failure),
                    None,
                );
                make_query_terminal_failure_outcome(failure.clone())
            } else {
                let answer_started = Utc::now();
                match run_mcp_tool_agent_turn(McpToolAgentTurnInput {
                    state,
                    auth,
                    library_id: library.id,
                    library_ref: &library_ref,
                    user_question: &content_text,
                    conversation_history: &agent_conversation_history,
                    follow_up_context_messages: &prior_grounded_answer_context_messages,
                    grounded_answer_tool_history: &conversation_context
                        .grounded_answer_tool_history,
                    request_id: &agent_request_id,
                    grounded_answer_top_k: top_k,
                    iteration_cap: ui_agent_iteration_cap(),
                    max_parallel_actions: usize::from(QueryAnswerTask::spec().max_parallel_actions),
                    deadline: Duration::from_millis(ASSISTANT_AGENT_LOOP_DEADLINE_MS),
                    soft_final_answer_deadline: Some(Duration::from_millis(
                        ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS,
                    )),
                    activity_tx: activity_tx.clone(),
                })
                .await
                {
                    Ok(mut agent_result) => {
                        record_query_runtime_stage(
                            state.agent_runtime.executor(),
                            &mut runtime_session,
                            RuntimeStageKind::Answer,
                            RuntimeStageState::Completed,
                            false,
                            None,
                            Some(answer_started),
                        );
                        if let Err(error) =
                            crate::services::query::llm_context_debug::upsert_snapshot(
                                &state.persistence.postgres,
                                &crate::services::query::llm_context_debug::LlmContextSnapshot {
                                    execution_id: execution.id,
                                    library_id: library.id,
                                    question: content_text.clone(),
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
                            warn!(
                                error = %error,
                                execution_id = %execution.id,
                                "failed to persist UI agent LLM context snapshot"
                            );
                        }

                        let mut revised_agent_answer_for_snapshot = false;
                        let verification_outcome = if let Err(failure) = begin_query_runtime_stage(
                            state.agent_runtime.executor(),
                            &mut runtime_session,
                            RuntimeStageKind::Verify,
                        )
                        .await
                        {
                            record_query_runtime_stage(
                                state.agent_runtime.executor(),
                                &mut runtime_session,
                                RuntimeStageKind::Verify,
                                RuntimeStageState::Failed,
                                false,
                                Some(&failure),
                                None,
                            );
                            Some(make_query_terminal_failure_outcome(failure.clone()))
                        } else {
                            let verify_started = Utc::now();
                            let mut verify_failure = None;
                            let child_query_execution_ids =
                                agent_result.child_query_execution_ids.clone();
                            let has_verifiable_tool_evidence = agent_has_verifiable_tool_evidence(
                                &child_query_execution_ids,
                                &agent_result.assistant_grounding,
                            );
                            let tool_loop_called_any_tool = agent_result
                                .agent_loop
                                .as_ref()
                                .is_some_and(|metadata| metadata.tool_call_count > 0);
                            if !has_verifiable_tool_evidence {
                                let (verification_state, verification_warnings) =
                                    if tool_loop_called_any_tool {
                                        (
                                            QueryVerificationState::InsufficientEvidence,
                                            no_verifiable_tool_evidence_warnings(),
                                        )
                                    } else {
                                        (
                                            QueryVerificationState::InsufficientEvidence,
                                            no_agent_tool_evidence_warnings(),
                                        )
                                    };
                                if let Err(error) = ensure_agent_tool_context_bundle(
                                    state,
                                    &execution,
                                    execution_context_bundle_id,
                                    &agent_result.assistant_grounding,
                                    verification_state,
                                    verification_warnings,
                                )
                                .await
                                {
                                    verify_failure = Some(make_query_answer_failure(
                                        "query_agent_verify_failed",
                                        format!(
                                            "failed to mark UI agent answer as unverifiable: {error}"
                                        ),
                                    ));
                                }
                            } else if !child_query_execution_ids.is_empty() {
                                match materialize_agent_grounding_from_child_execution(
                                    state,
                                    &execution,
                                    execution_context_bundle_id,
                                    &child_query_execution_ids,
                                )
                                .await
                                {
                                    Ok(Some(materialized)) => {
                                        tracing::info!(
                                            execution_id = %execution.id,
                                            source_execution_id = %materialized.source_execution_id,
                                            primary_execution_id = %materialized.primary_execution_id,
                                            "attached child grounded-answer evidence to UI agent execution"
                                        );
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        verify_failure = Some(make_query_answer_failure(
                                            "query_agent_grounding_failed",
                                            format!(
                                                "failed to attach MCP tool evidence to UI agent execution: {error}"
                                            ),
                                        ));
                                    }
                                }
                            }
                            if verify_failure.is_none()
                                && agent_answer_requires_parent_tool_evidence_verification(
                                    has_verifiable_tool_evidence,
                                )
                            {
                                match verify_agent_answer_against_tool_evidence(
                                    state,
                                    &execution,
                                    execution_context_bundle_id,
                                    &agent_result.answer,
                                    &agent_result.assistant_grounding,
                                )
                                .await
                                {
                                    Ok(verification) => {
                                        if agent_verification_needs_literal_revision(&verification)
                                        {
                                            let prompt_context = agent_result
                                                .assistant_grounding
                                                .verification_corpus
                                                .join("\n\n");
                                            let revision_targets = literal_revision_targets(
                                                &agent_result.answer,
                                                &verification.unsupported_literals,
                                            );
                                            if !revision_targets.is_empty() {
                                                match run_literal_fidelity_revision_turn(
                                                    state,
                                                    library.id,
                                                    &content_text,
                                                    &conversation_context.prompt_history_messages,
                                                    &agent_result.answer,
                                                    &revision_targets,
                                                    &prompt_context,
                                                )
                                                .await
                                                {
                                                    Ok(revision) => {
                                                        match verify_agent_answer_against_tool_evidence(
                                                            state,
                                                            &execution,
                                                            execution_context_bundle_id,
                                                            &revision.answer,
                                                            &agent_result.assistant_grounding,
                                                        )
                                                        .await
                                                        {
                                                            Ok(revised_verification)
                                                                if revised_verification.state
                                                                    == QueryVerificationState::Verified =>
                                                            {
                                                                tracing::info!(
                                                                    execution_id = %execution.id,
                                                                    unsupported_literals =
                                                                        verification.unsupported_literals.len(),
                                                                    revised_answer_chars =
                                                                        revision.answer.chars().count(),
                                                                    "accepted literal-fidelity revision for UI agent answer"
                                                                );
                                                                crate::services::query::agent_loop::merge_usage_into(
                                                                    &mut agent_result.usage_json,
                                                                    &revision.usage_json,
                                                                );
                                                                agent_result
                                                                    .debug_iterations
                                                                    .extend(revision.debug_iterations);
                                                                agent_result.answer = revision.answer;
                                                                revised_agent_answer_for_snapshot = true;
                                                            }
                                                            Ok(revised_verification) => {
                                                                tracing::info!(
                                                                    execution_id = %execution.id,
                                                                    original_state = ?verification.state,
                                                                    revised_state = ?revised_verification.state,
                                                                    original_warnings =
                                                                        verification.warnings.len(),
                                                                    revised_warnings =
                                                                        revised_verification.warnings.len(),
                                                                    "literal-fidelity revision for UI agent answer was not verified"
                                                                );
                                                            }
                                                            Err(error) => {
                                                                warn!(
                                                                    error = %error,
                                                                    execution_id = %execution.id,
                                                                    "failed to verify literal-fidelity revision for UI agent answer"
                                                                );
                                                            }
                                                        }
                                                    }
                                                    Err(error) => {
                                                        warn!(
                                                            error = %error,
                                                            execution_id = %execution.id,
                                                            "literal-fidelity revision failed for UI agent answer"
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        let coverage_targets =
                                            literal_inventory_coverage_revision_targets(
                                                &agent_result.answer,
                                                &conversation_context.grounded_answer_tool_history,
                                                &agent_result.assistant_grounding,
                                            );
                                        if !coverage_targets.is_empty() {
                                            let revision_context =
                                                literal_inventory_revision_context(
                                                    &agent_result.assistant_grounding,
                                                );
                                            let revision_started = Instant::now();
                                            match tokio::time::timeout(
                                                ASSISTANT_LITERAL_INVENTORY_REVISION_TIMEOUT,
                                                run_literal_inventory_coverage_revision_turn(
                                                    state,
                                                    library.id,
                                                    &content_text,
                                                    &conversation_context.prompt_history_messages,
                                                    &agent_result.answer,
                                                    &coverage_targets,
                                                    &revision_context,
                                                ),
                                            )
                                            .await
                                            {
                                                Ok(Ok(revision)) => {
                                                    match verify_agent_answer_against_tool_evidence(
                                                        state,
                                                        &execution,
                                                        execution_context_bundle_id,
                                                        &revision.answer,
                                                        &agent_result.assistant_grounding,
                                                    )
                                                    .await
                                                    {
                                                        Ok(revised_verification)
                                                            if revised_verification.state
                                                                == QueryVerificationState::Verified =>
                                                        {
                                                            tracing::info!(
                                                                stage = "literal_inventory_coverage_revision",
                                                                execution_id = %execution.id,
                                                                required_literal_count =
                                                                    coverage_targets.len(),
                                                                elapsed_ms = revision_started
                                                                    .elapsed()
                                                                    .as_millis(),
                                                                revised_answer_chars =
                                                                    revision.answer.chars().count(),
                                                                "accepted literal-inventory coverage revision for UI agent answer"
                                                            );
                                                            crate::services::query::agent_loop::merge_usage_into(
                                                                &mut agent_result.usage_json,
                                                                &revision.usage_json,
                                                            );
                                                            agent_result
                                                                .debug_iterations
                                                                .extend(revision.debug_iterations);
                                                            agent_result.answer = revision.answer;
                                                            revised_agent_answer_for_snapshot =
                                                                true;
                                                        }
                                                        Ok(revised_verification) => {
                                                            tracing::info!(
                                                                stage = "literal_inventory_coverage_revision",
                                                                execution_id = %execution.id,
                                                                revised_state = ?revised_verification.state,
                                                                revised_warnings =
                                                                    revised_verification.warnings.len(),
                                                                required_literal_count =
                                                                    coverage_targets.len(),
                                                                "literal-inventory coverage revision for UI agent answer was not verified"
                                                            );
                                                        }
                                                        Err(error) => {
                                                            warn!(
                                                                stage = "literal_inventory_coverage_revision",
                                                                error = %error,
                                                                execution_id = %execution.id,
                                                                "failed to verify literal-inventory coverage revision for UI agent answer"
                                                            );
                                                        }
                                                    }
                                                }
                                                Ok(Err(error)) => {
                                                    warn!(
                                                        stage = "literal_inventory_coverage_revision",
                                                        error = %error,
                                                        execution_id = %execution.id,
                                                        "literal-inventory coverage revision failed for UI agent answer"
                                                    );
                                                }
                                                Err(_) => {
                                                    warn!(
                                                        stage = "literal_inventory_coverage_revision",
                                                        execution_id = %execution.id,
                                                        timeout_ms = ASSISTANT_LITERAL_INVENTORY_REVISION_TIMEOUT.as_millis(),
                                                        elapsed_ms = revision_started.elapsed().as_millis(),
                                                        "literal-inventory coverage revision timed out for UI agent answer"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        verify_failure = Some(make_query_answer_failure(
                                            "query_agent_verify_failed",
                                            format!(
                                                "failed to verify UI agent answer against MCP tool evidence: {error}"
                                            ),
                                        ));
                                    }
                                }
                            }
                            if let Some(failure) = verify_failure {
                                record_query_runtime_stage(
                                    state.agent_runtime.executor(),
                                    &mut runtime_session,
                                    RuntimeStageKind::Verify,
                                    RuntimeStageState::Failed,
                                    false,
                                    Some(&failure),
                                    Some(verify_started),
                                );
                                Some(make_query_terminal_failure_outcome(failure))
                            } else {
                                record_query_runtime_stage(
                                    state.agent_runtime.executor(),
                                    &mut runtime_session,
                                    RuntimeStageKind::Verify,
                                    RuntimeStageState::Completed,
                                    false,
                                    None,
                                    Some(verify_started),
                                );
                                None
                            }
                        };

                        if verification_outcome.is_none() && revised_agent_answer_for_snapshot {
                            if let Err(error) =
                                crate::services::query::llm_context_debug::upsert_snapshot(
                                    &state.persistence.postgres,
                                    &crate::services::query::llm_context_debug::LlmContextSnapshot {
                                        execution_id: execution.id,
                                        library_id: library.id,
                                        question: content_text.clone(),
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
                                warn!(
                                    error = %error,
                                    execution_id = %execution.id,
                                    "failed to update UI agent LLM context snapshot after revision"
                                );
                            }
                        }

                        if let Some(outcome) = verification_outcome {
                            outcome
                        } else if let Err(failure) = begin_query_runtime_stage(
                            state.agent_runtime.executor(),
                            &mut runtime_session,
                            RuntimeStageKind::Persist,
                        )
                        .await
                        {
                            record_query_runtime_stage(
                                state.agent_runtime.executor(),
                                &mut runtime_session,
                                RuntimeStageKind::Persist,
                                RuntimeStageState::Failed,
                                true,
                                Some(&failure),
                                None,
                            );
                            make_query_terminal_failure_outcome(failure.clone())
                        } else {
                            let persist_started = Utc::now();
                            match persist_agent_answer(
                                state,
                                conversation.id,
                                execution.id,
                                request_turn.id,
                                &agent_result.answer,
                            )
                            .await
                            {
                                Ok(answer_text) => {
                                    record_query_runtime_stage(
                                        state.agent_runtime.executor(),
                                        &mut runtime_session,
                                        RuntimeStageKind::Persist,
                                        RuntimeStageState::Completed,
                                        true,
                                        None,
                                        Some(persist_started),
                                    );
                                    RuntimeTerminalOutcome::Completed {
                                        success: QueryAnswerTaskSuccess {
                                            answer_text,
                                            provider: agent_result.provider,
                                            usage_json: agent_result.usage_json,
                                        },
                                    }
                                }
                                Err(failure) => {
                                    record_query_runtime_stage(
                                        state.agent_runtime.executor(),
                                        &mut runtime_session,
                                        RuntimeStageKind::Persist,
                                        RuntimeStageState::Failed,
                                        true,
                                        Some(&failure),
                                        Some(persist_started),
                                    );
                                    make_query_terminal_failure_outcome(failure)
                                }
                            }
                        }
                    }
                    Err(agent_failure) => {
                        persist_failed_agent_debug_snapshot(
                            state,
                            execution.id,
                            library.id,
                            &content_text,
                            &agent_failure,
                        )
                        .await;
                        let failure = make_query_answer_failure(
                            "query_agent_loop_failed",
                            agent_failure.to_string(),
                        );
                        record_query_runtime_stage(
                            state.agent_runtime.executor(),
                            &mut runtime_session,
                            RuntimeStageKind::Answer,
                            RuntimeStageState::Failed,
                            false,
                            Some(&failure),
                            Some(answer_started),
                        );
                        make_query_terminal_failure_outcome(failure)
                    }
                }
            };

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
            RuntimeTerminalOutcome::Completed { success }
            | RuntimeTerminalOutcome::Recovered { success, .. } => {
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
                            call_kind: "query_agent".to_string(),
                            usage_json: success.usage_json.clone(),
                        },
                    )
                    .await
                {
                    warn!(error = %error, execution_id = %terminal_execution.id, "UI agent query billing capture failed");
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
        let total_ms = turn_started_at.elapsed().as_millis() as u64;
        tracing::info!(
            total_ms,
            execution_id = %terminal_execution.id,
            library_id = %terminal_execution.library_id,
            conversation_id = %terminal_execution.conversation_id,
            turn_count = terminal_execution.turn_count,
            stage_summary_count = detail.runtime_stage_summaries.len(),
            "query.agent_turn.completed"
        );

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

    async fn execute_grounded_answer_pipeline(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        // Wall-clock clock for the whole turn. Captured at entry so the
        // `query.turn.completed` structured log at the bottom of this
        // function can report `total_ms` alongside the per-stage numbers
        // already persisted on `runtime_stage_record`. Turn latency on
        // a reference library is ~40 s end-to-end; this single log line
        // lets operators see which phase dominated without cross-joining
        // the stage table manually.
        let turn_started_at = std::time::Instant::now();
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
        let conversation_context = if command.external_prior_turns.is_empty() {
            build_conversation_runtime_context(&conversation_turns, request_turn.id)
        } else {
            build_conversation_runtime_context_with_external_history(
                &conversation_turns,
                request_turn.id,
                &command.external_prior_turns,
            )
        };

        let binding_id = ai_repository::get_effective_binding_assignment_by_purpose(
            &state.persistence.postgres,
            conversation.library_id,
            AiBindingPurpose::QueryAnswer.as_str(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .map(|binding| binding.id);

        let top_k = resolve_top_k(Some(command.top_k));
        let cache_context = build_query_result_cache_context(
            state,
            &conversation,
            &conversation_context,
            &content_text,
            top_k,
        )
        .await
        .map_err(|error| {
            warn!(
                error = %error,
                conversation_id = %conversation.id,
                library_id = %conversation.library_id,
                "query result cache context unavailable"
            );
            ApiError::InternalMessage("query answer coordination is unavailable".to_string())
        })?;
        let mut _cache_fill_guard = None;
        {
            let cache_context_ref = &cache_context;
            if let Some(replayed) = self
                .try_replay_query_result_cache(
                    state,
                    cache_context_ref,
                    &conversation,
                    &request_turn,
                )
                .await?
            {
                return Ok(replayed);
            }
            let wait_started = std::time::Instant::now();
            loop {
                let lock_owner = Uuid::now_v7();
                match result_cache::try_acquire_fill_guard(
                    &state.persistence.redis,
                    &cache_context_ref.cache_key,
                    lock_owner,
                )
                .await
                {
                    Ok(Some(guard)) => {
                        _cache_fill_guard = Some(guard);
                        break;
                    }
                    Ok(None) => {
                        if wait_started.elapsed() >= result_cache::QUERY_RESULT_CACHE_WAIT_TIMEOUT {
                            warn!(
                                cache_key = %cache_context_ref.cache_key,
                                wait_ms = wait_started.elapsed().as_millis() as u64,
                                "query result cache fill wait timed out before source execution completed"
                            );
                            return Err(ApiError::Conflict(
                                "query answer is still being prepared".to_string(),
                            ));
                        }
                    }
                    Err(error) => {
                        warn!(
                            error = %error,
                            cache_key = %cache_context_ref.cache_key,
                            "query result cache fill lock unavailable"
                        );
                        return Err(ApiError::InternalMessage(
                            "query answer coordination is unavailable".to_string(),
                        ));
                    }
                }

                tokio::time::sleep(result_cache::QUERY_RESULT_CACHE_WAIT_INTERVAL).await;
                if let Some(replayed) = self
                    .try_replay_query_result_cache(
                        state,
                        cache_context_ref,
                        &conversation,
                        &request_turn,
                    )
                    .await?
                {
                    return Ok(replayed);
                }
            }
        }

        let execution_id = Uuid::now_v7();
        let execution_context_bundle_id = Uuid::now_v7();
        let mut runtime_session = seed_query_runtime_session(
            state,
            execution_id,
            &conversation_context,
            command.surface_kind,
        )
        .await?;
        let runtime_execution_id = runtime_session.execution.id;
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
                    library_id: Some(conversation.library_id),
                    operation_kind: "query_execution".to_string(),
                    surface_kind: command.surface_kind.as_str().to_string(),
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

        let mut query_embedding_usage = None;
        // Compile + embed both bill separately from answer generation:
        // different bindings, different models, different rates. We
        // hold the snapshots here so the capture rows on
        // `billing_provider_call` at the end of the turn attribute
        // each LLM call to the right `call_kind`.
        let mut query_compile_usage = None;
        let outcome: RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> = {
            if let Err(failure) = begin_query_runtime_stage(
                state.agent_runtime.executor(),
                &mut runtime_session,
                RuntimeStageKind::Retrieve,
            )
            .await
            {
                // policy-deny before stage work started; the zero-duration record is expected
                record_query_runtime_stage(
                    state.agent_runtime.executor(),
                    &mut runtime_session,
                    RuntimeStageKind::Retrieve,
                    RuntimeStageState::Failed,
                    true,
                    Some(&failure),
                    None,
                );
                make_query_terminal_failure_outcome(failure.clone())
            } else {
                let enriched_query_text = enrich_query_with_coreference_entities(
                    &conversation_context.effective_query_text,
                    &conversation_context.coreference_entities,
                );
                let retrieve_started = Utc::now();
                let prepared = match prepare_answer_query(
                    state,
                    library.id,
                    enriched_query_text,
                    conversation_context.query_planning_history_text.as_deref(),
                    CANONICAL_QUERY_MODE,
                    top_k,
                    command.include_debug,
                )
                .await
                {
                    Ok(mut result) => {
                        // Hand the captured retrieval spans to the snapshot
                        // writer for this execution so the inspector can show
                        // where time went (DB queries, lanes).
                        crate::services::query::turn_spans::stash_execution_spans(
                            execution.id,
                            std::mem::take(&mut result.retrieval_spans),
                        );
                        record_query_runtime_stage(
                            state.agent_runtime.executor(),
                            &mut runtime_session,
                            RuntimeStageKind::Retrieve,
                            RuntimeStageState::Completed,
                            true,
                            None,
                            Some(retrieve_started),
                        );
                        // Persist the final ranked chunks that shaped
                        // this execution's answer context so operators
                        // can trace which chunks grounded which answer
                        // long after the turn completed. One UNNEST
                        // insert regardless of bundle size; failures
                        // are logged (not fatal) — a missing audit
                        // row must never block the user's answer.
                        let chunk_refs: Vec<query_repository::NewQueryChunkReference> = result
                            .structured
                            .chunk_references
                            .iter()
                            .map(|reference| query_repository::NewQueryChunkReference {
                                chunk_id: reference.chunk_id,
                                rank: reference.rank,
                                score: reference.score,
                            })
                            .collect();
                        if let Err(error) = query_repository::append_chunk_references(
                            &state.persistence.postgres,
                            execution.id,
                            &chunk_refs,
                        )
                        .await
                        {
                            tracing::warn!(
                                %error,
                                execution_id = %execution.id,
                                chunk_count = chunk_refs.len(),
                                "failed to persist query_chunk_reference rows"
                            );
                        }
                        query_embedding_usage = result.embedding_usage.clone();
                        query_compile_usage = result.query_compile_usage.clone();
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
                            Some(retrieve_started),
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
                    // policy-deny before stage work started; the zero-duration record is expected
                    record_query_runtime_stage(
                        state.agent_runtime.executor(),
                        &mut runtime_session,
                        RuntimeStageKind::AssembleContext,
                        RuntimeStageState::Failed,
                        true,
                        Some(&failure),
                        None,
                    );
                    make_query_terminal_failure_outcome(failure.clone())
                } else {
                    let assemble_context_started = Utc::now();
                    match assemble_context_bundle(
                        state,
                        &conversation,
                        execution.id,
                        execution_context_bundle_id,
                        &conversation_context.effective_query_text,
                        &prepared.query_ir,
                        CANONICAL_QUERY_MODE,
                        top_k,
                        command.include_debug,
                        prepared.structured.planned_mode,
                        &prepared.structured.chunk_references,
                        &prepared.structured.graph_entity_references,
                        &prepared.structured.graph_relation_references,
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
                                Some(assemble_context_started),
                            );

                            if let Err(failure) = begin_query_runtime_stage(
                                state.agent_runtime.executor(),
                                &mut runtime_session,
                                RuntimeStageKind::Answer,
                            )
                            .await
                            {
                                // policy-deny before stage work started; the zero-duration record is expected
                                record_query_runtime_stage(
                                    state.agent_runtime.executor(),
                                    &mut runtime_session,
                                    RuntimeStageKind::Answer,
                                    RuntimeStageState::Failed,
                                    false,
                                    Some(&failure),
                                    None,
                                );
                                make_query_terminal_failure_outcome(failure.clone())
                            } else {
                                let answer_started = Utc::now();
                                match generate_answer_query(
                                    state,
                                    library.id,
                                    execution.id,
                                    &conversation_context.effective_query_text,
                                    &content_text,
                                    conversation_context.prompt_history_text.as_deref(),
                                    &conversation_context.prompt_history_messages,
                                    prepared,
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
                                            Some(answer_started),
                                        );
                                        let answer_text = answer;

                                        if let Err(failure) = begin_query_runtime_stage(
                                            state.agent_runtime.executor(),
                                            &mut runtime_session,
                                            RuntimeStageKind::Persist,
                                        )
                                        .await
                                        {
                                            // policy-deny before stage work started; the zero-duration record is expected
                                            record_query_runtime_stage(
                                                state.agent_runtime.executor(),
                                                &mut runtime_session,
                                                RuntimeStageKind::Persist,
                                                RuntimeStageState::Failed,
                                                true,
                                                Some(&failure),
                                                None,
                                            );
                                            make_query_terminal_failure_outcome(failure.clone())
                                        } else {
                                            let persist_started = Utc::now();
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
                                                                Some(persist_started),
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
                                                                Some(persist_started),
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
                                                                Some(persist_started),
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
                                                        Some(persist_started),
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
                                            Some(answer_started),
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
                                Some(assemble_context_started),
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
                            call_kind: "query_answer".to_string(),
                            usage_json: success.usage_json.clone(),
                        },
                    )
                    .await
                {
                    warn!(error = %error, execution_id = %terminal_execution.id, "query billing capture failed");
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
                                call_kind: "query_retrieve".to_string(),
                                usage_json: embed_usage.usage_json.clone(),
                            },
                        )
                        .await
                    {
                        warn!(error = %error, execution_id = %terminal_execution.id, "query embedding billing capture failed");
                    }
                }
                capture_query_compile_usage_if_any(
                    state,
                    &conversation,
                    &terminal_execution,
                    &runtime_result.execution,
                    query_compile_usage.as_ref(),
                )
                .await;
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
                            call_kind: "query_answer".to_string(),
                            usage_json: success.usage_json.clone(),
                        },
                    )
                    .await
                {
                    warn!(error = %error, execution_id = %terminal_execution.id, "query billing capture failed");
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
                                call_kind: "query_retrieve".to_string(),
                                usage_json: embed_usage.usage_json.clone(),
                            },
                        )
                        .await
                    {
                        warn!(error = %error, execution_id = %terminal_execution.id, "query embedding billing capture failed");
                    }
                }
                capture_query_compile_usage_if_any(
                    state,
                    &conversation,
                    &terminal_execution,
                    &runtime_result.execution,
                    query_compile_usage.as_ref(),
                )
                .await;
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
        store_query_result_cache_winner(state, &cache_context, &detail).await;
        let request_turn = detail.request_turn.ok_or(ApiError::Internal)?;

        // One structured log line at turn completion with total
        // wall-clock. Per-stage timings live on `runtime_stage_record`
        // in Postgres (written via `record_query_runtime_stage` during
        // each phase); this line lets operators filter
        // `query.turn.completed` to get one latency number per turn
        // without joining the stage table, then drill down if needed.
        let total_ms = turn_started_at.elapsed().as_millis() as u64;
        tracing::info!(
            total_ms,
            execution_id = %terminal_execution.id,
            library_id = %terminal_execution.library_id,
            conversation_id = %terminal_execution.conversation_id,
            turn_count = terminal_execution.turn_count,
            stage_summary_count = detail.runtime_stage_summaries.len(),
            "query.turn.completed"
        );

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

    async fn try_replay_query_result_cache(
        &self,
        state: &AppState,
        cache_context: &QueryResultCacheContext,
        conversation: &query_repository::QueryConversationRow,
        request_turn: &query_repository::QueryTurnRow,
    ) -> Result<Option<QueryTurnExecutionResult>, ApiError> {
        match result_cache::get_cached_execution_id(
            &state.persistence.redis,
            &cache_context.cache_key,
        )
        .await
        {
            Ok(Some(source_execution_id)) => {
                if let Some(replayed) = self
                    .replay_query_result_cache_hit(
                        state,
                        cache_context,
                        conversation,
                        request_turn,
                        source_execution_id,
                    )
                    .await?
                {
                    return Ok(Some(replayed));
                }
            }
            Ok(None) => {}
            Err(error) => {
                warn!(
                    error = %error,
                    cache_key = %cache_context.cache_key,
                    "redis query result cache read failed"
                );
            }
        }

        let cached = query_result_cache_repository::get_query_result_cache(
            &state.persistence.postgres,
            &cache_context.cache_key,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some(cached) = cached else {
            return Ok(None);
        };
        if let Err(error) = result_cache::put_cached_execution_id(
            &state.persistence.redis,
            &cache_context.cache_key,
            cached.source_execution_id,
        )
        .await
        {
            warn!(
                error = %error,
                cache_key = %cache_context.cache_key,
                source_execution_id = %cached.source_execution_id,
                "redis query result cache refresh failed"
            );
        }
        self.replay_query_result_cache_hit(
            state,
            cache_context,
            conversation,
            request_turn,
            cached.source_execution_id,
        )
        .await
    }

    async fn replay_query_result_cache_hit(
        &self,
        state: &AppState,
        cache_context: &QueryResultCacheContext,
        conversation: &query_repository::QueryConversationRow,
        request_turn: &query_repository::QueryTurnRow,
        source_execution_id: Uuid,
    ) -> Result<Option<QueryTurnExecutionResult>, ApiError> {
        let detail = match self.get_execution(state, source_execution_id).await {
            Ok(detail) => detail,
            Err(error) => {
                warn!(
                    error = %error,
                    cache_key = %cache_context.cache_key,
                    source_execution_id = %source_execution_id,
                    "query result cache source execution is unavailable"
                );
                evict_query_result_cache_entry(
                    state,
                    cache_context,
                    source_execution_id,
                    "source execution unavailable",
                )
                .await;
                return Ok(None);
            }
        };
        if detail.verification_state != QueryVerificationState::Verified {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution is not verified",
            )
            .await;
            return Ok(None);
        }
        if !query_detail_has_grounding_references(&detail) {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution has no grounding references",
            )
            .await;
            return Ok(None);
        }
        let Some(source_response_turn) = detail.response_turn.as_ref() else {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution has no response turn",
            )
            .await;
            return Ok(None);
        };
        let answer_text = source_response_turn.content_text.trim();
        if answer_text.is_empty() {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution answer is empty",
            )
            .await;
            return Ok(None);
        }

        let response_turn = query_repository::create_turn(
            &state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: conversation.id,
                turn_kind: "assistant",
                author_principal_id: None,
                content_text: answer_text,
                execution_id: Some(source_execution_id),
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        query_result_cache_repository::record_query_execution_replay(
            &state.persistence.postgres,
            &query_result_cache_repository::NewQueryExecutionReplay {
                workspace_id: conversation.workspace_id,
                library_id: conversation.library_id,
                conversation_id: conversation.id,
                request_turn_id: request_turn.id,
                response_turn_id: response_turn.id,
                source_execution_id,
                cache_key: &cache_context.cache_key,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        tracing::info!(
            stage = "query.result_cache.hit",
            cache_key = %cache_context.cache_key,
            source_execution_id = %source_execution_id,
            conversation_id = %conversation.id,
            request_turn_id = %request_turn.id,
            response_turn_id = %response_turn.id,
            "query result replayed from source execution"
        );

        Ok(Some(QueryTurnExecutionResult {
            conversation: map_conversation_row(conversation.clone()),
            request_turn: map_turn_row(request_turn.clone()),
            response_turn: Some(map_turn_row(response_turn)),
            context_bundle_id: detail.execution.context_bundle_id,
            execution: detail.execution,
            runtime_summary: detail.runtime_summary,
            runtime_stage_summaries: detail.runtime_stage_summaries,
            chunk_references: detail.chunk_references,
            prepared_segment_references: detail.prepared_segment_references,
            technical_fact_references: detail.technical_fact_references,
            graph_node_references: detail.graph_node_references,
            graph_edge_references: detail.graph_edge_references,
            verification_state: detail.verification_state,
            verification_warnings: detail.verification_warnings,
        }))
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
        let prepared_reference_context = match tokio::time::timeout(
            REFERENCE_CONTEXT_HYDRATION_TIMEOUT,
            load_execution_prepared_reference_context(state, execution.id),
        )
        .await
        {
            Ok(Ok(reference_context)) => reference_context,
            Ok(Err(error)) => {
                warn!(
                    execution_id = %execution.id,
                    error = %error,
                    "failed to resolve prepared references for query execution detail"
                );
                Default::default()
            }
            Err(_) => {
                warn!(
                    execution_id = %execution.id,
                    timeout_ms = REFERENCE_CONTEXT_HYDRATION_TIMEOUT.as_millis(),
                    "timed out resolving prepared references for query execution detail"
                );
                Default::default()
            }
        };

        let query_text = execution.query_text.clone();
        let has_prepared_bundle_refs = prepared_reference_context.bundle_refs.is_some();
        let mut graph_node_references = prepared_reference_context
            .bundle_refs
            .as_ref()
            .map_or_else(Vec::new, map_entity_references);

        if should_backfill_graph_entity_references(
            has_prepared_bundle_refs,
            graph_node_references.is_empty(),
        ) {
            graph_node_references = search_runtime_graph_entity_references(
                &state.persistence.postgres,
                execution.library_id,
                execution.id,
                &query_text,
            )
            .await;
        }
        let mut graph_edge_references = prepared_reference_context
            .bundle_refs
            .as_ref()
            .map_or_else(Vec::new, map_relation_references);
        if !graph_node_references.is_empty() || !graph_edge_references.is_empty() {
            let graph_projection_version = crate::infra::repositories::get_runtime_graph_snapshot(
                &state.persistence.postgres,
                execution.library_id,
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .map_or(0, |snapshot| snapshot.projection_version.max(0));
            graph_node_references = hydrate_entity_references(
                &state.persistence.postgres,
                execution.library_id,
                graph_projection_version,
                graph_node_references,
            )
            .await;
            graph_edge_references = hydrate_relation_references(
                &state.persistence.postgres,
                execution.library_id,
                graph_projection_version,
                graph_edge_references,
            )
            .await;
        }
        let chunk_references = prepared_reference_context
            .bundle_refs
            .as_ref()
            .map_or_else(Vec::new, map_chunk_references);
        let mut prepared_segment_references = build_prepared_segment_references(
            prepared_reference_context.bundle_refs.as_ref(),
            &prepared_reference_context.structured_block_rows,
            &prepared_reference_context.block_rank_refs,
            &query_text,
            &prepared_reference_context.segment_revision_info,
        );
        prepared_segment_references.extend(build_assistant_document_references(
            execution.id,
            &prepared_reference_context.assistant_document_references,
        ));
        let technical_fact_references = build_technical_fact_references(
            prepared_reference_context.bundle_refs.as_ref(),
            &prepared_reference_context.technical_fact_rows,
            &prepared_reference_context.fact_rank_refs,
        );
        let verification_state = prepared_reference_context
            .bundle_refs
            .as_ref()
            .map_or(QueryVerificationState::NotRun, |bundle| {
                parse_query_verification_state(&bundle.bundle.verification_state)
            });
        let verification_warnings =
            prepared_reference_context.bundle_refs.as_ref().map_or_else(Vec::new, |bundle| {
                parse_query_verification_warnings(&bundle.bundle.verification_warnings)
            });
        Ok(QueryExecutionDetail {
            execution: map_execution_row(execution.clone()),
            runtime_summary: map_execution_runtime_summary(&execution, &runtime_policy_rows),
            runtime_stage_summaries: map_execution_runtime_stage_summaries(
                &execution,
                &runtime_stage_records,
            ),
            request_turn,
            response_turn,
            chunk_references,
            prepared_segment_references,
            technical_fact_references,
            graph_node_references,
            graph_edge_references,
            verification_state,
            verification_warnings,
        })
    }
}

async fn build_query_result_cache_context(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    conversation_context: &ConversationRuntimeContext,
    user_question: &str,
    top_k: usize,
) -> anyhow::Result<QueryResultCacheContext> {
    let readable_content_fingerprint =
        crate::infra::repositories::content_repository::get_library_readable_content_fingerprint(
            &state.persistence.postgres,
            conversation.library_id,
        )
        .await?;
    let (graph_projection_version, graph_topology_generation) =
        crate::infra::repositories::get_runtime_graph_snapshot(
            &state.persistence.postgres,
            conversation.library_id,
        )
        .await?
        .map_or((0, 0), |snapshot| {
            (snapshot.projection_version.max(0), snapshot.topology_generation.max(0))
        });
    let binding_fingerprint =
        build_query_result_binding_fingerprint(state, conversation.library_id).await?;
    let cache_key = result_cache::cache_key(&result_cache::QueryResultCacheKeyInput {
        workspace_id: conversation.workspace_id,
        library_id: conversation.library_id,
        readable_content_fingerprint: &readable_content_fingerprint,
        graph_projection_version,
        binding_fingerprint: &binding_fingerprint,
        answer_system_prompt:
            crate::services::query::assistant_prompt::GROUNDED_SINGLE_SHOT_SYSTEM_PROMPT,
        answer_runtime_fingerprint: result_cache::answer_runtime_fingerprint(),
        mode_label: super::runtime_mode_label(CANONICAL_QUERY_MODE),
        top_k,
        user_question,
        effective_question: &conversation_context.effective_query_text,
        answer_history_text: conversation_context.prompt_history_text.as_deref(),
    });
    Ok(QueryResultCacheContext {
        cache_key,
        readable_content_fingerprint,
        graph_projection_version,
        graph_topology_generation,
        binding_fingerprint,
    })
}

async fn build_query_result_binding_fingerprint(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<String> {
    let mut parts = Vec::new();
    for purpose in [
        AiBindingPurpose::QueryCompile,
        AiBindingPurpose::ExtractGraph,
        AiBindingPurpose::EmbedChunk,
        AiBindingPurpose::QueryRetrieve,
        AiBindingPurpose::QueryAnswer,
    ] {
        let binding = ai_repository::get_effective_binding_assignment_by_purpose(
            &state.persistence.postgres,
            library_id,
            purpose.as_str(),
        )
        .await?;
        let part = match binding {
            Some(binding) => format!(
                "{}:{}:{}:{}:{}",
                purpose.as_str(),
                binding.id,
                binding.provider_credential_id,
                binding.model_preset_id,
                binding.updated_at.timestamp_micros()
            ),
            None => format!("{}:none", purpose.as_str()),
        };
        parts.push(part);
    }
    Ok(parts.join("|"))
}

async fn store_query_result_cache_winner(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
    detail: &QueryExecutionDetail,
) {
    if detail.verification_state != QueryVerificationState::Verified {
        return;
    }
    if !query_detail_has_grounding_references(detail) {
        return;
    }
    if detail.execution.failure_code.is_some() || detail.execution.runtime_execution_id.is_none() {
        return;
    }
    let Some(response_turn) = detail.response_turn.as_ref() else {
        return;
    };
    if response_turn.content_text.trim().is_empty() {
        return;
    }
    let row = match query_result_cache_repository::upsert_query_result_cache_winner(
        &state.persistence.postgres,
        &query_result_cache_repository::UpsertQueryResultCacheInput {
            cache_key: &cache_context.cache_key,
            workspace_id: detail.execution.workspace_id,
            library_id: detail.execution.library_id,
            source_execution_id: detail.execution.id,
            readable_content_fingerprint: &cache_context.readable_content_fingerprint,
            graph_projection_version: cache_context.graph_projection_version,
            graph_topology_generation: cache_context.graph_topology_generation,
            binding_fingerprint: &cache_context.binding_fingerprint,
        },
    )
    .await
    {
        Ok(row) => row,
        Err(error) => {
            warn!(
                error = %error,
                cache_key = %cache_context.cache_key,
                execution_id = %detail.execution.id,
                "failed to store query result cache winner"
            );
            return;
        }
    };
    if let Err(error) = result_cache::put_cached_execution_id(
        &state.persistence.redis,
        &cache_context.cache_key,
        row.source_execution_id,
    )
    .await
    {
        warn!(
            error = %error,
            cache_key = %cache_context.cache_key,
            source_execution_id = %row.source_execution_id,
            "failed to refresh redis query result cache winner"
        );
    }
    if row.source_execution_id != detail.execution.id {
        warn!(
            cache_key = %cache_context.cache_key,
            winner_execution_id = %row.source_execution_id,
            completed_execution_id = %detail.execution.id,
            "query result cache winner already existed"
        );
    }
}

async fn evict_query_result_cache_entry(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
    source_execution_id: Uuid,
    reason: &'static str,
) {
    if let Err(error) =
        result_cache::delete_cached_execution_id(&state.persistence.redis, &cache_context.cache_key)
            .await
    {
        warn!(
            error = %error,
            cache_key = %cache_context.cache_key,
            source_execution_id = %source_execution_id,
            reason,
            "failed to delete redis query result cache entry"
        );
    }
    if let Err(error) = query_result_cache_repository::delete_query_result_cache(
        &state.persistence.postgres,
        &cache_context.cache_key,
    )
    .await
    {
        warn!(
            error = %error,
            cache_key = %cache_context.cache_key,
            source_execution_id = %source_execution_id,
            reason,
            "failed to delete postgres query result cache row"
        );
    }
}

fn query_detail_has_grounding_references(detail: &QueryExecutionDetail) -> bool {
    !detail.chunk_references.is_empty()
        || !detail.prepared_segment_references.is_empty()
        || !detail.technical_fact_references.is_empty()
        || !detail.graph_node_references.is_empty()
        || !detail.graph_edge_references.is_empty()
}

fn should_backfill_graph_entity_references(
    has_prepared_bundle_refs: bool,
    graph_node_references_empty: bool,
) -> bool {
    graph_node_references_empty && !has_prepared_bundle_refs
}

fn context_reference_set_grounding_count(
    reference_set: &KnowledgeContextBundleReferenceSetRow,
) -> usize {
    reference_set.chunk_references.len()
        + reference_set.entity_references.len()
        + reference_set.relation_references.len()
        + reference_set.evidence_references.len()
        + reference_set.bundle.selected_fact_ids.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MaterializedAgentGrounding {
    source_execution_id: Uuid,
    primary_execution_id: Uuid,
}

async fn materialize_agent_grounding_from_child_execution(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    parent_context_bundle_id: Uuid,
    child_query_execution_ids: &[Uuid],
) -> anyhow::Result<Option<MaterializedAgentGrounding>> {
    let mut primary_reference_set: Option<KnowledgeContextBundleReferenceSetRow> = None;
    let mut primary_execution_id: Option<Uuid> = None;
    let mut grounding_sources = Vec::new();
    let mut chunk_references = HashMap::new();
    let mut entity_references = HashMap::new();
    let mut relation_references = HashMap::new();
    let mut evidence_references = HashMap::new();
    let mut selected_fact_ids = Vec::new();
    let mut primary_reference_score = 0usize;

    // Prefer the strongest grounded child as the parent bundle template.
    // Iterating newest-first keeps ties on the latest refine attempt without
    // letting a weaker retry replace richer verified grounding.
    for child_execution_id in child_query_execution_ids.iter().rev().copied() {
        if child_execution_id == parent_execution.id {
            continue;
        }
        let Some(child_execution) =
            query_repository::get_execution_by_id(&state.persistence.postgres, child_execution_id)
                .await?
        else {
            continue;
        };
        if child_execution.workspace_id != parent_execution.workspace_id
            || child_execution.library_id != parent_execution.library_id
        {
            continue;
        }
        let Some(reference_set) = state
            .arango_context_store
            .get_bundle_reference_set_by_query_execution(child_execution_id)
            .await?
        else {
            continue;
        };
        if !context_reference_set_has_grounding(&reference_set) {
            continue;
        }

        let reference_score = context_reference_set_grounding_count(&reference_set);
        if primary_reference_set.is_none() || reference_score > primary_reference_score {
            primary_reference_score = reference_score;
            primary_execution_id = Some(child_execution.id);
            primary_reference_set = Some(reference_set.clone());
        }

        grounding_sources.push((child_execution.id, child_execution.runtime_execution_id));
        for fact_id in &reference_set.bundle.selected_fact_ids {
            if !selected_fact_ids.contains(fact_id) {
                selected_fact_ids.push(*fact_id);
            }
        }
        for reference in &reference_set.chunk_references {
            merge_chunk_reference(&mut chunk_references, reference.clone());
        }
        for reference in &reference_set.entity_references {
            merge_entity_reference(&mut entity_references, reference.clone());
        }
        for reference in &reference_set.relation_references {
            merge_relation_reference(&mut relation_references, reference.clone());
        }
        for reference in &reference_set.evidence_references {
            merge_evidence_reference(&mut evidence_references, reference.clone());
        }
    }

    let (Some(reference_set), Some(primary_execution_id)) =
        (primary_reference_set, primary_execution_id)
    else {
        return Ok(None);
    };

    let now = Utc::now();
    let mut bundle = reference_set.bundle.clone();
    bundle.key = parent_context_bundle_id.to_string();
    bundle.arango_id = None;
    bundle.arango_rev = None;
    bundle.bundle_id = parent_context_bundle_id;
    bundle.workspace_id = parent_execution.workspace_id;
    bundle.library_id = parent_execution.library_id;
    bundle.query_execution_id = Some(parent_execution.id);
    bundle.selected_fact_ids = selected_fact_ids;
    bundle.assembly_diagnostics =
        agent_grounding_assembly_diagnostics(&bundle.assembly_diagnostics, &grounding_sources);
    bundle.created_at = now;
    bundle.updated_at = now;

    let mut chunk_references = chunk_references.into_values().collect::<Vec<_>>();
    let mut entity_references = entity_references.into_values().collect::<Vec<_>>();
    let mut relation_references = relation_references.into_values().collect::<Vec<_>>();
    let mut evidence_references = evidence_references.into_values().collect::<Vec<_>>();
    sort_chunk_references(&mut chunk_references);
    sort_entity_references(&mut entity_references);
    sort_relation_references(&mut relation_references);
    sort_evidence_references(&mut evidence_references);

    state.arango_context_store.upsert_bundle(&bundle).await?;
    state
        .arango_context_store
        .replace_bundle_chunk_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_chunk_reference_edges(parent_context_bundle_id, &chunk_references, now),
        )
        .await?;
    state
        .arango_context_store
        .replace_bundle_entity_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_entity_reference_edges(parent_context_bundle_id, &entity_references, now),
        )
        .await?;
    state
        .arango_context_store
        .replace_bundle_relation_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_relation_reference_edges(parent_context_bundle_id, &relation_references, now),
        )
        .await?;
    state
        .arango_context_store
        .replace_bundle_evidence_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_evidence_reference_edges(parent_context_bundle_id, &evidence_references, now),
        )
        .await?;

    let chunk_refs = chunk_references
        .iter()
        .map(|reference| query_repository::NewQueryChunkReference {
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect::<Vec<_>>();
    query_repository::append_chunk_references(
        &state.persistence.postgres,
        parent_execution.id,
        &chunk_refs,
    )
    .await?;

    Ok(grounding_sources.first().map(|(execution_id, _)| MaterializedAgentGrounding {
        source_execution_id: *execution_id,
        primary_execution_id,
    }))
}

async fn verify_agent_answer_against_tool_evidence(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    context_bundle_id: Uuid,
    answer_text: &str,
    assistant_grounding: &AssistantGroundingEvidence,
) -> anyhow::Result<RuntimeAnswerVerification> {
    ensure_agent_tool_context_bundle(
        state,
        execution,
        context_bundle_id,
        assistant_grounding,
        QueryVerificationState::NotRun,
        serde_json::json!([]),
    )
    .await?;
    let reference_context =
        load_execution_prepared_reference_context(state, execution.id).await.map_err(|error| {
            anyhow::anyhow!("failed to hydrate UI agent verifier evidence: {error}")
        })?;
    let canonical_evidence = CanonicalAnswerEvidence {
        bundle: reference_context.bundle_refs.as_ref().map(|refs| refs.bundle.clone()),
        chunk_rows: reference_context.chunk_rows,
        structured_blocks: reference_context.structured_block_rows,
        technical_facts: reference_context.technical_fact_rows,
    };
    let prompt_context = assistant_grounding.verification_corpus.join("\n\n");
    let verification = verify_answer_against_canonical_evidence(
        &execution.query_text,
        answer_text,
        &QueryIntentProfile::default(),
        &canonical_evidence,
        &[],
        &prompt_context,
        assistant_grounding,
    );
    persist_query_verification(
        state,
        execution.id,
        &verification,
        &canonical_evidence,
        assistant_grounding,
    )
    .await?;
    Ok(verification)
}

async fn ensure_agent_tool_context_bundle(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    context_bundle_id: Uuid,
    assistant_grounding: &AssistantGroundingEvidence,
    verification_state: QueryVerificationState,
    verification_warnings: serde_json::Value,
) -> anyhow::Result<()> {
    if state.arango_context_store.get_bundle_by_query_execution(execution.id).await?.is_some() {
        return Ok(());
    }

    let now = Utc::now();
    let bundle = KnowledgeContextBundleRow {
        key: context_bundle_id.to_string(),
        arango_id: None,
        arango_rev: None,
        bundle_id: context_bundle_id,
        workspace_id: execution.workspace_id,
        library_id: execution.library_id,
        query_execution_id: Some(execution.id),
        bundle_state: "ready".to_string(),
        bundle_strategy: "agent_tool_evidence".to_string(),
        requested_mode: super::runtime_mode_label(CANONICAL_QUERY_MODE).to_string(),
        resolved_mode: super::runtime_mode_label(CANONICAL_QUERY_MODE).to_string(),
        selected_fact_ids: Vec::new(),
        verification_state: verification_state_storage_label(verification_state).to_string(),
        verification_warnings,
        freshness_snapshot: serde_json::json!({}),
        candidate_summary: serde_json::json!({
            "finalAssistantDocumentReferences": assistant_grounding.document_references.len(),
            "finalToolEvidenceFragments": assistant_grounding.verification_corpus.len(),
        }),
        assembly_diagnostics: serde_json::json!({
            "queryExecutionId": execution.id,
            "bundleId": context_bundle_id,
            "toolEvidenceFragmentCount": assistant_grounding.verification_corpus.len(),
        }),
        created_at: now,
        updated_at: now,
    };
    state.arango_context_store.upsert_bundle(&bundle).await?;
    Ok(())
}

fn verification_state_storage_label(state: QueryVerificationState) -> &'static str {
    match state {
        QueryVerificationState::NotRun => "not_run",
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
    }
}

fn no_verifiable_tool_evidence_warnings() -> serde_json::Value {
    serde_json::to_value([QueryVerificationWarning {
        code: "no_verifiable_tool_evidence".to_string(),
        message: "The UI agent used MCP tools, but none returned evidence that can verify the final answer.".to_string(),
        related_segment_id: None,
        related_fact_id: None,
    }])
    .unwrap_or_else(|_| serde_json::json!([]))
}

fn no_agent_tool_evidence_warnings() -> serde_json::Value {
    serde_json::to_value([QueryVerificationWarning {
        code: "no_agent_tool_evidence".to_string(),
        message:
            "The UI agent produced a final answer before collecting verifier-grade MCP evidence."
                .to_string(),
        related_segment_id: None,
        related_fact_id: None,
    }])
    .unwrap_or_else(|_| serde_json::json!([]))
}

fn literal_inventory_revision_context(assistant_grounding: &AssistantGroundingEvidence) -> String {
    let mut sections = assistant_grounding.verification_corpus.clone();
    sections.extend(assistant_grounding.document_references.iter().map(|reference| {
        format!("Document reference: {}\n{}", reference.document_title, reference.excerpt)
    }));
    sections.join("\n\n")
}

fn literal_inventory_coverage_revision_targets(
    answer: &str,
    history: &[ExternalConversationTurn],
    assistant_grounding: &AssistantGroundingEvidence,
) -> Vec<String> {
    const MIN_HISTORY_LITERALS: usize = 4;
    const MIN_PRESENT_LITERALS: usize = 2;
    const MAX_REVISION_TARGETS: usize = 16;

    let inventory = latest_dense_history_identifier_literals(history);
    if inventory.len() < MIN_HISTORY_LITERALS {
        return Vec::new();
    }
    let present = inventory
        .iter()
        .filter(|literal| text_contains_literal(answer, literal))
        .cloned()
        .collect::<Vec<_>>();
    if present.len() < MIN_PRESENT_LITERALS {
        return Vec::new();
    }
    inventory
        .into_iter()
        .filter(|literal| !text_contains_literal(answer, literal))
        .filter(|literal| {
            assistant_grounding_contains_literal_with_present_inventory(
                assistant_grounding,
                literal,
                &present,
                MIN_PRESENT_LITERALS,
            )
        })
        .take(MAX_REVISION_TARGETS)
        .collect()
}

fn latest_dense_history_identifier_literals(history: &[ExternalConversationTurn]) -> Vec<String> {
    let mut literals = history
        .iter()
        .rev()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .map(|turn| dense_history_literals(&turn.content_text))
        .unwrap_or_default()
        .into_iter()
        .filter(|literal| literal_text_is_identifier_shaped(literal))
        .collect::<Vec<_>>();
    let mut seen = HashSet::<String>::new();
    literals.retain(|literal| seen.insert(literal.clone()));
    literals
}

fn dense_history_literals(text: &str) -> Vec<String> {
    let literal_line =
        text.lines().map(str::trim).find(|line| line.starts_with("literals:")).unwrap_or("");
    extract_backtick_literals(literal_line)
}

fn extract_backtick_literals(text: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut search_from = 0usize;
    while let Some(start) = text[search_from..].find('`') {
        let abs_start = search_from + start + 1;
        if abs_start >= text.len() {
            break;
        }
        let Some(end) = text[abs_start..].find('`') else {
            break;
        };
        let literal = text[abs_start..abs_start + end].trim();
        if !literal.is_empty() && !literal.contains('\n') {
            literals.push(literal.to_string());
        }
        search_from = abs_start + end + 1;
    }
    literals
}

fn assistant_grounding_contains_literal_with_present_inventory(
    assistant_grounding: &AssistantGroundingEvidence,
    literal: &str,
    present_literals: &[String],
    required_present_count: usize,
) -> bool {
    assistant_grounding.verification_corpus.iter().any(|fragment| {
        grounding_fragment_supports_inventory_literal(
            fragment,
            literal,
            present_literals,
            required_present_count,
        )
    }) || assistant_grounding.document_references.iter().any(|reference| {
        let fragment = format!("{}\n{}", reference.document_title, reference.excerpt);
        grounding_fragment_supports_inventory_literal(
            &fragment,
            literal,
            present_literals,
            required_present_count,
        )
    })
}

fn grounding_fragment_supports_inventory_literal(
    fragment: &str,
    literal: &str,
    present_literals: &[String],
    required_present_count: usize,
) -> bool {
    text_contains_literal(fragment, literal)
        && present_literals
            .iter()
            .filter(|present| text_contains_literal(fragment, present))
            .take(required_present_count)
            .count()
            >= required_present_count
}

fn text_contains_literal(text: &str, literal: &str) -> bool {
    if literal.is_empty() {
        return true;
    }
    let mut search_from = 0usize;
    while let Some(relative) = text[search_from..].find(literal) {
        let start = search_from + relative;
        let end = start + literal.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !literal_boundary_continues(before) && !literal_boundary_continues(after) {
            return true;
        }
        search_from = end;
    }
    false
}

fn literal_boundary_continues(ch: Option<char>) -> bool {
    ch.is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn context_reference_set_has_grounding(
    reference_set: &KnowledgeContextBundleReferenceSetRow,
) -> bool {
    !reference_set.chunk_references.is_empty()
        || !reference_set.entity_references.is_empty()
        || !reference_set.relation_references.is_empty()
        || !reference_set.evidence_references.is_empty()
        || !reference_set.bundle.selected_fact_ids.is_empty()
}

fn agent_has_verifiable_tool_evidence(
    child_query_execution_ids: &[Uuid],
    assistant_grounding: &AssistantGroundingEvidence,
) -> bool {
    !child_query_execution_ids.is_empty()
        || assistant_grounding
            .document_references
            .iter()
            .any(|reference| !reference.excerpt.trim().is_empty())
        || assistant_grounding
            .verification_corpus
            .iter()
            .any(|fragment| verification_fragment_is_verifier_grade_tool_evidence(fragment))
}

fn agent_answer_requires_parent_tool_evidence_verification(
    has_verifiable_tool_evidence: bool,
) -> bool {
    has_verifiable_tool_evidence
}

fn agent_verification_needs_literal_revision(verification: &RuntimeAnswerVerification) -> bool {
    verification.warnings.iter().any(|warning| {
        warning.code == "unsupported_literal" || warning.code == "unsupported_canonical_claim"
    })
}

fn verification_fragment_is_verifier_grade_tool_evidence(fragment: &str) -> bool {
    let Some(tool_name) = fragment
        .strip_prefix("[MCP tool result: ")
        .and_then(|tail| tail.split_once(']').map(|(tool_name, _)| tool_name))
    else {
        return false;
    };
    match tool_name {
        "grounded_answer" | "read_document" => true,
        "search_documents" => {
            fragment.contains("\"excerpt\"")
                || fragment.contains("\"chunkReferences\"")
                || fragment.contains("\"technicalFactReferences\"")
                || fragment.contains("\"evidenceReferences\"")
        }
        "search_entities"
        | "get_graph_topology"
        | "list_relations"
        | "get_communities"
        | "get_runtime_execution"
        | "get_runtime_execution_trace" => true,
        _ => false,
    }
}

fn ui_agent_iteration_cap() -> usize {
    usize::from(QueryAnswerTask::spec().max_turns).saturating_add(1)
}

fn agent_grounding_assembly_diagnostics(
    source: &serde_json::Value,
    child_executions: &[(Uuid, Uuid)],
) -> serde_json::Value {
    let mut diagnostics = source.clone();
    let marker = serde_json::json!(
        child_executions
            .iter()
            .map(|(execution_id, runtime_execution_id)| {
                serde_json::json!({
                    "sourceExecutionId": execution_id,
                    "sourceRuntimeExecutionId": runtime_execution_id
                })
            })
            .collect::<Vec<_>>()
    );
    match diagnostics.as_object_mut() {
        Some(object) => {
            object.insert("uiAgentGroundingSources".to_string(), marker);
            diagnostics
        }
        None => serde_json::json!({
            "source": diagnostics,
            "uiAgentGroundingSources": marker
        }),
    }
}

fn should_replace_reference(current_rank: i32, current_score: f64, rank: i32, score: f64) -> bool {
    rank < current_rank || (rank == current_rank && score > current_score)
}

fn merge_chunk_reference(
    references: &mut HashMap<Uuid, KnowledgeBundleChunkReferenceRow>,
    reference: KnowledgeBundleChunkReferenceRow,
) {
    let replace = references.get(&reference.chunk_id).is_none_or(|current| {
        should_replace_reference(current.rank, current.score, reference.rank, reference.score)
    });
    if replace {
        references.insert(reference.chunk_id, reference);
    }
}

fn merge_entity_reference(
    references: &mut HashMap<Uuid, KnowledgeBundleEntityReferenceRow>,
    reference: KnowledgeBundleEntityReferenceRow,
) {
    let replace = references.get(&reference.entity_id).is_none_or(|current| {
        should_replace_reference(current.rank, current.score, reference.rank, reference.score)
    });
    if replace {
        references.insert(reference.entity_id, reference);
    }
}

fn merge_relation_reference(
    references: &mut HashMap<Uuid, KnowledgeBundleRelationReferenceRow>,
    reference: KnowledgeBundleRelationReferenceRow,
) {
    let replace = references.get(&reference.relation_id).is_none_or(|current| {
        should_replace_reference(current.rank, current.score, reference.rank, reference.score)
    });
    if replace {
        references.insert(reference.relation_id, reference);
    }
}

fn merge_evidence_reference(
    references: &mut HashMap<Uuid, KnowledgeBundleEvidenceReferenceRow>,
    reference: KnowledgeBundleEvidenceReferenceRow,
) {
    let replace = references.get(&reference.evidence_id).is_none_or(|current| {
        should_replace_reference(current.rank, current.score, reference.rank, reference.score)
    });
    if replace {
        references.insert(reference.evidence_id, reference);
    }
}

fn sort_chunk_references(references: &mut [KnowledgeBundleChunkReferenceRow]) {
    references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
}

fn sort_entity_references(references: &mut [KnowledgeBundleEntityReferenceRow]) {
    references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.entity_id.cmp(&right.entity_id))
    });
}

fn sort_relation_references(references: &mut [KnowledgeBundleRelationReferenceRow]) {
    references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.relation_id.cmp(&right.relation_id))
    });
}

fn sort_evidence_references(references: &mut [KnowledgeBundleEvidenceReferenceRow]) {
    references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
}

fn clone_chunk_reference_edges(
    bundle_id: Uuid,
    references: &[KnowledgeBundleChunkReferenceRow],
    created_at: chrono::DateTime<Utc>,
) -> Vec<KnowledgeBundleChunkEdgeRow> {
    references
        .iter()
        .map(|reference| KnowledgeBundleChunkEdgeRow {
            key: format!("{bundle_id}:{}", reference.chunk_id),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_CHUNK_COLLECTION}/{}", reference.chunk_id),
            bundle_id,
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: reference.inclusion_reason.clone(),
            created_at,
        })
        .collect()
}

fn clone_entity_reference_edges(
    bundle_id: Uuid,
    references: &[KnowledgeBundleEntityReferenceRow],
    created_at: chrono::DateTime<Utc>,
) -> Vec<KnowledgeBundleEntityEdgeRow> {
    references
        .iter()
        .map(|reference| KnowledgeBundleEntityEdgeRow {
            key: format!("{bundle_id}:{}", reference.entity_id),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_ENTITY_COLLECTION}/{}", reference.entity_id),
            bundle_id,
            entity_id: reference.entity_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: reference.inclusion_reason.clone(),
            created_at,
        })
        .collect()
}

fn clone_relation_reference_edges(
    bundle_id: Uuid,
    references: &[KnowledgeBundleRelationReferenceRow],
    created_at: chrono::DateTime<Utc>,
) -> Vec<KnowledgeBundleRelationEdgeRow> {
    references
        .iter()
        .map(|reference| KnowledgeBundleRelationEdgeRow {
            key: format!("{bundle_id}:{}", reference.relation_id),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_RELATION_COLLECTION}/{}", reference.relation_id),
            bundle_id,
            relation_id: reference.relation_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: reference.inclusion_reason.clone(),
            created_at,
        })
        .collect()
}

fn clone_evidence_reference_edges(
    bundle_id: Uuid,
    references: &[KnowledgeBundleEvidenceReferenceRow],
    created_at: chrono::DateTime<Utc>,
) -> Vec<KnowledgeBundleEvidenceEdgeRow> {
    references
        .iter()
        .map(|reference| KnowledgeBundleEvidenceEdgeRow {
            key: format!("{bundle_id}:{}", reference.evidence_id),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_EVIDENCE_COLLECTION}/{}", reference.evidence_id),
            bundle_id,
            evidence_id: reference.evidence_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: reference.inclusion_reason.clone(),
            created_at,
        })
        .collect()
}

async fn seed_query_runtime_session(
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
            question: conversation_context.effective_query_text.clone(),
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
        .map_err(map_runtime_execution_error)
}

async fn persist_agent_answer(
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
            failure_code: None,
            completed_at: Some(Utc::now()),
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

async fn persist_failed_agent_debug_snapshot(
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
            agent_loop: failure.agent_loop.clone(),
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

fn record_query_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
    stage_state: RuntimeStageState,
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
    let resolved_started_at = started_at.unwrap_or_else(chrono::Utc::now);
    executor.complete_stage(
        session,
        stage_kind,
        stage_state,
        deterministic,
        failure.map(|value| value.code.clone()),
        failure.map(|value| truncate_failure_code(&value.summary).to_string()),
        resolved_started_at,
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

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::domains::query::QueryTurnKind;
    use crate::services::query::assistant_grounding::AssistantGroundingEvidence;
    use crate::services::query::service::ExternalConversationTurn;

    use super::{
        ASSISTANT_AGENT_LOOP_DEADLINE_MS, ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS,
        agent_answer_requires_parent_tool_evidence_verification,
        agent_has_verifiable_tool_evidence, is_query_vector_source_mismatch,
        latest_dense_history_identifier_literals, literal_inventory_coverage_revision_targets,
        no_agent_tool_evidence_warnings, should_backfill_graph_entity_references,
        text_contains_literal, ui_agent_iteration_cap,
    };

    #[test]
    fn classifies_runtime_vector_source_mismatch() {
        assert!(is_query_vector_source_mismatch(
            "active query retrieval binding must use the same vector source as active chunk embedding binding"
        ));
    }

    #[test]
    fn ignores_non_vector_source_mismatch_messages() {
        assert!(!is_query_vector_source_mismatch(
            "query retrieval provider is healthy and ready for embeddings"
        ));
    }

    #[test]
    fn ui_agent_no_tool_answer_has_no_verifiable_tool_evidence() {
        let grounding = AssistantGroundingEvidence::default();

        assert!(!agent_has_verifiable_tool_evidence(&[], &grounding));
    }

    #[test]
    fn ui_agent_no_tool_answer_warning_is_insufficient_evidence_signal() {
        let warnings = no_agent_tool_evidence_warnings();

        assert_eq!(warnings[0]["code"], "no_agent_tool_evidence");
    }

    #[test]
    fn graph_entity_reference_backfill_only_runs_without_prepared_bundle_refs() {
        assert!(should_backfill_graph_entity_references(false, true));
        assert!(!should_backfill_graph_entity_references(true, true));
        assert!(!should_backfill_graph_entity_references(false, false));
    }

    #[test]
    fn ui_agent_child_execution_is_verifiable_tool_evidence() {
        let grounding = AssistantGroundingEvidence::default();
        let child_query_execution_ids = [Uuid::nil()];

        assert!(agent_has_verifiable_tool_evidence(&child_query_execution_ids, &grounding));
    }

    #[test]
    fn ui_agent_tool_evidence_always_requires_parent_verification() {
        assert!(agent_answer_requires_parent_tool_evidence_verification(true));
        assert!(!agent_answer_requires_parent_tool_evidence_verification(false));
    }

    #[test]
    fn ui_agent_verification_corpus_is_verifiable_tool_evidence() {
        let grounding = AssistantGroundingEvidence {
            verification_corpus: vec![
                "[MCP tool result: read_document]\ncontent:\nThe supported claim.".to_string(),
            ],
            document_references: Vec::new(),
        };

        assert!(agent_has_verifiable_tool_evidence(&[], &grounding));
    }

    #[test]
    fn literal_inventory_coverage_targets_missing_identifier_literals() {
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "literals: `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`\nProvider Alpha setup.".to_string(),
        }];
        let grounding = AssistantGroundingEvidence {
            verification_corpus: vec![
                "Current evidence supports `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, and `sendDetails`.".to_string(),
            ],
            document_references: Vec::new(),
        };
        let answer = "Configure `endpointUrl`, `partnerId`, `secretKey`, and `sendDetails`.";

        let targets = literal_inventory_coverage_revision_targets(answer, &history, &grounding);

        assert_eq!(targets, vec!["alphaPackage".to_string(), "retryTimeout".to_string()]);
    }

    #[test]
    fn literal_inventory_coverage_waits_until_answer_enumerates_inventory() {
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "literals: `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`".to_string(),
        }];
        let grounding = AssistantGroundingEvidence {
            verification_corpus: vec![
                "Current evidence supports `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, and `sendDetails`.".to_string(),
            ],
            document_references: Vec::new(),
        };

        assert!(
            literal_inventory_coverage_revision_targets(
                "Use `endpointUrl` for this one setting.",
                &history,
                &grounding,
            )
            .is_empty()
        );
    }

    #[test]
    fn literal_inventory_coverage_requires_current_grounding() {
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text:
                "literals: `alphaPackage`, `endpointUrl`, `secretKey`, `/opt/alpha.conf`, `retryTimeout`"
                    .to_string(),
        }];
        let grounding = AssistantGroundingEvidence {
            verification_corpus: vec![
                "Current evidence supports `alphaPackage`, `endpointUrl`, and `retryTimeout`."
                    .to_string(),
            ],
            document_references: Vec::new(),
        };
        let answer = "Configure `endpointUrl` and `alphaPackage`.";

        let targets = literal_inventory_coverage_revision_targets(answer, &history, &grounding);

        assert_eq!(targets, vec!["retryTimeout".to_string()]);
    }

    #[test]
    fn literal_inventory_presence_uses_identifier_boundaries() {
        assert!(text_contains_literal("Set `retryTimeout`.", "retryTimeout"));
        assert!(!text_contains_literal("Set `retryTimeoutMs`.", "retryTimeout"));
        assert!(!text_contains_literal("Set `alpha.retryTimeout`.", "retryTimeout"));
        assert!(!text_contains_literal("Set `retry-timeout`.", "timeout"));
    }

    #[test]
    fn literal_inventory_dedup_preserves_case_sensitive_literals() {
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "literals: `retryTimeout`, `RetryTimeout`, `endpointUrl`, `secretKey`"
                .to_string(),
        }];

        assert_eq!(
            latest_dense_history_identifier_literals(&history),
            vec![
                "retryTimeout".to_string(),
                "RetryTimeout".to_string(),
                "endpointUrl".to_string(),
                "secretKey".to_string()
            ]
        );
    }

    #[test]
    fn ui_agent_title_only_search_result_is_not_verifier_grade_evidence() {
        let grounding = AssistantGroundingEvidence {
            verification_corpus: vec![
                "[MCP tool result: search_documents]\nstructuredContent:\n{\"hits\":[{\"documentTitle\":\"Alpha Guide\"}]}".to_string(),
            ],
            document_references: Vec::new(),
        };

        assert!(!agent_has_verifiable_tool_evidence(&[], &grounding));
    }

    #[test]
    fn ui_agent_deadline_budget_covers_runtime_turns() {
        let iteration_cap = ui_agent_iteration_cap();

        assert!(ASSISTANT_AGENT_LOOP_DEADLINE_MS / iteration_cap as u64 >= 30_000);
    }

    #[test]
    fn ui_agent_soft_tool_collection_target_leaves_synthesis_budget() {
        assert!(ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS < ASSISTANT_AGENT_LOOP_DEADLINE_MS);
        assert!(ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS <= 45_000);
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
        || normalized.contains("active query retrieval binding is not configured")
        || normalized.contains("active chunk embedding binding is not configured")
        || normalized.contains("query retrieval binding must use the same model")
        || is_query_vector_source_mismatch(&normalized)
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

    // Preserve the underlying execution failure message instead of
    // collapsing to a bare `internal server error`. The classifier
    // matches several known patterns above; everything else still
    // belongs to "internal" but we keep the original chain so the
    // 5xx log line carries enough context to diagnose without a
    // separate trace dive.
    ApiError::InternalMessage(formatted)
}

fn is_query_vector_source_mismatch(normalized_message: &str) -> bool {
    const VECTOR_SOURCE_MISMATCH_MARKERS: [&str; 3] = [
        "must use the same vector source",
        "query retrieval and chunk embedding bindings must use the same vector source",
        "vector source mismatch",
    ];
    VECTOR_SOURCE_MISMATCH_MARKERS.iter().any(|marker| normalized_message.contains(marker))
}

/// Record a billing row for the QueryCompiler LLM call that produced
/// the `QueryIR` used by this turn. `None` means the compiler served
/// the IR from cache, in which case no token usage was spent and no
/// billing row is required. Binding/provider failures fail the turn
/// before this helper is called. The helper is called from both the
/// `Completed` and `Recovered` terminal branches — both go through
/// the same answer pipeline and both need the compile-stage cost
/// attributed to the same `query_execution` row.
async fn capture_query_compile_usage_if_any(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    terminal_execution: &query_repository::QueryExecutionRow,
    runtime_execution: &crate::domains::agent_runtime::RuntimeExecution,
    compile_usage: Option<&crate::services::query::execution::QueryCompileUsage>,
) {
    let Some(compile_usage) = compile_usage else {
        return;
    };
    if let Err(error) = state
        .canonical_services
        .billing
        .capture_execution_provider_call(
            state,
            CaptureExecutionBillingCommand {
                workspace_id: conversation.workspace_id,
                library_id: conversation.library_id,
                owning_execution_kind: "query_execution".to_string(),
                owning_execution_id: terminal_execution.id,
                runtime_execution_id: Some(runtime_execution.id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer),
                binding_id: None,
                provider_kind: compile_usage.provider_kind.clone(),
                model_name: compile_usage.model_name.clone(),
                call_kind: "query_compile".to_string(),
                usage_json: compile_usage.usage_json.clone(),
            },
        )
        .await
    {
        warn!(
            error = %error,
            execution_id = %terminal_execution.id,
            "query compiler billing capture failed",
        );
    }
}
