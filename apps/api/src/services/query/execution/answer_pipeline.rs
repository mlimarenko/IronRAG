use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        agent_runtime::RuntimeTaskKind,
        query::QueryVerificationState,
        query_ir::{QueryIR, VerificationLevel},
    },
    services::ingest::runtime::resolve_effective_provider_profile,
    services::query::compiler::{CompileHistoryTurn, CompileQueryCommand, QueryCompilerService},
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
    conversation_history: Option<&str>,
    mode: crate::domains::query::RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<PreparedAnswerQueryResult> {
    let query_ir = compile_query_ir(state, library_id, &question, conversation_history).await;
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
    Ok(PreparedAnswerQueryResult { structured, answer_context, embedding_usage, query_ir })
}

/// Runs the NL→IR compiler for the current question + conversation history.
///
/// On any failure — missing binding, provider outage, malformed model output
/// — we log a warning and return the fallback IR (`QueryAct::Describe` /
/// `confidence: 0.0`). The rest of the pipeline degrades gracefully: a
/// fallback IR has `VerificationLevel::Lenient`, so answers still reach the
/// user while we fix the upstream problem.
async fn compile_query_ir(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    conversation_history: Option<&str>,
) -> QueryIR {
    let history = history_turns_from_serialized(conversation_history);
    match QueryCompilerService
        .compile(state, CompileQueryCommand { library_id, question: question.to_string(), history })
        .await
    {
        Ok(outcome) => {
            if let Some(reason) = outcome.fallback_reason.as_deref() {
                tracing::warn!(
                    %library_id,
                    reason,
                    "query compile produced fallback IR"
                );
            }
            outcome.ir
        }
        Err(error) => {
            tracing::warn!(
                %library_id,
                ?error,
                "query compile dispatch failed — using fallback IR"
            );
            // Safe default: descriptive / lenient verification so the user
            // still gets an answer rather than a stub.
            QueryIR {
                act: crate::domains::query_ir::QueryAct::Describe,
                scope: crate::domains::query_ir::QueryScope::SingleDocument,
                language: crate::domains::query_ir::QueryLanguage::Auto,
                target_types: Vec::new(),
                target_entities: Vec::new(),
                literal_constraints: Vec::new(),
                comparison: None,
                document_focus: None,
                conversation_refs: Vec::new(),
                needs_clarification: None,
                confidence: 0.0,
            }
        }
    }
}

/// `conversation_history` arrives pre-serialized as a plain multi-line string
/// (`"role: content\nrole: content"`). Split it back into per-turn entries
/// so the compiler can reason about each turn individually; bad lines are
/// passed through as user content so the compiler still has context.
fn history_turns_from_serialized(history: Option<&str>) -> Vec<CompileHistoryTurn> {
    let Some(raw) = history else {
        return Vec::new();
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if let Some((role, content)) = line.split_once(':') {
                CompileHistoryTurn {
                    role: role.trim().to_string(),
                    content: content.trim().to_string(),
                }
            } else {
                CompileHistoryTurn { role: "user".to_string(), content: line.trim().to_string() }
            }
        })
        .collect()
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
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
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
                query_ir: prepared.query_ir.clone(),
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
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
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
        query_ir: Some(serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null)),
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
            query_ir: prepared.query_ir.clone(),
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
        query_ir: Some(serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null)),
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
    let verifier_tripped =
        has_hallucinated_literal || has_wrong_canonical_target || has_unsupported_canonical_claim;

    // Strictness is driven by the compiled QueryIR, not by keyword lists.
    // Strict (exact-literal retrieve with quoted literals) is the only path
    // that swaps the answer for a safe stub — the cost of returning a
    // hallucinated endpoint / port / config key is high. Moderate and
    // Lenient paths surface warnings in metadata but never overwrite the
    // answer, which restores useful replies to "как настроить X", "что
    // такое Y", and other descriptive asks that the old blanket guard
    // was silently deleting.
    let verification_level = generation.query_ir.verification_level();
    if verifier_tripped && matches!(verification_level, VerificationLevel::Strict) {
        tracing::warn!(
            %execution_id,
            ?verification_level,
            warnings = verification.warnings.len(),
            confidence = generation.query_ir.confidence,
            "answer suppressed on strict exact-literal request with unverified literals"
        );
        generation.answer = "I can't give a confident answer for this question — the most recent \
draft contained values that I couldn't verify against the uploaded documents. Please rephrase \
the question, narrow it to a specific document, or rerun the query."
            .to_string();
    } else if verifier_tripped {
        tracing::info!(
            %execution_id,
            ?verification_level,
            warnings = verification.warnings.len(),
            "answer kept despite verification warnings (moderate/lenient path)"
        );
    } else if matches!(verification.state, QueryVerificationState::Conflicting) {
        tracing::info!(
            %execution_id,
            "answer kept despite conflicting evidence (verification flag only)"
        );
    }

    Ok(AnswerVerificationStage { generation })
}
