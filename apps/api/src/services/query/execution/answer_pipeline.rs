use std::collections::{BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        query::QueryVerificationState,
        query_ir::{EntityRole, QueryAct, QueryIR},
    },
    infra::arangodb::document_store::KnowledgeDocumentRow,
    services::query::{
        compiler::{CompileHistoryTurn, CompileQueryCommand, QueryCompilerService},
        latest_versions::query_requests_latest_versions,
    },
};

use super::technical_literals::{
    TechnicalLiteralIntent, detect_technical_literal_intent_from_query_ir,
    extract_explicit_path_literals, extract_http_methods, extract_parameter_literals,
    extract_prefix_literals, extract_url_literals,
};
use super::tuning::{
    CLARIFY_DOMINANCE_RATIO, CLARIFY_MAX_VARIANTS, CLARIFY_MIN_DISTINCT_DOCUMENTS,
    MAX_APPENDED_SOURCES, SINGLE_SHOT_CONFIDENT_ANSWER_CHARS, SINGLE_SHOT_MIN_ANSWER_CHARS,
    SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS,
};
use super::{
    AnswerGenerationStage, AnswerVerificationStage, FocusReason, PreparedAnswerQueryResult,
    QueryChunkReferenceSnapshot, QueryCompileUsage, RuntimeAnswerQueryResult, RuntimeMatchedChunk,
    RuntimeRetrievedDocumentBrief, apply_query_execution_library_summary,
    apply_query_execution_warning, assemble_answer_context, load_query_execution_library_context,
    render_targeted_evidence_chunk_section, should_prioritize_retrieved_context_for_query,
    verify_answer_against_canonical_evidence,
};

const COMPARE_OPERAND_PROBE_LIMIT: usize = 8;
const COMPARE_OPERAND_PROBE_MAX_CHUNKS: usize = 6;
const COMPARE_OPERAND_PROBE_MAX_CHUNKS_PER_OPERAND: usize = 2;

struct CanonicalAnswerCandidate {
    verification_stage: AnswerVerificationStage,
    debug_iterations: Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
    total_iterations: usize,
}

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
        &question,
        top_k,
    )
    .await;
    let consolidation_elapsed_ms = consolidation_started.elapsed().as_millis();
    let document_index = rerank_stage.retrieval.planning.document_index.clone();
    let plan_keywords = rerank_stage.retrieval.planning.plan.keywords.clone();
    let stale_after_consolidation = super::retain_canonical_document_head_chunks(
        &mut rerank_stage.retrieval.bundle.chunks,
        &document_index,
    );
    if stale_after_consolidation > 0 {
        tracing::info!(
            stage = "retrieval.canonical_head_filter",
            library_id = %library_id,
            stale_chunk_count = stale_after_consolidation,
            "removed non-head revision chunks after focused-document consolidation"
        );
    }
    let source_context = super::augment_structured_source_context(
        state,
        library_id,
        &question,
        Some(&query_ir),
        &document_index,
        &plan_keywords,
        &mut rerank_stage.retrieval.bundle.chunks,
    )
    .await?;
    // Temporal hard-filter on the bundle AFTER source-context augmentation.
    // The companion paths (focused-match, source profile, neighbor expansion,
    // library source profile) bypass the AQL temporal filter and pull
    // chunks regardless of `occurred_at`. When the user explicitly scoped
    // the question to a date range, drop any chunk whose underlying
    // `KnowledgeChunkRow.occurred_at` is null OR falls outside the bounds.
    // Verified necessary on stage 2026-05-03: image-OCR chunks (no
    // occurred_at) were leaking into "messages in March 2026" answers via
    // the prepared-segment / source-context path. Single Arango round-trip
    // via `list_chunks_by_ids`; no per-chunk lookup.
    let (bundle_temporal_start, bundle_temporal_end) = query_ir.resolved_temporal_bounds();
    if bundle_temporal_start.is_some()
        && bundle_temporal_end.is_some()
        && !rerank_stage.retrieval.bundle.chunks.is_empty()
    {
        let chunk_ids: Vec<uuid::Uuid> = rerank_stage
            .retrieval
            .bundle
            .chunks
            .iter()
            .map(|c| c.chunk_id)
            .collect();
        let rows = state
            .arango_document_store
            .list_chunks_by_ids(&chunk_ids)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to look up chunks for bundle-temporal post-filter: {error}"
                )
            })?;
        let allowed: std::collections::HashSet<uuid::Uuid> = rows
            .into_iter()
            .filter(|row| {
                let Some(at) = row.occurred_at else {
                    return false;
                };
                if let Some(start) = bundle_temporal_start
                    && row.occurred_until.unwrap_or(at) < start
                {
                    return false;
                }
                if let Some(end) = bundle_temporal_end
                    && at >= end
                {
                    return false;
                }
                true
            })
            .map(|row| row.chunk_id)
            .collect();
        let before = rerank_stage.retrieval.bundle.chunks.len();
        rerank_stage
            .retrieval
            .bundle
            .chunks
            .retain(|c| allowed.contains(&c.chunk_id));
        tracing::info!(
            stage = "answer.bundle_temporal_post_filter",
            library_id = %library_id,
            before,
            after = rerank_stage.retrieval.bundle.chunks.len(),
            "applied temporal hard-filter to bundle (post source-context)"
        );
    }
    if source_context.source_profile_count > 0
        || source_context.neighbor_count > 0
        || source_context.focused_match_count > 0
        || source_context.source_slice_count > 0
    {
        tracing::info!(
            stage = "retrieval.structured_source_context",
            library_id = %library_id,
            eligible_document_count = source_context.eligible_document_count,
            source_profile_count = source_context.source_profile_count,
            neighbor_count = source_context.neighbor_count,
            focused_match_count = source_context.focused_match_count,
            library_profile_count = source_context.library_profile_count,
            source_slice_count = source_context.source_slice_count,
            "structured source context companions added after consolidation"
        );
    }
    let stale_after_source_context = super::retain_canonical_document_head_chunks(
        &mut rerank_stage.retrieval.bundle.chunks,
        &document_index,
    );
    if stale_after_source_context > 0 {
        tracing::info!(
            stage = "retrieval.canonical_head_filter",
            library_id = %library_id,
            stale_chunk_count = stale_after_source_context,
            "removed non-head revision chunks after structured source context"
        );
    }
    let topical_prune = super::prune_non_topical_document_tail(
        &mut rerank_stage.retrieval.bundle,
        &question,
        query_requests_latest_versions(&query_ir),
    );
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

    // Stage 2: library summary is answer evidence; graph community
    // summaries are intentionally excluded from the final answer prompt
    // because they are broad topology hints, not cited evidence.
    let stage_2_started = std::time::Instant::now();
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
    let stage_2_elapsed_ms = stage_2_started.elapsed().as_millis();

    apply_query_execution_warning(
        &mut structured.diagnostics,
        library_context.as_ref().and_then(|context| context.warning.as_ref()),
    );
    apply_query_execution_library_summary(&mut structured.diagnostics, library_context.as_ref());
    let mut answer_context = library_context.as_ref().map_or_else(
        || structured.context_text.clone(),
        |context| {
            assemble_answer_context(
                &context.summary,
                &structured.retrieved_documents,
                structured.technical_literals_text.as_deref(),
                &structured.context_text,
                should_prioritize_retrieved_context_for_query(&query_ir, &structured.context_text),
            )
        },
    );
    let compare_probe = augment_partial_compare_context(
        state,
        library_id,
        &query_ir,
        &document_index,
        &plan_keywords,
        &mut answer_context,
        &mut structured,
    )
    .await?;
    if compare_probe.attempted {
        tracing::info!(
            stage = "answer.compare_context_probe",
            library_id = %library_id,
            missing_operand_count = compare_probe.missing_operand_count,
            added_chunk_count = compare_probe.added_chunk_count,
            unresolved_operand_count = compare_probe.unresolved_operand_count,
            "partial compare evidence probe completed"
        );
    }

    tracing::info!(
        stage = "answer.prepare",
        library_id = %library_id,
        stage_1_compile_retrieval_ms = stage_1_elapsed_ms,
        stage_2_library_ms = stage_2_elapsed_ms,
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
        consolidation,
        query_ir,
        query_compile_usage,
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
                temporal_constraints: Vec::new(),
                comparison: None,
                document_focus: None,
                conversation_refs: Vec::new(),
                needs_clarification: None,
                source_slice: None,
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
    prepared: PreparedAnswerQueryResult,
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

    if let Some(answer) = super::build_ordered_source_units_answer(
        &prepared.query_ir,
        &prepared.structured.ordered_source_units,
    ) {
        tracing::info!(
            stage = "answer.source_slice_deterministic",
            %execution_id,
            %library_id,
            source_unit_count = prepared.structured.ordered_source_units.len(),
            "deterministic ordered source-slice answer selected"
        );
        let usage_json = serde_json::json!({
            "deterministic": true,
            "answer_kind": "ordered_source_slice",
            "source_unit_count": prepared.structured.ordered_source_units.len(),
        });
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
                assistant_grounding:
                    crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(
                    ),
                answer,
                provider: _answer_provider.clone(),
                usage_json,
                prompt_context: prepared.answer_context.clone(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        let final_answer = verification_stage.generation.answer.clone();
        state.llm_context_debug.insert(
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(final_answer.clone()),
                captured_at: chrono::Utc::now(),
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
            },
        );
        return Ok(RuntimeAnswerQueryResult {
            answer: final_answer,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
        });
    }

    // Single-shot fast path tried FIRST — we no longer pay the
    // ~2–3 s `prepare_canonical_answer_preflight` tax before every
    // question. Preflight loads document_index, canonical evidence,
    // and answer chunks. None of that is needed for the initial
    // grounded-answer LLM call: `prepared.answer_context` already
    // carries the retrieved chunks, technical literals, library
    // summary, and selected graph context. Preflight is now deferred
    // to the escalation path, where the verifier and deterministic
    // `answer_override` logic still use it.
    let should_try_single_shot =
        should_use_single_shot_answer(effective_question, &prepared, conversation_history);
    let mut canonical_candidate: Option<CanonicalAnswerCandidate> = None;
    let mut attempted_answer_generation = false;

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
        attempted_answer_generation = true;
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
                let mut single_debug = single.debug_iterations.clone();
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
                // retry through canonical preflight over the same
                // retrieved evidence, which pays the full preflight
                // cost and runs the complete verifier.
                let verify_started = std::time::Instant::now();
                let mut verification_stage = verify_generated_answer(
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
                if answer_needs_literal_revision(&verification_stage) {
                    tracing::info!(
                        stage = "answer.single_shot_literal_revision_start",
                        %execution_id,
                        unsupported_literals =
                            verification_stage.verification.unsupported_literals.len(),
                        "single-shot answer needs literal-fidelity revision over the same retrieved evidence"
                    );
                    let revision_context = literal_revision_context(
                        &prepared.answer_context,
                        &crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                    );
                    match crate::services::query::agent_loop::run_literal_fidelity_revision_turn(
                        state,
                        library_id,
                        user_question,
                        conversation_history,
                        &verification_stage.generation.answer,
                        &verification_stage.verification.unsupported_literals,
                        &revision_context,
                    )
                    .await
                    {
                        Ok(revision) => {
                            let usage_json = merge_generation_usage(
                                verification_stage.generation.usage_json.clone(),
                                &revision.usage_json,
                            );
                            let revised_stage = verify_generated_answer(
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
                                    assistant_grounding:
                                        crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                                    answer: revision.answer.clone(),
                                    provider: revision.provider.clone(),
                                    usage_json,
                                    prompt_context: prepared.answer_context.clone(),
                                    query_ir: prepared.query_ir.clone(),
                                },
                            )
                            .await?;
                            single_debug.extend(revision.debug_iterations);
                            verification_stage = revised_stage;
                        }
                        Err(error) => {
                            tracing::warn!(
                                stage = "answer.single_shot_literal_revision_error",
                                %execution_id,
                                ?error,
                                "literal-fidelity revision failed for single-shot answer"
                            );
                        }
                    }
                }
                let verify_elapsed_ms = verify_started.elapsed().as_millis();

                if single_shot_answer_is_acceptable(
                    &verification_stage.generation.answer,
                    &verification_stage,
                    prepared.structured.retrieved_documents.len(),
                    &prepared.query_ir,
                    &prepared.answer_context,
                ) {
                    tracing::info!(
                        stage = "answer.single_shot_accepted",
                        %execution_id,
                        verify_elapsed_ms,
                        total_elapsed_ms = single_shot_start.elapsed().as_millis(),
                        "single-shot grounded-answer accepted"
                    );
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
                canonical_candidate = Some(CanonicalAnswerCandidate {
                    verification_stage,
                    debug_iterations: single_debug,
                    total_iterations: single.iterations,
                });
                tracing::info!(
                    stage = "answer.single_shot_rejected",
                    %execution_id,
                    "single-shot answer unacceptable — escalating to canonical preflight over the same retrieved evidence"
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

    // Canonical preflight path. Pay the preflight cost now: we need
    // `canonical_evidence` and `canonical_answer_chunks` both for the
    // strict verifier and for the deterministic `answer_override`
    // short-circuit (missing-document / unsupported-capability /
    // exact-literal-grounded answer).
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

    let has_conversation_history =
        conversation_history.map(str::trim).is_some_and(|value| !value.is_empty());
    let preflight_single_shot_coverage = evaluate_single_shot_evidence_coverage_for_context(
        &prepared.query_ir,
        &preflight.prompt_context,
        has_conversation_history,
    );
    if !preflight.prompt_context.trim().is_empty() {
        if !single_shot_coverage_allows_attempt(&preflight_single_shot_coverage) {
            tracing::info!(
                stage = "answer.preflight_single_shot_canonical_attempt",
                %execution_id,
                coverage = ?preflight_single_shot_coverage,
                "canonical preflight answer will stay on fixed retrieved evidence despite incomplete structural coverage"
            );
        }
        let preflight_single_started = std::time::Instant::now();
        attempted_answer_generation = true;
        tracing::info!(
            stage = "answer.preflight_single_shot_start",
            %execution_id,
            %library_id,
            canonical_chunks = preflight.canonical_answer_chunks.len(),
            prompt_context_chars = preflight.prompt_context.chars().count(),
            "canonical preflight single-shot answer start"
        );
        match crate::services::query::agent_loop::run_single_shot_turn(
            state,
            library_id,
            user_question,
            conversation_history,
            &preflight.prompt_context,
        )
        .await
        {
            Ok(preflight_single) => {
                tracing::info!(
                    stage = "answer.preflight_single_shot_done",
                    %execution_id,
                    answer_len = preflight_single.answer.len(),
                    elapsed_ms = preflight_single_started.elapsed().as_millis(),
                    "canonical preflight single-shot answer done"
                );
                let mut preflight_debug = preflight_single.debug_iterations.clone();
                let mut verification_stage = verify_generated_answer(
                    state,
                    execution_id,
                    effective_question,
                    AnswerGenerationStage {
                        intent_profile: prepared.structured.intent_profile.clone(),
                        canonical_answer_chunks: preflight.canonical_answer_chunks.clone(),
                        canonical_evidence: preflight.canonical_evidence.clone(),
                        assistant_grounding:
                            crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                        answer: preflight_single.answer.clone(),
                        provider: preflight_single.provider.clone(),
                        usage_json: preflight_single.usage_json.clone(),
                        prompt_context: preflight.prompt_context.clone(),
                        query_ir: prepared.query_ir.clone(),
                    },
                )
                .await?;
                if answer_needs_literal_revision(&verification_stage) {
                    tracing::info!(
                        stage = "answer.preflight_single_shot_literal_revision_start",
                        %execution_id,
                        unsupported_literals =
                            verification_stage.verification.unsupported_literals.len(),
                        "canonical preflight single-shot answer needs literal-fidelity revision"
                    );
                    let revision_context = literal_revision_context(
                        &preflight.prompt_context,
                        &crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                    );
                    match crate::services::query::agent_loop::run_literal_fidelity_revision_turn(
                        state,
                        library_id,
                        user_question,
                        conversation_history,
                        &verification_stage.generation.answer,
                        &verification_stage.verification.unsupported_literals,
                        &revision_context,
                    )
                    .await
                    {
                        Ok(revision) => {
                            let usage_json = merge_generation_usage(
                                verification_stage.generation.usage_json.clone(),
                                &revision.usage_json,
                            );
                            let revised_stage = verify_generated_answer(
                                state,
                                execution_id,
                                effective_question,
                                AnswerGenerationStage {
                                    intent_profile: prepared.structured.intent_profile.clone(),
                                    canonical_answer_chunks:
                                        preflight.canonical_answer_chunks.clone(),
                                    canonical_evidence: preflight.canonical_evidence.clone(),
                                    assistant_grounding:
                                        crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                                    answer: revision.answer.clone(),
                                    provider: revision.provider.clone(),
                                    usage_json,
                                    prompt_context: preflight.prompt_context.clone(),
                                    query_ir: prepared.query_ir.clone(),
                                },
                            )
                            .await?;
                            preflight_debug.extend(revision.debug_iterations);
                            verification_stage = revised_stage;
                        }
                        Err(error) => {
                            tracing::warn!(
                                stage = "answer.preflight_single_shot_literal_revision_error",
                                %execution_id,
                                ?error,
                                "literal-fidelity revision failed for canonical preflight answer"
                            );
                        }
                    }
                }
                if single_shot_answer_is_acceptable(
                    &verification_stage.generation.answer,
                    &verification_stage,
                    prepared.structured.retrieved_documents.len(),
                    &prepared.query_ir,
                    &preflight.prompt_context,
                ) {
                    tracing::info!(
                        stage = "answer.preflight_single_shot_accepted",
                        %execution_id,
                        verify_state = ?verification_stage.verification.state,
                        total_elapsed_ms = preflight_single_started.elapsed().as_millis(),
                        "canonical preflight single-shot answer accepted"
                    );
                    state.llm_context_debug.insert(
                        crate::services::query::llm_context_debug::LlmContextSnapshot {
                            execution_id,
                            library_id,
                            question: user_question.to_string(),
                            total_iterations: preflight_debug.len(),
                            iterations: preflight_debug,
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
                let verify_state = verification_stage.verification.state;
                let warning_count = verification_stage.verification.warnings.len();
                tracing::info!(
                    stage = "answer.preflight_single_shot_rejected",
                    %execution_id,
                    verify_state = ?verify_state,
                    warning_count,
                    "canonical preflight single-shot answer has verifier warnings — returning fixed-evidence result without re-retrieval"
                );
                canonical_candidate = Some(CanonicalAnswerCandidate {
                    total_iterations: preflight_debug.len(),
                    debug_iterations: preflight_debug,
                    verification_stage,
                });
            }
            Err(error) => {
                tracing::warn!(
                    stage = "answer.preflight_single_shot_error",
                    %execution_id,
                    ?error,
                    "canonical preflight single-shot answer failed"
                );
            }
        }
    }

    if canonical_candidate.is_none() && !attempted_answer_generation {
        let answer = "No grounded evidence was retrieved for this question.".to_string();
        tracing::info!(
            stage = "answer.no_evidence_finalized",
            %execution_id,
            "finalizing deterministic insufficient-evidence answer because retrieval produced no answer context"
        );
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
                assistant_grounding:
                    crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(
                    ),
                answer,
                provider: _answer_provider,
                usage_json: serde_json::json!({
                    "deterministic": true,
                    "reason": "no_grounded_evidence",
                }),
                prompt_context: String::new(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        canonical_candidate = Some(CanonicalAnswerCandidate {
            verification_stage,
            debug_iterations: Vec::new(),
            total_iterations: 0,
        });
    }

    let Some(candidate) = canonical_candidate else {
        anyhow::bail!(
            "canonical grounded-answer generation produced no answer candidate for execution {execution_id}"
        );
    };
    tracing::info!(
        stage = "answer.fixed_evidence_finalized",
        %execution_id,
        verify_state = ?candidate.verification_stage.verification.state,
        warning_count = candidate.verification_stage.verification.warnings.len(),
        "finalizing grounded answer from fixed retrieved evidence without a second retrieval pass"
    );
    state.llm_context_debug.insert(crate::services::query::llm_context_debug::LlmContextSnapshot {
        execution_id,
        library_id,
        question: user_question.to_string(),
        total_iterations: candidate.total_iterations,
        iterations: candidate.debug_iterations,
        final_answer: Some(candidate.verification_stage.generation.answer.clone()),
        captured_at: chrono::Utc::now(),
        query_ir: Some(serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null)),
    });
    let answer_with_sources = append_source_section(
        candidate.verification_stage.generation.answer,
        &prepared.structured.retrieved_documents,
        prepared.query_ir.language,
    );
    Ok(RuntimeAnswerQueryResult {
        answer: answer_with_sources,
        provider: candidate.verification_stage.generation.provider,
        usage_json: candidate.verification_stage.generation.usage_json,
    })
}

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

    let _ = query_language;
    let header = "Sources";

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
/// `Clarify` fires when the retrieved evidence itself shows the user's
/// topic is split across multiple variants. The compiler's explicit
/// clarification flag lowers the evidence threshold, but it is not the
/// sole authority: the compiler cannot see the retrieved bundle, so a
/// no-clarify IR must not force a weak single-shot answer when retrieval
/// has already found several query-aligned variants.
///
/// Required structural signals:
///   1. IR is underspecified enough that a clarifying question could help —
///      `ConfigureHow` / `Describe` / `RetrieveValue` without
///      `literal_constraints` or `document_focus`. Multiple target entities
///      normally mean the query is specific, except when the compiler already
///      requested clarification or the user sent a terse topic-selector
///      follow-up such as `<product> <topic>`.
///   2. Retrieval is multi-modal — at least
///      `CLARIFY_MIN_DISTINCT_DOCUMENTS` distinct documents hit the
///      bundle and no single document dominates by score.
///   3. The retrieved context names variants — we can pull at
///      least two human-readable labels (document titles, graph
///      node labels) to offer the user.
///   4. Without an explicit compiler flag, the query must be a configure
///      intent or a terse topic-selection follow-up; definition and
///      enumerate intents stay on the answer path.
///
/// Any one failing → `Answer`. `Compare` / `FollowUp` / `Meta`
/// queries never clarify here; they stay on the answer path because
/// retrieval coverage decides whether the fixed context is sufficient.
fn classify_answer_disposition(
    prepared: &PreparedAnswerQueryResult,
    user_question: &str,
) -> AnswerDisposition {
    if consolidation_commits_to_focused_answer(&prepared.consolidation) {
        return AnswerDisposition::Answer;
    }

    classify_answer_disposition_from_evidence(
        user_question,
        &prepared.query_ir,
        &prepared.structured.retrieved_documents,
        &prepared.structured.retrieved_context_document_titles,
        &prepared.structured.diagnostics.grouped_references,
    )
}

fn consolidation_commits_to_focused_answer(
    consolidation: &super::ConsolidationDiagnostics,
) -> bool {
    consolidation.focused_document_id.is_some()
        && !matches!(consolidation.focus_reason, FocusReason::None)
        && consolidation.winner_chunk_count > 0
}

#[cfg(test)]
fn classify_answer_disposition_from_groups(
    user_question: &str,
    ir: &QueryIR,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    groups: &[crate::domains::query::GroupedReference],
) -> AnswerDisposition {
    classify_answer_disposition_from_evidence(user_question, ir, retrieved_documents, &[], groups)
}

fn classify_answer_disposition_from_evidence(
    user_question: &str,
    ir: &QueryIR,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    context_document_titles: &[String],
    groups: &[crate::domains::query::GroupedReference],
) -> AnswerDisposition {
    use crate::domains::query_ir::QueryAct;

    let compiler_requested_clarification = ir.should_request_clarification();

    // 1. IR-level: is the question underspecified enough that a
    //    clarifying question could plausibly help?
    let act_can_clarify =
        matches!(ir.act, QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::RetrieveValue);
    let target_entities_allow_clarify = ir.target_entities.len() <= 1
        || compiler_requested_clarification
        || (matches!(ir.act, QueryAct::RetrieveValue)
            && question_is_terse_variant_selector(user_question));
    let is_underspecified = ir.literal_constraints.is_empty()
        && ir.document_focus.is_none()
        && target_entities_allow_clarify;
    if !(act_can_clarify && is_underspecified) {
        return AnswerDisposition::Answer;
    }
    // Temporal hard-filter already scoped retrieval. If the IR carries
    // resolved RFC3339 bounds the user has narrowed the question by
    // window — the AQL filter (T1.4) drops every off-window chunk before
    // ranking, so the retrieved cluster is by construction a single
    // window. Routing into the multi-variant clarify prompt then asks
    // the user to disambiguate between off-window topics that
    // retrieval has already excluded, which produces the off-topic
    // "could be one of: X, Y, Z" replies the date-anchored benchmarks
    // surfaced. Stay on the answer path so the grounded prompt can
    // describe what the in-window evidence actually says (or refuse
    // cleanly when it says nothing).
    let (temporal_start, temporal_end) = ir.resolved_temporal_bounds();
    if temporal_start.is_some() || temporal_end.is_some() {
        return AnswerDisposition::Answer;
    }
    if !compiler_requested_clarification
        && !structural_clarify_allowed_without_compiler(ir, user_question)
    {
        return AnswerDisposition::Answer;
    }

    // 2. Retrieval-level: use the already-ranked `grouped_references`
    //    from the structured-query diagnostics. Each entry has a
    //    `title`, a `rank` (already sorted by the runtime) and an
    //    `evidence_count` — the number of distinct chunks /
    //    structured blocks / graph edges that support this group.
    //    A dominant cluster looks like one high evidence count
    //    followed by a sharp drop; a multi-modal spread looks like
    //    several groups with comparable evidence counts.
    let evidence_document_count =
        groups.len().max(context_document_titles.len()).max(retrieved_documents.len());
    if evidence_document_count < CLARIFY_MIN_DISTINCT_DOCUMENTS {
        return AnswerDisposition::Answer;
    }

    let mut ranked: Vec<(usize, String)> = groups
        .iter()
        .map(|reference| (reference.evidence_count, reference.title.clone()))
        .collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));

    // 3. Variant extraction: keep only titles that match the user's
    //    topic tokens. Falling back to unrelated ranked tail labels
    //    creates a worse UX than answering from the retrieved context:
    //    the user asked about one thing and the router manufactures a
    //    menu about another. If too few query-aligned labels survive
    //    deduplication we cannot form a useful clarify menu.
    let variants = extract_query_specific_variants(
        user_question,
        retrieved_documents,
        context_document_titles,
        &ranked,
    );
    let required_variant_count =
        if compiler_requested_clarification { 2 } else { CLARIFY_MIN_DISTINCT_DOCUMENTS };
    if variants.len() < required_variant_count {
        return AnswerDisposition::Answer;
    }

    // Dominance check is applied only to query-aligned groups. A large
    // off-topic evidence cluster should not suppress clarification for
    // the smaller set of documents that actually share the user's topic.
    let variant_labels = variants.iter().map(|label| label.to_lowercase()).collect::<Vec<_>>();
    let topic_ranked = ranked
        .iter()
        .filter(|(_, label)| {
            let lowered = label.to_lowercase();
            variant_labels.iter().any(|variant| variant == &lowered)
        })
        .cloned()
        .collect::<Vec<_>>();

    // If the top query-aligned group has strictly more evidence than
    // `CLARIFY_DOMINANCE_RATIO × second`, it's the main cluster — the
    // single-shot prompt can answer from it.
    if topic_ranked.len() >= variants.len() {
        if let (Some(top), Some(second)) = (topic_ranked.first(), topic_ranked.get(1)) {
            let (top_n, _) = top;
            let (second_n, _) = second;
            let materially_more_evidence = top_n.saturating_sub(*second_n) >= 2;
            if *top_n > 0
                && *second_n > 0
                && materially_more_evidence
                && (*top_n as f32) >= (*second_n as f32) * CLARIFY_DOMINANCE_RATIO
            {
                return AnswerDisposition::Answer;
            }
        }
    }

    AnswerDisposition::Clarify { variants }
}

fn structural_clarify_allowed_without_compiler(ir: &QueryIR, user_question: &str) -> bool {
    use crate::domains::query_ir::QueryAct;

    match ir.act {
        QueryAct::ConfigureHow => true,
        QueryAct::RetrieveValue => question_is_terse_variant_selector(user_question),
        _ => false,
    }
}

fn question_is_terse_variant_selector(user_question: &str) -> bool {
    let topic_tokens = clarification_topic_tokens(user_question);
    !topic_tokens.is_empty() && topic_tokens.len() <= 3
}

fn extract_query_specific_variants(
    user_question: &str,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    context_document_titles: &[String],
    ranked_labels: &[(usize, String)],
) -> Vec<String> {
    use std::collections::HashSet;

    let candidate_labels = context_document_titles
        .iter()
        .map(String::as_str)
        .chain(retrieved_documents.iter().map(|document| document.title.as_str()))
        .chain(ranked_labels.iter().map(|(_, label)| label.as_str()))
        .collect::<Vec<_>>();
    let topic_tokens = clarification_focus_tokens(user_question, candidate_labels.iter().copied());
    let mut seen: HashSet<String> = HashSet::new();
    let mut topical: Vec<String> = Vec::new();
    for title in context_document_titles {
        let trimmed = title.trim().to_string();
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
    if !topical.is_empty() {
        return topical;
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

fn clarification_focus_tokens<'a, I>(
    user_question: &str,
    candidate_labels: I,
) -> std::collections::BTreeSet<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let topic_tokens = clarification_topic_tokens(user_question);
    if topic_tokens.len() <= 1 {
        return topic_tokens;
    }

    let label_token_sets = candidate_labels
        .into_iter()
        .map(|label| crate::services::query::text_match::normalized_alnum_tokens(label, 3))
        .filter(|tokens| !tokens.is_empty())
        .collect::<Vec<_>>();
    if label_token_sets.is_empty() {
        return topic_tokens;
    }

    let mut repeated = std::collections::BTreeSet::new();
    let mut discriminating = std::collections::BTreeSet::new();
    for token in &topic_tokens {
        let hit_count = label_token_sets
            .iter()
            .filter(|label_tokens| {
                label_tokens.iter().any(|label_token| {
                    crate::services::query::text_match::near_token_match(token, label_token)
                })
            })
            .count();
        if hit_count >= 2 {
            repeated.insert(token.clone());
        }
        if hit_count >= 2 && hit_count < label_token_sets.len() {
            discriminating.insert(token.clone());
        }
    }

    if !discriminating.is_empty() {
        return discriminating;
    }
    if !repeated.is_empty() {
        return repeated;
    }
    topic_tokens
}

fn label_matches_topic_tokens(
    topic_tokens: &std::collections::BTreeSet<String>,
    label: &str,
) -> bool {
    if topic_tokens.is_empty() {
        return false;
    }
    let label_tokens = crate::services::query::text_match::normalized_alnum_tokens(label, 3);
    crate::services::query::text_match::near_token_overlap_count(topic_tokens, &label_tokens) > 0
}

fn clarification_topic_tokens(user_question: &str) -> std::collections::BTreeSet<String> {
    crate::services::query::text_match::normalized_alnum_tokens(user_question, 3)
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, Default)]
struct CompareContextProbeOutcome {
    attempted: bool,
    missing_operand_count: usize,
    added_chunk_count: usize,
    unresolved_operand_count: usize,
}

async fn augment_partial_compare_context(
    state: &AppState,
    library_id: Uuid,
    ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    answer_context: &mut String,
    structured: &mut super::RuntimeStructuredQueryResult,
) -> anyhow::Result<CompareContextProbeOutcome> {
    if !matches!(ir.act, QueryAct::Compare) {
        return Ok(CompareContextProbeOutcome::default());
    }
    let EvidenceCoverage::Partial { covered_operands, missing_operands } =
        compare_operands_covered_by_context(ir, answer_context)
    else {
        return Ok(CompareContextProbeOutcome::default());
    };
    let mut outcome = CompareContextProbeOutcome {
        attempted: true,
        missing_operand_count: missing_operands.len(),
        added_chunk_count: 0,
        unresolved_operand_count: missing_operands.len(),
    };
    let existing_chunk_ids = structured
        .chunk_references
        .iter()
        .map(|reference| reference.chunk_id)
        .collect::<HashSet<_>>();
    let probe_chunks = probe_missing_compare_operands(
        state,
        library_id,
        &missing_operands,
        document_index,
        plan_keywords,
        &existing_chunk_ids,
    )
    .await?;
    if !probe_chunks.is_empty() {
        let probe_question = missing_operands.join(" ");
        let probe_context = render_targeted_evidence_chunk_section(&probe_question, &probe_chunks);
        append_answer_context_section(answer_context, &probe_context);
        append_probe_chunk_references(structured, &probe_chunks);
        append_probe_document_titles(structured, &probe_chunks);
        outcome.added_chunk_count = probe_chunks.len();
    }

    match compare_operands_covered_by_context(ir, answer_context) {
        EvidenceCoverage::Sufficient => {
            outcome.unresolved_operand_count = 0;
        }
        EvidenceCoverage::Partial { covered_operands, missing_operands } => {
            outcome.unresolved_operand_count = missing_operands.len();
            append_answer_context_section(
                answer_context,
                &render_partial_comparison_coverage(&covered_operands, &missing_operands),
            );
        }
        EvidenceCoverage::Insufficient(_) => {
            append_answer_context_section(
                answer_context,
                &render_partial_comparison_coverage(&covered_operands, &missing_operands),
            );
        }
    }
    Ok(outcome)
}

async fn probe_missing_compare_operands(
    state: &AppState,
    library_id: Uuid,
    missing_operands: &[String],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let mut score_by_chunk = HashMap::<Uuid, f32>::new();
    for operand in missing_operands {
        let rows = state
            .arango_search_store
            .search_chunks(library_id, operand, COMPARE_OPERAND_PROBE_LIMIT, None, None)
            .await?;
        let mut accepted_for_operand = 0usize;
        for row in rows {
            if existing_chunk_ids.contains(&row.chunk_id)
                || !search_row_covers_operand(operand, &row)
            {
                continue;
            }
            let score = row.score as f32;
            score_by_chunk
                .entry(row.chunk_id)
                .and_modify(|existing| {
                    if score > *existing {
                        *existing = score;
                    }
                })
                .or_insert(score);
            accepted_for_operand += 1;
            if accepted_for_operand >= COMPARE_OPERAND_PROBE_MAX_CHUNKS_PER_OPERAND {
                break;
            }
        }
    }
    if score_by_chunk.is_empty() {
        return Ok(Vec::new());
    }
    let chunk_ids = score_by_chunk.keys().copied().collect::<Vec<_>>();
    let rows = state.arango_document_store.list_chunks_by_ids(&chunk_ids).await?;
    let mut chunks = Vec::<RuntimeMatchedChunk>::new();
    for row in rows {
        let Some(score) = score_by_chunk.get(&row.chunk_id).copied() else {
            continue;
        };
        let Some(chunk) = super::retrieve::map_chunk_hit(row, score, document_index, plan_keywords)
        else {
            continue;
        };
        chunks.push(chunk);
    }
    chunks.sort_by(|left, right| {
        right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal)
    });
    chunks.truncate(COMPARE_OPERAND_PROBE_MAX_CHUNKS);
    Ok(chunks)
}

fn search_row_covers_operand(
    operand: &str,
    row: &crate::infra::arangodb::search_store::KnowledgeChunkSearchRow,
) -> bool {
    let section = row.section_path.join(" ");
    let heading = row.heading_trail.join(" ");
    let evidence = [
        row.content_text.as_str(),
        row.normalized_text.as_str(),
        section.as_str(),
        heading.as_str(),
    ];
    operand_covered_by_evidence(operand, &evidence)
}

fn append_probe_chunk_references(
    structured: &mut super::RuntimeStructuredQueryResult,
    chunks: &[RuntimeMatchedChunk],
) {
    let mut seen = structured
        .chunk_references
        .iter()
        .map(|reference| reference.chunk_id)
        .collect::<HashSet<_>>();
    let mut next_rank =
        structured.chunk_references.iter().map(|reference| reference.rank).max().unwrap_or(0) + 1;
    for chunk in chunks {
        if !seen.insert(chunk.chunk_id) {
            continue;
        }
        structured.chunk_references.push(QueryChunkReferenceSnapshot {
            chunk_id: chunk.chunk_id,
            rank: next_rank,
            score: chunk.score.unwrap_or(0.0) as f64,
        });
        next_rank += 1;
    }
}

fn append_probe_document_titles(
    structured: &mut super::RuntimeStructuredQueryResult,
    chunks: &[RuntimeMatchedChunk],
) {
    let mut seen = structured
        .retrieved_context_document_titles
        .iter()
        .map(|title| title.to_lowercase())
        .collect::<HashSet<_>>();
    for chunk in chunks {
        let title = chunk.document_label.trim();
        if title.is_empty() {
            continue;
        }
        if seen.insert(title.to_lowercase()) {
            structured.retrieved_context_document_titles.push(title.to_string());
        }
    }
}

fn append_answer_context_section(answer_context: &mut String, section: &str) {
    let section = section.trim();
    if section.is_empty() {
        return;
    }
    if !answer_context.trim().is_empty() {
        answer_context.push_str("\n\n");
    }
    answer_context.push_str(section);
}

fn render_partial_comparison_coverage(
    covered_operands: &[String],
    missing_operands: &[String],
) -> String {
    let mut lines = vec!["COMPARISON_COVERAGE status=partial".to_string()];
    for operand in covered_operands {
        lines.push(format!("- covered_operand: {}", operand.trim()));
    }
    for operand in missing_operands {
        lines.push(format!("- uncovered_operand: {}", operand.trim()));
    }
    lines.join("\n")
}

fn should_use_single_shot_answer(
    question: &str,
    prepared: &PreparedAnswerQueryResult,
    conversation_history: Option<&str>,
) -> bool {
    let _ = question;
    // Only hard requirement: the prepared context must carry *something*
    // the model can ground an answer in. Even when structured retrieval
    // returned zero chunks, `answer_context` still packs the library
    // summary, recent documents, and selected graph context. That alone
    // is enough for the model to produce a grounded insufficiency answer
    // without spending another pass rediscovering the same empty result.
    if prepared.answer_context.trim().is_empty() {
        return false;
    }
    // Single-shot is evidence-gated, not act-blacklisted. The
    // prepared retrieval context is authoritative when it structurally
    // covers the operands required by the IR. Questions that still
    // depend on unresolved conversation anchors or library operations
    // remain off the initial fast path until those requirements are
    // represented in the same coverage model.
    // Retrieval injects version-sorted release chunks directly into
    // `answer_context`; a second retrieval pass would only repeat
    // document reads without adding canonical evidence.
    let has_conversation_history =
        conversation_history.map(str::trim).is_some_and(|v| !v.is_empty());
    match evaluate_single_shot_evidence_coverage(prepared, has_conversation_history) {
        EvidenceCoverage::Sufficient => true,
        EvidenceCoverage::Partial { missing_operands, .. } => {
            tracing::info!(
                stage = "answer.single_shot_coverage",
                query_ir_act = ?prepared.query_ir.act,
                missing_operand_count = missing_operands.len(),
                "prepared answer context partially covers comparison operands; single-shot must answer with explicit insufficiency for uncovered operands"
            );
            true
        }
        EvidenceCoverage::Insufficient(reason) => {
            tracing::info!(
                stage = "answer.single_shot_coverage",
                query_ir_act = ?prepared.query_ir.act,
                reason,
                "prepared answer context does not structurally cover the single-shot requirements"
            );
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvidenceCoverage {
    Sufficient,
    Partial { covered_operands: Vec<String>, missing_operands: Vec<String> },
    Insufficient(&'static str),
}

fn evaluate_single_shot_evidence_coverage(
    prepared: &PreparedAnswerQueryResult,
    has_conversation_history: bool,
) -> EvidenceCoverage {
    evaluate_single_shot_evidence_coverage_for_context(
        &prepared.query_ir,
        &prepared.answer_context,
        has_conversation_history,
    )
}

fn evaluate_single_shot_evidence_coverage_for_context(
    ir: &QueryIR,
    answer_context: &str,
    has_conversation_history: bool,
) -> EvidenceCoverage {
    if ir.is_follow_up() && has_conversation_history {
        return EvidenceCoverage::Insufficient("follow_up_context_anchor_unresolved");
    }
    if matches!(ir.act, QueryAct::Meta) {
        return EvidenceCoverage::Insufficient("library_meta_requires_catalog_evidence");
    }
    if matches!(ir.act, QueryAct::Compare) {
        return compare_operands_covered_by_context(ir, answer_context);
    }
    if single_shot_context_lacks_query_focus_support(ir, answer_context) {
        return EvidenceCoverage::Insufficient("query_focus_uncovered");
    }
    EvidenceCoverage::Sufficient
}

fn single_shot_coverage_allows_attempt(coverage: &EvidenceCoverage) -> bool {
    matches!(coverage, EvidenceCoverage::Sufficient | EvidenceCoverage::Partial { .. })
}

fn compare_operands_covered_by_context(ir: &QueryIR, answer_context: &str) -> EvidenceCoverage {
    let operands = comparison_operands(ir);
    if operands.len() < 2 {
        return EvidenceCoverage::Insufficient("compare_operands_missing");
    }
    let evidence_lines =
        answer_context.lines().map(str::trim).filter(is_context_evidence_line).collect::<Vec<_>>();
    if evidence_lines.is_empty() {
        return EvidenceCoverage::Insufficient("compare_evidence_empty");
    }
    let mut covered_operands = Vec::<String>::new();
    let mut missing_operands = Vec::<String>::new();
    for operand in operands {
        if operand_covered_by_evidence(&operand, &evidence_lines) {
            covered_operands.push(operand);
        } else {
            missing_operands.push(operand);
        }
    }
    if missing_operands.is_empty() {
        EvidenceCoverage::Sufficient
    } else if covered_operands.is_empty() {
        EvidenceCoverage::Insufficient("compare_operands_uncovered")
    } else {
        EvidenceCoverage::Partial { covered_operands, missing_operands }
    }
}

fn is_context_evidence_line(line: &&str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && !line.starts_with("COMPARISON_COVERAGE ")
        && !line.starts_with("- covered_operand:")
        && !line.starts_with("- uncovered_operand:")
}

fn comparison_operands(ir: &QueryIR) -> Vec<String> {
    let mut operands = Vec::<String>::new();
    if let Some(comparison) = &ir.comparison {
        if let Some(value) = comparison.a.as_deref() {
            push_operand(&mut operands, value);
        }
        if let Some(value) = comparison.b.as_deref() {
            push_operand(&mut operands, value);
        }
    }
    if operands.len() >= 2 {
        return operands;
    }
    for entity in &ir.target_entities {
        push_operand(&mut operands, &entity.label);
    }
    operands
}

fn push_operand(operands: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if operands.iter().any(|existing| existing.eq_ignore_ascii_case(trimmed)) {
        return;
    }
    operands.push(trimmed.to_string());
}

fn operand_covered_by_evidence(operand: &str, evidence_lines: &[&str]) -> bool {
    let operand_tokens = crate::services::query::text_match::normalized_alnum_tokens(operand, 2);
    if operand_tokens.is_empty() {
        return false;
    }
    let required_overlap = operand_tokens.len().clamp(1, 2);
    evidence_lines.iter().any(|line| {
        let line_tokens = crate::services::query::text_match::normalized_alnum_tokens(line, 2);
        crate::services::query::text_match::near_token_overlap_count(&operand_tokens, &line_tokens)
            >= required_overlap
    })
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
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    let trimmed = raw_answer.trim();
    let answer_chars = trimmed.chars().count();
    if answer_chars < SINGLE_SHOT_MIN_ANSWER_CHARS {
        return false;
    }
    if answer_needs_literal_revision(verification) {
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
    if let Some(min_chars) = source_slice_single_shot_min_chars(query_ir)
        && answer_chars < min_chars
    {
        return false;
    }
    if answer_omits_expected_technical_literals(trimmed, query_ir, grounding_context) {
        return false;
    }
    if single_shot_lacks_query_focus_support(trimmed, query_ir, grounding_context) {
        return false;
    }
    true
}

fn single_shot_context_lacks_query_focus_support(
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !query_requires_single_shot_focus_support(query_ir) {
        return false;
    }
    let focus_segments = query_focus_support_segments(query_ir);
    if focus_segments.is_empty() {
        return false;
    }
    let context_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(grounding_context, 4);
    !focus_segments
        .iter()
        .any(|segment| focus_segment_supported_by_tokens(segment, &context_tokens))
}

fn single_shot_lacks_query_focus_support(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !query_requires_single_shot_focus_support(query_ir) {
        return false;
    }
    let focus_segments = query_focus_support_segments(query_ir);
    if focus_segments.is_empty() {
        return false;
    }
    let context_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(grounding_context, 4);
    let answer_tokens = crate::services::query::text_match::normalized_alnum_tokens(answer, 4);
    let supported_segments = focus_segments
        .iter()
        .filter(|segment| focus_segment_supported_by_tokens(segment, &context_tokens))
        .collect::<Vec<_>>();
    if supported_segments.is_empty() {
        return true;
    }
    !supported_segments
        .iter()
        .any(|segment| focus_segment_supported_by_tokens(segment, &answer_tokens))
}

fn query_requires_single_shot_focus_support(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::RetrieveValue) {
        return false;
    }
    let intent = detect_technical_literal_intent_from_query_ir("", query_ir);
    intent.any() || query_ir.document_focus.is_some() || !query_ir.literal_constraints.is_empty()
}

fn query_focus_support_segments(query_ir: &QueryIR) -> Vec<BTreeSet<String>> {
    let mut segments = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject | EntityRole::Object))
        .filter_map(|entity| focus_support_tokens(&entity.label))
        .collect::<Vec<_>>();
    if segments.is_empty() {
        if let Some(document_focus) = &query_ir.document_focus
            && let Some(tokens) = focus_support_tokens(&document_focus.hint)
        {
            segments.push(tokens);
        }
    }
    for literal in &query_ir.literal_constraints {
        if let Some(tokens) = focus_support_tokens(&literal.text) {
            segments.push(tokens);
        }
    }
    segments
}

fn focus_support_tokens(value: &str) -> Option<BTreeSet<String>> {
    let tokens = crate::services::query::text_match::normalized_alnum_tokens(value, 4);
    (!tokens.is_empty()).then_some(tokens)
}

fn focus_segment_supported_by_tokens(
    segment_tokens: &BTreeSet<String>,
    available_tokens: &BTreeSet<String>,
) -> bool {
    if segment_tokens.is_empty() {
        return false;
    }
    crate::services::query::text_match::near_token_overlap_count(segment_tokens, available_tokens)
        >= focus_segment_required_overlap(segment_tokens)
}

fn focus_segment_required_overlap(segment_tokens: &BTreeSet<String>) -> usize {
    if segment_tokens.len() <= 2 { 1 } else { 2 }
}

fn answer_omits_expected_technical_literals(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    let intent = detect_technical_literal_intent_from_query_ir("", query_ir);
    if !intent.any() {
        return false;
    }
    let context_literals = collect_intended_technical_literals(grounding_context, intent, 8);
    if context_literals.is_empty() {
        return false;
    }
    collect_intended_technical_literals(answer, intent, 2).is_empty()
}

fn collect_intended_technical_literals(
    text: &str,
    intent: TechnicalLiteralIntent,
    limit: usize,
) -> HashSet<String> {
    let mut literals = HashSet::<String>::new();
    if intent.wants_urls {
        literals.extend(extract_url_literals(text, limit));
    }
    if intent.wants_prefixes {
        literals.extend(extract_prefix_literals(text, limit));
    }
    if intent.wants_paths {
        literals.extend(extract_explicit_path_literals(text, limit));
    }
    if intent.wants_methods {
        literals.extend(extract_http_methods(text, limit));
    }
    if intent.wants_parameters {
        literals.extend(extract_parameter_literals(text, limit));
    }
    literals
}

fn answer_needs_literal_revision(verification: &AnswerVerificationStage) -> bool {
    verification.verification.warnings.iter().any(|warning| {
        warning.code == "unsupported_literal" || warning.code == "unsupported_canonical_claim"
    })
}

fn literal_revision_context(
    prompt_context: &str,
    assistant_grounding: &crate::services::query::assistant_grounding::AssistantGroundingEvidence,
) -> String {
    let mut context = prompt_context.trim().to_string();
    if assistant_grounding.verification_corpus.is_empty() {
        return context;
    }
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    context.push_str("Additional tool evidence observed by the answer generator:\n");
    for (index, fragment) in assistant_grounding.verification_corpus.iter().enumerate() {
        let trimmed = fragment.trim();
        if trimmed.is_empty() {
            continue;
        }
        context.push_str(&format!("\n[TOOL_EVIDENCE {}]\n{}\n", index + 1, trimmed));
    }
    context
}

fn merge_generation_usage(
    mut primary: serde_json::Value,
    additional: &serde_json::Value,
) -> serde_json::Value {
    crate::services::query::agent_loop::merge_usage_into(&mut primary, additional);
    primary
}

fn source_slice_single_shot_min_chars(query_ir: &QueryIR) -> Option<usize> {
    let requested = super::source_slice_requested_count(query_ir)?;
    Some((requested.saturating_mul(48)).max(SINGLE_SHOT_CONFIDENT_ANSWER_CHARS))
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
    let has_unsupported_canonical_claim =
        verification.warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim");
    let verifier_tripped = has_hallucinated_literal || has_unsupported_canonical_claim;

    // Verifier warnings are surfaced as metadata; the grounded answer body
    // is not replaced by a static fallback.
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

    Ok(AnswerVerificationStage { generation, verification })
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        AnswerDisposition, append_source_section, classify_answer_disposition,
        classify_answer_disposition_from_evidence, classify_answer_disposition_from_groups,
    };
    use crate::domains::query::{GroupedReference, GroupedReferenceKind};
    use crate::domains::query_ir::{
        ClarificationReason, ClarificationSpec, ComparisonSpec, ConversationRefKind, EntityMention,
        EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope, UnresolvedRef,
    };
    use crate::services::query::execution::{ConsolidationDiagnostics, FocusReason};

    fn sample_ir(confidence: f32, needs_clarification: Option<ClarificationReason>) -> QueryIR {
        sample_ir_with_act(QueryAct::ConfigureHow, confidence, needs_clarification)
    }

    fn sample_ir_with_act(
        act: QueryAct,
        confidence: f32,
        needs_clarification: Option<ClarificationReason>,
    ) -> QueryIR {
        QueryIR {
            act,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Ru,
            target_types: vec!["procedure".to_string()],
            target_entities: vec![EntityMention {
                label: "payment module".to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: needs_clarification
                .map(|reason| ClarificationSpec { reason, suggestion: String::new() }),
            source_slice: None,
            confidence,
        }
    }

    fn sample_ir_with_two_target_entities(
        act: QueryAct,
        confidence: f32,
        needs_clarification: Option<ClarificationReason>,
    ) -> QueryIR {
        let mut ir = sample_ir_with_act(act, confidence, needs_clarification);
        ir.target_entities = vec![
            EntityMention { label: "Alpha Suite".to_string(), role: EntityRole::Subject },
            EntityMention { label: "payments".to_string(), role: EntityRole::Object },
        ];
        ir
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

    fn prepared_for_single_shot(query_ir: QueryIR) -> super::PreparedAnswerQueryResult {
        use crate::domains::query::{
            ContextAssemblyMetadata, ContextAssemblyStatus, IntentKeywords, QueryIntentCacheStatus,
            QueryPlanningMetadata, RerankMetadata, RerankStatus, RuntimeQueryMode,
        };

        super::PreparedAnswerQueryResult {
            structured: super::super::types::RuntimeStructuredQueryResult {
                planned_mode: RuntimeQueryMode::Hybrid,
                embedding_usage: None,
                intent_profile: Default::default(),
                context_text: "context-fragment-a".to_string(),
                technical_literals_text: None,
                technical_literal_chunks: Vec::new(),
                diagnostics: super::super::types::RuntimeStructuredQueryDiagnostics {
                    requested_mode: RuntimeQueryMode::Hybrid,
                    planned_mode: RuntimeQueryMode::Hybrid,
                    keywords: Vec::new(),
                    high_level_keywords: Vec::new(),
                    low_level_keywords: Vec::new(),
                    top_k: 8,
                    reference_counts: super::super::types::RuntimeStructuredQueryReferenceCounts {
                        entity_count: 0,
                        relationship_count: 0,
                        chunk_count: 1,
                        graph_node_count: 0,
                        graph_edge_count: 0,
                    },
                    planning: QueryPlanningMetadata {
                        requested_mode: RuntimeQueryMode::Hybrid,
                        planned_mode: RuntimeQueryMode::Hybrid,
                        intent_cache_status: QueryIntentCacheStatus::Miss,
                        keywords: IntentKeywords::default(),
                        warnings: Vec::new(),
                    },
                    rerank: RerankMetadata {
                        status: RerankStatus::NotApplicable,
                        candidate_count: 1,
                        reordered_count: None,
                    },
                    context_assembly: ContextAssemblyMetadata {
                        status: ContextAssemblyStatus::DocumentOnly,
                        warning: None,
                    },
                    grouped_references: Vec::new(),
                    context_text: None,
                    warning: None,
                    warning_kind: None,
                    library_summary: None,
                },
                retrieved_documents: vec![super::RuntimeRetrievedDocumentBrief {
                    title: "document-a".to_string(),
                    preview_excerpt: "context-fragment-a".to_string(),
                    source_uri: None,
                }],
                retrieved_context_document_titles: vec!["document-a".to_string()],
                chunk_references: Vec::new(),
                context_chunks: Vec::new(),
                ordered_source_units: Vec::new(),
                graph_evidence_context_lines: Vec::new(),
            },
            answer_context: "context-fragment-a".to_string(),
            embedding_usage: None,
            consolidation: ConsolidationDiagnostics::noop(),
            query_ir,
            query_compile_usage: None,
        }
    }

    #[test]
    fn latest_version_enumeration_uses_single_shot_when_context_is_prepared() {
        let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.8, None);
        ir.target_types = vec!["release".to_string(), "version".to_string()];
        ir.literal_constraints.push(crate::domains::query_ir::LiteralSpan {
            text: "5".to_string(),
            kind: crate::domains::query_ir::LiteralKind::NumericCode,
        });
        let prepared = prepared_for_single_shot(ir);

        assert!(
            super::should_use_single_shot_answer("q", &prepared, None),
            "latest-version retrieval already prepares grounded context and must not force a second retrieval pass"
        );
    }

    #[test]
    fn disposition_answers_when_consolidation_committed_focused_document() {
        let mut ir = sample_ir(
            0.74,
            Some(crate::domains::query_ir::ClarificationReason::MultipleInterpretations),
        );
        ir.target_entities =
            vec![EntityMention { label: "return process".to_string(), role: EntityRole::Subject }];
        let mut prepared = prepared_for_single_shot(ir);
        prepared.structured.retrieved_context_document_titles = vec![
            "Return process".to_string(),
            "Return process: attachment.png".to_string(),
            "Adjacent return workflow".to_string(),
        ];
        prepared.structured.retrieved_documents = vec![
            retrieved_doc("Return process", "source://return-process"),
            retrieved_doc("Return process: attachment.png", "source://return-attachment"),
            retrieved_doc("Adjacent return workflow", "source://adjacent-return"),
        ];
        prepared.structured.diagnostics.grouped_references = sample_groups();
        prepared.consolidation = ConsolidationDiagnostics {
            focused_document_id: Some(Uuid::now_v7()),
            focus_reason: FocusReason::ScoreDominance,
            winner_chunk_count: 1,
            tangential_chunk_count: 5,
        };

        let disposition = classify_answer_disposition(&prepared, "How do I complete return?");

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "focused-document consolidation means retained tangentials must not force clarify"
        );
    }

    #[test]
    fn stateless_conversation_refs_can_use_initial_fast_path() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.55, None);
        ir.conversation_refs.push(UnresolvedRef {
            surface: "source-local anchor".to_string(),
            kind: ConversationRefKind::Deictic,
        });
        let prepared = prepared_for_single_shot(ir);

        assert!(
            super::should_use_single_shot_answer("q", &prepared, None),
            "without prior conversation, unresolved refs cannot be resolved by another retrieval pass and prepared context should answer or refuse directly"
        );
    }

    #[test]
    fn conversation_refs_with_history_wait_for_canonical_preflight() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.55, None);
        ir.conversation_refs.push(UnresolvedRef {
            surface: "that topic".to_string(),
            kind: ConversationRefKind::Deictic,
        });
        let prepared = prepared_for_single_shot(ir);

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, Some("user: earlier topic")),
            "real prior conversation should skip only the initial fast path; canonical preflight still answers from fixed evidence"
        );
    }

    #[test]
    fn literal_free_answer_is_not_acceptable_when_context_has_requested_technical_literals() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types = vec!["path".to_string(), "config_key".to_string()];
        let context = "Use `/srv/scans`, then set scan_path = /srv/scans in the share block.";

        assert!(super::answer_omits_expected_technical_literals(
            "The provided context does not include setup details.",
            &ir,
            context,
        ));
        assert!(super::answer_omits_expected_technical_literals(
            "The provided context does not include setup details.",
            &ir,
            "Use `/srv/scans` for the scan directory.",
        ));
        assert!(!super::answer_omits_expected_technical_literals(
            "Use `/srv/scans` for the scan directory.",
            &ir,
            context,
        ));
    }

    #[test]
    fn targeted_technical_query_without_focus_context_skips_initial_fast_path() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types = vec!["path".to_string(), "config_key".to_string()];
        ir.target_entities = vec![
            EntityMention {
                label: "RareProtocol scan share".to_string(),
                role: EntityRole::Subject,
            },
            EntityMention { label: "RareProtocol daemon".to_string(), role: EntityRole::Object },
        ];
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context =
            "[EVIDENCE_CHUNK document=\"nearby\"] Alpha Suite setup uses `/srv/scans`.".to_string();

        assert!(
            !super::should_use_single_shot_answer(
                "How do I configure the RareProtocol scan share?",
                &prepared,
                None,
            ),
            "technical initial single-shot needs focus evidence before it can finalize without preflight"
        );
    }

    #[test]
    fn targeted_technical_single_shot_answer_needs_context_backed_focus() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types = vec!["path".to_string(), "config_key".to_string()];
        ir.target_entities = vec![
            EntityMention {
                label: "RareProtocol scan share".to_string(),
                role: EntityRole::Subject,
            },
            EntityMention { label: "RareProtocol daemon".to_string(), role: EntityRole::Object },
        ];
        let unrelated_context =
            "[EVIDENCE_CHUNK document=\"nearby\"] Alpha Suite setup uses `/srv/scans`.";
        let focused_context =
            "[EVIDENCE_CHUNK document=\"target\"] RareProtocol daemon setup uses `/srv/scans`.";

        assert!(super::single_shot_lacks_query_focus_support(
            "The context has no RareProtocol details, but mentions `/srv/scans`.",
            &ir,
            unrelated_context,
        ));
        assert!(super::single_shot_lacks_query_focus_support(
            "Use `/srv/scans` for the scan directory.",
            &ir,
            focused_context,
        ));
        assert!(!super::single_shot_lacks_query_focus_support(
            "RareProtocol daemon uses `/srv/scans` for the scan directory.",
            &ir,
            focused_context,
        ));
    }

    #[test]
    fn compare_without_structural_operands_skips_initial_fast_path() {
        let prepared = prepared_for_single_shot(sample_ir_with_act(QueryAct::Compare, 0.8, None));

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, None),
            "compare questions without covered operands should wait for canonical preflight evidence"
        );
    }

    #[test]
    fn compare_uses_single_shot_when_prepared_context_covers_operands() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        ir.comparison = Some(ComparisonSpec {
            a: Some("Alpha Suite".to_string()),
            b: Some("Beta Suite".to_string()),
            dimension: "capability".to_string(),
        });
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context = "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"alpha\"] Alpha Suite stores audit events.\n\
[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"beta\"] Beta Suite stores billing events."
            .to_string();

        assert!(
            super::should_use_single_shot_answer("q", &prepared, None),
            "compare can use the prepared single-shot context when every IR operand is covered"
        );
    }

    #[test]
    fn compare_uses_single_shot_when_prepared_context_partially_covers_operands() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        ir.comparison = Some(ComparisonSpec {
            a: Some("Alpha Suite".to_string()),
            b: Some("Beta Suite".to_string()),
            dimension: "capability".to_string(),
        });
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context =
            "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"alpha\"] Alpha Suite stores audit events."
                .to_string();

        assert!(
            super::should_use_single_shot_answer("q", &prepared, None),
            "one-sided compare evidence should produce a fast grounded partial answer instead of a second retrieval pass"
        );
    }

    #[test]
    fn compare_skips_initial_fast_path_when_no_operand_is_covered() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        ir.comparison = Some(ComparisonSpec {
            a: Some("Alpha Suite".to_string()),
            b: Some("Beta Suite".to_string()),
            dimension: "capability".to_string(),
        });
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context =
            "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"gamma\"] Gamma Console stores audit events."
                .to_string();

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, None),
            "compare with zero covered operands should not use the initial fast path"
        );
    }

    #[test]
    fn comparison_coverage_metadata_does_not_count_as_grounding_evidence() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        ir.comparison = Some(ComparisonSpec {
            a: Some("Alpha Suite".to_string()),
            b: Some("Beta Suite".to_string()),
            dimension: "capability".to_string(),
        });
        let coverage = super::compare_operands_covered_by_context(
            &ir,
            "COMPARISON_COVERAGE status=partial\n\
- covered_operand: Alpha Suite\n\
- uncovered_operand: Beta Suite",
        );

        assert!(
            matches!(coverage, super::EvidenceCoverage::Insufficient("compare_evidence_empty")),
            "internal coverage markers must not become synthetic evidence"
        );
    }

    #[test]
    fn append_source_section_skips_when_answer_already_has_http_citations() {
        let answer = "See [relevant doc](https://example.test/relevant) for setup.".to_string();
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
            "Configure the module via the configuration file.".to_string(),
            &[retrieved_doc("Config guide", "https://example.test/config")],
            QueryLanguage::Ru,
        );

        assert!(rendered.contains("Sources:"));
        assert!(rendered.contains("https://example.test/config"));
    }

    #[test]
    fn append_source_section_does_not_treat_config_url_literal_as_citation() {
        let rendered = append_source_section(
            "The url parameter is set as `https://<localhost>/api`.".to_string(),
            &[retrieved_doc("Config guide", "https://example.test/config")],
            QueryLanguage::Ru,
        );

        assert!(rendered.contains("Sources:"));
        assert!(rendered.contains("https://example.test/config"));
    }

    #[test]
    fn disposition_keeps_confident_ir_on_answer_path() {
        let disposition = classify_answer_disposition_from_groups(
            "how do i configure target payments?",
            &sample_ir(0.9, None),
            &[],
            &sample_groups(),
        );

        assert!(matches!(disposition, AnswerDisposition::Answer));
    }

    #[test]
    fn disposition_keeps_low_confidence_ir_on_answer_path_without_explicit_reason() {
        let disposition = classify_answer_disposition_from_groups(
            "how do i configure target payments?",
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
    fn disposition_allows_explicit_compiler_clarify_with_multiple_target_entities() {
        let ir = sample_ir_with_two_target_entities(
            QueryAct::RetrieveValue,
            0.4,
            Some(ClarificationReason::AmbiguousTooShort),
        );

        let disposition = classify_answer_disposition_from_groups(
            "provider configuration",
            &ir,
            &[],
            &sample_groups(),
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants.len(), 3);
                assert_eq!(variants[0], "Provider A configuration");
            }
            AnswerDisposition::Answer => {
                panic!("explicit compiler clarification must bypass multi-entity specificity")
            }
        }
    }

    #[test]
    fn disposition_can_structurally_clarify_configure_without_compiler_reason() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "PaymentLink Provider Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "PaymentLink Provider Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "PaymentLink Provider Gamma Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how configure paymentlink?",
            &sample_ir(0.6, None),
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
                        "PaymentLink Provider Gamma Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected structural clarify without compiler clarification")
            }
        }
    }

    #[test]
    fn disposition_can_structurally_clarify_terse_followup_without_compiler_reason() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "PaymentLink Provider Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "PaymentLink Provider Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "PaymentLink Provider Gamma Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "paymentlink platform",
            &sample_ir_with_two_target_entities(QueryAct::RetrieveValue, 0.6, None),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants.len(), 3);
            }
            AnswerDisposition::Answer => {
                panic!("expected terse query-aligned follow-up to clarify")
            }
        }
    }

    #[test]
    fn disposition_does_not_structurally_clarify_describe_without_compiler_reason() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "API Gateway Alpha".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "API Gateway Beta".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "API Gateway Gamma".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "what is api?",
            &sample_ir_with_act(QueryAct::Describe, 0.6, None),
            &[],
            &groups,
        );

        assert!(matches!(disposition, AnswerDisposition::Answer));
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
    fn disposition_uses_discriminating_topic_token_over_shared_product_tokens() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Platform Pay Provider Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Platform Inventory Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Platform Pay Provider Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "platform pay",
            &sample_ir_with_act(
                QueryAct::RetrieveValue,
                0.4,
                Some(ClarificationReason::AmbiguousTooShort),
            ),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "Platform Pay Provider Alpha Manual".to_string(),
                        "Platform Pay Provider Beta Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected discriminating topic token to filter shared product labels")
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
    fn disposition_uses_final_context_titles_when_briefs_and_groups_are_truncated() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "PaymentLink Provider Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 6,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "PaymentLink Provider Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:2".to_string()],
            },
        ];
        let context_titles = vec![
            "PaymentLink Provider Alpha Manual".to_string(),
            "PaymentLink Provider Beta Manual".to_string(),
            "PaymentLink Provider Gamma Manual".to_string(),
            "PaymentLink Provider Delta Manual".to_string(),
        ];

        let disposition = classify_answer_disposition_from_evidence(
            "how configure paymentlink?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &context_titles,
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants, context_titles);
            }
            AnswerDisposition::Answer => {
                panic!("expected final context titles to preserve the variant menu")
            }
        }
    }

    #[test]
    fn disposition_answers_when_final_context_has_one_focused_document() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Container Return Procedure".to_string(),
                excerpt: None,
                evidence_count: 4,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "node:1".to_string(),
                kind: GroupedReferenceKind::Entity,
                rank: 2,
                title: "return document".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["node:1".to_string()],
            },
        ];
        let retrieved_documents =
            vec![crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
                title: "Container Return Procedure".to_string(),
                preview_excerpt: String::new(),
                source_uri: None,
            }];
        let context_titles = vec!["Container Return Procedure".to_string()];

        let disposition = classify_answer_disposition_from_evidence(
            "how do i process container return?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &retrieved_documents,
            &context_titles,
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "single focused document evidence should answer instead of clarifying on graph labels"
        );
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
    fn disposition_ignores_question_word_substrings_in_variant_labels() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Branch Director".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Fruit Notes".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Operations Handbook".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "who is TargetName?",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "question words must not match as substrings inside unrelated labels"
        );
    }

    #[test]
    fn disposition_keeps_short_acronym_variants_on_exact_token_match() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "API Gateway Alpha".to_string(),
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
                title: "API Gateway Beta".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how configure api",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec!["API Gateway Alpha".to_string(), "API Gateway Beta".to_string()]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected exact short acronym matches to remain valid variants")
            }
        }
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
