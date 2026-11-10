use super::super::types::AnswerVerificationStage;
use super::{
    SEMANTIC_PROVIDER_TIMEOUT, apply_semantic_report, execute_semantic_verification,
    load_semantic_sources, preparation_failure_report, prepare, receipt_matches,
};
use crate::{
    agent_runtime::tasks::query_answer::QueryProviderCall,
    app::state::AppState,
    services::query::{
        llm_context_debug::LlmIterationDebug, provider_billing::QueryProviderExecutionContext,
    },
};
use ironrag_contracts::assistant::semantic_verification::{
    SemanticFailureCode, SemanticVerificationReport,
};

/// Called only after draft selection/revision. Paid attempts are retained before
/// any subsequent provenance, visibility, snapshot or persistence can fail.
pub(crate) struct SemanticCandidateContext<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) execution: QueryProviderExecutionContext,
    pub(crate) question: &'a str,
    pub(crate) reuse: Option<&'a SemanticVerificationReport>,
}

pub(crate) async fn verify_selected_candidate(
    context: SemanticCandidateContext<'_>,
    stage: &mut AnswerVerificationStage,
    calls: &mut Vec<QueryProviderCall>,
    iterations: &mut Vec<LlmIterationDebug>,
) -> anyhow::Result<()> {
    let SemanticCandidateContext { state, execution, question, reuse } = context;
    let sources = load_semantic_sources(
        state,
        execution,
        &stage.generation.primary_answer_chunks,
        &stage.generation.authoritative_evidence,
        &stage.generation.assistant_grounding,
    )
    .await;
    let prepared = match sources
        .and_then(|sources| prepare(question, &stage.generation.answer, sources))
    {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::warn!(
                execution_id = %execution.query_execution_id,
                error = ?error,
                "semantic preparation failed"
            );
            let report = preparation_failure_report(question, &stage.generation.answer, error, 0);
            apply_semantic_report(&mut stage.verification, report);
            return Ok(());
        }
    };
    let mut report = dispatch_selected(&prepared, reuse, calls, iterations, || {
        execute_semantic_verification(state, execution, &prepared, SEMANTIC_PROVIDER_TIMEOUT)
    })
    .await?;
    // A head swap during provider I/O must not turn a paid response into a
    // current-readable verification. Retain the response and its exact usage.
    let refreshed = load_semantic_sources(
        state,
        execution,
        &stage.generation.primary_answer_chunks,
        &stage.generation.authoritative_evidence,
        &stage.generation.assistant_grounding,
    )
    .await
    .and_then(|sources| prepare(question, &stage.generation.answer, sources));
    if !refreshed.as_ref().is_ok_and(|fresh| receipt_matches(fresh, &report)) {
        report = prepared.failed(
            SemanticFailureCode::InvalidProvenance,
            report.elapsed_ms,
            report.provider_call_id,
        );
    }
    apply_semantic_report(&mut stage.verification, report);
    Ok(())
}

/// FnOnce admits at most one selected-candidate dispatch. A reused receipt is
/// checked before invoking the callback; financial errors follow retention.
pub(super) async fn dispatch_selected<F, Fut>(
    prepared: &super::PreparedSemanticVerification,
    reuse: Option<&SemanticVerificationReport>,
    calls: &mut Vec<QueryProviderCall>,
    iterations: &mut Vec<LlmIterationDebug>,
    dispatch: F,
) -> anyhow::Result<SemanticVerificationReport>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<
            Output = Result<
                super::SemanticProviderAttempt,
                crate::interfaces::http::router_support::ApiError,
            >,
        >,
{
    if let Some(receipt) = reuse.filter(|receipt| receipt_matches(prepared, receipt)) {
        return Ok(receipt.clone());
    }
    let attempt = dispatch()
        .await
        .map_err(|error| anyhow::anyhow!("semantic provider/billing operation failed: {error}"))?;
    if let Some(call) = attempt.provider_call {
        calls.push(call);
    }
    if let Some(mut iteration) = attempt.iteration {
        iteration.iteration = iterations.len() + 1;
        iterations.push(iteration);
    }
    if let Some(error) = attempt.completion_error {
        anyhow::bail!("semantic usage completion failed after paid response: {error}");
    }
    Ok(attempt.report)
}
