mod parse;
mod prompt;
mod recovery;
mod session;
mod types;

#[cfg(test)]
mod tests;

// Public re-exports used across the crate
pub use recovery::{extraction_lifecycle_from_record, extraction_recovery_summary_from_record};
pub use types::{
    GraphEntityCandidate, GraphExtractionCandidateSet, GraphExtractionExecutionError,
    GraphExtractionOutcome, GraphExtractionRequest, GraphExtractionResumeState,
    GraphExtractionStructuredChunkContext, GraphExtractionSubTypeHintEntry,
    GraphExtractionSubTypeHintGroup, GraphExtractionSubTypeHints, GraphExtractionTaskFailure,
    GraphExtractionTaskFailureCode, GraphExtractionTechnicalFact, GraphRelationCandidate,
};

#[allow(unused_imports)]
#[cfg(test)]
pub use prompt::build_graph_extraction_prompt;

use crate::{
    agent_runtime::{
        builder::StructuredRequestBuilder,
        executor::RuntimeExecutionSession,
        persistence as runtime_persistence,
        response::{RuntimeRecoveryOutcome, RuntimeTerminalOutcome},
        tasks::graph_extract::{GraphExtractTask, GraphExtractTaskInput},
    },
    app::state::AppState,
    domains::{
        agent_runtime::{RuntimeExecutionOwner, RuntimeStageKind, RuntimeStageState},
        ai::AiBindingPurpose,
        graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
    },
    infra::repositories,
    services::ingest::runtime::RuntimeTaskExecutionContext,
};

use prompt::normalized_downgrade_level;
use recovery::{
    append_graph_runtime_policy_audit, begin_graph_runtime_stage, graph_async_operation_status,
    graph_extraction_execution_error, graph_failure_code_from_outcome,
    make_graph_runtime_failure_summary, make_graph_terminal_failure_outcome,
    map_graph_runtime_execution_error, record_graph_runtime_stage,
};
use session::build_raw_output_json;
use types::GraphExtractionFailureOutcome;

pub async fn extract_chunk_graph_candidates(
    state: &AppState,
    runtime_context: &RuntimeTaskExecutionContext,
    request: &GraphExtractionRequest,
) -> std::result::Result<GraphExtractionOutcome, GraphExtractionExecutionError> {
    let extraction_record_id = uuid::Uuid::now_v7();
    let mut runtime_session =
        seed_graph_extract_runtime_session(state, extraction_record_id, request, runtime_context)
            .await
            .map_err(|error| map_graph_runtime_execution_error(request, None, error))?;

    let initial_selection = runtime_context
        .provider_profile
        .selection_for_binding_purpose(AiBindingPurpose::ExtractGraph);
    repositories::create_runtime_graph_extraction_record(
        &state.persistence.postgres,
        &repositories::CreateRuntimeGraphExtractionRecordInput {
            id: extraction_record_id,
            runtime_execution_id: runtime_session.execution.id,
            library_id: request.library_id,
            document_id: request.document.id,
            chunk_id: request.chunk.id,
            provider_kind: initial_selection.provider_kind.as_str().to_string(),
            model_name: initial_selection.model_name.clone(),
            extraction_version: prompt::GRAPH_EXTRACTION_VERSION.to_string(),
            prompt_hash: "pending".to_string(),
            status: "processing".to_string(),
            raw_output_json: serde_json::json!({}),
            normalized_output_json: serde_json::json!({ "entities": [], "relations": [] }),
            glean_pass_count: 0,
            error_message: None,
        },
    )
    .await
    .map_err(|error| {
        graph_extraction_execution_error(
            request,
            format!("failed to create graph extraction owner record: {error:#}"),
            None,
            ExtractionRecoverySummary {
                status: ExtractionOutcomeStatus::Failed,
                second_pass_applied: false,
                warning: None,
            },
            Vec::new(),
        )
    })?;

    let execution_result = run_graph_extraction_runtime(
        state,
        runtime_context,
        request,
        extraction_record_id,
        &mut runtime_session,
    )
    .await;

    match execution_result {
        Ok((runtime_outcome, extraction_outcome)) => {
            let runtime_result = state
                .agent_runtime
                .executor()
                .finalize_session::<GraphExtractTask>(runtime_session, runtime_outcome)
                .await;
            runtime_persistence::persist_runtime_result(
                &state.persistence.postgres,
                &runtime_result.execution,
                &runtime_result.trace,
            )
            .await
            .map_err(|error| {
                graph_extraction_execution_error(
                    request,
                    format!("failed to persist graph extraction runtime trace: {error:#}"),
                    extraction_outcome.provider_failure.clone(),
                    extraction_outcome.recovery_summary.clone(),
                    extraction_outcome.recovery_attempts.clone(),
                )
            })?;
            repositories::update_runtime_graph_extraction_record(
                &state.persistence.postgres,
                extraction_record_id,
                &repositories::UpdateRuntimeGraphExtractionRecordInput {
                    provider_kind: extraction_outcome.provider_kind.clone(),
                    model_name: extraction_outcome.model_name.clone(),
                    prompt_hash: extraction_outcome.prompt_hash.clone(),
                    status: "ready".to_string(),
                    raw_output_json: extraction_outcome.raw_output_json.clone(),
                    normalized_output_json: serde_json::to_value(&extraction_outcome.normalized)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    glean_pass_count: i32::try_from(extraction_outcome.usage_calls.len())
                        .unwrap_or(i32::MAX),
                    error_message: None,
                },
            )
            .await
            .map_err(|error| {
                graph_extraction_execution_error(
                    request,
                    format!("failed to update graph extraction owner record: {error:#}"),
                    extraction_outcome.provider_failure.clone(),
                    extraction_outcome.recovery_summary.clone(),
                    extraction_outcome.recovery_attempts.clone(),
                )
            })?
            .ok_or_else(|| {
                graph_extraction_execution_error(
                    request,
                    format!(
                        "graph extraction owner record {} was not found during update",
                        extraction_record_id
                    ),
                    extraction_outcome.provider_failure.clone(),
                    extraction_outcome.recovery_summary.clone(),
                    extraction_outcome.recovery_attempts.clone(),
                )
            })?;
            Ok(GraphExtractionOutcome {
                graph_extraction_id: Some(extraction_record_id),
                runtime_execution_id: Some(runtime_result.execution.id),
                ..extraction_outcome
            })
        }
        Err((runtime_outcome, error)) => {
            let runtime_result = state
                .agent_runtime
                .executor()
                .finalize_session::<GraphExtractTask>(runtime_session, runtime_outcome)
                .await;
            runtime_persistence::persist_runtime_result(
                &state.persistence.postgres,
                &runtime_result.execution,
                &runtime_result.trace,
            )
            .await
            .map_err(|persist_error| GraphExtractionExecutionError {
                message: format!(
                    "failed to persist graph extraction runtime trace: {persist_error:#}"
                ),
                request_shape_key: error.request_shape_key.clone(),
                request_size_bytes: error.request_size_bytes,
                provider_failure: error.provider_failure.clone(),
                recovery_summary: error.recovery_summary.clone(),
                recovery_attempts: error.recovery_attempts.clone(),
                resume_state: error.resume_state.clone(),
            })?;
            repositories::update_runtime_graph_extraction_record(
                &state.persistence.postgres,
                extraction_record_id,
                &repositories::UpdateRuntimeGraphExtractionRecordInput {
                    provider_kind: error
                        .provider_failure
                        .as_ref()
                        .and_then(|failure| failure.provider_kind.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    model_name: error
                        .provider_failure
                        .as_ref()
                        .and_then(|failure| failure.model_name.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    prompt_hash: "unknown".to_string(),
                    status: graph_async_operation_status(&runtime_result.outcome).to_string(),
                    raw_output_json: serde_json::json!({}),
                    normalized_output_json: serde_json::json!({ "entities": [], "relations": [] }),
                    glean_pass_count: i32::try_from(error.resume_state.replay_count)
                        .unwrap_or(i32::MAX),
                    error_message: Some(error.message.clone()),
                },
            )
            .await
            .map_err(|persist_error| GraphExtractionExecutionError {
                message: format!(
                    "failed to update graph extraction failure record: {persist_error:#}"
                ),
                request_shape_key: error.request_shape_key.clone(),
                request_size_bytes: error.request_size_bytes,
                provider_failure: error.provider_failure.clone(),
                recovery_summary: error.recovery_summary.clone(),
                recovery_attempts: error.recovery_attempts.clone(),
                resume_state: error.resume_state.clone(),
            })?
            .ok_or_else(|| GraphExtractionExecutionError {
                message: format!(
                    "graph extraction owner record {} was not found during failure update",
                    extraction_record_id
                ),
                request_shape_key: error.request_shape_key.clone(),
                request_size_bytes: error.request_size_bytes,
                provider_failure: error.provider_failure.clone(),
                recovery_summary: error.recovery_summary.clone(),
                recovery_attempts: error.recovery_attempts.clone(),
                resume_state: error.resume_state.clone(),
            })?;
            append_graph_runtime_policy_audit(
                state,
                request,
                extraction_record_id,
                &runtime_result,
            )
            .await;
            Err(error)
        }
    }
}

async fn seed_graph_extract_runtime_session(
    state: &AppState,
    graph_extraction_id: uuid::Uuid,
    request: &GraphExtractionRequest,
    runtime_context: &RuntimeTaskExecutionContext,
) -> std::result::Result<
    RuntimeExecutionSession,
    crate::agent_runtime::executor::RuntimeExecutionError,
> {
    let runtime_request = StructuredRequestBuilder::<GraphExtractTask>::new(
        GraphExtractTaskInput {
            library_id: request.library_id,
            document_id: request.document.id,
            chunk_id: request.chunk.id,
            revision_id: request.revision_id,
            normalized_text: request.chunk.content.clone(),
            technical_facts: request.technical_facts.clone(),
        },
        RuntimeExecutionOwner::graph_extraction_attempt(graph_extraction_id),
    )
    .with_budget_limits(
        runtime_context.runtime_overrides.max_turns,
        runtime_context.runtime_overrides.max_parallel_actions,
    )
    .build();

    state
        .agent_runtime
        .seed_and_persist_session(&state.persistence.postgres, &runtime_request)
        .await
}

async fn run_graph_extraction_runtime(
    state: &AppState,
    runtime_context: &RuntimeTaskExecutionContext,
    request: &GraphExtractionRequest,
    graph_extraction_id: uuid::Uuid,
    runtime_session: &mut RuntimeExecutionSession,
) -> std::result::Result<
    (
        RuntimeTerminalOutcome<
            types::GraphExtractionCandidateSet,
            types::GraphExtractionTaskFailure,
        >,
        GraphExtractionOutcome,
    ),
    (
        RuntimeTerminalOutcome<
            types::GraphExtractionCandidateSet,
            types::GraphExtractionTaskFailure,
        >,
        GraphExtractionExecutionError,
    ),
> {
    let provider_profile = &runtime_context.provider_profile;

    if let Err(failure) = begin_graph_runtime_stage(
        state.agent_runtime.executor(),
        runtime_session,
        RuntimeStageKind::ExtractGraph,
    )
    .await
    {
        record_graph_runtime_stage(
            state.agent_runtime.executor(),
            runtime_session,
            RuntimeStageKind::ExtractGraph,
            RuntimeStageState::Failed,
            false,
            Some(&failure),
        );
        let error = graph_extraction_execution_error(
            request,
            failure.summary.clone(),
            None,
            ExtractionRecoverySummary {
                status: ExtractionOutcomeStatus::Failed,
                second_pass_applied: false,
                warning: None,
            },
            Vec::new(),
        );
        return Err((make_graph_terminal_failure_outcome(failure.clone()), error));
    }

    match resolve_graph_extraction(state, provider_profile, request).await {
        Ok(resolved) => {
            record_graph_runtime_stage(
                state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::ExtractGraph,
                RuntimeStageState::Completed,
                false,
                None,
            );

            let runtime_execution_id = runtime_session.execution.id;
            let recovery_status = resolved.recovery_summary.status.clone();
            if matches!(
                recovery_status,
                ExtractionOutcomeStatus::Recovered | ExtractionOutcomeStatus::Partial
            ) {
                if let Err(failure) = begin_graph_runtime_stage(
                    state.agent_runtime.executor(),
                    runtime_session,
                    RuntimeStageKind::Recovery,
                )
                .await
                {
                    record_graph_runtime_stage(
                        state.agent_runtime.executor(),
                        runtime_session,
                        RuntimeStageKind::Recovery,
                        RuntimeStageState::Failed,
                        false,
                        Some(&failure),
                    );
                    let error = graph_extraction_execution_error(
                        request,
                        failure.summary.clone(),
                        resolved.provider_failure.clone(),
                        resolved.recovery_summary.clone(),
                        resolved.recovery_attempts.clone(),
                    );
                    return Err((make_graph_terminal_failure_outcome(failure.clone()), error));
                }
                record_graph_runtime_stage(
                    state.agent_runtime.executor(),
                    runtime_session,
                    RuntimeStageKind::Recovery,
                    RuntimeStageState::Recovered,
                    false,
                    None,
                );
            }

            let normalized = resolved.normalized.clone();
            let outcome = GraphExtractionOutcome {
                graph_extraction_id: Some(graph_extraction_id),
                runtime_execution_id: Some(runtime_execution_id),
                provider_kind: resolved.provider_kind.clone(),
                model_name: resolved.model_name.clone(),
                prompt_hash: resolved.prompt_hash.clone(),
                raw_output_json: build_raw_output_json(
                    &resolved.output_text,
                    resolved.usage_json.clone(),
                    &resolved.lifecycle,
                    &resolved.recovery,
                    &resolved.recovery_summary,
                    &resolved.usage_calls,
                ),
                usage_json: resolved.usage_json.clone(),
                usage_calls: resolved.usage_calls.clone(),
                normalized: resolved.normalized,
                provider_failure: resolved.provider_failure.clone(),
                recovery_summary: resolved.recovery_summary.clone(),
                recovery_attempts: resolved.recovery_attempts.clone(),
            };

            let runtime_outcome = match recovery_status {
                ExtractionOutcomeStatus::Clean => {
                    RuntimeTerminalOutcome::Completed { success: normalized }
                }
                ExtractionOutcomeStatus::Recovered | ExtractionOutcomeStatus::Partial => {
                    RuntimeTerminalOutcome::Recovered {
                        success: normalized,
                        recovery: RuntimeRecoveryOutcome {
                            attempts: u8::try_from(outcome.recovery_attempts.len())
                                .unwrap_or(u8::MAX),
                            summary_redacted: outcome.recovery_summary.warning.clone(),
                        },
                    }
                }
                ExtractionOutcomeStatus::Failed => RuntimeTerminalOutcome::Failed {
                    failure: GraphExtractionTaskFailure {
                        code: GraphExtractionTaskFailureCode::MalformedOutput.as_str().to_string(),
                        summary: "graph extraction resolved with failed recovery status"
                            .to_string(),
                    },
                    summary: make_graph_runtime_failure_summary(
                        GraphExtractionTaskFailureCode::MalformedOutput.as_str(),
                        "graph extraction resolved with failed recovery status",
                    ),
                },
            };
            match runtime_outcome {
                RuntimeTerminalOutcome::Failed { failure, summary } => Err((
                    RuntimeTerminalOutcome::Failed { failure, summary },
                    graph_extraction_execution_error(
                        request,
                        "graph extraction resolved with failed recovery status",
                        outcome.provider_failure.clone(),
                        outcome.recovery_summary.clone(),
                        outcome.recovery_attempts.clone(),
                    ),
                )),
                _ => Ok((runtime_outcome, outcome)),
            }
        }
        Err(failure) => {
            record_graph_runtime_stage(
                state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::ExtractGraph,
                RuntimeStageState::Failed,
                false,
                Some(&GraphExtractionTaskFailure {
                    code: graph_failure_code_from_outcome(&failure).to_string(),
                    summary: failure.error_message.clone(),
                }),
            );
            let task_failure = GraphExtractionTaskFailure {
                code: graph_failure_code_from_outcome(&failure).to_string(),
                summary: failure.error_message.clone(),
            };
            Err((
                make_graph_terminal_failure_outcome(task_failure.clone()),
                GraphExtractionExecutionError {
                    message: failure.error_message,
                    request_shape_key: failure.request_shape_key,
                    request_size_bytes: failure.request_size_bytes,
                    provider_failure: failure.provider_failure,
                    recovery_summary: failure.recovery_summary,
                    recovery_attempts: failure.recovery_attempts,
                    resume_state: GraphExtractionResumeState {
                        resumed_from_checkpoint: false,
                        replay_count: request
                            .resume_hint
                            .as_ref()
                            .map(|hint| hint.replay_count.saturating_add(1))
                            .unwrap_or(1),
                        downgrade_level: normalized_downgrade_level(request),
                    },
                },
            ))
        }
    }
}

async fn resolve_graph_extraction(
    state: &AppState,
    provider_profile: &crate::domains::provider_profiles::EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> std::result::Result<types::ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    let library_id = request.library_id;
    let runtime_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractGraph)
        .await
        .map_err(|error| {
            recovery::unconfigured_graph_extraction_failure(request, error.to_string())
        })?
        .ok_or_else(|| {
            recovery::unconfigured_graph_extraction_failure(
                request,
                "active graph extraction binding is not configured for this library",
            )
        })?;
    session::resolve_graph_extraction_with_gateway(
        state.llm_gateway.as_ref(),
        &state.retrieval_intelligence_services.extraction_recovery,
        &state.resolve_settle_blockers_services.provider_failure_classification,
        provider_profile,
        &runtime_binding,
        request,
        state.retrieval_intelligence.extraction_recovery_enabled,
        state
            .retrieval_intelligence
            .extraction_recovery_max_attempts
            .clamp(1, prompt::GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS),
        state.resolve_settle_blockers.provider_timeout_retry_limit.max(1),
    )
    .await
}
