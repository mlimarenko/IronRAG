use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        agent_runtime::RuntimeTaskKind,
        query::QueryVerificationState,
        query_ir::QueryIR,
    },
    services::ingest::runtime::resolve_effective_provider_profile,
    services::query::{
        compiler::{CompileHistoryTurn, CompileQueryCommand, QueryCompilerService},
        latest_versions::question_requests_latest_versions,
    },
};

use super::tuning::{
    CLARIFY_DOMINANCE_RATIO, CLARIFY_MAX_VARIANTS, CLARIFY_MIN_DISTINCT_DOCUMENTS,
    MAX_APPENDED_SOURCES, SINGLE_SHOT_CONFIDENT_ANSWER_CHARS, SINGLE_SHOT_MIN_ANSWER_CHARS,
    SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS,
};
use super::{
    AnswerGenerationStage, AnswerVerificationStage, PreparedAnswerQueryResult, QueryCompileUsage,
    RuntimeAnswerQueryResult, RuntimeRetrievedDocumentBrief, apply_query_execution_library_summary,
    apply_query_execution_warning, assemble_answer_context, format_community_context,
    load_query_execution_library_context, search_community_summaries,
    verify_answer_against_canonical_evidence,
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
    // Stage 1: compile + planning run in parallel, then retrieval waits
    // for the compiled IR. This keeps the expensive planning/embedding
    // work overlapped while still letting retrieval consume
    // `document_focus`, scope, and subject entities on the first pass.
    let stage_1_started = std::time::Instant::now();
    let compile_future = compile_query_ir(state, library_id, &question, conversation_history);
    let planning_future = crate::agent_runtime::pipeline::try_op::run_async_try_op((), |_| {
        super::plan_structured_query(state, library_id, &question, mode, top_k)
    });
    let ((query_ir, query_compile_usage), planning_result) =
        tokio::join!(compile_future, planning_future);
    let planning_stage = planning_result?;
    let query_ir_for_retrieval = query_ir.clone();
    let retrieval_question = question.clone();
    let retrieval_stage = crate::agent_runtime::pipeline::try_op::run_async_try_op(
        planning_stage,
        |planning_stage| {
            let query_ir = query_ir_for_retrieval.clone();
            let question = retrieval_question.clone();
            async move {
                super::retrieve_structured_query(
                    state,
                    library_id,
                    &question,
                    planning_stage,
                    Some(&query_ir),
                )
                .await
            }
        },
    )
    .await?;
    let rerank_question = question.clone();
    let mut rerank_stage = crate::agent_runtime::pipeline::try_op::run_async_try_op(
        retrieval_stage,
        |retrieval_stage| {
            let question = rerank_question.clone();
            async move { super::rerank_structured_query(state, &question, retrieval_stage).await }
        },
    )
    .await?;
    let stage_1_elapsed_ms = stage_1_started.elapsed().as_millis();

    // IR-aware consolidation: if the compiler pinned the question to
    // one document (explicit hint / single-doc subject) or the
    // retrieval itself shows one document dominating the evidence,
    // reallocate the top_k slot budget to pack contiguous neighbours
    // of that winner instead of keeping 7 tangentials + 1 winning intro.
    let consolidation_started = std::time::Instant::now();
    let consolidation = super::focused_document_consolidation(
        state,
        &mut rerank_stage.retrieval.bundle,
        &query_ir,
        top_k,
    )
    .await;
    let consolidation_elapsed_ms = consolidation_started.elapsed().as_millis();
    let topical_prune =
        super::prune_non_topical_document_tail(&mut rerank_stage.retrieval.bundle, &question);
    if topical_prune.removed_chunk_count > 0 {
        tracing::info!(
            stage = "answer.topical_prune",
            library_id = %library_id,
            removed_chunk_count = topical_prune.removed_chunk_count,
            kept_chunk_count = topical_prune.kept_chunk_count,
            topical_token_count = topical_prune.topical_token_count,
            "pruned non-topical retrieval tail before answer context assembly"
        );
    }

    // Context assembly runs AFTER consolidation so the assembled
    // `context_text` reflects the reshuffled bundle. The winner
    // document_id is threaded in so `load_retrieved_document_briefs`
    // can build the winner preview out of the anchor-window chunks
    // already in the bundle (rather than re-fetching intro chunks
    // that consolidation deliberately demoted).
    let mut structured = super::finalize_structured_query(
        state,
        &question,
        &query_ir,
        rerank_stage,
        include_debug,
        consolidation.focused_document_id,
    )
    .await?;

    // Stage 2: library_context + community run in parallel — also
    // independent of each other and of the stage-1 outputs. Before
    // this was sequential: library_context await, then community
    // await, adding ~1–3 s of pure serial dead time on every turn.
    let stage_2_started = std::time::Instant::now();
    let library_context_future = load_query_execution_library_context(state, library_id);
    let community_future = search_community_summaries(state, library_id, &question, 3);
    let (library_context_result, community_matches) =
        tokio::join!(library_context_future, community_future);
    let library_context = match library_context_result {
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
    let stage_2_elapsed_ms = stage_2_started.elapsed().as_millis();

    apply_query_execution_warning(
        &mut structured.diagnostics,
        library_context.as_ref().and_then(|context| context.warning.as_ref()),
    );
    apply_query_execution_library_summary(&mut structured.diagnostics, library_context.as_ref());
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

    tracing::info!(
        stage = "answer.prepare",
        library_id = %library_id,
        stage_1_compile_retrieval_ms = stage_1_elapsed_ms,
        stage_2_library_community_ms = stage_2_elapsed_ms,
        consolidation_ms = consolidation_elapsed_ms,
        consolidation_reason = consolidation.focus_reason.as_str(),
        consolidation_winner_chunks = consolidation.winner_chunk_count,
        consolidation_tangential_chunks = consolidation.tangential_chunk_count,
        topical_pruned_chunks = topical_prune.removed_chunk_count,
        retrieved_document_count = structured.retrieved_documents.len(),
        answer_context_chars = answer_context.chars().count(),
        query_ir_confidence = query_ir.confidence,
        query_ir_act = ?query_ir.act,
        "prepare_answer_query stages"
    );

    let embedding_usage = structured.embedding_usage.clone();
    Ok(PreparedAnswerQueryResult {
        structured,
        answer_context,
        embedding_usage,
        query_ir,
        query_compile_usage,
        consolidation,
    })
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
) -> (QueryIR, Option<QueryCompileUsage>) {
    let started_at = std::time::Instant::now();
    let history = history_turns_from_serialized(conversation_history);
    match QueryCompilerService
        .compile(state, CompileQueryCommand { library_id, question: question.to_string(), history })
        .await
    {
        Ok(outcome) => {
            // Single structured line per compile so operators can
            // filter the log on `query.compile.ir` and see cache hit
            // rate + per-call LLM latency at a glance. `served_from_cache`
            // short-circuits LLM entirely, so elapsed_ms < 10 ms on hits
            // and typically 500–3 000 ms on cache-miss LLM calls.
            tracing::info!(
                %library_id,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                served_from_cache = outcome.served_from_cache,
                fallback_reason = outcome.fallback_reason.as_deref().unwrap_or(""),
                provider_kind = %outcome.provider_kind,
                model_name = %outcome.model_name,
                "query.compile.ir"
            );
            if let Some(reason) = outcome.fallback_reason.as_deref() {
                tracing::warn!(
                    %library_id,
                    reason,
                    "query compile produced fallback IR"
                );
            }
            // Capture usage only when the LLM actually ran. Cache hits
            // and fallbacks reuse the `usage_json` of the original
            // call, so billing them here would double-charge (cache
            // hit) or invent phantom tokens (fallback usage_json is
            // a sentinel `{aggregation:"none",call_count:0}`).
            let billable_usage = (!outcome.served_from_cache && outcome.fallback_reason.is_none())
                .then(|| QueryCompileUsage {
                    provider_kind: outcome.provider_kind.clone(),
                    model_name: outcome.model_name.clone(),
                    usage_json: outcome.usage_json.clone(),
                });
            (outcome.ir, billable_usage)
        }
        Err(error) => {
            tracing::warn!(
                %library_id,
                ?error,
                "query compile dispatch failed — using fallback IR"
            );
            // Safe default: descriptive / lenient verification so the user
            // still gets an answer rather than a stub.
            let fallback_ir = QueryIR {
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
            };
            (fallback_ir, None)
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
        tokio::sync::mpsc::UnboundedSender<crate::services::query::agent_loop::AgentProgressEvent>,
    >,
    auth: &crate::interfaces::http::auth::AuthContext,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    // Resolves just the QueryAnswer binding (one Postgres lookup)
    // instead of the full `resolve_effective_provider_profile` which
    // sequentially loaded ExtractGraph + EmbedChunk + QueryCompile
    // + QueryAnswer + Vision — five serial round-trips for something
    // the answer path only needs one of. The selection is still
    // threaded into the deterministic-preflight override branch below
    // (`provider: _answer_provider`), so behaviour is identical.
    let _answer_provider = {
        let binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(
                state,
                library_id,
                crate::domains::ai::AiBindingPurpose::QueryAnswer,
            )
            .await
            .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no active query_answer binding configured for library {library_id}"
                )
            })?;
        crate::domains::provider_profiles::ProviderModelSelection {
            provider_kind: binding.provider_kind.parse().unwrap_or_default(),
            model_name: binding.model_name.clone(),
        }
    };

    // Single-shot fast path tried FIRST — we no longer pay the
    // ~2–3 s `prepare_canonical_answer_preflight` tax before every
    // question. Preflight loads document_index, canonical evidence,
    // answer chunks, and duplicates the community-summary search
    // that `prepare_answer_query` already ran. None of that is
    // needed for the grounded-answer LLM call: `prepared.answer_context`
    // already carries the retrieved chunks, technical literals,
    // library summary, and graph-aware community text. Preflight is
    // now deferred to the escalation path, where the verifier and
    // deterministic `answer_override` logic still use it.
    let should_try_single_shot = should_use_single_shot_answer(effective_question, &prepared);

    // Post-retrieval disposition router: before burning the answer
    // model on a single-shot attempt that will almost certainly
    // hedge, check whether retrieval returned a *dominant* cluster
    // of evidence or a *multi-modal* spread across several distinct
    // subsystems / variants. In the latter case, returning ONE
    // short clarifying question listing those variants is strictly
    // more useful than a "scattered mentions" summary. See
    // `classify_answer_disposition` for the structural signals —
    // no hardcoded domain vocabulary is involved.
    if should_try_single_shot {
        if let AnswerDisposition::Clarify { variants } =
            classify_answer_disposition(&prepared, user_question)
        {
            let clarify_start = std::time::Instant::now();
            tracing::info!(
                stage = "answer.clarify_start",
                %execution_id,
                %library_id,
                variant_count = variants.len(),
                query_ir_act = ?prepared.query_ir.act,
                query_ir_confidence = prepared.query_ir.confidence,
                "post-retrieval router chose clarify path"
            );
            let clarify_result = crate::services::query::agent_loop::run_clarify_turn(
                state,
                library_id,
                user_question,
                conversation_history,
                &variants,
            )
            .await;
            match clarify_result {
                Ok(clarify) => {
                    if !clarify.answer.trim().is_empty() {
                        tracing::info!(
                            stage = "answer.clarify_done",
                            %execution_id,
                            answer_len = clarify.answer.len(),
                            elapsed_ms = clarify_start.elapsed().as_millis(),
                            "clarify path returned a question to the user"
                        );
                        if let Some(sender) = on_progress.as_ref() {
                            let _ = sender.send(
                                crate::services::query::agent_loop::AgentProgressEvent::AnswerDelta(
                                    clarify.answer.clone(),
                                ),
                            );
                        }
                        let clarify_debug = clarify.debug_iterations.clone();
                        state.llm_context_debug.insert(
                            crate::services::query::llm_context_debug::LlmContextSnapshot {
                                execution_id,
                                library_id,
                                question: user_question.to_string(),
                                total_iterations: clarify.iterations,
                                iterations: clarify_debug,
                                final_answer: Some(clarify.answer.clone()),
                                captured_at: chrono::Utc::now(),
                                query_ir: Some(
                                    serde_json::to_value(&prepared.query_ir)
                                        .unwrap_or(serde_json::Value::Null),
                                ),
                            },
                        );
                        return Ok(RuntimeAnswerQueryResult {
                            answer: clarify.answer,
                            provider: clarify.provider,
                            usage_json: clarify.usage_json,
                        });
                    }
                    tracing::info!(
                        stage = "answer.clarify_empty",
                        %execution_id,
                        "clarify path returned empty text — falling back to single-shot"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        stage = "answer.clarify_error",
                        %execution_id,
                        ?error,
                        "clarify path failed — falling back to single-shot"
                    );
                }
            }
        }

        let single_shot_start = std::time::Instant::now();
        tracing::info!(
            stage = "answer.single_shot_start",
            %execution_id,
            %library_id,
            question_len = user_question.len(),
            query_ir_act = ?prepared.query_ir.act,
            query_ir_confidence = prepared.query_ir.confidence,
            retrieved_document_count = prepared.structured.retrieved_documents.len(),
            answer_context_chars = prepared.answer_context.chars().count(),
            "single-shot grounded-answer fast path start"
        );
        let single_shot_result = crate::services::query::agent_loop::run_single_shot_turn(
            state,
            library_id,
            user_question,
            conversation_history,
            &prepared.answer_context,
        )
        .await;
        match single_shot_result {
            Ok(single) => {
                let single_shot_elapsed_ms = single_shot_start.elapsed().as_millis();
                tracing::info!(
                    stage = "answer.single_shot_done",
                    %execution_id,
                    answer_len = single.answer.len(),
                    elapsed_ms = single_shot_elapsed_ms,
                    "single-shot grounded-answer fast path done"
                );
                let single_debug = single.debug_iterations.clone();
                state.llm_context_debug.insert(
                    crate::services::query::llm_context_debug::LlmContextSnapshot {
                        execution_id,
                        library_id,
                        question: user_question.to_string(),
                        total_iterations: single.iterations,
                        iterations: single_debug.clone(),
                        final_answer: (!single.answer.is_empty()).then(|| single.answer.clone()),
                        captured_at: chrono::Utc::now(),
                        query_ir: Some(
                            serde_json::to_value(&prepared.query_ir)
                                .unwrap_or(serde_json::Value::Null),
                        ),
                    },
                );
                // Lightweight verify: no canonical evidence is
                // required on the fast path because we have not
                // loaded it. The verifier degrades to the
                // "no canonical chunks, no bundle" case and applies
                // only the QueryIR-driven strictness level, which
                // still suppresses hallucinated literals on strict
                // paths. Non-strict paths pass through as they did
                // before. When the fast path fails this check we
                // escalate to the tool loop, which pays the full
                // preflight cost and runs the complete verifier.
                let verify_started = std::time::Instant::now();
                let verification_stage = verify_generated_answer(
                    state,
                    execution_id,
                    effective_question,
                    AnswerGenerationStage {
                        intent_profile: prepared.structured.intent_profile.clone(),
                        canonical_answer_chunks: Vec::new(),
                        canonical_evidence: super::CanonicalAnswerEvidence {
                            bundle: None,
                            chunk_rows: Vec::new(),
                            structured_blocks: Vec::new(),
                            technical_facts: Vec::new(),
                        },
                        assistant_grounding: single.assistant_grounding,
                        answer: single.answer.clone(),
                        provider: single.provider.clone(),
                        usage_json: single.usage_json.clone(),
                        prompt_context: prepared.answer_context.clone(),
                        query_ir: prepared.query_ir.clone(),
                    },
                )
                .await?;
                let verify_elapsed_ms = verify_started.elapsed().as_millis();

                if single_shot_answer_is_acceptable(
                    &single.answer,
                    &verification_stage,
                    prepared.structured.retrieved_documents.len(),
                ) {
                    tracing::info!(
                        stage = "answer.single_shot_accepted",
                        %execution_id,
                        verify_elapsed_ms,
                        total_elapsed_ms = single_shot_start.elapsed().as_millis(),
                        "single-shot grounded-answer accepted"
                    );
                    if let Some(sender) = on_progress.as_ref() {
                        let _ = sender.send(
                            crate::services::query::agent_loop::AgentProgressEvent::AnswerDelta(
                                verification_stage.generation.answer.clone(),
                            ),
                        );
                    }
                    state.llm_context_debug.insert(
                        crate::services::query::llm_context_debug::LlmContextSnapshot {
                            execution_id,
                            library_id,
                            question: user_question.to_string(),
                            total_iterations: single.iterations,
                            iterations: single_debug,
                            final_answer: Some(verification_stage.generation.answer.clone()),
                            captured_at: chrono::Utc::now(),
                            query_ir: Some(
                                serde_json::to_value(&prepared.query_ir)
                                    .unwrap_or(serde_json::Value::Null),
                            ),
                        },
                    );
                    let answer_with_sources = append_source_section(
                        verification_stage.generation.answer,
                        &prepared.structured.retrieved_documents,
                        prepared.query_ir.language,
                    );
                    return Ok(RuntimeAnswerQueryResult {
                        answer: answer_with_sources,
                        provider: verification_stage.generation.provider,
                        usage_json: verification_stage.generation.usage_json,
                    });
                }
                tracing::info!(
                    stage = "answer.single_shot_rejected",
                    %execution_id,
                    "single-shot answer unacceptable — escalating to preflight + tool loop"
                );
            }
            Err(error) => {
                tracing::warn!(
                    stage = "answer.single_shot_error",
                    %execution_id,
                    ?error,
                    "single-shot grounded-answer fast path failed — escalating"
                );
            }
        }
    }

    // Escalation path. Pay the preflight cost now: we need
    // `canonical_evidence` and `canonical_answer_chunks` both for
    // the tool loop's verifier and for the deterministic
    // `answer_override` short-circuit (missing-document /
    // unsupported-capability / exact-literal-grounded answer).
    let preflight_started = std::time::Instant::now();
    let preflight = super::prepare_canonical_answer_preflight(
        state,
        library_id,
        execution_id,
        effective_question,
        &prepared,
    )
    .await?;
    let preflight_elapsed_ms = preflight_started.elapsed().as_millis();
    tracing::info!(
        stage = "answer.preflight_done",
        %execution_id,
        preflight_elapsed_ms,
        canonical_chunks = preflight.canonical_answer_chunks.len(),
        has_override = preflight.answer_override.is_some(),
        "canonical-answer preflight loaded (escalation)"
    );
    if let Some(answer) = preflight.answer_override.clone() {
        if let Some(sender) = on_progress.as_ref() {
            let _ = sender.send(
                crate::services::query::agent_loop::AgentProgressEvent::AnswerDelta(answer.clone()),
            );
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
                provider: _answer_provider,
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
        let answer_with_sources = append_source_section(
            verification_stage.generation.answer,
            &prepared.structured.retrieved_documents,
            prepared.query_ir.language,
        );
        return Ok(RuntimeAnswerQueryResult {
            answer: answer_with_sources,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
        });
    }

    let tool_loop_started = std::time::Instant::now();
    tracing::info!(
        stage = "answer.tool_loop_start",
        %execution_id,
        %library_id,
        question_len = user_question.len(),
        escalated = should_try_single_shot,
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
    let tool_loop_elapsed_ms = tool_loop_started.elapsed().as_millis();
    tracing::info!(
        stage = "answer.tool_loop_done",
        %execution_id,
        iterations = result.iterations,
        tool_calls = result.tool_calls_total,
        answer_len = result.answer.len(),
        tool_loop_elapsed_ms,
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
    let answer_with_sources = append_source_section(
        verification_stage.generation.answer,
        &prepared.structured.retrieved_documents,
        prepared.query_ir.language,
    );
    Ok(RuntimeAnswerQueryResult {
        answer: answer_with_sources,
        provider: verification_stage.generation.provider,
        usage_json: verification_stage.generation.usage_json,
    })
}

/// Decide whether the single-shot grounded-answer fast path is a safe
/// substitute for the tool-using agent loop on this turn. The gate
/// is intentionally conservative: it only opts in when the retrieval
/// stage produced meaningful evidence AND the question class is one
/// the single-shot prompt can realistically answer without iterating.
/// Everything else (multi-document `Compare`, follow-up references,
/// low-confidence compiler output, meta-questions about the library)
/// still goes through the tool loop, which can call `list_documents`,
/// read whole files, or walk the graph directly.
/// Append a deterministic "Sources" section to a final answer when
/// the retrieval bundle carries document source URIs. The single-
/// shot prompt already tells the model to quote source URLs inline,
/// but models frequently drop them when summarising long context —
/// so the runtime guarantees the citations downstream regardless.
///
/// Library-agnostic: we filter to entries whose `source_uri` looks
/// like an actual URL (`http://` or `https://`) and keep at most
/// `MAX_APPENDED_SOURCES` unique ones in retrieval order. Non-URL
/// source pointers (e.g. `upload://…`, `file://…`) are NOT
/// appended — they're not clickable and only add noise for the
/// user. The header is picked from the query language so the
/// surrounding answer stays consistent.
fn append_source_section(
    answer: String,
    retrieved_documents: &[RuntimeRetrievedDocumentBrief],
    query_language: crate::domains::query_ir::QueryLanguage,
) -> String {
    use std::collections::HashSet;
    if answer.trim().is_empty() {
        return answer;
    }
    // Skip if the model already rendered markdown HTTP(S) citations
    // inline. Do not treat arbitrary config literals like
    // `https://<host>/api` as citations; those still need a source
    // footer.
    let answer_lower = answer.to_lowercase();
    if answer_lower.contains("](http://") || answer_lower.contains("](https://") {
        return answer;
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut urls: Vec<(String, String)> = Vec::new();
    for document in retrieved_documents {
        let Some(source) = document.source_uri.as_deref() else {
            continue;
        };
        let trimmed = source.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        // Only treat real HTTP(S) URLs as clickable sources. Upload
        // placeholders / file: URIs stay out of the appended block.
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            continue;
        }
        if answer_lower.contains(&lower) {
            // Model already cited this URL — don't duplicate.
            continue;
        }
        if !seen.insert(lower) {
            continue;
        }
        let title = document.title.trim().to_string();
        urls.push((title, trimmed.to_string()));
        if urls.len() >= MAX_APPENDED_SOURCES {
            break;
        }
    }
    tracing::info!(
        stage = "answer.sources_append",
        candidate_count = retrieved_documents.len(),
        appended_count = urls.len(),
        "append_source_section ran"
    );
    if urls.is_empty() {
        return answer;
    }

    // Header picked from the compiled query language. `Auto` falls
    // back to English — there is no reliable script-based router
    // for the footer alone, and the runtime already uses `Auto`
    // as the "mixed / indeterminate" signal.
    use crate::domains::query_ir::QueryLanguage;
    let header = match query_language {
        QueryLanguage::Ru => "Источники",
        QueryLanguage::En | QueryLanguage::Auto => "Sources",
    };

    let mut rendered = String::from(&answer);
    rendered.push_str("\n\n---\n");
    rendered.push_str(header);
    rendered.push_str(":\n");
    for (title, url) in urls {
        if title.is_empty() {
            rendered.push_str(&format!("- {url}\n"));
        } else {
            rendered.push_str(&format!("- [{title}]({url})\n"));
        }
    }
    rendered
}

/// Post-retrieval routing decision: should the runtime answer the
/// question from the evidence it has, or should it ask the user a
/// short clarifying question first?
///
/// This is a *corpus-conditioned* signal — QueryCompiler sees only
/// the raw NL question, but the retrieval bundle reveals whether
/// the library has one dominant procedure for the asked topic or
/// several competing variants / subsystems that a single-shot
/// answer will inevitably hedge across (the observed "scattered
/// mentions but no full guide" failure mode on short
/// `ConfigureHow` queries). Driven purely by structural signals on
/// the retrieved context — no hardcoded domain words, no library-
/// specific lists.
#[derive(Debug, Clone)]
enum AnswerDisposition {
    /// Proceed with single-shot grounded answering; the evidence
    /// has a dominant cluster or the question is specific enough.
    Answer,
    /// Ask a short clarifying question that enumerates the distinct
    /// variants the retrieval bundle found. `variants` are human-
    /// readable labels pulled from retrieved document titles, graph
    /// node labels, or grouped references — whichever are most
    /// naming on the fetched context.
    Clarify { variants: Vec<String> },
}

/// Classify whether the runtime should answer from the retrieved
/// evidence or clarify with the user.
///
/// `Clarify` fires when all four structural signals agree:
///   1. The compiler explicitly asked for clarification via
///      `QueryIR::should_request_clarification()`. Retrieval may shape
///      the clarification menu, but low confidence alone may not
///      override a grounded answer path.
///   2. IR is otherwise underspecified — `ConfigureHow` / `Describe` /
///      `Enumerate` / `RetrieveValue` without `literal_constraints`,
///      `document_focus`, or strong target entities.
///   3. Retrieval is multi-modal — at least
///      `CLARIFY_MIN_DISTINCT_DOCUMENTS` distinct documents hit the
///      bundle and no single document dominates by score.
///   4. The retrieved context names variants — we can pull at
///      least two human-readable labels (document titles, graph
///      node labels) to offer the user.
///
/// Any one failing → `Answer`. `Compare` / `FollowUp` / `Meta`
/// queries never clarify here — they already have their own
/// routing (tool loop).
fn classify_answer_disposition(
    prepared: &PreparedAnswerQueryResult,
    user_question: &str,
) -> AnswerDisposition {
    classify_answer_disposition_from_groups(
        user_question,
        &prepared.query_ir,
        &prepared.structured.retrieved_documents,
        &prepared.structured.diagnostics.grouped_references,
    )
}

fn classify_answer_disposition_from_groups(
    user_question: &str,
    ir: &QueryIR,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    groups: &[crate::domains::query::GroupedReference],
) -> AnswerDisposition {
    use crate::domains::query_ir::QueryAct;

    // 1. Compiler-level: retrieval may help *shape* a clarification
    //    menu, but only an explicit compiler clarification signal is
    //    allowed to interrupt the answer path.
    if !ir.should_request_clarification() {
        return AnswerDisposition::Answer;
    }

    // 2. IR-level: is the question underspecified enough that a
    //    clarifying question could plausibly help?
    let act_can_clarify = matches!(
        ir.act,
        QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
    );
    let is_underspecified = ir.literal_constraints.is_empty()
        && ir.document_focus.is_none()
        && ir.target_entities.len() <= 1;
    if !(act_can_clarify && is_underspecified) {
        return AnswerDisposition::Answer;
    }

    // 3. Retrieval-level: use the already-ranked `grouped_references`
    //    from the structured-query diagnostics. Each entry has a
    //    `title`, a `rank` (already sorted by the runtime) and an
    //    `evidence_count` — the number of distinct chunks /
    //    structured blocks / graph edges that support this group.
    //    A dominant cluster looks like one high evidence count
    //    followed by a sharp drop; a multi-modal spread looks like
    //    several groups with comparable evidence counts.
    if groups.len() < CLARIFY_MIN_DISTINCT_DOCUMENTS {
        return AnswerDisposition::Answer;
    }

    let mut ranked: Vec<(usize, String)> = groups
        .iter()
        .map(|reference| (reference.evidence_count, reference.title.clone()))
        .collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));

    // Dominance check: if the top group has strictly more evidence
    // than `CLARIFY_DOMINANCE_RATIO × second`, it's the main
    // cluster — the single-shot prompt can answer from it.
    if let (Some(top), Some(second)) = (ranked.first(), ranked.get(1)) {
        let (top_n, _) = top;
        let (second_n, _) = second;
        if *top_n > 0
            && *second_n > 0
            && (*top_n as f32) >= (*second_n as f32) * CLARIFY_DOMINANCE_RATIO
        {
            return AnswerDisposition::Answer;
        }
    }

    // 4. Variant extraction: keep only titles that match the user's
    //    topic tokens. Falling back to unrelated ranked tail labels
    //    creates a worse UX than answering from the retrieved context:
    //    the user asked about one thing and the router manufactures a
    //    menu about another. If <2 query-aligned labels survive
    //    deduplication we cannot form a useful clarify menu.
    let variants = extract_query_specific_variants(user_question, retrieved_documents, &ranked);
    if variants.len() < 2 {
        return AnswerDisposition::Answer;
    }

    AnswerDisposition::Clarify { variants }
}

fn extract_query_specific_variants(
    user_question: &str,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    ranked_labels: &[(usize, String)],
) -> Vec<String> {
    use std::collections::HashSet;

    let topic_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(user_question, 3);
    let mut seen: HashSet<String> = HashSet::new();
    let mut topical: Vec<String> = Vec::new();
    for document in retrieved_documents {
        let trimmed = document.title.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let lowered = trimmed.to_lowercase();
        if label_matches_topic_tokens(&topic_tokens, &trimmed) && seen.insert(lowered) {
            topical.push(trimmed);
        }
        if topical.len() >= CLARIFY_MAX_VARIANTS {
            return topical;
        }
    }
    for (_, label) in ranked_labels {
        let trimmed = label.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let lowered = trimmed.to_lowercase();
        if label_matches_topic_tokens(&topic_tokens, &trimmed) && seen.insert(lowered) {
            topical.push(trimmed);
        }
        if topical.len() >= CLARIFY_MAX_VARIANTS {
            break;
        }
    }
    if !topical.is_empty() {
        return topical;
    }

    Vec::new()
}

fn label_matches_topic_tokens(
    topic_tokens: &std::collections::BTreeSet<String>,
    label: &str,
) -> bool {
    if topic_tokens.is_empty() {
        return false;
    }
    let label_lower = label.to_lowercase();
    if topic_tokens.iter().any(|token| label_lower.contains(token)) {
        return true;
    }
    let label_tokens = crate::services::query::text_match::normalized_alnum_tokens(label, 3);
    crate::services::query::text_match::near_token_overlap_count(topic_tokens, &label_tokens) > 0
}

fn should_use_single_shot_answer(question: &str, prepared: &PreparedAnswerQueryResult) -> bool {
    use crate::domains::query_ir::QueryAct;

    if question_requests_latest_versions(question) {
        return false;
    }
    // Only hard requirement: the prepared context must carry *something*
    // the model can ground an answer in. Even when structured retrieval
    // returned zero chunks, `answer_context` still packs the library
    // summary, recent documents, and graph-aware community text — the
    // single-shot prompt will honestly say "нет в документах" from
    // that alone, at ~5 s instead of the 40 s the tool loop would
    // burn re-discovering the same empty result through MCP tools.
    if prepared.answer_context.trim().is_empty() {
        return false;
    }
    // Only three QueryAct classes legitimately need the tool loop:
    //   * `Compare` — requires reading 2+ specific docs side-by-side.
    //   * `FollowUp` — relies on prior-turn tool output the runtime
    //     did not carry over into `answer_context`.
    //   * `Meta` — "what documents do you have" is answered by
    //     `list_documents` / `get_graph_topology`, not retrieval.
    // Everything else (Describe, ConfigureHow, Enumerate, RetrieveValue)
    // goes through the single-shot fast path. Low compiler confidence
    // and non-empty `conversation_refs` no longer block the fast
    // path — they either drive the model toward an honest decline
    // (cheap escalation) or succeed on the retrieval evidence alone.
    !matches!(prepared.query_ir.act, QueryAct::Compare | QueryAct::FollowUp | QueryAct::Meta)
}

/// Treat a single-shot answer as acceptable when it carries enough
/// text to be useful, the verifier did not rewrite it, AND the
/// model did not obviously capitulate in front of a non-empty
/// retrieval bundle.
///
/// Structural signals:
///   * Absolute length floor — below `SINGLE_SHOT_MIN_ANSWER_CHARS`
///     is always treated as a decline.
///   * Verifier rewrite — `verify_generated_answer` only rewrites
///     the answer under strict-mode suppression of a hallucinated
///     literal; a matching trimmed raw vs. verified string means
///     the verifier let the answer through.
///   * Retrieval-vs-length heuristic — when retrieval surfaced
///     `>= SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS` and the
///     answer is still `< SINGLE_SHOT_CONFIDENT_ANSWER_CHARS`, the
///     single-shot path almost certainly refused on partial
///     evidence (see the one-word vs. "who is X" observation above).
///     Escalate instead of returning the stub.
///
/// No decline-phrase matching, no language-specific strings: the
/// verifier owns grounding, length owns "did the model produce
/// something", and the retrieval footprint owns "did the model
/// refuse in the face of real evidence".
fn single_shot_answer_is_acceptable(
    raw_answer: &str,
    verification: &AnswerVerificationStage,
    retrieved_document_count: usize,
) -> bool {
    let trimmed = raw_answer.trim();
    let answer_chars = trimmed.chars().count();
    if answer_chars < SINGLE_SHOT_MIN_ANSWER_CHARS {
        return false;
    }
    let verified = verification.generation.answer.trim();
    if verified.is_empty() {
        return false;
    }
    if trimmed != verified {
        return false;
    }
    if answer_chars < SINGLE_SHOT_CONFIDENT_ANSWER_CHARS
        && retrieved_document_count >= SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS
    {
        return false;
    }
    true
}

#[allow(dead_code)]
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
    generation: AnswerGenerationStage,
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

    // Verifier warnings are surfaced to the UI as banner metadata
    // (`verification.state` + `verification.warnings`), never by
    // overwriting the LLM's answer text. The old Strict path used to
    // swap the whole response for a hardcoded English stub whenever the
    // verifier flagged an `unsupported_literal` — that cost users
    // correct answers against the right retrieved document just because
    // a literal (version string, date) failed strict chunk-substring
    // match. User feedback 2026-04-24: "не надо показывать заглушку,
    // исправь логику чтобы он мог прочитать документ и ответить
    // нормально". The banner + warnings flow is enough — operators and
    // the UI can decide what to do with an untrusted literal, but the
    // grounded answer body stays.
    let verification_level = generation.query_ir.verification_level();
    if verifier_tripped {
        tracing::info!(
            %execution_id,
            ?verification_level,
            warnings = verification.warnings.len(),
            confidence = generation.query_ir.confidence,
            "answer kept despite verification warnings; surfacing via state + warnings only"
        );
    } else if matches!(verification.state, QueryVerificationState::Conflicting) {
        tracing::info!(
            %execution_id,
            "answer kept despite conflicting evidence (verification flag only)"
        );
    }

    Ok(AnswerVerificationStage { generation })
}

#[cfg(test)]
mod tests {
    use super::{
        AnswerDisposition, append_source_section, classify_answer_disposition_from_groups,
    };
    use crate::domains::query::{GroupedReference, GroupedReferenceKind};
    use crate::domains::query_ir::{
        ClarificationReason, ClarificationSpec, EntityMention, EntityRole, QueryAct, QueryIR,
        QueryLanguage, QueryScope,
    };

    fn sample_ir(confidence: f32, needs_clarification: Option<ClarificationReason>) -> QueryIR {
        QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Ru,
            target_types: vec!["procedure".to_string()],
            target_entities: vec![EntityMention {
                label: "платежный модуль".to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: needs_clarification
                .map(|reason| ClarificationSpec { reason, suggestion: String::new() }),
            confidence,
        }
    }

    fn sample_groups() -> Vec<GroupedReference> {
        vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Provider A configuration".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec![
                    "chunk:1".to_string(),
                    "chunk:2".to_string(),
                    "chunk:3".to_string(),
                ],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Provider B configuration".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec![
                    "chunk:4".to_string(),
                    "chunk:5".to_string(),
                    "chunk:6".to_string(),
                ],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Provider C configuration".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:7".to_string(), "chunk:8".to_string()],
            },
        ]
    }

    fn retrieved_doc(title: &str, source_uri: &str) -> super::RuntimeRetrievedDocumentBrief {
        super::RuntimeRetrievedDocumentBrief {
            title: title.to_string(),
            preview_excerpt: String::new(),
            source_uri: Some(source_uri.to_string()),
        }
    }

    #[test]
    fn append_source_section_skips_when_answer_already_has_http_citations() {
        let answer =
            "См. [релевантный документ](https://example.test/relevant) по настройке.".to_string();
        let rendered = append_source_section(
            answer.clone(),
            &[retrieved_doc("Unrelated tail", "https://example.test/unrelated")],
            QueryLanguage::Ru,
        );

        assert_eq!(rendered, answer);
    }

    #[test]
    fn append_source_section_adds_source_when_answer_has_no_links() {
        let rendered = append_source_section(
            "Настройте модуль через конфигурационный файл.".to_string(),
            &[retrieved_doc("Config guide", "https://example.test/config")],
            QueryLanguage::Ru,
        );

        assert!(rendered.contains("Источники:"));
        assert!(rendered.contains("https://example.test/config"));
    }

    #[test]
    fn append_source_section_does_not_treat_config_url_literal_as_citation() {
        let rendered = append_source_section(
            "Параметр url задается как `https://<localhost>/api`.".to_string(),
            &[retrieved_doc("Config guide", "https://example.test/config")],
            QueryLanguage::Ru,
        );

        assert!(rendered.contains("Источники:"));
        assert!(rendered.contains("https://example.test/config"));
    }

    #[test]
    fn disposition_keeps_confident_ir_on_answer_path() {
        let disposition = classify_answer_disposition_from_groups(
            "how do i configure provider payments?",
            &sample_ir(0.9, None),
            &[],
            &sample_groups(),
        );

        assert!(matches!(disposition, AnswerDisposition::Answer));
    }

    #[test]
    fn disposition_keeps_low_confidence_ir_on_answer_path_without_explicit_reason() {
        let disposition = classify_answer_disposition_from_groups(
            "how do i configure provider payments?",
            &sample_ir(0.4, None),
            &[],
            &sample_groups(),
        );

        assert!(matches!(disposition, AnswerDisposition::Answer));
    }

    #[test]
    fn disposition_can_clarify_when_compiler_explicitly_requests_it() {
        let disposition = classify_answer_disposition_from_groups(
            "how do i configure provider payments?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &sample_groups(),
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants.len(), 3);
                assert_eq!(variants[0], "Provider A configuration");
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition for explicit compiler clarification")
            }
        }
    }

    #[test]
    fn disposition_prefers_query_specific_variant_titles_over_noise() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "PaymentLink Provider Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Embedded Browser Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
            GroupedReference {
                id: "document:4".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 4,
                title: "PaymentLink Provider Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:4".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how configure paymentlink?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "PaymentLink Provider Alpha Manual".to_string(),
                        "PaymentLink Provider Beta Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition with query-aligned variants")
            }
        }
    }

    #[test]
    fn disposition_uses_query_specific_retrieved_documents_when_group_titles_are_noisy() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Embedded Browser Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Inventory Exchange Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];
        let retrieved_documents = vec![
            crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
                title: "PaymentLink Provider Alpha Manual".to_string(),
                preview_excerpt: String::new(),
                source_uri: None,
            },
            crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
                title: "PaymentLink Provider Beta Manual".to_string(),
                preview_excerpt: String::new(),
                source_uri: None,
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how configure paymentlink?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &retrieved_documents,
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "PaymentLink Provider Alpha Manual".to_string(),
                        "PaymentLink Provider Beta Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition with query-aligned retrieved documents")
            }
        }
    }

    #[test]
    fn disposition_answers_when_only_one_query_specific_variant_survives() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "TargetName Payment Connector Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Inventory Exchange Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Embedded Browser Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetnme how",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "a single fuzzy topic match must answer from that document instead of clarifying on noise"
        );
    }

    #[test]
    fn disposition_does_not_clarify_from_unmatched_ranked_tail() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Inventory Exchange Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Embedded Browser Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetname how",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "unmatched tail labels must not be turned into a misleading clarify menu"
        );
    }

    #[test]
    fn disposition_clarifies_with_multiple_fuzzy_query_specific_variants() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "TargetName Provider Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "TargetName Provider Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetnme how",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "TargetName Provider Alpha Manual".to_string(),
                        "TargetName Provider Beta Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition with multiple fuzzy topic matches")
            }
        }
    }
}
