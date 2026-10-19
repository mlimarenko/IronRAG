use super::super::preflight::AnswerEvidencePreflight;
use super::answer_snapshot::AnswerSnapshotContext;
use super::clarification::{
    ClarificationPromptKind, deterministic_clarification_question, disposition_clarification,
    finalize_typed_clarification_verification, render_typed_clarification_answer,
};
use super::coverage::{should_use_single_shot_answer, single_shot_answer_is_acceptable};
use super::deterministic::try_deterministic_answer;
use super::disposition::{
    AnswerDisposition, classify_answer_disposition, prepared_with_preflight_context_titles,
};
use super::document_statement::finalize_deterministic_document_statement;
use super::literal_coverage::{answer_generation_question, answer_question_for_disposition};
use super::literal_revision::{
    LiteralRevisionAttempt, LiteralRevisionPath, finalize_verified_answer,
    revise_answer_literals_if_needed,
};
use super::output_boundary::{
    enforce_hard_output_boundary, selected_runtime_answer_chunks,
    selected_runtime_grounding_evidence,
};
use super::preflight::{FastPathOutcome, PreflightAnswerCandidate, persist_llm_context_snapshot};
use super::verification::verify_generated_answer;
use super::{
    AnswerGenerationStage, AnswerKind, AppState, ChatMessage, PreparedAnswerQueryResult,
    RuntimeAnswerQueryResult, query_ir_requires_remediation_synthesis,
};
use crate::services::query::provider_billing::QueryProviderExecutionContext;
use anyhow::Context as _;

use self::logging::{log_single_shot_accepted, log_single_shot_done, log_single_shot_rejected};

mod logging;
mod preflight_stages;

/// The identity and question context shared by every grounded-answer stage.
/// Grouping it lets each stage take one value instead of six loose arguments.
pub(crate) struct AnswerQueryRequest<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) execution_context: QueryProviderExecutionContext,
    pub(crate) effective_question: &'a str,
    pub(crate) user_question: &'a str,
    pub(crate) conversation_history: Option<&'a str>,
    pub(crate) conversation_history_messages: &'a [ChatMessage],
}

/// The shared inputs of the fast-path single-shot stage.
struct FastPathContext<'r, 'q> {
    request: &'r AnswerQueryRequest<'q>,
    prepared: &'r PreparedAnswerQueryResult,
    snapshot_context: &'r AnswerSnapshotContext,
}

/// The shared inputs of the evidence-preflight answer stages.
struct PreflightAnswer<'r, 'q> {
    request: &'r AnswerQueryRequest<'q>,
    prepared: &'r PreparedAnswerQueryResult,
    preflight: &'r AnswerEvidencePreflight,
    snapshot_context: &'r AnswerSnapshotContext,
}

/// Whether the preflight single-shot attempt accepted an answer, produced a
/// rejected candidate for the fixed-evidence finalize, ran without producing a
/// candidate, or did not run at all.
enum PreflightSingleShot {
    Accepted(Box<RuntimeAnswerQueryResult>),
    Candidate(Box<PreflightAnswerCandidate>),
    Ran,
    Skipped,
}

/// The preflight candidate carried out of the fast path into the fixed-evidence
/// finalize.
struct PreflightCarry {
    candidate: Option<PreflightAnswerCandidate>,
    attempted_answer_generation: bool,
}

/// The fast-path single-shot result: an accepted answer or a rejected candidate
/// (absent when the single-shot provider call failed outright).
enum SingleShotOutcome {
    Accepted(Box<RuntimeAnswerQueryResult>),
    Rejected(Option<Box<PreflightAnswerCandidate>>),
}

pub(super) async fn generate_answer_query_inner(
    request: AnswerQueryRequest<'_>,
    prepared: PreparedAnswerQueryResult,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let library_id = request.execution_context.library_id;
    // Resolve just the QueryAnswer binding instead of reloading the complete
    // provider profile. Preserve the binding precondition for deterministic
    // and provider-backed paths alike. Provider-call billing provenance lives
    // in the provider-call ledger, so the verification stage need not duplicate
    // a provider/model selection that it never consumes.
    request
        .state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(
            request.state,
            library_id,
            crate::domains::ai::AiBindingPurpose::QueryAnswer,
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to resolve query_answer binding: {error}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("no active query_answer binding configured for library {library_id}")
        })?;

    Box::pin(generate_answer_query_after_binding(request, prepared, provider_calls)).await
}

async fn try_fast_path_answer(
    context: &FastPathContext<'_, '_>,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<FastPathOutcome> {
    if let Some(result) = try_pre_retrieval_clarification(context).await? {
        return Ok(FastPathOutcome::Accepted(result));
    }
    // Single-shot fast path tried FIRST — we no longer pay the
    // ~2–3 s `prepare_answer_preflight` tax before every
    // question. Preflight loads document_index, authoritative evidence,
    // and answer chunks. None of that is needed for the initial
    // grounded-answer LLM call: `prepared.answer_context` already
    // carries the retrieved chunks, technical literals, library
    // summary, and selected graph context. Preflight is now deferred
    // to the escalation path, where the verifier and deterministic
    // `answer_override` logic still use it.
    if !should_use_single_shot_answer(
        context.request.effective_question,
        context.prepared,
        context.request.conversation_history,
    ) {
        return Ok(FastPathOutcome::Escalate {
            candidate: Box::new(None),
            attempted_answer_generation: false,
        });
    }
    let single_shot_start = std::time::Instant::now();
    let request = context.request;
    let prepared = context.prepared;
    tracing::info!(
        stage = "answer.single_shot_start",
        execution_id = %context.snapshot_context.execution_id(),
        library_id = %context.snapshot_context.library_id(),
        question_len = request.user_question.len(),
        query_ir_act = ?prepared.query_ir.act,
        query_ir_confidence = prepared.query_ir.confidence,
        retrieved_document_count = prepared.structured.retrieved_documents.len(),
        answer_context_chars = prepared.answer_context.chars().count(),
        "single-shot grounded-answer fast path start"
    );
    match run_single_shot_fast_path(context, single_shot_start, provider_calls).await? {
        SingleShotOutcome::Accepted(result) => Ok(FastPathOutcome::Accepted(*result)),
        SingleShotOutcome::Rejected(candidate) => Ok(FastPathOutcome::Escalate {
            candidate: Box::new(candidate.map(|candidate| *candidate)),
            attempted_answer_generation: true,
        }),
    }
}

/// Post-retrieval disposition router: only a compiled typed query request may
/// authorize clarification. Retrieved evidence is used after that gate solely
/// to build a bounded, sanitized candidate menu.
async fn try_pre_retrieval_clarification(
    context: &FastPathContext<'_, '_>,
) -> anyhow::Result<Option<RuntimeAnswerQueryResult>> {
    let request = context.request;
    let prepared = context.prepared;
    let state = request.state;
    let execution_id = context.snapshot_context.execution_id();
    let library_id = context.snapshot_context.library_id();
    let answer_question =
        answer_question_for_disposition(request.effective_question, request.user_question);
    let AnswerDisposition::Clarify { variants } =
        classify_answer_disposition(prepared, answer_question)
    else {
        return Ok(None);
    };
    let question = deterministic_clarification_question(
        prepared.query_ir.language,
        ClarificationPromptKind::Source,
    );
    let clarification = disposition_clarification(question, &variants);
    let visible_answer =
        render_typed_clarification_answer(prepared.query_ir.language, &clarification);
    tracing::info!(
        stage = "answer.clarify_deterministic",
        %execution_id,
        %library_id,
        variant_count = variants.len(),
        query_ir_act = ?prepared.query_ir.act,
        query_ir_confidence = prepared.query_ir.confidence,
        "post-retrieval router returned a typed deterministic clarification"
    );
    finalize_typed_clarification_verification(
        state,
        execution_id,
        &visible_answer,
        &prepared.query_ir,
        &clarification,
    )
    .await?;
    persist_llm_context_snapshot(state, context.snapshot_context.snapshot(&visible_answer)).await?;
    Ok(Some(RuntimeAnswerQueryResult { answer: visible_answer, provider_calls: Vec::new() }))
}

async fn run_single_shot_fast_path(
    context: &FastPathContext<'_, '_>,
    single_shot_start: std::time::Instant,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<SingleShotOutcome> {
    let request = context.request;
    let state = request.state;
    let execution_context = request.execution_context;
    let generation_question =
        answer_generation_question(request.effective_question, request.user_question);
    let single_shot_result = crate::services::query::agent_loop::run_single_shot_turn(
        state,
        execution_context,
        crate::services::query::agent_loop::SingleShotTurnRequest {
            library_id: execution_context.library_id,
            user_question: generation_question,
            conversation_history: request.conversation_history_messages,
            grounded_context: &context.prepared.answer_context,
            answer_language: context.prepared.query_ir.language,
        },
    )
    .await;
    match single_shot_result {
        Ok(single) => {
            finish_single_shot_fast_path(context, single, single_shot_start, provider_calls).await
        }
        Err(error) => {
            tracing::warn!(
                stage = "answer.single_shot_error",
                execution_id = %execution_context.query_execution_id,
                ?error,
                "single-shot grounded-answer fast path failed — escalating"
            );
            Ok(SingleShotOutcome::Rejected(None))
        }
    }
}

async fn finish_single_shot_fast_path(
    context: &FastPathContext<'_, '_>,
    single: crate::services::query::agent_loop::AgentTurnResult,
    single_shot_start: std::time::Instant,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<SingleShotOutcome> {
    let request = context.request;
    let prepared = context.prepared;
    let state = request.state;
    let execution_context = request.execution_context;
    let execution_id = execution_context.query_execution_id;
    let generation_question =
        answer_generation_question(request.effective_question, request.user_question);
    provider_calls.extend(single.provider_calls.iter().cloned());
    let single_shot_elapsed_ms = single_shot_start.elapsed().as_millis();
    let single_answer = enforce_hard_output_boundary(
        execution_id,
        "answer.single_shot",
        &prepared.query_ir,
        single.answer.clone(),
    );
    log_single_shot_done(execution_id, single_answer.len(), single_shot_elapsed_ms);
    let mut single_debug = single.debug_iterations.clone();
    // No pre-verification snapshot write here: `upsert_snapshot`
    // replaces the whole row keyed by `execution_id`, and every
    // continuation from this point — accepted below, or any
    // escalation outcome — writes the row again with the verified
    // answer and the revised iteration list. A write here would
    // also drain the per-execution span store, so the surviving
    // row ended up with an empty `spans` array.

    // Lightweight verify: no authoritative evidence is
    // required on the fast path because we have not
    // loaded it. The verifier degrades to the
    // "no primary chunks, no bundle" case and applies
    // only the QueryIR-driven strictness level, which
    // still suppresses hallucinated literals on strict
    // paths. Non-strict paths pass through as they did
    // before. When the fast path fails this check we
    // retry through evidence preflight over the same
    // retrieved evidence, which pays the full preflight
    // cost and runs the complete verifier.
    let verify_started = std::time::Instant::now();
    let fast_path_chunks = selected_runtime_answer_chunks(prepared);
    let fast_path_grounding =
        selected_runtime_grounding_evidence(prepared, single.assistant_grounding);
    let mut verification_stage = verify_generated_answer(
        execution_id,
        request.effective_question,
        AnswerGenerationStage {
            intent_profile: prepared.structured.intent_profile.clone(),
            primary_answer_chunks: fast_path_chunks.clone(),
            authoritative_evidence: super::super::AuthoritativeAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            assistant_grounding: fast_path_grounding.clone(),
            answer: single_answer,
            usage_json: single.usage_json.clone(),
            prompt_context: prepared.answer_context.clone(),
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
            path: LiteralRevisionPath::FastPath,
        },
        &mut verification_stage,
        provider_calls,
        &mut single_debug,
    )
    .await?;
    verification_stage = finalize_verified_answer(state, execution_id, verification_stage).await?;
    let verify_elapsed_ms = verify_started.elapsed().as_millis();

    if single_shot_answer_is_acceptable(
        &verification_stage.generation.answer,
        &verification_stage,
        prepared.structured.retrieved_documents.len(),
        &prepared.query_ir,
        &prepared.answer_context,
    ) {
        log_single_shot_accepted(
            execution_id,
            verify_elapsed_ms,
            single_shot_start.elapsed().as_millis(),
        );
        persist_llm_context_snapshot(
            state,
            context.snapshot_context.snapshot_with_iterations(
                &verification_stage.generation.answer,
                single_debug,
                single.iterations,
            ),
        )
        .await?;
        return Ok(SingleShotOutcome::Accepted(Box::new(RuntimeAnswerQueryResult {
            answer: verification_stage.generation.answer,
            provider_calls: std::mem::take(provider_calls),
        })));
    }
    log_single_shot_rejected(execution_id);
    Ok(SingleShotOutcome::Rejected(Some(Box::new(PreflightAnswerCandidate {
        verification_stage,
        debug_iterations: single_debug,
        total_iterations: single.iterations,
    }))))
}

/// The preflight's deterministic override. The product's own statement about
/// the library is terminal and returned verbatim; any other override still
/// passes verification. `None` means the caller continues with the normal path.
async fn finish_preflight_override(
    answer: &PreflightAnswer<'_, '_>,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<Option<RuntimeAnswerQueryResult>> {
    let request = answer.request;
    let prepared = answer.prepared;
    let state = request.state;
    let execution_id = request.execution_context.query_execution_id;
    let Some(answer_override) = answer.preflight.answer_override.clone() else {
        return Ok(None);
    };
    let override_answer = answer_override.answer;
    if answer_override.answer_kind == AnswerKind::MissingExplicitDocument {
        // The body is the product's own deterministic statement about the
        // library, not a corpus-grounded claim: verification and repair do not
        // apply, so it is persisted as a terminal non-factual safe fallback and
        // returned verbatim.
        finalize_deterministic_document_statement(
            state,
            execution_id,
            &override_answer,
            &prepared.query_ir,
        )
        .await?;
        persist_llm_context_snapshot(state, answer.snapshot_context.snapshot(&override_answer))
            .await?;
        return Ok(Some(RuntimeAnswerQueryResult {
            answer: override_answer,
            provider_calls: std::mem::take(provider_calls),
        }));
    }
    // No pre-verification snapshot write here, matching the fast path above:
    // verification can only replace this answer, and the write after it
    // persists the row for this execution once, with the verified answer.
    let verification_stage = verify_generated_answer(
        execution_id,
        request.effective_question,
        AnswerGenerationStage {
            intent_profile: prepared.structured.intent_profile.clone(),
            primary_answer_chunks: answer.preflight.primary_answer_chunks.clone(),
            authoritative_evidence: answer.preflight.authoritative_evidence.clone(),
            assistant_grounding:
                crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
            answer: override_answer,
            usage_json: serde_json::json!({
                "deterministic": true,
                "reason": "\u{63}anonical_preflight_answer",
                "answer_kind": answer_override.answer_kind.as_str(),
            }),
            prompt_context: answer.preflight.prompt_context.clone(),
            query_ir: prepared.query_ir.clone(),
        },
    )
    .await?;
    let verification_stage =
        finalize_verified_answer(state, execution_id, verification_stage).await?;
    persist_llm_context_snapshot(
        state,
        answer.snapshot_context.snapshot(&verification_stage.generation.answer),
    )
    .await?;
    Ok(Some(RuntimeAnswerQueryResult {
        answer: verification_stage.generation.answer,
        provider_calls: std::mem::take(provider_calls),
    }))
}

pub(super) async fn generate_answer_query_after_binding(
    request: AnswerQueryRequest<'_>,
    prepared: PreparedAnswerQueryResult,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let execution_context = request.execution_context;
    let library_id = execution_context.library_id;
    let execution_id = execution_context.query_execution_id;
    let generation_question =
        answer_generation_question(request.effective_question, request.user_question);
    let query_ir_snapshot = serde_json::to_value(&prepared.query_ir)
        .context("failed to serialize query IR for the LLM context snapshot")?;
    // Built once for this execution and read by reference at every write below,
    // so a path that writes the row twice repeats the retrieval spans and the
    // compiled IR instead of blanking them.
    let snapshot_context = AnswerSnapshotContext::new(
        execution_id,
        library_id,
        request.user_question,
        query_ir_snapshot,
        &prepared,
    );

    let requires_remediation_synthesis =
        query_ir_requires_remediation_synthesis(&prepared.query_ir);

    if let Some(result) = try_deterministic_answer(
        request.state,
        request.effective_question,
        generation_question,
        &prepared,
        &snapshot_context,
        requires_remediation_synthesis,
    )
    .await?
    {
        return Ok(result);
    }

    let fast_path_context = FastPathContext {
        request: &request,
        prepared: &prepared,
        snapshot_context: &snapshot_context,
    };
    let (preflight_candidate, attempted_answer_generation) =
        match try_fast_path_answer(&fast_path_context, provider_calls).await? {
            FastPathOutcome::Accepted(result) => return Ok(result),
            FastPathOutcome::Escalate { candidate, attempted_answer_generation } => {
                (*candidate, attempted_answer_generation)
            }
        };

    // Evidence preflight path. Pay the preflight cost now: we need
    // `authoritative_evidence` and `primary_answer_chunks` both for the
    // strict verifier and for the deterministic `answer_override`
    // short-circuit (missing-document / unsupported-capability /
    // exact-literal-grounded answer).
    let preflight_started = std::time::Instant::now();
    let preflight = super::super::prepare_answer_preflight(
        request.state,
        execution_context.workspace_id,
        library_id,
        execution_id,
        request.effective_question,
        &prepared,
    )
    .await?;
    let preflight_elapsed_ms = preflight_started.elapsed().as_millis();
    tracing::info!(
        stage = "answer.preflight_done",
        %execution_id,
        preflight_elapsed_ms,
        primary_chunks = preflight.primary_answer_chunks.len(),
        has_override = preflight.answer_override.is_some(),
        "evidence preflight loaded (escalation)"
    );
    let preflight_answer = PreflightAnswer {
        request: &request,
        prepared: &prepared,
        preflight: &preflight,
        snapshot_context: &snapshot_context,
    };
    if let Some(result) = finish_preflight_override(&preflight_answer, provider_calls).await? {
        return Ok(result);
    }
    if let Some(result) = try_preflight_clarification(&preflight_answer, provider_calls).await? {
        return Ok(result);
    }
    Box::pin(preflight_stages::finalize_preflight_answer(
        &preflight_answer,
        PreflightCarry { candidate: preflight_candidate, attempted_answer_generation },
        provider_calls,
    ))
    .await
}

/// Clarification routed after the evidence preflight loaded, over the preflight
/// context titles. `None` means the caller continues with the normal path.
async fn try_preflight_clarification(
    answer: &PreflightAnswer<'_, '_>,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
) -> anyhow::Result<Option<RuntimeAnswerQueryResult>> {
    let request = answer.request;
    let prepared = answer.prepared;
    let state = request.state;
    let execution_id = request.execution_context.query_execution_id;
    let library_id = request.execution_context.library_id;
    let answer_question =
        answer_question_for_disposition(request.effective_question, request.user_question);
    let preflight_prepared =
        prepared_with_preflight_context_titles(prepared, &answer.preflight.primary_answer_chunks);
    let AnswerDisposition::Clarify { variants } =
        classify_answer_disposition(&preflight_prepared, answer_question)
    else {
        return Ok(None);
    };
    let question = deterministic_clarification_question(
        prepared.query_ir.language,
        ClarificationPromptKind::Source,
    );
    let clarification = disposition_clarification(question, &variants);
    let visible_answer =
        render_typed_clarification_answer(prepared.query_ir.language, &clarification);
    tracing::info!(
        stage = "answer.preflight_clarify_deterministic",
        %execution_id,
        %library_id,
        variant_count = variants.len(),
        query_ir_act = ?prepared.query_ir.act,
        query_ir_confidence = prepared.query_ir.confidence,
        "evidence preflight evidence returned a typed deterministic clarification"
    );
    finalize_typed_clarification_verification(
        state,
        execution_id,
        &visible_answer,
        &prepared.query_ir,
        &clarification,
    )
    .await?;
    persist_llm_context_snapshot(state, answer.snapshot_context.snapshot(&visible_answer)).await?;
    Ok(Some(RuntimeAnswerQueryResult {
        answer: visible_answer,
        provider_calls: std::mem::take(provider_calls),
    }))
}
