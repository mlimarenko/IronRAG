use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{agent_runtime::RuntimeTaskKind, query::QueryVerificationState},
    services::ingest::runtime::resolve_effective_provider_profile,
};

use super::{
    AnswerGenerationStage, AnswerVerificationStage, PreparedAnswerQueryResult,
    RuntimeAnswerQueryResult, apply_query_execution_library_summary, apply_query_execution_warning,
    assemble_answer_context, format_community_context, load_query_execution_library_context,
    search_community_summaries, verify_answer_against_canonical_evidence,
};

pub(crate) async fn prepare_answer_query(
    state: &AppState,
    library_id: Uuid,
    question: String,
    mode: crate::domains::query::RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<PreparedAnswerQueryResult> {
    let mut structured = crate::agent_runtime::pipeline::try_op::run_async_try_op((), |_| {
        super::execute_structured_query(state, library_id, &question, mode, top_k, include_debug)
    })
    .await?;
    let library_context = match load_query_execution_library_context(state, library_id).await {
        Ok(context) => Some(context),
        Err(error) => {
            tracing::warn!(
                error = %error,
                library_id = %library_id,
                "skipping non-critical query library context enrichment"
            );
            None
        }
    };
    apply_query_execution_warning(
        &mut structured.diagnostics,
        library_context.as_ref().and_then(|context| context.warning.as_ref()),
    );
    apply_query_execution_library_summary(&mut structured.diagnostics, library_context.as_ref());
    let community_matches = search_community_summaries(state, library_id, &question, 3).await;
    let community_context_text = format_community_context(&community_matches);
    let mut answer_context = library_context.as_ref().map_or_else(
        || structured.context_text.clone(),
        |context| {
            assemble_answer_context(
                &context.summary,
                &context.recent_documents,
                &structured.retrieved_documents,
                structured.technical_literals_text.as_deref(),
                &structured.context_text,
            )
        },
    );
    if let Some(community_text) = &community_context_text {
        answer_context = format!("{community_text}\n\n{answer_context}");
    }

    let embedding_usage = structured.embedding_usage.clone();
    Ok(PreparedAnswerQueryResult { structured, answer_context, embedding_usage })
}

pub(crate) async fn generate_answer_query(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    effective_question: &str,
    user_question: &str,
    conversation_history: Option<&str>,
    _system_prompt: Option<String>,
    prepared: PreparedAnswerQueryResult,
    on_progress: Option<
        &mut (dyn FnMut(crate::services::query::agent_loop::AgentProgressEvent) + Send),
    >,
    auth: &crate::interfaces::http::auth::AuthContext,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let answer_provider = resolve_query_answer_provider_selection(state, library_id).await?;
    let preflight = super::prepare_canonical_answer_preflight(
        state,
        library_id,
        execution_id,
        effective_question,
        &prepared,
    )
    .await?;
    if let Some(answer) = preflight.answer_override.clone() {
        if let Some(emit) = on_progress {
            emit(crate::services::query::agent_loop::AgentProgressEvent::AnswerDelta(
                answer.clone(),
            ));
        }
        state.llm_context_debug.insert(
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(answer.clone()),
                captured_at: chrono::Utc::now(),
            },
        );
        let verification_stage = verify_generated_answer(
            state,
            execution_id,
            effective_question,
            AnswerGenerationStage {
                intent_profile: prepared.structured.intent_profile.clone(),
                canonical_answer_chunks: preflight.canonical_answer_chunks,
                canonical_evidence: preflight.canonical_evidence,
                assistant_grounding:
                    crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                answer,
                provider: answer_provider,
                usage_json: serde_json::json!({
                    "deterministic": true,
                    "reason": "canonical_preflight_answer",
                }),
                prompt_context: preflight.prompt_context,
            },
        )
        .await?;
        state.llm_context_debug.insert(
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(verification_stage.generation.answer.clone()),
                captured_at: chrono::Utc::now(),
            },
        );
        return Ok(RuntimeAnswerQueryResult {
            answer: verification_stage.generation.answer,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
        });
    }

    tracing::info!(
        %execution_id,
        %library_id,
        question_len = user_question.len(),
        "assistant agent loop start"
    );
    let result = match crate::services::query::agent_loop::run_assistant_turn(
        state,
        auth,
        library_id,
        &execution_id.to_string(),
        user_question,
        conversation_history,
        on_progress,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(
                %execution_id,
                %library_id,
                ?error,
                "assistant agent loop failed"
            );
            return Err(error);
        }
    };
    tracing::info!(
        %execution_id,
        iterations = result.iterations,
        tool_calls = result.tool_calls_total,
        answer_len = result.answer.len(),
        "assistant agent loop done"
    );
    let total_iterations = result.iterations;
    let debug_iterations = result.debug_iterations.clone();
    state.llm_context_debug.insert(crate::services::query::llm_context_debug::LlmContextSnapshot {
        execution_id,
        library_id,
        question: user_question.to_string(),
        total_iterations,
        iterations: debug_iterations.clone(),
        final_answer: (!result.answer.is_empty()).then(|| result.answer.clone()),
        captured_at: chrono::Utc::now(),
    });
    let verification_stage = verify_generated_answer(
        state,
        execution_id,
        effective_question,
        AnswerGenerationStage {
            intent_profile: prepared.structured.intent_profile.clone(),
            canonical_answer_chunks: preflight.canonical_answer_chunks,
            canonical_evidence: preflight.canonical_evidence,
            assistant_grounding: result.assistant_grounding,
            answer: result.answer,
            provider: result.provider,
            usage_json: result.usage_json,
            prompt_context: prepared.answer_context,
        },
    )
    .await?;
    state.llm_context_debug.insert(crate::services::query::llm_context_debug::LlmContextSnapshot {
        execution_id,
        library_id,
        question: user_question.to_string(),
        total_iterations,
        iterations: debug_iterations,
        final_answer: Some(verification_stage.generation.answer.clone()),
        captured_at: chrono::Utc::now(),
    });
    Ok(RuntimeAnswerQueryResult {
        answer: verification_stage.generation.answer,
        provider: verification_stage.generation.provider,
        usage_json: verification_stage.generation.usage_json,
    })
}

pub(crate) async fn resolve_query_answer_provider_selection(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<crate::domains::provider_profiles::ProviderModelSelection> {
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    Ok(provider_profile
        .selection_for_runtime_task_kind(RuntimeTaskKind::QueryAnswer)
        .cloned()
        .unwrap_or_else(|| provider_profile.answer.clone()))
}

pub(crate) async fn verify_generated_answer(
    state: &AppState,
    execution_id: Uuid,
    question: &str,
    mut generation: AnswerGenerationStage,
) -> anyhow::Result<AnswerVerificationStage> {
    let verification = verify_answer_against_canonical_evidence(
        question,
        &generation.answer,
        &generation.intent_profile,
        &generation.canonical_evidence,
        &generation.canonical_answer_chunks,
        &generation.prompt_context,
        &generation.assistant_grounding,
    );
    super::persist_query_verification(
        state,
        execution_id,
        &verification,
        &generation.canonical_evidence,
        &generation.assistant_grounding,
    )
    .await?;

    let has_hallucinated_literal =
        verification.warnings.iter().any(|warning| warning.code == "unsupported_literal");
    let has_wrong_canonical_target =
        verification.warnings.iter().any(|warning| warning.code == "wrong_canonical_target");
    let has_unsupported_canonical_claim =
        verification.warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim");

    if has_hallucinated_literal || has_wrong_canonical_target || has_unsupported_canonical_claim {
        tracing::warn!(
            %execution_id,
            warnings = verification.warnings.len(),
            "answer suppressed due to hallucinated literals or wrong canonical target"
        );
        generation.answer = "I can't give a confident answer for this question — the most recent \
draft contained values that I couldn't verify against the uploaded documents. Please rephrase \
the question, narrow it to a specific document, or rerun the query."
            .to_string();
    } else if matches!(verification.state, QueryVerificationState::Conflicting) {
        tracing::info!(
            %execution_id,
            "answer kept despite conflicting evidence (verification flag only)"
        );
    }

    Ok(AnswerVerificationStage { generation })
}
