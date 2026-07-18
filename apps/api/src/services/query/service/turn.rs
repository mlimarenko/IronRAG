use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use anyhow::Context;
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
    domains::catalog::{CatalogLibrary, CatalogLifecycleState},
    domains::query::{
        QueryClarification, QueryConversationState, QueryExecutionDetail, QueryTurnKind,
        QueryVerificationState, QueryVerificationWarning, resolve_top_k,
    },
    domains::query_ir::{
        QueryIR, QueryLanguage, VerificationLevel, literal_text_is_identifier_shaped,
    },
    domains::{
        agent_runtime::{
            RuntimeDecisionKind, RuntimeExecutionOwner, RuntimeStageKind, RuntimeStageState,
            RuntimeSurfaceKind,
        },
        ai::AiBindingPurpose,
    },
    infra::{
        knowledge_rows::{
            KnowledgeBundleChunkEdgeRow, KnowledgeBundleChunkReferenceRow,
            KnowledgeBundleEntityEdgeRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleEvidenceReferenceRow,
            KnowledgeBundleRelationEdgeRow, KnowledgeBundleRelationReferenceRow,
            KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow,
        },
        repositories::{
            ai_repository, catalog_repository, query_repository, query_result_cache_repository,
            runtime_repository,
        },
    },
    integrations::retry::ProviderCallError,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    services::{
        ingest::runtime::bounded_runtime_overrides,
        mcp::access::library_catalog_ref,
        ops::service::CreateAsyncOperationCommand,
        query::{
            agent_loop::{
                AgentAnswerProvenance, AgentCanonicalAnswerOutcome, AgentLoopActivityEvent,
                AgentTurnFailure, AgentTurnResult, LiteralRevisionRequest, McpToolAgentTurnInput,
                run_literal_revision_turn, run_mcp_tool_agent_turn,
            },
            assistant_grounding::AssistantGroundingEvidence,
            error::QueryServiceError,
            execution::{
                AnswerVisibilityKind, CanonicalAnswerEvidence, RuntimeAnswerQueryFailure,
                RuntimeAnswerQueryResult, RuntimeAnswerVerification,
                SemanticRerankExecutionContext, finalize_answer_visibility, generate_answer_query,
                literal_revision_targets, persist_query_verification,
                persisted_query_answer_outcome, prepare_answer_query,
                verify_answer_against_canonical_evidence,
            },
            planner::query_intent_profile_from_query_ir,
            provider_billing::QueryProviderExecutionContext,
            result_cache,
        },
    },
};

use super::{
    CANONICAL_QUERY_MODE, ConversationRuntimeContext, ExecuteConversationTurnCommand,
    ExternalConversationTurn, QueryService, QueryTurnExecutionResult,
    bounded_runtime_policy_summary,
    context::{
        AssembleContextBundleRequest, assemble_context_bundle,
        load_execution_prepared_reference_context, query_ir_from_bundle_diagnostics,
    },
    formatting::{
        build_assistant_document_references, build_prepared_segment_references,
        build_technical_fact_references, hydrate_entity_references, hydrate_relation_references,
        map_chunk_references, map_entity_references, map_execution_runtime_stage_summaries,
        map_execution_runtime_summary, map_relation_references, parse_query_verification_state,
        parse_query_verification_warnings, search_runtime_graph_entity_references,
    },
    is_runtime_policy_failure_code, runtime_failure_summary_from_typed_code,
    session::{
        build_conversation_runtime_context,
        build_conversation_runtime_context_with_external_history, derive_conversation_title,
        load_prior_grounded_answer_context_messages, map_conversation_row, map_execution_row,
        map_turn_row, normalize_required_text, should_refresh_conversation_title,
        should_replay_prior_grounded_answer_context,
    },
};

const REFERENCE_CONTEXT_HYDRATION_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const ASSISTANT_AGENT_LOOP_DEADLINE_MS: u64 = 180_000;
pub(crate) const ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS: u64 = 35_000;
const _: () = {
    assert!(ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS < ASSISTANT_AGENT_LOOP_DEADLINE_MS);
    assert!(ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS <= 45_000);
};
const ASSISTANT_AGENT_LOOP_MIN_ITERATIONS: usize = 10;
#[cfg(test)]
const ASSISTANT_AGENT_LOOP_MIN_ITERATION_BUDGET_MS: u64 = 15_000;
const ASSISTANT_LITERAL_INVENTORY_REVISION_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone)]
struct QueryResultCacheContext {
    cache_key: String,
    library_id: Uuid,
    source_truth_version: i64,
    readable_content_fingerprint: String,
    graph_projection_version: i64,
    graph_topology_generation: i64,
    binding_fingerprint: String,
}

#[derive(Debug, thiserror::Error)]
#[error("canonical content and the readable knowledge projection are converging")]
struct QueryContentProjectionConverging;

fn query_content_projection_converging_error() -> ApiError {
    ApiError::service_unavailable(
        "library content is converging; retry the query shortly",
        "query_content_projection_converging",
    )
}

struct QueryExecutionInterruptionGuard {
    postgres: sqlx::PgPool,
    execution_id: Uuid,
    runtime_execution_id: Uuid,
    async_operation_id: Uuid,
    armed: bool,
}

impl QueryExecutionInterruptionGuard {
    fn new(
        postgres: &sqlx::PgPool,
        execution_id: Uuid,
        runtime_execution_id: Uuid,
        async_operation_id: Uuid,
    ) -> Self {
        Self {
            postgres: postgres.clone(),
            execution_id,
            runtime_execution_id,
            async_operation_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for QueryExecutionInterruptionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                execution_id = %self.execution_id,
                "query execution was interrupted without an active cleanup runtime"
            );
            return;
        };
        let postgres = self.postgres.clone();
        let execution_id = self.execution_id;
        let runtime_execution_id = self.runtime_execution_id;
        let async_operation_id = self.async_operation_id;
        runtime.spawn(async move {
            match query_repository::cancel_interrupted_execution(
                &postgres,
                execution_id,
                runtime_execution_id,
                async_operation_id,
            )
            .await
            {
                Ok(true) => tracing::warn!(
                    %execution_id,
                    %runtime_execution_id,
                    "canceled interrupted query execution"
                ),
                Ok(false) => {}
                Err(error) => tracing::error!(
                    %error,
                    %execution_id,
                    %runtime_execution_id,
                    "failed to cancel interrupted query execution"
                ),
            }
        });
    }
}

fn compiled_query_uses_answer_history(question: &str, ir: &QueryIR) -> bool {
    if ir.is_follow_up() {
        return true;
    }
    ir.retrieval_query
        .as_deref()
        .map(str::trim)
        .is_some_and(|resolved| !resolved.is_empty() && resolved != question.trim())
}

struct AgentAnswerStageInput<'a> {
    state: &'a AppState,
    auth: &'a AuthContext,
    library: &'a CatalogLibrary,
    library_ref: &'a str,
    content_text: &'a str,
    conversation: &'a query_repository::QueryConversationRow,
    conversation_context: &'a ConversationRuntimeContext,
    request_turn: &'a query_repository::QueryTurnRow,
    execution: &'a query_repository::QueryExecutionRow,
    execution_context_bundle_id: Uuid,
    provider_execution_context: QueryProviderExecutionContext,
    prior_grounded_answer_context_messages: &'a [crate::integrations::llm::ChatMessage],
    agent_conversation_history: &'a [crate::integrations::llm::ChatMessage],
    agent_grounded_answer_tool_history: &'a [ExternalConversationTurn],
    agent_request_id: &'a str,
    activity_tx: Option<Sender<AgentLoopActivityEvent>>,
    top_k: usize,
    runtime_session: &'a mut RuntimeExecutionSession,
}

async fn execute_agent_answer_stages(
    mut input: AgentAnswerStageInput<'_>,
) -> RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure> {
    if let Err(failure) = begin_agent_answer_stage(&mut input).await {
        return failure;
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

async fn begin_agent_answer_stage(
    input: &mut AgentAnswerStageInput<'_>,
) -> Result<(), RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>> {
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
        RuntimeStageState::Failed,
        false,
        Some(&failure),
        None,
    );
    Err(make_query_terminal_failure_outcome(failure))
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
        RuntimeStageState::Completed,
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
        RuntimeStageState::Failed,
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
                RuntimeStageState::Completed,
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
                RuntimeStageState::Failed,
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
    let has_evidence =
        agent_has_verifiable_tool_evidence(&child_execution_ids, &agent_result.assistant_grounding);
    ensure_agent_grounding_available(input, agent_result, &child_execution_ids, has_evidence)
        .await?;
    if !agent_answer_needs_parent_verification(agent_result, has_evidence) {
        return Ok(false);
    }
    let verification = verify_agent_answer_against_tool_evidence(
        input.state,
        input.execution,
        input.execution_context_bundle_id,
        &agent_result.answer,
        &agent_result.assistant_grounding,
        agent_result.answer_provenance,
        agent_result.canonical_answer_outcome.as_ref(),
    )
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
            AgentAnswerProvenance::CanonicalGroundedAnswerPassthrough
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
            warn!(stage = "literal_inventory_coverage_revision", error = %error, execution_id = %input.execution.id, "literal-inventory coverage revision failed for UI agent answer");
            return false;
        }
        Err(_) => {
            warn!(stage = "literal_inventory_coverage_revision", execution_id = %input.execution.id, timeout_ms = ASSISTANT_LITERAL_INVENTORY_REVISION_TIMEOUT.as_millis(), elapsed_ms = revision_started.elapsed().as_millis(), "literal-inventory coverage revision timed out for UI agent answer");
            return false;
        }
    };
    if !literal_revision_covers_required_literals(&revision.answer, &targets) {
        agent_result.provider_calls.extend(revision.provider_calls);
        tracing::info!(stage = "literal_inventory_coverage_revision", execution_id = %input.execution.id, required_literal_count = targets.len(), elapsed_ms = revision_started.elapsed().as_millis(), "rejected literal-inventory coverage revision missing required anchors");
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
    let verification = verify_agent_answer_against_tool_evidence(
        input.state,
        input.execution,
        input.execution_context_bundle_id,
        &revision.answer,
        &agent_result.assistant_grounding,
        agent_result.answer_provenance,
        agent_result.canonical_answer_outcome.as_ref(),
    )
    .await;
    match verification {
        Ok(verification) if verification.state == QueryVerificationState::Verified => {
            tracing::info!(execution_id = %input.execution.id, revised_answer_chars = revision.answer.chars().count(), revision_kind, "accepted revised UI agent answer");
            crate::services::query::agent_loop::merge_usage_into(
                &mut agent_result.usage_json,
                &revision.usage_json,
            );
            agent_result.debug_iterations.extend(revision.debug_iterations);
            agent_result.answer = revision.answer;
            true
        }
        Ok(verification) => {
            tracing::info!(execution_id = %input.execution.id, revised_state = ?verification.state, revised_warnings = verification.warnings.len(), revision_kind, "revised UI agent answer was not verified");
            false
        }
        Err(error) => {
            warn!(error = %error, execution_id = %input.execution.id, revision_kind, "failed to verify revised UI agent answer");
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
        RuntimeStageState::Failed,
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
                RuntimeStageState::Completed,
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
                RuntimeStageState::Failed,
                true,
                Some(&failure),
                Some(persist_started),
            );
            make_query_terminal_failure_outcome(failure)
        }
    }
}

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
        RuntimeStageState::Failed,
        false,
        Some(&failure),
        Some(answer_started),
    );
    make_query_terminal_failure_outcome(failure)
}

fn validate_active_conversation(
    conversation: &query_repository::QueryConversationRow,
) -> Result<(), ApiError> {
    if conversation.conversation_state == QueryConversationState::Active {
        return Ok(());
    }
    Err(ApiError::Conflict(format!("conversation {} is not active", conversation.id)))
}

fn validate_conversation_library(
    conversation: &query_repository::QueryConversationRow,
    library: &CatalogLibrary,
) -> Result<(), ApiError> {
    if library.workspace_id != conversation.workspace_id {
        return Err(ApiError::Conflict(format!(
            "conversation {} has library {} outside workspace {}",
            conversation.id, library.id, conversation.workspace_id
        )));
    }
    if library.lifecycle_state != CatalogLifecycleState::Active {
        return Err(ApiError::Conflict(format!("library {} is not active", library.id)));
    }
    Ok(())
}

async fn refresh_conversation_title(
    state: &AppState,
    conversation: query_repository::QueryConversationRow,
    content_text: &str,
) -> Result<query_repository::QueryConversationRow, ApiError> {
    let Some(derived_title) = derive_conversation_title(content_text) else {
        return Ok(conversation);
    };
    if !should_refresh_conversation_title(conversation.title.as_deref(), &derived_title) {
        return Ok(conversation);
    }
    query_repository::initialize_conversation_title(
        &state.persistence.postgres,
        conversation.id,
        &derived_title,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

fn conversation_runtime_context(
    conversation_turns: &[query_repository::QueryTurnRow],
    request_turn_id: Uuid,
    external_prior_turns: &[ExternalConversationTurn],
) -> ConversationRuntimeContext {
    if external_prior_turns.is_empty() {
        return build_conversation_runtime_context(conversation_turns, request_turn_id);
    }
    build_conversation_runtime_context_with_external_history(
        conversation_turns,
        request_turn_id,
        external_prior_turns,
    )
}

async fn prior_agent_grounded_answer_context(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    library: &CatalogLibrary,
    conversation_context: &ConversationRuntimeContext,
) -> Vec<crate::integrations::llm::ChatMessage> {
    if !should_replay_prior_grounded_answer_context(conversation_context) {
        return Vec::new();
    }
    match load_prior_grounded_answer_context_messages(state, conversation.id, library.id).await {
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
}

struct GroundedAnswerStageInput<'a> {
    state: &'a AppState,
    library: &'a CatalogLibrary,
    conversation: &'a query_repository::QueryConversationRow,
    conversation_context: &'a ConversationRuntimeContext,
    request_turn: &'a query_repository::QueryTurnRow,
    execution: &'a query_repository::QueryExecutionRow,
    execution_context_bundle_id: Uuid,
    runtime_execution_id: Uuid,
    provider_execution_context: QueryProviderExecutionContext,
    content_text: &'a str,
    top_k: usize,
    include_debug: bool,
}

async fn run_grounded_retrieve_stage(
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
            RuntimeStageState::Failed,
            true,
            Some(&failure),
            None,
        );
        return Err(failure);
    }
    let retrieve_started = Utc::now();
    let result = Box::pin(prepare_answer_query(
        input.state,
        input.library.id,
        SemanticRerankExecutionContext {
            workspace_id: input.conversation.workspace_id,
            query_execution_id: input.execution.id,
            runtime_execution_id: input.runtime_execution_id,
        },
        input.conversation_context.current_question_text.clone(),
        &input.conversation_context.query_compiler_history,
        CANONICAL_QUERY_MODE,
        input.top_k,
        input.include_debug,
    ))
    .await;
    match result {
        Ok(mut prepared) => {
            crate::services::query::turn_spans::stash_execution_spans(
                input.execution.id,
                std::mem::take(&mut prepared.retrieval_spans),
            );
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Retrieve,
                RuntimeStageState::Completed,
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
                error.to_string(),
            );
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Retrieve,
                RuntimeStageState::Failed,
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

async fn run_grounded_answer_stages(
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
            RuntimeStageState::Failed,
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
        requested_mode: CANONICAL_QUERY_MODE,
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
                RuntimeStageState::Completed,
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
                RuntimeStageState::Failed,
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
            RuntimeStageState::Failed,
            false,
            Some(&failure),
            None,
        );
        return Err(failure);
    }
    let started = Utc::now();
    let (history_text, history_messages) = grounded_answer_history(input, &prepared.query_ir);
    let result = Box::pin(generate_answer_query(
        input.state,
        input.provider_execution_context,
        &input.conversation_context.current_question_text,
        input.content_text,
        history_text,
        history_messages,
        prepared,
    ))
    .await;
    match result {
        Ok(answer) => {
            record_query_runtime_stage(
                input.state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::Answer,
                RuntimeStageState::Completed,
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
                RuntimeStageState::Failed,
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
            RuntimeStageState::Failed,
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
                RuntimeStageState::Completed,
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
                RuntimeStageState::Failed,
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
            failure_code: None,
            completed_at: Some(Utc::now()),
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

async fn finalize_grounded_retrieve_failure(
    state: &AppState,
    command: &ExecuteConversationTurnCommand,
    conversation: &query_repository::QueryConversationRow,
    request_turn: &query_repository::QueryTurnRow,
    execution: &query_repository::QueryExecutionRow,
    async_operation_id: Uuid,
    interruption_guard: &mut QueryExecutionInterruptionGuard,
    runtime_session: RuntimeExecutionSession,
    failure: QueryAnswerTaskFailure,
) -> Result<ApiError, ApiError> {
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
                    .unwrap_or("query_embedding_failed"),
            ),
            completed_at: runtime_result.execution.completed_at,
        },
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))?;
    if let Err(error) = state
        .canonical_services
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

async fn finalize_grounded_runtime_execution(
    state: &AppState,
    command: &ExecuteConversationTurnCommand,
    conversation: &query_repository::QueryConversationRow,
    request_turn: &query_repository::QueryTurnRow,
    execution: &query_repository::QueryExecutionRow,
    async_operation_id: Uuid,
    interruption_guard: &mut QueryExecutionInterruptionGuard,
    runtime_session: RuntimeExecutionSession,
    outcome: RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>,
) -> Result<query_repository::QueryExecutionRow, ApiError> {
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
    let terminal_execution = load_terminal_query_execution(
        state,
        request_turn,
        execution,
        &runtime_result.outcome,
        runtime_result.execution.failure_code.as_deref(),
        runtime_result.execution.completed_at,
    )
    .await?;
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

async fn build_grounded_answer_cache_context(
    state: &AppState,
    library: &CatalogLibrary,
    conversation: &query_repository::QueryConversationRow,
    conversation_context: &ConversationRuntimeContext,
    content_text: &str,
    top_k: usize,
    request_turn: &query_repository::QueryTurnRow,
) -> Result<Option<QueryResultCacheContext>, ApiError> {
    if !query_result_cache_enabled_for_semantic_rerank(
        state.retrieval_intelligence.semantic_rerank.mode,
    ) {
        return Ok(None);
    }
    match build_query_result_cache_context(
        state,
        library,
        conversation,
        conversation_context,
        content_text,
        top_k,
    )
    .await
    {
        Ok(context) => Ok(Some(context)),
        Err(error) => {
            warn!(
                error = %error,
                conversation_id = %conversation.id,
                library_id = %conversation.library_id,
                "query result cache context unavailable"
            );
            discard_unexecuted_query_request_turn(state, conversation, request_turn).await;
            if error.downcast_ref::<QueryContentProjectionConverging>().is_some() {
                return Err(query_content_projection_converging_error());
            }
            Err(ApiError::InternalMessage("query answer coordination is unavailable".to_string()))
        }
    }
}

async fn finalize_agent_runtime_execution(
    state: &AppState,
    command: &ExecuteConversationTurnCommand,
    conversation: &query_repository::QueryConversationRow,
    request_turn: &query_repository::QueryTurnRow,
    execution: &query_repository::QueryExecutionRow,
    async_operation_id: Uuid,
    runtime_session: RuntimeExecutionSession,
    outcome: RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>,
) -> Result<query_repository::QueryExecutionRow, ApiError> {
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
    let terminal_execution = load_terminal_query_execution(
        state,
        request_turn,
        execution,
        &runtime_result.outcome,
        runtime_result.execution.failure_code.as_deref(),
        runtime_result.execution.completed_at,
    )
    .await?;
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
    request_turn: &query_repository::QueryTurnRow,
    execution: &query_repository::QueryExecutionRow,
    outcome: &RuntimeTerminalOutcome<QueryAnswerTaskSuccess, QueryAnswerTaskFailure>,
    failure_code: Option<&str>,
    completed_at: Option<chrono::DateTime<Utc>>,
) -> Result<query_repository::QueryExecutionRow, ApiError> {
    let row = match outcome {
        RuntimeTerminalOutcome::Completed { .. } | RuntimeTerminalOutcome::Recovered { .. } => {
            query_repository::get_execution_by_id(&state.persistence.postgres, execution.id).await
        }
        RuntimeTerminalOutcome::Failed { .. } | RuntimeTerminalOutcome::Canceled { .. } => {
            query_repository::update_execution(
                &state.persistence.postgres,
                execution.id,
                &query_repository::UpdateQueryExecution {
                    request_turn_id: Some(request_turn.id),
                    response_turn_id: None,
                    failure_code,
                    completed_at,
                },
            )
            .await
        }
    }
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    row.ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))
}

async fn update_query_async_operation_ready(
    state: &AppState,
    operation_id: Uuid,
    completed_at: Option<chrono::DateTime<Utc>>,
) {
    if let Err(error) = state
        .canonical_services
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
        .canonical_services
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

enum CacheFillCoordination {
    Replayed(Box<QueryTurnExecutionResult>),
    Ready(Box<Option<result_cache::QueryResultCacheFillGuard>>),
}

impl CacheFillCoordination {
    fn replayed(result: QueryTurnExecutionResult) -> Self {
        Self::Replayed(Box::new(result))
    }

    fn ready(guard: Option<result_cache::QueryResultCacheFillGuard>) -> Self {
        Self::Ready(Box::new(guard))
    }
}

enum CacheFillAttempt {
    Replayed(Box<QueryTurnExecutionResult>),
    Acquired(Box<result_cache::QueryResultCacheFillGuard>),
    Contended,
    FailOpen,
}

impl CacheFillAttempt {
    fn replayed(result: QueryTurnExecutionResult) -> Self {
        Self::Replayed(Box::new(result))
    }
}

async fn ensure_cache_fill_wait_can_continue(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
    conversation: &query_repository::QueryConversationRow,
    request_turn: &query_repository::QueryTurnRow,
    wait_started: Instant,
    source_checked_at: &mut Instant,
) -> Result<(), ApiError> {
    if source_checked_at.elapsed()
        >= result_cache::QUERY_RESULT_CACHE_NOTIFICATION_FALLBACK_INTERVAL
    {
        *source_checked_at = Instant::now();
        if !query_result_cache_source_is_current(state, cache_context).await? {
            tracing::info!(
                cache_key = %cache_context.cache_key,
                library_id = %conversation.library_id,
                expected_source_truth_version = cache_context.source_truth_version,
                "query result cache wait rejected after source generation changed"
            );
            discard_unexecuted_query_request_turn(state, conversation, request_turn).await;
            return Err(query_content_projection_converging_error());
        }
    }
    if wait_started.elapsed() < result_cache::QUERY_RESULT_CACHE_WAIT_TIMEOUT {
        return Ok(());
    }
    warn!(
        cache_key = %cache_context.cache_key,
        wait_ms = wait_started.elapsed().as_millis() as u64,
        "query result cache fill wait timed out before source execution completed"
    );
    discard_unexecuted_query_request_turn(state, conversation, request_turn).await;
    Err(ApiError::Conflict("query answer is still being prepared".to_string()))
}

async fn wait_for_cache_fill_progress(
    cache_waiter: &mut Option<result_cache::QueryResultCacheWaiter>,
    cache_context: &QueryResultCacheContext,
    wait_started: Instant,
) {
    let remaining =
        result_cache::QUERY_RESULT_CACHE_WAIT_TIMEOUT.saturating_sub(wait_started.elapsed());
    let mut use_polling_fallback = cache_waiter.is_none();
    if let Some(waiter) = cache_waiter.as_mut() {
        let wait_for =
            remaining.min(result_cache::QUERY_RESULT_CACHE_NOTIFICATION_FALLBACK_INTERVAL);
        if let Err(error) = waiter.wait_for_notification(wait_for).await {
            warn!(
                error = %error,
                cache_key = %cache_context.cache_key,
                "query result cache notification stream failed; using bounded polling"
            );
            use_polling_fallback = true;
        }
    }
    if !use_polling_fallback {
        return;
    }
    *cache_waiter = None;
    tokio::time::sleep(remaining.min(result_cache::QUERY_RESULT_CACHE_WAIT_INTERVAL)).await;
}

impl QueryService {
    pub async fn execute_grounded_answer_turn(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        // Mark this scope as the latency-sensitive query lane so its provider
        // calls draw on the query-reserved budget and never starve behind an
        // ingest burst.
        Box::pin(crate::integrations::provider_budget::with_lane(
            crate::integrations::provider_budget::ProviderLane::Query,
            self.execute_grounded_answer_pipeline(state, command),
        ))
        .await
    }

    pub async fn execute_assistant_agent_turn(
        &self,
        state: &AppState,
        auth: &AuthContext,
        command: ExecuteConversationTurnCommand,
        activity_tx: Option<Sender<AgentLoopActivityEvent>>,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        // Mark this scope as the latency-sensitive query lane so every provider
        // call in the agent loop draws on the query-reserved budget and never
        // starves behind an ingest burst.
        Box::pin(crate::integrations::provider_budget::with_lane(
            crate::integrations::provider_budget::ProviderLane::Query,
            self.execute_assistant_agent_turn_inner(state, auth, command, activity_tx),
        ))
        .await
    }

    async fn execute_assistant_agent_turn_inner<'a>(
        &'a self,
        state: &'a AppState,
        auth: &'a AuthContext,
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
        validate_active_conversation(&conversation)?;
        let library =
            state.canonical_services.catalog.get_library(state, conversation.library_id).await?;
        validate_conversation_library(&conversation, &library)?;

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

        conversation = refresh_conversation_title(state, conversation, &content_text).await?;
        let conversation_turns = query_repository::list_turns_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let conversation_context = conversation_runtime_context(
            &conversation_turns,
            request_turn.id,
            &command.external_prior_turns,
        );

        let binding_id = ai_repository::get_effective_binding_by_purpose(
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
        let prior_grounded_answer_context_messages = prior_agent_grounded_answer_context(
            state,
            &conversation,
            &library,
            &conversation_context,
        )
        .await;
        let agent_conversation_history = conversation_context.prompt_history_messages.clone();
        let agent_grounded_answer_tool_history =
            conversation_context.grounded_answer_tool_history.as_slice();

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
        let provider_execution_context = QueryProviderExecutionContext {
            workspace_id: conversation.workspace_id,
            library_id: conversation.library_id,
            query_execution_id: execution_id,
            runtime_execution_id,
        };
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

        let outcome = execute_agent_answer_stages(AgentAnswerStageInput {
            state,
            auth,
            library: &library,
            library_ref: &library_ref,
            content_text: &content_text,
            conversation: &conversation,
            conversation_context: &conversation_context,
            request_turn: &request_turn,
            execution: &execution,
            execution_context_bundle_id,
            provider_execution_context,
            prior_grounded_answer_context_messages: prior_grounded_answer_context_messages
                .as_slice(),
            agent_conversation_history: agent_conversation_history.as_slice(),
            agent_grounded_answer_tool_history,
            agent_request_id: &agent_request_id,
            activity_tx,
            top_k,
            runtime_session: &mut runtime_session,
        })
        .await;

        let terminal_execution = finalize_agent_runtime_execution(
            state,
            &command,
            &conversation,
            &request_turn,
            &execution,
            async_operation.id,
            runtime_session,
            outcome,
        )
        .await?;

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
            answer_disposition: detail.answer_disposition,
            query_ir: None,
            clarification: detail.clarification,
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
        validate_active_conversation(&conversation)?;
        let library =
            state.canonical_services.catalog.get_library(state, conversation.library_id).await?;
        validate_conversation_library(&conversation, &library)?;

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

        conversation = refresh_conversation_title(state, conversation, &content_text).await?;
        let conversation_turns = query_repository::list_turns_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let conversation_context = conversation_runtime_context(
            &conversation_turns,
            request_turn.id,
            &command.external_prior_turns,
        );

        let binding_id = ai_repository::get_effective_binding_by_purpose(
            &state.persistence.postgres,
            conversation.library_id,
            AiBindingPurpose::QueryAnswer.as_str(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .map(|binding| binding.id);

        let top_k = resolve_top_k(Some(command.top_k));
        let cache_context = build_grounded_answer_cache_context(
            state,
            &library,
            &conversation,
            &conversation_context,
            &content_text,
            top_k,
            &request_turn,
        )
        .await?;
        let _cache_fill_guard = match self
            .coordinate_grounded_answer_cache_fill(
                state,
                cache_context.as_ref(),
                &conversation,
                &request_turn,
            )
            .await?
        {
            CacheFillCoordination::Replayed(replayed) => return Ok(*replayed),
            CacheFillCoordination::Ready(guard) => *guard,
        };

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
        let provider_execution_context = QueryProviderExecutionContext {
            workspace_id: conversation.workspace_id,
            library_id: conversation.library_id,
            query_execution_id: execution_id,
            runtime_execution_id,
        };
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
        let mut interruption_guard = QueryExecutionInterruptionGuard::new(
            &state.persistence.postgres,
            execution.id,
            runtime_execution_id,
            async_operation.id,
        );

        let stage_input = GroundedAnswerStageInput {
            state,
            library: &library,
            conversation: &conversation,
            conversation_context: &conversation_context,
            request_turn: &request_turn,
            execution: &execution,
            execution_context_bundle_id,
            runtime_execution_id,
            provider_execution_context,
            content_text: &content_text,
            top_k,
            include_debug: command.include_debug,
        };
        let prepared = match run_grounded_retrieve_stage(&stage_input, &mut runtime_session).await {
            Ok(prepared) => prepared,
            Err(failure) => {
                return Err(finalize_grounded_retrieve_failure(
                    state,
                    &command,
                    &conversation,
                    &request_turn,
                    &execution,
                    async_operation.id,
                    &mut interruption_guard,
                    runtime_session,
                    failure,
                )
                .await?);
            }
        };
        let turn_query_ir = Some(prepared.query_ir.clone());
        let outcome =
            run_grounded_answer_stages(&stage_input, &mut runtime_session, prepared).await;

        let terminal_execution = finalize_grounded_runtime_execution(
            state,
            &command,
            &conversation,
            &request_turn,
            &execution,
            async_operation.id,
            &mut interruption_guard,
            runtime_session,
            outcome,
        )
        .await?;

        let detail = self.get_execution(state, terminal_execution.id).await?;
        if let Some(cache_context) = cache_context.as_ref() {
            store_query_result_cache_winner(state, cache_context, &detail).await;
        }
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

        interruption_guard.disarm();
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
            answer_disposition: detail.answer_disposition,
            query_ir: turn_query_ir,
            clarification: detail.clarification,
        })
    }

    async fn coordinate_grounded_answer_cache_fill(
        &self,
        state: &AppState,
        cache_context: Option<&QueryResultCacheContext>,
        conversation: &query_repository::QueryConversationRow,
        request_turn: &query_repository::QueryTurnRow,
    ) -> Result<CacheFillCoordination, ApiError> {
        let Some(cache_context) = cache_context else {
            return Ok(CacheFillCoordination::ready(None));
        };
        if let Some(replayed) = self
            .try_replay_query_result_cache(state, cache_context, conversation, request_turn)
            .await?
        {
            return Ok(CacheFillCoordination::replayed(replayed));
        }
        let wait_started = Instant::now();
        let mut source_checked_at = Instant::now();
        let mut cache_waiter = None;
        let mut cache_waiter_attempted = false;
        loop {
            match self
                .attempt_grounded_answer_cache_fill(
                    state,
                    cache_context,
                    conversation,
                    request_turn,
                    wait_started,
                    &mut source_checked_at,
                )
                .await?
            {
                CacheFillAttempt::Replayed(replayed) => {
                    return Ok(CacheFillCoordination::Replayed(replayed));
                }
                CacheFillAttempt::Acquired(guard) => {
                    return Ok(CacheFillCoordination::ready(Some(*guard)));
                }
                CacheFillAttempt::FailOpen => return Ok(CacheFillCoordination::ready(None)),
                CacheFillAttempt::Contended => {}
            }
            if let Some(replayed) = self
                .ensure_cache_fill_waiter(
                    state,
                    cache_context,
                    conversation,
                    request_turn,
                    &mut cache_waiter,
                    &mut cache_waiter_attempted,
                )
                .await?
            {
                return Ok(CacheFillCoordination::replayed(replayed));
            }
            wait_for_cache_fill_progress(&mut cache_waiter, cache_context, wait_started).await;
            if let Some(replayed) = self
                .try_replay_query_result_cache(state, cache_context, conversation, request_turn)
                .await?
            {
                return Ok(CacheFillCoordination::replayed(replayed));
            }
        }
    }

    async fn attempt_grounded_answer_cache_fill(
        &self,
        state: &AppState,
        cache_context: &QueryResultCacheContext,
        conversation: &query_repository::QueryConversationRow,
        request_turn: &query_repository::QueryTurnRow,
        wait_started: Instant,
        source_checked_at: &mut Instant,
    ) -> Result<CacheFillAttempt, ApiError> {
        let result = result_cache::try_acquire_fill_guard(
            &state.persistence.redis,
            &cache_context.cache_key,
            Uuid::now_v7(),
        )
        .await;
        match result {
            Ok(Some(guard)) => {
                self.resolve_acquired_cache_fill_guard(
                    state,
                    cache_context,
                    conversation,
                    request_turn,
                    guard,
                )
                .await
            }
            Ok(None) => {
                ensure_cache_fill_wait_can_continue(
                    state,
                    cache_context,
                    conversation,
                    request_turn,
                    wait_started,
                    source_checked_at,
                )
                .await?;
                Ok(CacheFillAttempt::Contended)
            }
            Err(error) if result_cache::fill_lock_error_fails_open(&error) => {
                warn!(
                    error = %error,
                    cache_key = %cache_context.cache_key,
                    "query result cache fill lock unavailable; proceeding without cache coordination"
                );
                Ok(CacheFillAttempt::FailOpen)
            }
            Err(error) => {
                warn!(
                    error = %error,
                    cache_key = %cache_context.cache_key,
                    "query result cache fill lock unavailable"
                );
                discard_unexecuted_query_request_turn(state, conversation, request_turn).await;
                Err(ApiError::InternalMessage(
                    "query answer coordination is unavailable".to_string(),
                ))
            }
        }
    }

    async fn resolve_acquired_cache_fill_guard(
        &self,
        state: &AppState,
        cache_context: &QueryResultCacheContext,
        conversation: &query_repository::QueryConversationRow,
        request_turn: &query_repository::QueryTurnRow,
        guard: result_cache::QueryResultCacheFillGuard,
    ) -> Result<CacheFillAttempt, ApiError> {
        if !query_result_cache_source_is_current(state, cache_context).await? {
            drop(guard);
            tracing::info!(
                cache_key = %cache_context.cache_key,
                library_id = %conversation.library_id,
                expected_source_truth_version = cache_context.source_truth_version,
                "query result cache fill rejected after source generation changed"
            );
            discard_unexecuted_query_request_turn(state, conversation, request_turn).await;
            return Err(query_content_projection_converging_error());
        }
        if let Some(replayed) = self
            .try_replay_query_result_cache(state, cache_context, conversation, request_turn)
            .await?
        {
            return Ok(CacheFillAttempt::replayed(replayed));
        }
        Ok(CacheFillAttempt::Acquired(Box::new(guard)))
    }

    async fn ensure_cache_fill_waiter(
        &self,
        state: &AppState,
        cache_context: &QueryResultCacheContext,
        conversation: &query_repository::QueryConversationRow,
        request_turn: &query_repository::QueryTurnRow,
        cache_waiter: &mut Option<result_cache::QueryResultCacheWaiter>,
        cache_waiter_attempted: &mut bool,
    ) -> Result<Option<QueryTurnExecutionResult>, ApiError> {
        if *cache_waiter_attempted {
            return Ok(None);
        }
        *cache_waiter_attempted = true;
        match result_cache::subscribe_fill_notifications(
            &state.persistence.redis,
            &cache_context.cache_key,
        )
        .await
        {
            Ok(waiter) => {
                *cache_waiter = Some(waiter);
                self.try_replay_query_result_cache(state, cache_context, conversation, request_turn)
                    .await
            }
            Err(error) => {
                warn!(
                    error = %error,
                    cache_key = %cache_context.cache_key,
                    "query result cache notifications unavailable; using bounded polling"
                );
                Ok(None)
            }
        }
    }

    async fn try_replay_query_result_cache(
        &self,
        state: &AppState,
        cache_context: &QueryResultCacheContext,
        conversation: &query_repository::QueryConversationRow,
        request_turn: &query_repository::QueryTurnRow,
    ) -> Result<Option<QueryTurnExecutionResult>, ApiError> {
        // PostgreSQL is the canonical winner mapping. Redis coordinates fills
        // and wakeups, but its payload is never trusted as answer provenance:
        // every replay starts from the tenant-scoped, DB-clock-fresh row.
        let cached = query_result_cache_repository::get_query_result_cache(
            &state.persistence.postgres,
            &cache_context.cache_key,
            result_cache::QUERY_RESULT_CACHE_TTL_SECONDS,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some(cached) = cached else {
            return Ok(None);
        };
        if cached.workspace_id != conversation.workspace_id
            || cached.library_id != conversation.library_id
        {
            evict_query_result_cache_entry(
                state,
                cache_context,
                cached.source_execution_id,
                "persistent cache winner scope does not match request",
            )
            .await;
            return Ok(None);
        }
        if result_cache::bounded_db_remaining_ttl_seconds(cached.remaining_ttl_seconds).is_none() {
            evict_query_result_cache_entry(
                state,
                cache_context,
                cached.source_execution_id,
                "persistent cache winner expired",
            )
            .await;
            return Ok(None);
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
        if detail.execution.workspace_id != conversation.workspace_id
            || detail.execution.library_id != conversation.library_id
        {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution scope does not match request",
            )
            .await;
            return Ok(None);
        }
        if !detail.answer_disposition.is_factual_ready() {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution is not factual-ready",
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
        let Some(query_ir) = detail.query_ir.clone() else {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution has no canonical query IR",
            )
            .await;
            return Ok(None);
        };
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

        let Some((response_turn, _replay)) =
            query_result_cache_repository::create_query_execution_replay(
                &state.persistence.postgres,
                &query_result_cache_repository::CreateQueryExecutionReplayInput {
                    workspace_id: conversation.workspace_id,
                    library_id: conversation.library_id,
                    conversation_id: conversation.id,
                    request_turn_id: request_turn.id,
                    source_execution_id,
                    expected_source_truth_version: cache_context.source_truth_version,
                    cache_key: &cache_context.cache_key,
                    ttl_seconds: result_cache::QUERY_RESULT_CACHE_TTL_SECONDS,
                },
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            // The library changed after this request computed its cache key.
            // Treat that as a normal miss; the caller will execute against the
            // current source state, and this old generation remains isolated
            // under its versioned key.
            tracing::info!(
                cache_key = %cache_context.cache_key,
                source_execution_id = %source_execution_id,
                library_id = %conversation.library_id,
                expected_source_truth_version = cache_context.source_truth_version,
                "query result cache replay skipped after source generation changed"
            );
            return Ok(None);
        };
        tracing::info!(
            stage = "query.result_cache.hit",
            cache_key = %cache_context.cache_key,
            source_execution_id = %source_execution_id,
            conversation_id = %conversation.id,
            request_turn_id = %request_turn.id,
            response_turn_id = %response_turn.id,
            semantic_rerank_mode = state.retrieval_intelligence.semantic_rerank.mode.as_str(),
            semantic_rerank_sample_scheduled = false,
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
            answer_disposition: detail.answer_disposition,
            query_ir: Some(query_ir),
            clarification: detail.clarification,
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
        // The persisted typed outcome is authoritative control-plane state and
        // must not be discarded when the heavier evidence/reference hydration
        // times out. Load the bundle header independently and in parallel so
        // this separation does not add another serial database round trip.
        let persisted_outcome_bundle =
            state.context_store.get_bundle_by_query_execution(execution.id);
        let prepared_reference_context = tokio::time::timeout(
            REFERENCE_CONTEXT_HYDRATION_TIMEOUT,
            load_execution_prepared_reference_context(state, execution.id),
        );
        let (persisted_outcome_bundle, prepared_reference_context) =
            tokio::join!(persisted_outcome_bundle, prepared_reference_context);
        let persisted_outcome_bundle = persisted_outcome_bundle
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let (answer_disposition, clarification) = persisted_execution_answer_outcome(
            execution.id,
            persisted_outcome_bundle.as_ref().map(|bundle| &bundle.candidate_summary),
        );
        let prepared_reference_context = match prepared_reference_context {
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
        let query_ir = prepared_reference_context.bundle_refs.as_ref().and_then(|bundle| {
            query_ir_from_bundle_diagnostics(&bundle.bundle.assembly_diagnostics)
        });
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
            response_turn.as_ref().map(|turn| turn.content_text.as_str()),
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
            answer_disposition,
            clarification,
            query_ir,
        })
    }
}

fn persisted_execution_answer_outcome(
    execution_id: Uuid,
    candidate_summary: Option<&serde_json::Value>,
) -> (crate::domains::query::QueryAnswerDisposition, QueryClarification) {
    let Some(candidate_summary) = candidate_summary else {
        return Default::default();
    };
    persisted_query_answer_outcome(candidate_summary).unwrap_or_else(|_| {
        warn!(
            %execution_id,
            "persisted query answer outcome is invalid; using conservative non-terminal outcome"
        );
        Default::default()
    })
}

async fn discard_unexecuted_query_request_turn(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    request_turn: &query_repository::QueryTurnRow,
) {
    match query_repository::delete_unexecuted_request_turn(
        &state.persistence.postgres,
        conversation.id,
        request_turn.id,
    )
    .await
    {
        Ok(true) => tracing::debug!(
            conversation_id = %conversation.id,
            request_turn_id = %request_turn.id,
            "discarded query request before execution started"
        ),
        Ok(false) => tracing::debug!(
            conversation_id = %conversation.id,
            request_turn_id = %request_turn.id,
            "query request cleanup skipped because it was already accepted or removed"
        ),
        Err(error) => warn!(
            error = %error,
            conversation_id = %conversation.id,
            request_turn_id = %request_turn.id,
            "failed to discard query request after pre-execution coordination error"
        ),
    }
}

async fn build_query_result_cache_context(
    state: &AppState,
    library: &CatalogLibrary,
    conversation: &query_repository::QueryConversationRow,
    conversation_context: &ConversationRuntimeContext,
    user_question: &str,
    top_k: usize,
) -> anyhow::Result<QueryResultCacheContext> {
    anyhow::ensure!(
        library.id == conversation.library_id && library.workspace_id == conversation.workspace_id,
        "query result cache library scope does not match conversation scope"
    );
    let readable_content_identity =
        crate::infra::repositories::content_repository::get_library_readable_content_fingerprint(
            &state.persistence.postgres,
            conversation.library_id,
        )
        .await?;
    let crate::infra::repositories::content_repository::LibraryReadableContentFingerprint {
        value: readable_content_fingerprint,
        source_truth_version,
        projection_is_current,
    } = readable_content_identity;
    if !projection_is_current {
        return Err(QueryContentProjectionConverging.into());
    }
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
    let retrieval_policy_fingerprint = result_cache::retrieval_policy_fingerprint(
        &state.retrieval_intelligence,
        &state.bulk_ingest_hardening,
    );
    let library_answer_config_fingerprint = serde_json::to_string(&(
        library.include_document_hint_in_mcp_answers,
        &library.retrieval_config,
    ))?;
    let cache_key = result_cache::cache_key(&result_cache::QueryResultCacheKeyInput {
        workspace_id: conversation.workspace_id,
        library_id: conversation.library_id,
        source_truth_version,
        library_answer_config_fingerprint: &library_answer_config_fingerprint,
        readable_content_fingerprint: &readable_content_fingerprint,
        graph_projection_version,
        graph_topology_generation,
        binding_fingerprint: &binding_fingerprint,
        retrieval_policy_fingerprint: &retrieval_policy_fingerprint,
        answer_system_prompt:
            crate::services::query::assistant_prompt::GROUNDED_SINGLE_SHOT_SYSTEM_PROMPT,
        answer_runtime_fingerprint: result_cache::answer_runtime_fingerprint(),
        mode_label: super::runtime_mode_label(CANONICAL_QUERY_MODE),
        top_k,
        user_question,
        effective_question: &conversation_context.current_question_text,
        answer_history_text: conversation_context.prompt_history_text.as_deref(),
    });
    Ok(QueryResultCacheContext {
        cache_key,
        library_id: conversation.library_id,
        source_truth_version,
        readable_content_fingerprint,
        graph_projection_version,
        graph_topology_generation,
        binding_fingerprint,
    })
}

async fn query_result_cache_source_is_current(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
) -> Result<bool, ApiError> {
    let current = match catalog_repository::get_library_source_truth_version(
        &state.persistence.postgres,
        cache_context.library_id,
    )
    .await
    {
        Ok(current) => current,
        Err(sqlx::Error::RowNotFound) => return Ok(false),
        Err(error) => return Err(ApiError::internal_with_log(error, "internal")),
    };
    Ok(current == cache_context.source_truth_version)
}

async fn build_query_result_binding_fingerprint(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<String> {
    let mut parts = Vec::new();
    let embedding_profile_key = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .map(|binding| binding.embedding_execution_profile_key());
    parts.push(embedding_profile_key.map_or_else(
        || "embed_profile:none".to_string(),
        |profile_key| format!("embed_profile:{profile_key}"),
    ));
    let purposes = query_result_binding_purposes(state.retrieval_intelligence.semantic_rerank.mode);
    let purpose_names =
        purposes.iter().map(|purpose| purpose.as_str().to_string()).collect::<Vec<_>>();
    let identities = ai_repository::list_effective_binding_identities(
        &state.persistence.postgres,
        library_id,
        &purpose_names,
    )
    .await?
    .into_iter()
    .map(|identity| (identity.binding_purpose.clone(), identity))
    .collect::<HashMap<_, _>>();
    for purpose in purposes {
        let part = match identities.get(purpose.as_str()) {
            Some(binding) => format!(
                "{}:{}:{}:{}:{}",
                purpose.as_str(),
                binding.id,
                binding.account_id,
                binding.model_catalog_id,
                binding.updated_at.timestamp_micros()
            ),
            None => format!("{}:none", purpose.as_str()),
        };
        parts.push(part);
    }
    Ok(parts.join("|"))
}

fn query_result_binding_purposes(
    _semantic_rerank_mode: crate::domains::query::SemanticRerankMode,
) -> &'static [AiBindingPurpose] {
    const PURPOSES: &[AiBindingPurpose] = &[
        AiBindingPurpose::QueryCompile,
        AiBindingPurpose::ExtractGraph,
        AiBindingPurpose::QueryAnswer,
    ];
    PURPOSES
}

const fn query_result_cache_enabled_for_semantic_rerank(
    _mode: crate::domains::query::SemanticRerankMode,
) -> bool {
    true
}

async fn store_query_result_cache_winner(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
    detail: &QueryExecutionDetail,
) {
    if !detail.answer_disposition.is_factual_ready() {
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
            expected_source_truth_version: cache_context.source_truth_version,
            readable_content_fingerprint: &cache_context.readable_content_fingerprint,
            graph_projection_version: cache_context.graph_projection_version,
            graph_topology_generation: cache_context.graph_topology_generation,
            binding_fingerprint: &cache_context.binding_fingerprint,
            ttl_seconds: result_cache::QUERY_RESULT_CACHE_TTL_SECONDS,
        },
    )
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::info!(
                cache_key = %cache_context.cache_key,
                execution_id = %detail.execution.id,
                library_id = %detail.execution.library_id,
                expected_source_truth_version = cache_context.source_truth_version,
                "query result cache winner skipped after source generation changed"
            );
            return;
        }
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
    if let Some(remaining_ttl_seconds) =
        result_cache::bounded_db_remaining_ttl_seconds(row.remaining_ttl_seconds)
    {
        if let Err(error) = result_cache::put_cached_execution_id(
            &state.persistence.redis,
            &cache_context.cache_key,
            row.source_execution_id,
            remaining_ttl_seconds,
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
    } else {
        warn!(
            cache_key = %cache_context.cache_key,
            source_execution_id = %row.source_execution_id,
            "query result cache winner expired before redis refresh"
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
    if let Err(error) = result_cache::delete_cached_execution_id_if_matches(
        &state.persistence.redis,
        &cache_context.cache_key,
        source_execution_id,
    )
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
        source_execution_id,
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

#[derive(Default)]
struct CollectedAgentGrounding {
    primary_reference_set: Option<KnowledgeContextBundleReferenceSetRow>,
    primary_execution_id: Option<Uuid>,
    primary_reference_score: usize,
    grounding_sources: Vec<(Uuid, Uuid)>,
    chunk_references: HashMap<Uuid, KnowledgeBundleChunkReferenceRow>,
    entity_references: HashMap<Uuid, KnowledgeBundleEntityReferenceRow>,
    relation_references: HashMap<Uuid, KnowledgeBundleRelationReferenceRow>,
    evidence_references: HashMap<Uuid, KnowledgeBundleEvidenceReferenceRow>,
    selected_fact_ids: Vec<Uuid>,
}

struct MaterializedGroundingReferences {
    chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    entity_references: Vec<KnowledgeBundleEntityReferenceRow>,
    relation_references: Vec<KnowledgeBundleRelationReferenceRow>,
    evidence_references: Vec<KnowledgeBundleEvidenceReferenceRow>,
}

impl CollectedAgentGrounding {
    fn merge(
        &mut self,
        child_execution: &query_repository::QueryExecutionRow,
        reference_set: KnowledgeContextBundleReferenceSetRow,
    ) {
        let reference_score = context_reference_set_grounding_count(&reference_set);
        if self.primary_reference_set.is_none() || reference_score > self.primary_reference_score {
            self.primary_reference_score = reference_score;
            self.primary_execution_id = Some(child_execution.id);
            self.primary_reference_set = Some(reference_set.clone());
        }
        self.grounding_sources.push((child_execution.id, child_execution.runtime_execution_id));
        for fact_id in &reference_set.bundle.selected_fact_ids {
            if !self.selected_fact_ids.contains(fact_id) {
                self.selected_fact_ids.push(*fact_id);
            }
        }
        for reference in reference_set.chunk_references {
            merge_chunk_reference(&mut self.chunk_references, reference);
        }
        for reference in reference_set.entity_references {
            merge_entity_reference(&mut self.entity_references, reference);
        }
        for reference in reference_set.relation_references {
            merge_relation_reference(&mut self.relation_references, reference);
        }
        for reference in reference_set.evidence_references {
            merge_evidence_reference(&mut self.evidence_references, reference);
        }
    }
}

async fn materialize_agent_grounding_from_child_execution(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    parent_context_bundle_id: Uuid,
    child_query_execution_ids: &[Uuid],
) -> anyhow::Result<Option<MaterializedAgentGrounding>> {
    let Some(mut collected) =
        collect_agent_grounding_from_children(state, parent_execution, child_query_execution_ids)
            .await?
    else {
        return Ok(None);
    };
    let primary_execution_id =
        collected.primary_execution_id.context("primary agent grounding execution is missing")?;
    let source_execution_id = collected
        .grounding_sources
        .first()
        .map(|(execution_id, _)| *execution_id)
        .context("agent grounding source execution is missing")?;
    let reference_set = collected
        .primary_reference_set
        .take()
        .context("primary agent grounding reference set is missing")?;
    let now = Utc::now();
    let mut bundle = reference_set.bundle;
    bundle.bundle_id = parent_context_bundle_id;
    bundle.workspace_id = parent_execution.workspace_id;
    bundle.library_id = parent_execution.library_id;
    bundle.query_execution_id = Some(parent_execution.id);
    bundle.selected_fact_ids = std::mem::take(&mut collected.selected_fact_ids);
    bundle.assembly_diagnostics = agent_grounding_assembly_diagnostics(
        &bundle.assembly_diagnostics,
        &collected.grounding_sources,
    );
    bundle.created_at = now;
    bundle.updated_at = now;

    let references = sorted_materialized_grounding_references(collected);
    persist_materialized_agent_grounding(
        state,
        parent_execution,
        parent_context_bundle_id,
        &bundle,
        &references,
        now,
    )
    .await?;
    Ok(Some(MaterializedAgentGrounding { source_execution_id, primary_execution_id }))
}

async fn collect_agent_grounding_from_children(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    child_query_execution_ids: &[Uuid],
) -> anyhow::Result<Option<CollectedAgentGrounding>> {
    let mut collected = CollectedAgentGrounding::default();
    for child_execution_id in child_query_execution_ids.iter().rev().copied() {
        let Some((child_execution, reference_set)) =
            load_child_agent_grounding(state, parent_execution, child_execution_id).await?
        else {
            continue;
        };
        collected.merge(&child_execution, reference_set);
    }
    Ok(collected.primary_reference_set.is_some().then_some(collected))
}

async fn load_child_agent_grounding(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    child_execution_id: Uuid,
) -> anyhow::Result<
    Option<(query_repository::QueryExecutionRow, KnowledgeContextBundleReferenceSetRow)>,
> {
    if child_execution_id == parent_execution.id {
        return Ok(None);
    }
    let Some(child_execution) =
        query_repository::get_execution_by_id(&state.persistence.postgres, child_execution_id)
            .await?
    else {
        return Ok(None);
    };
    if child_execution.workspace_id != parent_execution.workspace_id
        || child_execution.library_id != parent_execution.library_id
    {
        return Ok(None);
    }
    let Some(reference_set) =
        state.context_store.get_bundle_reference_set_by_query_execution(child_execution_id).await?
    else {
        return Ok(None);
    };
    if !context_reference_set_has_grounding(&reference_set) {
        return Ok(None);
    }
    Ok(Some((child_execution, reference_set)))
}

fn sorted_materialized_grounding_references(
    collected: CollectedAgentGrounding,
) -> MaterializedGroundingReferences {
    let mut references = MaterializedGroundingReferences {
        chunk_references: collected.chunk_references.into_values().collect(),
        entity_references: collected.entity_references.into_values().collect(),
        relation_references: collected.relation_references.into_values().collect(),
        evidence_references: collected.evidence_references.into_values().collect(),
    };
    sort_chunk_references(&mut references.chunk_references);
    sort_entity_references(&mut references.entity_references);
    sort_relation_references(&mut references.relation_references);
    sort_evidence_references(&mut references.evidence_references);
    references
}

async fn persist_materialized_agent_grounding(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    parent_context_bundle_id: Uuid,
    bundle: &KnowledgeContextBundleRow,
    references: &MaterializedGroundingReferences,
    now: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    state.context_store.upsert_bundle(bundle).await?;
    state
        .context_store
        .replace_bundle_chunk_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_chunk_reference_edges(
                parent_context_bundle_id,
                &references.chunk_references,
                now,
            ),
        )
        .await?;
    state
        .context_store
        .replace_bundle_entity_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_entity_reference_edges(
                parent_context_bundle_id,
                &references.entity_references,
                now,
            ),
        )
        .await?;
    state
        .context_store
        .replace_bundle_relation_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_relation_reference_edges(
                parent_context_bundle_id,
                &references.relation_references,
                now,
            ),
        )
        .await?;
    state
        .context_store
        .replace_bundle_evidence_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_evidence_reference_edges(
                parent_context_bundle_id,
                &references.evidence_references,
                now,
            ),
        )
        .await?;
    append_materialized_chunk_references(state, parent_execution.id, &references.chunk_references)
        .await
}

async fn append_materialized_chunk_references(
    state: &AppState,
    parent_execution_id: Uuid,
    references: &[KnowledgeBundleChunkReferenceRow],
) -> anyhow::Result<()> {
    let chunk_refs = references
        .iter()
        .map(|reference| query_repository::NewQueryChunkReference {
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect::<Vec<_>>();
    query_repository::append_chunk_references(
        &state.persistence.postgres,
        parent_execution_id,
        &chunk_refs,
    )
    .await?;
    Ok(())
}

async fn verify_agent_answer_against_tool_evidence(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    context_bundle_id: Uuid,
    answer_text: &str,
    assistant_grounding: &AssistantGroundingEvidence,
    answer_provenance: AgentAnswerProvenance,
    canonical_answer_outcome: Option<&AgentCanonicalAnswerOutcome>,
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
    let query_ir = reference_context
        .bundle_refs
        .as_ref()
        .and_then(|refs| query_ir_from_bundle_diagnostics(&refs.bundle.assembly_diagnostics));
    let intent_profile = query_intent_profile_from_query_ir(query_ir.as_ref());
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
        &intent_profile,
        &canonical_evidence,
        &[],
        &prompt_context,
        assistant_grounding,
    );
    let finalized = finalize_answer_visibility(
        VerificationLevel::Moderate,
        verification.state,
        &verification.warnings,
        QueryLanguage::Auto,
        answer_text,
        AnswerVisibilityKind::FactualCandidate,
    );
    let (answer_disposition, clarification) = finalized_agent_answer_outcome(
        answer_provenance,
        canonical_answer_outcome,
        finalized.disposition,
    )?;
    persist_query_verification(
        state,
        execution.id,
        &verification,
        answer_disposition,
        &clarification,
        &canonical_evidence,
        assistant_grounding,
    )
    .await?;
    Ok(verification)
}

fn finalized_agent_answer_outcome(
    answer_provenance: AgentAnswerProvenance,
    canonical_answer_outcome: Option<&AgentCanonicalAnswerOutcome>,
    parent_disposition: crate::domains::query::QueryAnswerDisposition,
) -> anyhow::Result<(crate::domains::query::QueryAnswerDisposition, QueryClarification)> {
    match (answer_provenance, canonical_answer_outcome) {
        (AgentAnswerProvenance::Composed, None) => {
            Ok((parent_disposition, QueryClarification::default()))
        }
        (AgentAnswerProvenance::Composed, Some(_)) => {
            anyhow::bail!("composed agent answer unexpectedly carries a child finalizer outcome")
        }
        (AgentAnswerProvenance::CanonicalGroundedAnswerPassthrough, None) => {
            anyhow::bail!("canonical grounded-answer passthrough is missing its typed outcome")
        }
        (AgentAnswerProvenance::CanonicalGroundedAnswerPassthrough, Some(outcome)) => {
            anyhow::ensure!(
                outcome.disposition.is_terminal(),
                "canonical grounded-answer passthrough carries a nonterminal outcome"
            );
            anyhow::ensure!(
                matches!(
                    outcome.disposition,
                    crate::domains::query::QueryAnswerDisposition::Clarification
                ) == outcome.clarification.required,
                "canonical grounded-answer disposition and clarification disagree"
            );
            if !outcome.clarification.required {
                anyhow::ensure!(
                    outcome.clarification == QueryClarification::default(),
                    "non-clarification canonical outcome carries clarification metadata"
                );
            }
            let disposition = if matches!(
                outcome.disposition,
                crate::domains::query::QueryAnswerDisposition::FactualReady
            ) {
                parent_disposition
            } else {
                outcome.disposition
            };
            Ok((disposition, outcome.clarification.clone()))
        }
    }
}

async fn ensure_agent_tool_context_bundle(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    context_bundle_id: Uuid,
    assistant_grounding: &AssistantGroundingEvidence,
    verification_state: QueryVerificationState,
    verification_warnings: serde_json::Value,
) -> anyhow::Result<()> {
    if state.context_store.get_bundle_by_query_execution(execution.id).await?.is_some() {
        return Ok(());
    }

    let now = Utc::now();
    let bundle = KnowledgeContextBundleRow {
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
    state.context_store.upsert_bundle(&bundle).await?;
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

fn literal_revision_covers_required_literals(answer: &str, required_literals: &[String]) -> bool {
    required_literals.iter().all(|literal| text_contains_literal(answer, literal))
}

fn literal_revision_history_literal_coverage(
    draft_answer: &str,
    revised_answer: &str,
    history: &[ExternalConversationTurn],
) -> Option<(usize, usize, usize)> {
    const MIN_HISTORY_ANCHOR_LITERALS: usize = 4;
    const MIN_DRAFT_VISIBLE_LITERALS: usize = 2;

    let inventory = latest_dense_history_revision_anchor_literals(history);
    if inventory.len() < MIN_HISTORY_ANCHOR_LITERALS {
        return None;
    }
    let draft_visible = literal_visibility_count(draft_answer, &inventory);
    if draft_visible < MIN_DRAFT_VISIBLE_LITERALS {
        return None;
    }
    let revised_visible = literal_visibility_count(revised_answer, &inventory);
    Some((draft_visible, revised_visible, inventory.len()))
}

fn literal_visibility_count(answer: &str, literals: &[String]) -> usize {
    literals.iter().filter(|literal| text_contains_literal(answer, literal)).count()
}

fn latest_dense_history_revision_anchor_literals(
    history: &[ExternalConversationTurn],
) -> Vec<String> {
    let mut literals = history
        .iter()
        .rev()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .map(|turn| dense_history_literals(&turn.content_text))
        .unwrap_or_default()
        .into_iter()
        .filter(|literal| literal_text_is_revision_anchor_shaped(literal))
        .collect::<Vec<_>>();
    let mut seen = HashSet::<String>::new();
    literals.retain(|literal| seen.insert(literal.clone()));
    literals
}

fn literal_text_is_revision_anchor_shaped(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && !trimmed.chars().any(char::is_whitespace)
        && (literal_text_is_identifier_shaped(trimmed)
            || trimmed.chars().any(|ch| !ch.is_alphanumeric()))
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
    let literal_line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ir.memory.literals.v1:"))
        .unwrap_or("");
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
    !child_query_execution_ids.is_empty() || assistant_grounding.has_verifier_grade_evidence()
}

fn agent_answer_requires_parent_tool_evidence_verification(
    has_verifiable_tool_evidence: bool,
) -> bool {
    has_verifiable_tool_evidence
}

fn agent_answer_allows_parent_model_revision(provenance: AgentAnswerProvenance) -> bool {
    matches!(provenance, AgentAnswerProvenance::Composed)
}

fn agent_verification_needs_literal_revision(verification: &RuntimeAnswerVerification) -> bool {
    verification.warnings.iter().any(|warning| {
        warning.code == "unsupported_literal" || warning.code == "unsupported_canonical_claim"
    })
}

fn ui_agent_iteration_cap() -> usize {
    usize::from(QueryAnswerTask::spec().max_turns)
        .saturating_add(2)
        .max(ASSISTANT_AGENT_LOOP_MIN_ITERATIONS)
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

fn map_runtime_execution_error(execution_id: Uuid, error: RuntimeExecutionError) -> ApiError {
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

fn make_query_answer_failure(code: &str, summary: impl Into<String>) -> QueryAnswerTaskFailure {
    QueryAnswerTaskFailure::new(code, summary)
}

fn map_runtime_answer_query_failure(
    failure: RuntimeAnswerQueryFailure,
    fallback_code: &'static str,
) -> QueryAnswerTaskFailure {
    let RuntimeAnswerQueryFailure { error, provider_calls } = failure;
    let code = anyhow_failure_code(&error, fallback_code);
    make_query_answer_failure(code, error.to_string()).with_provider_calls(provider_calls)
}

fn query_service_failure_code(error: &QueryServiceError, fallback: &'static str) -> &'static str {
    match error {
        QueryServiceError::LibraryNotFound { .. } | QueryServiceError::NotFound { .. } => {
            "query_not_found"
        }
        QueryServiceError::BindingNotConfigured { .. } => "query_binding_not_configured",
        QueryServiceError::StateConflict { .. } => "query_state_conflict",
        QueryServiceError::ProviderUnavailable { .. } => "query_provider_failed",
        QueryServiceError::CacheUnavailable { .. } => "query_dependency_unavailable",
        QueryServiceError::Cancelled => "query_cancelled",
        QueryServiceError::DeadlineExceeded => "query_deadline_exceeded",
        QueryServiceError::Repository(_) | QueryServiceError::Internal(_) => fallback,
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

fn anyhow_failure_code(error: &anyhow::Error, fallback: &'static str) -> &'static str {
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

fn make_runtime_failure_summary(code: &str, summary: &str) -> RuntimeFailureSummary {
    RuntimeFailureSummary {
        code: code.to_string(),
        summary_redacted: if is_runtime_policy_failure_code(code) {
            bounded_runtime_policy_summary(summary)
        } else {
            runtime_failure_summary_from_typed_code(code)
        },
    }
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
        failure.and_then(|value| {
            if is_runtime_policy_failure_code(&value.code) {
                bounded_runtime_policy_summary(&value.summary)
            } else {
                runtime_failure_summary_from_typed_code(&value.code)
            }
        }),
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::items_after_test_module,
    reason = "test assertions require descriptive failures; keeping this large private test module adjacent to the runtime-stage helpers avoids a high-risk mechanical move"
)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use crate::agent_runtime::{
        default_policy::{DefaultRuntimePolicy, DefaultRuntimePolicyRules},
        executor::RuntimeExecutor,
        hooks::{NoopRuntimeHooks, RuntimeHooks},
        policy::RuntimePolicy,
        registry::RuntimeTaskRegistry,
        response::{RuntimeFailureSummary, RuntimeRecoveryOutcome, RuntimeTerminalOutcome},
        task::RuntimeTaskRequest,
        tasks::query_answer::{
            QueryAnswerTask, QueryAnswerTaskFailure, QueryAnswerTaskInput, QueryAnswerTaskSuccess,
            QueryProviderCall, QueryProviderCallAttribution, QueryProviderCallKind,
        },
    };
    use crate::domains::agent_runtime::{
        RuntimeExecutionOwner, RuntimeStageKind, RuntimeStageState,
    };
    use crate::domains::query_ir::{
        EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryIR, QueryLanguage,
        QueryScope, SourceSliceDirection, SourceSliceFilter, SourceSliceSpec,
    };
    use crate::domains::{
        ai::AiBindingPurpose,
        query::{QueryAnswerDisposition, QueryClarification, QueryTurnKind},
    };
    use crate::services::query::assistant_grounding::AssistantGroundingEvidence;
    use crate::services::query::error::QueryServiceError;
    use crate::services::query::service::ExternalConversationTurn;

    use super::{
        ASSISTANT_AGENT_LOOP_DEADLINE_MS, ASSISTANT_AGENT_LOOP_MIN_ITERATION_BUDGET_MS,
        ASSISTANT_AGENT_LOOP_MIN_ITERATIONS, agent_answer_allows_parent_model_revision,
        agent_answer_requires_parent_tool_evidence_verification,
        agent_has_verifiable_tool_evidence, anyhow_failure_code,
        compiled_query_uses_answer_history, finalized_agent_answer_outcome,
        latest_dense_history_identifier_literals, literal_inventory_coverage_revision_targets,
        literal_revision_covers_required_literals, literal_revision_history_literal_coverage,
        make_query_terminal_failure_outcome, map_query_execution_failure,
        map_runtime_answer_query_failure, map_runtime_execution_error,
        no_agent_tool_evidence_warnings, persisted_execution_answer_outcome,
        query_result_binding_purposes, query_result_cache_enabled_for_semantic_rerank,
        record_query_runtime_stage, should_backfill_graph_entity_references, text_contains_literal,
        ui_agent_iteration_cap,
    };
    use crate::services::query::agent_loop::{AgentAnswerProvenance, AgentCanonicalAnswerOutcome};

    fn release_inventory_query_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![crate::domains::query_ir::QueryTargetKind::Release],
            target_entities: vec![EntityMention {
                label: "Alpha Suite".to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: vec![LiteralSpan {
                text: "5".to_string(),
                kind: LiteralKind::NumericCode,
            }],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: Some(SourceSliceSpec {
                direction: SourceSliceDirection::Tail,
                count: Some(5),
                filter: SourceSliceFilter::ReleaseMarker,
            }),
            retrieval_query: Some("latest 5 Alpha Suite releases".to_string()),
            confidence: 0.96,
        }
    }

    #[test]
    fn query_failure_mapping_uses_typed_code_instead_of_message_text() {
        let execution_id = Uuid::now_v7();
        let failure = QueryAnswerTaskFailure::new(
            "query_answer_failed",
            "query_binding_not_configured query_retrieval_unavailable",
        );

        assert!(matches!(
            map_query_execution_failure(&execution_id, "question", &failure),
            crate::interfaces::http::router_support::ApiError::Internal
        ));
    }

    #[test]
    fn typed_binding_failure_maps_to_binding_code_despite_misleading_message() {
        let error = anyhow::Error::new(QueryServiceError::BindingNotConfigured {
            message: "query_provider_failed".to_string(),
        });

        assert_eq!(
            anyhow_failure_code(&error, "query_embedding_failed"),
            "query_binding_not_configured"
        );
    }

    #[test]
    fn untyped_binding_words_do_not_change_failure_code() {
        let error = anyhow::anyhow!("query_binding_not_configured");

        assert_eq!(anyhow_failure_code(&error, "query_embedding_failed"), "query_embedding_failed");
    }

    #[test]
    fn query_failure_mapping_does_not_expose_diagnostic_summary() {
        let execution_id = Uuid::now_v7();
        let private_diagnostic = "opaque-provider-diagnostic";
        let failure = QueryAnswerTaskFailure::new("query_provider_failed", private_diagnostic);

        let error = map_query_execution_failure(&execution_id, "private question", &failure);

        assert!(!error.to_string().contains(private_diagnostic));
        assert!(!error.to_string().contains("private question"));
    }

    #[test]
    fn runtime_initialization_failure_does_not_expose_diagnostic_text() {
        let private_diagnostic = "runtime-initialization-sentinel-secret";
        let error = map_runtime_execution_error(
            Uuid::now_v7(),
            crate::agent_runtime::executor::RuntimeExecutionError::InvalidTaskSpec(
                private_diagnostic.to_string(),
            ),
        );

        assert!(!error.to_string().contains(private_diagnostic));
    }

    #[test]
    fn query_terminal_failure_summary_does_not_persist_diagnostic_text() {
        let private_diagnostic = "terminal-sentinel-secret-and-private-query";
        let outcome = make_query_terminal_failure_outcome(QueryAnswerTaskFailure::new(
            "query_provider_failed",
            private_diagnostic,
        ));

        let RuntimeTerminalOutcome::Failed { summary, .. } = outcome else {
            panic!("provider failure must remain a failed terminal outcome");
        };

        assert_eq!(summary.summary_redacted.as_deref(), Some("query_provider_failed"));
        assert!(
            !summary.summary_redacted.as_deref().unwrap_or_default().contains(private_diagnostic)
        );
    }

    #[test]
    fn query_terminal_policy_failure_preserves_typed_redacted_summary() {
        let policy_summary = "operator-approved redacted policy explanation";
        let outcome = make_query_terminal_failure_outcome(QueryAnswerTaskFailure::new(
            "runtime_policy_rejected",
            policy_summary,
        ));

        let RuntimeTerminalOutcome::Canceled { summary, .. } = outcome else {
            panic!("policy rejection must remain a canceled terminal outcome");
        };

        assert_eq!(summary.summary_redacted.as_deref(), Some(policy_summary));
    }

    #[tokio::test]
    async fn query_runtime_stage_failure_summary_does_not_persist_diagnostic_text() {
        let registry = RuntimeTaskRegistry::default().register_task::<QueryAnswerTask>();
        let policy: Arc<dyn RuntimePolicy> =
            Arc::new(DefaultRuntimePolicy::new(2_000, DefaultRuntimePolicyRules::default()));
        let hooks: Arc<dyn RuntimeHooks> = Arc::new(NoopRuntimeHooks);
        let executor = RuntimeExecutor::new(registry, policy, hooks);
        let request = RuntimeTaskRequest::<QueryAnswerTask>::new(
            QueryAnswerTaskInput {
                query_execution_id: Uuid::now_v7(),
                question: "stage-sentinel-private-query".to_string(),
                prompt_history_text: None,
                grounded_context_text: String::new(),
            },
            RuntimeExecutionOwner::query_execution(Uuid::now_v7()),
        );
        let mut session = executor.seed_session(&request).await.expect("seed runtime session");
        let private_diagnostic = "stage-sentinel-secret-and-private-query";
        let failure = QueryAnswerTaskFailure::new("query_provider_failed", private_diagnostic);

        record_query_runtime_stage(
            &executor,
            &mut session,
            RuntimeStageKind::Answer,
            RuntimeStageState::Failed,
            false,
            Some(&failure),
            Some(chrono::Utc::now()),
        );

        let stage = session.trace.stages.last().expect("failed stage record");
        assert_eq!(stage.failure_summary_redacted.as_deref(), Some("query_provider_failed"));
        assert!(
            !stage
                .failure_summary_redacted
                .as_deref()
                .unwrap_or_default()
                .contains(private_diagnostic)
        );
    }

    #[test]
    fn query_failure_mapping_preserves_explicit_binding_code() {
        let execution_id = Uuid::now_v7();
        let failure = QueryAnswerTaskFailure::new("query_binding_not_configured", "opaque");

        assert!(matches!(
            map_query_execution_failure(&execution_id, "question", &failure),
            crate::interfaces::http::router_support::ApiError::ServiceUnavailable {
                kind: "query_binding_not_configured",
                ..
            }
        ));
    }

    #[test]
    fn query_failure_mapping_preserves_explicit_retrieval_code() {
        let execution_id = Uuid::now_v7();
        let failure = QueryAnswerTaskFailure::new("query_retrieval_unavailable", "opaque");

        assert!(matches!(
            map_query_execution_failure(&execution_id, "question", &failure),
            crate::interfaces::http::router_support::ApiError::ServiceUnavailable {
                kind: "query_retrieval_unavailable",
                ..
            }
        ));
    }

    #[test]
    fn query_failure_mapping_honors_explicit_deadline_code() {
        let execution_id = Uuid::now_v7();
        let failure = QueryAnswerTaskFailure::new("query_deadline_exceeded", "opaque");

        assert!(matches!(
            map_query_execution_failure(&execution_id, "question", &failure),
            crate::interfaces::http::router_support::ApiError::GatewayTimeout { .. }
        ));
    }

    #[test]
    fn compiled_ordinary_query_does_not_enable_answer_history() {
        let query_ir = release_inventory_query_ir();

        assert!(!compiled_query_uses_answer_history(
            query_ir.retrieval_query.as_deref().expect("retrieval query"),
            &query_ir,
        ));
    }

    #[test]
    fn compiled_follow_up_query_enables_answer_history() {
        let mut query_ir = release_inventory_query_ir();
        query_ir.act = QueryAct::FollowUp;

        assert!(compiled_query_uses_answer_history("refine it", &query_ir));
    }

    #[test]
    fn compiler_resolved_standalone_query_enables_answer_history_after_refs_are_cleared() {
        let mut query_ir = release_inventory_query_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.conversation_refs.clear();
        query_ir.retrieval_query = Some("Subject Alpha refinement".to_string());

        assert!(compiled_query_uses_answer_history("refinement", &query_ir));
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
    fn canonical_grounded_answer_passthrough_skips_only_parent_model_revision() {
        assert!(!agent_answer_allows_parent_model_revision(
            AgentAnswerProvenance::CanonicalGroundedAnswerPassthrough,
        ));
        assert!(agent_answer_allows_parent_model_revision(AgentAnswerProvenance::Composed,));
        assert!(agent_answer_requires_parent_tool_evidence_verification(true));
    }

    #[test]
    fn canonical_grounded_answer_outcome_preserves_nonfactual_child_outcomes() {
        let clarification = QueryClarification {
            required: true,
            question: Some("Which neutral variant?".to_string()),
            answer_candidates: Vec::new(),
        };
        for outcome in [
            AgentCanonicalAnswerOutcome {
                disposition: QueryAnswerDisposition::FactualReady,
                clarification: QueryClarification::default(),
            },
            AgentCanonicalAnswerOutcome {
                disposition: QueryAnswerDisposition::SafeFallback,
                clarification: QueryClarification::default(),
            },
            AgentCanonicalAnswerOutcome {
                disposition: QueryAnswerDisposition::Clarification,
                clarification: clarification.clone(),
            },
        ] {
            let selected = finalized_agent_answer_outcome(
                AgentAnswerProvenance::CanonicalGroundedAnswerPassthrough,
                Some(&outcome),
                QueryAnswerDisposition::FactualReady,
            )
            .expect("valid child outcome");

            assert_eq!(selected, (outcome.disposition, outcome.clarification));
        }
    }

    #[test]
    fn parent_verification_downgrades_factual_canonical_passthrough() {
        let selected = finalized_agent_answer_outcome(
            AgentAnswerProvenance::CanonicalGroundedAnswerPassthrough,
            Some(&AgentCanonicalAnswerOutcome {
                disposition: QueryAnswerDisposition::FactualReady,
                clarification: QueryClarification::default(),
            }),
            QueryAnswerDisposition::NonTerminal,
        )
        .expect("valid child outcome");

        assert_eq!(selected, (QueryAnswerDisposition::NonTerminal, QueryClarification::default()));
    }

    #[test]
    fn composed_answer_is_parent_finalized_and_outcome_mismatches_fail_closed() {
        assert_eq!(
            finalized_agent_answer_outcome(
                AgentAnswerProvenance::Composed,
                None,
                QueryAnswerDisposition::SafeFallback,
            )
            .expect("composed answer"),
            (QueryAnswerDisposition::SafeFallback, QueryClarification::default()),
        );
        assert!(
            finalized_agent_answer_outcome(
                AgentAnswerProvenance::CanonicalGroundedAnswerPassthrough,
                None,
                QueryAnswerDisposition::FactualReady,
            )
            .is_err()
        );
        assert!(
            finalized_agent_answer_outcome(
                AgentAnswerProvenance::Composed,
                Some(&AgentCanonicalAnswerOutcome {
                    disposition: QueryAnswerDisposition::FactualReady,
                    clarification: QueryClarification::default(),
                }),
                QueryAnswerDisposition::FactualReady,
            )
            .is_err()
        );
    }

    fn provider_call(
        binding_id: Uuid,
        binding_purpose: AiBindingPurpose,
        provider_kind: &str,
        model_name: &str,
        call_kind: QueryProviderCallKind,
        usage_json: serde_json::Value,
    ) -> QueryProviderCall {
        QueryProviderCallAttribution::try_new(
            binding_id,
            binding_purpose,
            crate::domains::provider_profiles::ProviderModelSelection {
                provider_kind: provider_kind.to_string(),
                model_name: model_name.to_string(),
            },
            call_kind,
        )
        .expect("test provider attribution must be valid")
        .record(Uuid::now_v7(), usage_json)
    }

    #[test]
    fn provider_call_keeps_the_preallocated_billing_event_id() {
        let provider_call = provider_call(
            Uuid::now_v7(),
            AiBindingPurpose::Agent,
            "agent-provider",
            "agent-model",
            QueryProviderCallKind::QueryAgent,
            serde_json::json!({"output_tokens": 5}),
        );

        assert_ne!(provider_call.provider_call_id(), Uuid::nil());
        assert_eq!(provider_call.clone().provider_call_id(), provider_call.provider_call_id());
    }

    #[test]
    fn every_terminal_outcome_preserves_the_precompleted_provider_event_identity() {
        let provider_call = provider_call(
            Uuid::now_v7(),
            AiBindingPurpose::QueryAnswer,
            "answer-provider",
            "answer-model",
            QueryProviderCallKind::QueryAnswer,
            serde_json::json!({"input_tokens": 3}),
        );
        let provider_call_id = provider_call.provider_call_id();
        let completed = RuntimeTerminalOutcome::Completed {
            success: QueryAnswerTaskSuccess {
                answer_text: "completed".to_string(),
                provider_calls: vec![provider_call.clone()],
            },
        };
        let recovered = RuntimeTerminalOutcome::Recovered {
            success: QueryAnswerTaskSuccess {
                answer_text: "recovered".to_string(),
                provider_calls: vec![provider_call.clone()],
            },
            recovery: RuntimeRecoveryOutcome { attempts: 1, summary_redacted: None },
        };
        let summary = RuntimeFailureSummary {
            code: "neutral_failure".to_string(),
            summary_redacted: Some("neutral_failure".to_string()),
        };
        let failed = RuntimeTerminalOutcome::Failed {
            failure: QueryAnswerTaskFailure::new("neutral_failure", "opaque")
                .with_provider_calls(vec![provider_call.clone()]),
            summary: summary.clone(),
        };
        let canceled = RuntimeTerminalOutcome::Canceled {
            failure: QueryAnswerTaskFailure::new("runtime_policy_rejected", "redacted")
                .with_provider_calls(vec![provider_call]),
            summary,
        };

        for outcome in [completed, recovered, failed, canceled] {
            let calls = match outcome {
                RuntimeTerminalOutcome::Completed { success }
                | RuntimeTerminalOutcome::Recovered { success, .. } => success.provider_calls,
                RuntimeTerminalOutcome::Failed { failure, .. }
                | RuntimeTerminalOutcome::Canceled { failure, .. } => failure.provider_calls,
            };
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].provider_call_id(), provider_call_id);
        }
    }

    #[test]
    fn ordinary_answer_failure_preserves_completed_provider_responses() {
        let provider_call = provider_call(
            Uuid::now_v7(),
            AiBindingPurpose::QueryAnswer,
            "answer-provider",
            "answer-model",
            QueryProviderCallKind::QueryAnswer,
            serde_json::json!({"output_tokens": 5}),
        );
        let failure = crate::services::query::execution::RuntimeAnswerQueryFailure {
            error: anyhow::anyhow!("post-response verification failed"),
            provider_calls: vec![provider_call.clone()],
        };

        let mapped = map_runtime_answer_query_failure(failure, "query_answer_failed");

        assert_eq!(mapped.code, "query_answer_failed");
        assert_eq!(mapped.provider_calls, vec![provider_call]);
    }

    #[test]
    fn persisted_typed_outcome_is_independent_of_heavy_reference_hydration() {
        let candidate_document_id = Uuid::now_v7();
        let summary = serde_json::json!({
            "answerDisposition": "clarification",
            "answerClarification": {
                "required": true,
                "question": "Which neutral variant?",
                "answerCandidates": [{
                    "label": "Variant A",
                    "kind": "document",
                    "confidence": 0.75,
                    "provenance": {
                        "entityId": null,
                        "documentId": candidate_document_id,
                        "chunkId": null
                    }
                }]
            }
        });

        // Reference hydration may time out or fail independently. The answer
        // outcome is read from the lightweight persisted bundle header.
        let (disposition, clarification) =
            persisted_execution_answer_outcome(Uuid::now_v7(), Some(&summary));

        assert_eq!(disposition, QueryAnswerDisposition::Clarification);
        assert!(clarification.required);
        assert_eq!(clarification.question.as_deref(), Some("Which neutral variant?"));
        assert_eq!(clarification.answer_candidates.len(), 1);
        assert_eq!(
            clarification.answer_candidates[0].provenance.document_id,
            Some(candidate_document_id)
        );
    }

    #[test]
    fn missing_or_corrupt_persisted_typed_outcome_fails_closed_to_non_terminal() {
        let missing_clarification = serde_json::json!({
            "answerDisposition": "clarification"
        });
        let contradictory_clarification = serde_json::json!({
            "answerDisposition": "factual_ready",
            "answerClarification": null
        });
        for summary in [None, Some(&missing_clarification), Some(&contradictory_clarification)] {
            assert_eq!(
                persisted_execution_answer_outcome(Uuid::now_v7(), summary),
                (QueryAnswerDisposition::NonTerminal, QueryClarification::default()),
            );
        }
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
            content_text: "ir.memory.literals.v1: `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`\nSample Subject setup.".to_string(),
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
            content_text: "ir.memory.literals.v1: `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`".to_string(),
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
                "ir.memory.literals.v1: `alphaPackage`, `endpointUrl`, `secretKey`, `/opt/alpha.conf`, `retryTimeout`"
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
    fn literal_fidelity_revision_detects_history_anchor_loss() {
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text:
                "ir.memory.literals.v1: `alphaPackage`, `/opt/alpha.ini`, `[Main]`, `retryTimeout`, `visible`, `false`"
                    .to_string(),
        }];
        let draft =
            "Install `alphaPackage`, edit `/opt/alpha.ini`, use `[Main]`, and set `retryTimeout`.";
        let revised = "Set `retryTimeout` after installation.";

        assert_eq!(
            literal_revision_history_literal_coverage(draft, revised, &history),
            Some((4, 1, 4))
        );
    }

    #[test]
    fn literal_fidelity_revision_ignores_plain_boolean_memory_values() {
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "ir.memory.literals.v1: `true`, `false`, `enabled`, `disabled`"
                .to_string(),
        }];

        assert_eq!(
            literal_revision_history_literal_coverage(
                "Set `true` and `false`.",
                "Set the supported values.",
                &history,
            ),
            None
        );
    }

    #[test]
    fn literal_inventory_revision_requires_target_coverage() {
        let required = vec!["alphaPackage".to_string(), "retryTimeout".to_string()];

        assert!(literal_revision_covers_required_literals(
            "Use `alphaPackage` and set `retryTimeout`.",
            &required,
        ));
        assert!(!literal_revision_covers_required_literals("Use `alphaPackage`.", &required,));
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
            content_text:
                "ir.memory.literals.v1: `retryTimeout`, `RetryTimeout`, `endpointUrl`, `secretKey`"
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
    fn ui_agent_reference_only_search_result_is_not_verifier_grade_evidence() {
        let grounding = AssistantGroundingEvidence {
            verification_corpus: vec![
                "[MCP tool result: search_documents]\nstructuredContent:\n{\"hits\":[{\"documentTitle\":\"Alpha Guide\",\"chunkReferences\":[{\"chunkId\":\"019f0000-0000-7000-8000-000000000001\"}]}]}"
                    .to_string(),
            ],
            document_references: Vec::new(),
        };

        assert!(!agent_has_verifiable_tool_evidence(&[], &grounding));
    }

    #[test]
    fn ui_agent_search_result_with_excerpt_is_verifier_grade_evidence() {
        let grounding = AssistantGroundingEvidence {
            verification_corpus: vec![
                "[MCP tool result: search_documents]\nstructuredContent:\n{\"hits\":[{\"documentTitle\":\"Alpha Guide\",\"excerpt\":\"The supported source statement.\"}]}"
                    .to_string(),
            ],
            document_references: Vec::new(),
        };

        assert!(agent_has_verifiable_tool_evidence(&[], &grounding));
    }

    #[test]
    fn ui_agent_deadline_budget_covers_runtime_turns() {
        let iteration_cap = ui_agent_iteration_cap();

        assert!(iteration_cap >= ASSISTANT_AGENT_LOOP_MIN_ITERATIONS);
        assert!(
            ASSISTANT_AGENT_LOOP_DEADLINE_MS / iteration_cap as u64
                >= ASSISTANT_AGENT_LOOP_MIN_ITERATION_BUDGET_MS
        );
    }

    #[test]
    fn result_cache_binding_identity_is_stable_across_rerank_rollout_modes() {
        use crate::domains::query::SemanticRerankMode;

        let expected = &[
            AiBindingPurpose::QueryCompile,
            AiBindingPurpose::ExtractGraph,
            AiBindingPurpose::QueryAnswer,
        ];
        assert_eq!(query_result_binding_purposes(SemanticRerankMode::Off), expected);
        assert_eq!(query_result_binding_purposes(SemanticRerankMode::Shadow), expected);
        assert_eq!(query_result_binding_purposes(SemanticRerankMode::Active), expected);
        assert!(query_result_cache_enabled_for_semantic_rerank(SemanticRerankMode::Off));
        assert!(
            query_result_cache_enabled_for_semantic_rerank(SemanticRerankMode::Shadow),
            "shadow rollout must not force every repeated answer through the paid query pipeline",
        );
        assert!(query_result_cache_enabled_for_semantic_rerank(SemanticRerankMode::Active));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryExecutionApiFailure {
    DeadlineExceeded,
    Conflict,
    ProviderFailure,
    NotFound,
    ServiceUnavailable(&'static str),
    Internal,
}

fn query_execution_api_failure(code: &str) -> QueryExecutionApiFailure {
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

fn map_query_execution_failure(
    execution_id: &Uuid,
    _query_text: &str,
    failure: &QueryAnswerTaskFailure,
) -> ApiError {
    tracing::error!(
        execution_id = %execution_id,
        failure_code = %failure.code,
        "query execution failed"
    );

    match query_execution_api_failure(&failure.code) {
        QueryExecutionApiFailure::DeadlineExceeded => {
            tracing::warn!(
                execution_id = %execution_id,
                "query answer exceeded its execution deadline"
            );
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
