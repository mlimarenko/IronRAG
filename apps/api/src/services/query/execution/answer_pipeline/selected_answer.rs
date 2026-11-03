use super::super::semantic_verification::{
    SemanticCandidateContext, bind_visible_answer, has_formal_blocker, is_bounded_abstention,
    verify_selected_candidate,
};
use super::{
    AnswerVerificationStage, AppState, QueryAnswerDisposition, QueryClarification,
    QueryVerificationPersistence, RuntimeAnswerQueryResult,
};
use super::{answer_snapshot::AnswerSnapshotContext, preflight::persist_llm_context_snapshot};
use crate::agent_runtime::tasks::query_answer::QueryProviderCall;
use crate::services::query::provider_billing::QueryProviderExecutionContext;

pub(super) struct SelectedAnswerContext<'a> {
    pub(super) state: &'a AppState,
    pub(super) execution: QueryProviderExecutionContext,
    pub(super) question: &'a str,
    pub(super) snapshot: &'a AnswerSnapshotContext,
}

/// The sole factual selected-candidate boundary. Drafts never call a provider,
/// mutate to a visibility fallback, persist an outcome or replace a snapshot.
pub(super) fn finish_selected_answer<'a>(
    context: SelectedAnswerContext<'a>,
    stage: AnswerVerificationStage,
    calls: &'a mut Vec<QueryProviderCall>,
) -> impl std::future::Future<Output = anyhow::Result<RuntimeAnswerQueryResult>> + Send + 'a {
    // Keep one selected-candidate state machine off every enclosing draft-path
    // future, rather than boxing each caller in the escalation chain.
    Box::pin(finish_selected_answer_inner(context, stage, calls))
}

async fn finish_selected_answer_inner(
    context: SelectedAnswerContext<'_>,
    mut stage: AnswerVerificationStage,
    calls: &mut Vec<QueryProviderCall>,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let SelectedAnswerContext { state, execution, question, snapshot } = context;
    let mut iterations = snapshot.take_iterations()?;
    if let Err(error) = verify_selected_candidate(
        SemanticCandidateContext { state, execution, question, reuse: None },
        &mut stage,
        calls,
        &mut iterations,
    )
    .await
    {
        persist_semantic_failure_snapshot(
            state,
            execution.query_execution_id,
            snapshot,
            iterations,
        )
        .await;
        return Err(error);
    }
    let nonfactual = !has_formal_blocker(&stage.verification)
        && stage.verification.semantic_verification.as_ref().is_some_and(is_bounded_abstention);
    let finalized = super::super::verification_policy::finalize_answer_visibility(
        stage.generation.query_ir.verification_level(),
        stage.verification.state,
        &stage.verification.warnings,
        stage.generation.query_ir.language,
        &stage.generation.answer,
        if nonfactual {
            super::super::verification_policy::AnswerVisibilityKind::DeterministicStatement
        } else {
            super::super::verification_policy::AnswerVisibilityKind::FactualCandidate
        },
    );
    let disposition =
        if nonfactual { QueryAnswerDisposition::SafeFallback } else { finalized.disposition };
    let visible = finalized.visible_answer.into_owned();
    if let Some(report) = &mut stage.verification.semantic_verification {
        bind_visible_answer(report, &visible);
    }
    let total_iterations = iterations.len();
    persist_llm_context_snapshot(
        state,
        snapshot.snapshot_with_iterations(&visible, iterations, total_iterations),
    )
    .await?;
    super::super::persist_query_verification(QueryVerificationPersistence {
        state,
        execution_id: execution.query_execution_id,
        verification: &stage.verification,
        answer_disposition: disposition,
        clarification: &QueryClarification::default(),
        authoritative_evidence: &stage.generation.authoritative_evidence,
        assistant_grounding: &stage.generation.assistant_grounding,
        answer_kind: None,
    })
    .await?;
    Ok(RuntimeAnswerQueryResult { answer: visible, provider_calls: std::mem::take(calls) })
}

async fn persist_semantic_failure_snapshot(
    state: &AppState,
    execution_id: uuid::Uuid,
    snapshot: &AnswerSnapshotContext,
    iterations: Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
) {
    let count = iterations.len();
    let mut failed_snapshot = snapshot.snapshot_with_iterations("", iterations, count);
    failed_snapshot.final_answer = None;
    if let Err(error) = persist_llm_context_snapshot(state, failed_snapshot).await {
        tracing::warn!(%error, %execution_id, "failed to retain paid semantic failure snapshot");
    }
}
