use super::super::coverage::{
    evaluate_single_shot_evidence_coverage_for_context, single_shot_answer_is_acceptable,
    single_shot_coverage_allows_attempt,
};
use super::super::literal_coverage::{answer_generation_question, requires_no_evidence_candidate};
use super::super::literal_revision::{
    LiteralRevisionAttempt, LiteralRevisionPath, finalize_verified_answer,
    revise_answer_literals_if_needed,
};
use super::super::output_boundary::enforce_hard_output_boundary;
use super::super::preflight::{PreflightAnswerCandidate, persist_llm_context_snapshot};
use super::super::verification::verify_generated_answer;
use super::super::{AnswerGenerationStage, RuntimeAnswerQueryResult};
use super::logging::{
    log_preflight_single_shot_accepted, log_preflight_single_shot_canonical_attempt,
    log_preflight_single_shot_done, log_preflight_single_shot_error,
    log_preflight_single_shot_rejected, log_preflight_single_shot_start,
};
use super::{PreflightAnswer, PreflightCarry, PreflightSingleShot};

/// Runs the evidence-preflight single-shot attempt over the fixed preflight
/// context: the fixed retrieved evidence is used without a second retrieval
/// pass. `Skipped` means the preflight carried no prompt context.
async fn run_preflight_single_shot(
    answer: &PreflightAnswer<'_, '_>,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<PreflightSingleShot> {
    let request = answer.request;
    let prepared = answer.prepared;
    let preflight = answer.preflight;
    if preflight.prompt_context.trim().is_empty() {
        return Ok(PreflightSingleShot::Skipped);
    }
    let state = request.state;
    let execution_context = request.execution_context;
    let library_id = execution_context.library_id;
    let execution_id = execution_context.query_execution_id;
    let generation_question =
        answer_generation_question(request.effective_question, request.user_question);
    let has_conversation_history =
        request.conversation_history.map(str::trim).is_some_and(|value| !value.is_empty());
    let coverage = evaluate_single_shot_evidence_coverage_for_context(
        &prepared.query_ir,
        &preflight.prompt_context,
        has_conversation_history,
    );
    if !single_shot_coverage_allows_attempt(&coverage) {
        log_preflight_single_shot_canonical_attempt(execution_id, &coverage);
    }
    let started = std::time::Instant::now();
    log_preflight_single_shot_start(
        execution_id,
        library_id,
        preflight.primary_answer_chunks.len(),
        preflight.prompt_context.chars().count(),
    );
    let result = crate::services::query::agent_loop::run_single_shot_turn(
        state,
        execution_context,
        crate::services::query::agent_loop::SingleShotTurnRequest {
            library_id,
            user_question: generation_question,
            conversation_history: request.conversation_history_messages,
            grounded_context: &preflight.prompt_context,
            answer_language: prepared.query_ir.language,
        },
    )
    .await;
    match result {
        Ok(single) => finish_preflight_single_shot(answer, single, started, provider_calls).await,
        Err(error) => {
            log_preflight_single_shot_error(execution_id, &error);
            Ok(PreflightSingleShot::Ran)
        }
    }
}

async fn finish_preflight_single_shot(
    answer: &PreflightAnswer<'_, '_>,
    single: crate::services::query::agent_loop::AgentTurnResult,
    started: std::time::Instant,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<PreflightSingleShot> {
    let request = answer.request;
    let prepared = answer.prepared;
    let preflight = answer.preflight;
    let state = request.state;
    let execution_context = request.execution_context;
    let execution_id = execution_context.query_execution_id;
    let generation_question =
        answer_generation_question(request.effective_question, request.user_question);
    provider_calls.extend(single.provider_calls.iter().cloned());
    log_preflight_single_shot_done(
        execution_id,
        single.answer.len(),
        started.elapsed().as_millis(),
    );
    let preflight_initial_answer = enforce_hard_output_boundary(
        execution_id,
        "answer.preflight_single_shot",
        &prepared.query_ir,
        single.answer.clone(),
    );
    let mut preflight_debug = single.debug_iterations.clone();
    let mut verification_stage = verify_generated_answer(
        execution_id,
        request.effective_question,
        AnswerGenerationStage {
            intent_profile: prepared.structured.intent_profile.clone(),
            primary_answer_chunks: preflight.primary_answer_chunks.clone(),
            authoritative_evidence: preflight.authoritative_evidence.clone(),
            assistant_grounding:
                crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
            answer: preflight_initial_answer,
            usage_json: single.usage_json.clone(),
            prompt_context: preflight.prompt_context.clone(),
            query_ir: prepared.query_ir.clone(),
        },
    )
    .await?;
    revise_answer_literals_if_needed(
        LiteralRevisionAttempt {
            state,
            execution_context,
            revision_question: generation_question,
            verification_question: request.effective_question,
            conversation_history: request.conversation_history_messages,
            prepared,
            path: LiteralRevisionPath::AnswerPreflight,
        },
        &mut verification_stage,
        provider_calls,
        &mut preflight_debug,
    )
    .await?;
    verification_stage = finalize_verified_answer(state, execution_id, verification_stage).await?;
    if single_shot_answer_is_acceptable(
        &verification_stage.generation.answer,
        &verification_stage,
        prepared.structured.retrieved_documents.len(),
        &prepared.query_ir,
        &preflight.prompt_context,
    ) {
        log_preflight_single_shot_accepted(
            execution_id,
            verification_stage.verification.state,
            started.elapsed().as_millis(),
        );
        let total_iterations = preflight_debug.len();
        persist_llm_context_snapshot(
            state,
            answer.snapshot_context.snapshot_with_iterations(
                &verification_stage.generation.answer,
                preflight_debug,
                total_iterations,
            ),
        )
        .await?;
        return Ok(PreflightSingleShot::Accepted(Box::new(RuntimeAnswerQueryResult {
            answer: verification_stage.generation.answer,
            provider_calls: std::mem::take(provider_calls),
        })));
    }
    let verify_state = verification_stage.verification.state;
    let warning_count = verification_stage.verification.warnings.len();
    log_preflight_single_shot_rejected(execution_id, verify_state, warning_count);
    Ok(PreflightSingleShot::Candidate(Box::new(PreflightAnswerCandidate {
        total_iterations: preflight_debug.len(),
        debug_iterations: preflight_debug,
        verification_stage,
    })))
}

/// The deterministic insufficient-evidence candidate finalized when retrieval
/// produced no answer context and no fast-path candidate survived.
async fn build_no_evidence_candidate(
    answer: &PreflightAnswer<'_, '_>,
) -> anyhow::Result<PreflightAnswerCandidate> {
    let request = answer.request;
    let prepared = answer.prepared;
    let execution_id = request.execution_context.query_execution_id;
    let no_evidence_answer = "No grounded evidence was retrieved for this question.".to_string();
    tracing::info!(
        stage = "answer.no_evidence_finalized",
        %execution_id,
        "finalizing deterministic insufficient-evidence answer because retrieval produced no answer context"
    );
    let verification_stage = verify_generated_answer(
        execution_id,
        request.effective_question,
        AnswerGenerationStage {
            intent_profile: prepared.structured.intent_profile.clone(),
            primary_answer_chunks: Vec::new(),
            authoritative_evidence: super::super::super::AuthoritativeAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            assistant_grounding:
                crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
            answer: no_evidence_answer,
            usage_json: serde_json::json!({
                "deterministic": true,
                "reason": "no_grounded_evidence",
            }),
            prompt_context: String::new(),
            query_ir: prepared.query_ir.clone(),
        },
    )
    .await?;
    Ok(PreflightAnswerCandidate {
        verification_stage,
        debug_iterations: Vec::new(),
        total_iterations: 0,
    })
}

/// Finalizes the fixed-evidence answer: runs the preflight single-shot attempt,
/// supplies the deterministic no-evidence candidate when nothing survived, then
/// verifies and persists the winning candidate.
pub(super) async fn finalize_preflight_answer(
    answer: &PreflightAnswer<'_, '_>,
    mut carry: PreflightCarry,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let state = answer.request.state;
    let execution_id = answer.request.execution_context.query_execution_id;
    match run_preflight_single_shot(answer, provider_calls).await? {
        PreflightSingleShot::Accepted(result) => return Ok(*result),
        PreflightSingleShot::Candidate(candidate) => {
            carry.candidate = Some(*candidate);
            carry.attempted_answer_generation = true;
        }
        PreflightSingleShot::Ran => carry.attempted_answer_generation = true,
        PreflightSingleShot::Skipped => {}
    }
    if requires_no_evidence_candidate(carry.candidate.is_none(), carry.attempted_answer_generation)
    {
        carry.candidate = Some(build_no_evidence_candidate(answer).await?);
    }

    let Some(candidate) = carry.candidate else {
        anyhow::bail!(
            "grounded-answer generation produced no answer candidate for execution {execution_id}"
        );
    };
    let verification_stage =
        finalize_verified_answer(state, execution_id, candidate.verification_stage).await?;
    let candidate = PreflightAnswerCandidate {
        verification_stage,
        debug_iterations: candidate.debug_iterations,
        total_iterations: candidate.total_iterations,
    };
    tracing::info!(
        stage = "answer.fixed_evidence_finalized",
        %execution_id,
        verify_state = ?candidate.verification_stage.verification.state,
        warning_count = candidate.verification_stage.verification.warnings.len(),
        "finalizing grounded answer from fixed retrieved evidence without a second retrieval pass"
    );
    persist_llm_context_snapshot(
        state,
        answer.snapshot_context.snapshot_with_iterations(
            &candidate.verification_stage.generation.answer,
            candidate.debug_iterations,
            candidate.total_iterations,
        ),
    )
    .await?;
    Ok(RuntimeAnswerQueryResult {
        answer: candidate.verification_stage.generation.answer,
        provider_calls: std::mem::take(provider_calls),
    })
}
