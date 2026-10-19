use std::collections::HashMap;

use crate::infra::knowledge_rows::KnowledgeDocumentRow;

use super::super::types::{
    StructuredQueryPlanningStage, StructuredQueryRerankStage, StructuredQueryRetrievalStage,
};
use super::super::{CompiledIrPlanRefresh, ResolvedRetrievalReplanRequest};
use super::context_probes::{
    EvidenceProbeContext, augment_partial_compare_context, augment_technical_focus_context,
};
use super::literal_revision::hydrate_runtime_document_index;
use super::{
    AnswerVerificationStage, ApiError, AppState, CompileHistoryTurn, CompileQueryCommand,
    ExternalConversationTurn, HashSet, PreparedAnswerQueryResult, QueryCompilerService, QueryIR,
    RuntimeAnswerQueryResult, RuntimeMatchedChunk, SemanticRerankExecutionContext, Uuid,
    apply_query_execution_library_summary, apply_query_execution_warning, assemble_answer_context,
    load_query_execution_library_context, provider_free_fallback_query_ir,
    query_requests_latest_versions, should_prioritize_retrieved_context_for_query,
};
use anyhow::Context as _;

use self::logging::{
    log_canonical_head_filter, log_compare_context_probe, log_compile_provider_fallback,
    log_library_context_skipped, log_plan_done, log_prepare_stages, log_rerank_done,
    log_retrieve_done, log_structured_source_context, log_technical_focus_probe, log_topical_prune,
};

mod logging;

pub(super) const COMPARE_OPERAND_PROBE_LIMIT: usize = 8;
pub(super) const COMPARE_OPERAND_PROBE_MAX_CHUNKS: usize = 6;
pub(super) const COMPARE_OPERAND_PROBE_MAX_CHUNKS_PER_OPERAND: usize = 2;
pub(super) const TECHNICAL_FOCUS_PROBE_TERM_LIMIT: usize = 8;
pub(super) const TECHNICAL_FOCUS_PROBE_HIT_LIMIT: usize = 10;
pub(super) const TECHNICAL_FOCUS_PROBE_MAX_CHUNKS: usize = 8;
pub(super) const TECHNICAL_FOCUS_PROBE_MAX_CHUNKS_PER_TERM: usize = 2;
pub(super) const STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHORS: usize = 4;
pub(super) const STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHOR_LINES: usize = 2;
pub(super) const STRUCTURAL_COVERAGE_MIN_ANSWER_ANCHORS: usize = 2;
pub(super) const STRUCTURAL_COVERAGE_MAX_ANCHORS: usize = 64;
pub(super) const STRUCTURAL_COVERAGE_MAX_ANCHOR_CHARS: usize = 80;
pub(super) const STRUCTURAL_COVERAGE_FOCUS_MIN_TOKEN_CHARS: usize = 3;
pub(super) const STRUCTURAL_QUESTION_TOKEN_MAX_CHARS: usize = 80;

pub(super) struct PreflightAnswerCandidate {
    pub(super) verification_stage: AnswerVerificationStage,
    pub(super) debug_iterations: Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
    pub(super) total_iterations: usize,
}

pub(super) enum FastPathOutcome {
    Accepted(RuntimeAnswerQueryResult),
    Escalate { candidate: Box<Option<PreflightAnswerCandidate>>, attempted_answer_generation: bool },
}

pub(super) async fn persist_llm_context_snapshot(
    state: &AppState,
    snapshot: crate::services::query::llm_context_debug::LlmContextSnapshot,
) -> anyhow::Result<()> {
    crate::services::query::llm_context_debug::upsert_snapshot(
        &state.persistence.postgres,
        &snapshot,
    )
    .await
    .with_context(|| format!("failed to persist LLM context snapshot {}", snapshot.execution_id))
}

/// Inputs for [`prepare_answer_query`]. The compile stage needs the full turn
/// context (library, question, history, mode and top-k) at once, so it travels
/// as one value instead of eight loose arguments.
pub(crate) struct PrepareAnswerQueryRequest<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) library_id: Uuid,
    pub(crate) semantic_rerank_context: SemanticRerankExecutionContext,
    pub(crate) question: String,
    pub(crate) conversation_history: &'a [ExternalConversationTurn],
    pub(crate) mode: crate::domains::query::RuntimeQueryMode,
    pub(crate) top_k: usize,
    pub(crate) include_debug: bool,
}

pub(crate) async fn prepare_answer_query(
    request: PrepareAnswerQueryRequest<'_>,
) -> anyhow::Result<PreparedAnswerQueryResult> {
    // Capture fine-grained timed spans (DB queries, retrieval lanes) recorded
    // during preparation so the debug inspector can show where time went. The
    // sink propagates across the same-task parallelism used below.
    let (result, spans) = Box::pin(crate::services::query::turn_spans::capture_turn_spans(
        prepare_answer_query_inner(request),
    ))
    .await;
    let mut prepared = result?;
    prepared.retrieval_spans = spans;
    Ok(prepared)
}

pub(super) async fn apply_bundle_temporal_filter(
    state: &AppState,
    library_id: Uuid,
    query_ir: &QueryIR,
    chunks: &mut Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<()> {
    let (temporal_start, temporal_end) = query_ir.resolved_temporal_bounds();
    let (Some(temporal_start), Some(temporal_end)) = (temporal_start, temporal_end) else {
        return Ok(());
    };
    if chunks.is_empty() {
        return Ok(());
    }
    let chunk_ids = chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
    let rows = state.document_store.list_chunks_by_ids(&chunk_ids).await.map_err(|error| {
        anyhow::anyhow!("failed to look up chunks for bundle-temporal post-filter: {error}")
    })?;
    let allowed = rows
        .into_iter()
        .filter(|row| row_matches_temporal_bounds(row, temporal_start, temporal_end))
        .map(|row| row.chunk_id)
        .collect::<HashSet<_>>();
    let before = chunks.len();
    chunks.retain(|chunk| allowed.contains(&chunk.chunk_id));
    tracing::info!(
        stage = "answer.bundle_temporal_post_filter",
        library_id = %library_id,
        before,
        after = chunks.len(),
        "applied temporal hard-filter to bundle (post source-context)"
    );
    Ok(())
}

pub(super) fn row_matches_temporal_bounds(
    row: &crate::infra::knowledge_rows::KnowledgeChunkRow,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
) -> bool {
    row.occurred_at.is_some_and(|occurred_at| {
        row.occurred_until.unwrap_or(occurred_at) >= start && occurred_at < end
    })
}

/// The compile+plan stage output: the compiled IR, the (possibly replanned)
/// planning stage, the standalone retrieval query and the plan timing.
struct CompiledPlan {
    query_ir: QueryIR,
    planning_stage: StructuredQueryPlanningStage,
    retrieval_question: String,
    plan_elapsed_ms: u128,
}

/// The reranked bundle after consolidation, source-context augmentation and
/// finalize, together with the diagnostics the caller logs in one line.
struct FinalizedBundle {
    structured: super::super::RuntimeStructuredQueryResult,
    document_index: HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: Vec<String>,
    consolidation_elapsed_ms: u128,
    consolidation_reason: &'static str,
    consolidation_winner_chunks: usize,
    consolidation_tangential_chunks: usize,
    topical_pruned_chunks: usize,
}

pub(super) async fn prepare_answer_query_inner(
    request: PrepareAnswerQueryRequest<'_>,
) -> anyhow::Result<PreparedAnswerQueryResult> {
    // Stage 1: compile + planning run in parallel, then retrieval waits for
    // the compiled IR. This overlaps planning/embedding while retrieval consumes
    // `document_focus`, scope, and subject entities on the first pass.
    let stage_1_started = std::time::Instant::now();
    let CompiledPlan { query_ir, planning_stage, retrieval_question, plan_elapsed_ms } =
        Box::pin(compile_and_plan(&request)).await?;
    log_plan_done(request.library_id, plan_elapsed_ms, &planning_stage);
    let retrieve_started = std::time::Instant::now();
    let retrieval_stage =
        run_structured_retrieval(&request, planning_stage, &retrieval_question, &query_ir).await?;
    log_retrieve_done(request.library_id, retrieve_started.elapsed().as_millis());
    let rerank_started = std::time::Instant::now();
    let rerank_stage =
        run_structured_rerank(&request, retrieval_stage, &retrieval_question).await?;
    log_rerank_done(request.library_id, rerank_started.elapsed().as_millis());
    let stage_1_elapsed_ms = stage_1_started.elapsed().as_millis();
    let mut finalized = finalize_retrieved_bundle(&request, &query_ir, rerank_stage).await?;
    let stage_2_started = std::time::Instant::now();
    let answer_context = assemble_prepared_answer_context(
        &request,
        &query_ir,
        &mut finalized.structured,
        &finalized.document_index,
        &finalized.plan_keywords,
    )
    .await?;
    let stage_2_elapsed_ms = stage_2_started.elapsed().as_millis();

    log_prepare_stages(
        request.library_id,
        &finalized,
        &answer_context,
        &query_ir,
        stage_1_elapsed_ms,
        stage_2_elapsed_ms,
    );

    Ok(PreparedAnswerQueryResult {
        structured: finalized.structured,
        answer_context,
        query_ir,
        // Filled in by the `prepare_answer_query` wrapper after draining the
        // span sink scoped around this call. The sink is drained exactly once,
        // here, and the spans then live on this value for the rest of the
        // execution.
        retrieval_spans: Vec::new(),
    })
}

/// Runs the NL->IR compile and the structured planning in parallel, then
/// reconciles the plan with the compiler-resolved retrieval query.
async fn compile_and_plan(request: &PrepareAnswerQueryRequest<'_>) -> anyhow::Result<CompiledPlan> {
    let state = request.state;
    let library_id = request.library_id;
    let provider_execution_context =
        crate::services::query::provider_billing::QueryProviderExecutionContext {
            workspace_id: request.semantic_rerank_context.workspace_id,
            library_id,
            query_execution_id: request.semantic_rerank_context.query_execution_id,
            runtime_execution_id: request.semantic_rerank_context.runtime_execution_id,
        };
    let compile_future = compile_query_ir(
        state,
        provider_execution_context,
        &request.question,
        request.conversation_history,
    );
    let plan_started = std::time::Instant::now();
    let planning_future = crate::agent_runtime::pipeline::try_op::run_async_try_op((), |_| {
        super::super::plan_structured_query(
            state,
            provider_execution_context,
            &request.question,
            request.mode,
            request.top_k,
        )
    });
    let (compile_result, planning_result) = tokio::join!(compile_future, planning_future);
    let plan_elapsed_ms = plan_started.elapsed().as_millis();
    let query_ir = compile_result?;
    let mut planning_stage = planning_result?;
    // For elliptic follow-ups the compiler folds the recovered subject/scope
    // into `retrieval_query`; when that resolved string differs from the raw
    // question we re-derive the question-dependent plan/embeddings so the
    // retrieval lanes search the standalone query instead of the bare
    // fragment. The expensive question-independent graph/document indexes are
    // reused, and the common path (resolved == raw) is byte-identical.
    let retrieval_question = query_ir.effective_retrieval_query(&request.question).to_string();
    if retrieval_question != request.question {
        tracing::info!(
            stage = "answer.resolved_retrieval_query",
            library_id = %library_id,
            "compiler emitted a resolved standalone retrieval query for an elliptic follow-up"
        );
        super::super::replan_for_resolved_retrieval_query(ResolvedRetrievalReplanRequest {
            state,
            execution_context: provider_execution_context,
            planning: &mut planning_stage,
            resolved_query: &retrieval_question,
            mode: request.mode,
            top_k: request.top_k,
            query_ir: &query_ir,
        })
        .await?;
    } else {
        super::super::refresh_query_plan_for_compiled_ir(
            state,
            provider_execution_context,
            CompiledIrPlanRefresh {
                planning: &mut planning_stage,
                retrieval_question: &retrieval_question,
                mode: request.mode,
                top_k: request.top_k,
                query_ir: &query_ir,
                rerank_enabled: state.retrieval_intelligence.rerank_enabled,
                rerank_candidate_limit: state.retrieval_intelligence.rerank_candidate_limit,
            },
        )
        .await?;
    }
    Ok(CompiledPlan { query_ir, planning_stage, retrieval_question, plan_elapsed_ms })
}

/// Runs the structured retrieval lanes on the compiler-resolved query.
async fn run_structured_retrieval(
    request: &PrepareAnswerQueryRequest<'_>,
    planning_stage: StructuredQueryPlanningStage,
    retrieval_question: &str,
    query_ir: &QueryIR,
) -> anyhow::Result<StructuredQueryRetrievalStage> {
    let state = request.state;
    let library_id = request.library_id;
    let query_ir_for_retrieval = query_ir.clone();
    Box::pin(crate::agent_runtime::pipeline::try_op::run_async_try_op(
        planning_stage,
        |planning_stage| {
            let query_ir = query_ir_for_retrieval.clone();
            let question = retrieval_question.to_string();
            async move {
                Box::pin(super::super::retrieve_structured_query(
                    state,
                    library_id,
                    &question,
                    planning_stage,
                    Some(&query_ir),
                ))
                .await
            }
        },
    ))
    .await
}

/// Reranks the same standalone query used by every retrieval lane. Using the
/// terse raw follow-up here (for example, "and its timeout?") discards the
/// compiler-resolved subject exactly at the final ordering boundary.
async fn run_structured_rerank(
    request: &PrepareAnswerQueryRequest<'_>,
    retrieval_stage: StructuredQueryRetrievalStage,
    retrieval_question: &str,
) -> anyhow::Result<StructuredQueryRerankStage> {
    let state = request.state;
    let library_id = request.library_id;
    let semantic_rerank_context = request.semantic_rerank_context;
    let rerank_question = retrieval_question.to_string();
    Box::pin(crate::agent_runtime::pipeline::try_op::run_async_try_op(
        retrieval_stage,
        |retrieval_stage| {
            let question = rerank_question.clone();
            async move {
                super::super::rerank_structured_query(
                    state,
                    library_id,
                    semantic_rerank_context,
                    &question,
                    retrieval_stage,
                )
                .await
            }
        },
    ))
    .await
}

/// Consolidates and augments the reranked bundle, then finalizes it into the
/// structured query result and returns it with the diagnostics the caller logs.
async fn finalize_retrieved_bundle(
    request: &PrepareAnswerQueryRequest<'_>,
    query_ir: &QueryIR,
    mut rerank_stage: StructuredQueryRerankStage,
) -> anyhow::Result<FinalizedBundle> {
    let state = request.state;
    let library_id = request.library_id;
    let question = request.question.as_str();
    let mut document_index = rerank_stage.retrieval.planning.document_index.clone();
    // The inventory must read the DOCUMENT, not the retrieved subset: a
    // declaration the question never names is exactly the one retrieval did
    // not bring back (measured — the target declaration sits mid-file and the
    // retrieved bundle held only head chunks, so the harvest had nothing to
    // find). Load the dominant document's own chunk rows, bounded, and fall
    // back to the retrieved bundle when the load fails or nothing dominates.
    let declaration_inventory_candidates =
        declaration_inventory_candidates(state, query_ir, &rerank_stage.retrieval.bundle.chunks)
            .await;
    // IR-aware consolidation: if the compiler pinned the question to
    // one document (explicit hint / single-doc subject) or the
    // retrieval itself shows one document dominating the evidence,
    // reallocate the top_k slot budget to pack contiguous neighbours
    // of that winner instead of keeping 7 tangentials + 1 winning intro.
    let consolidation_started = std::time::Instant::now();
    let consolidation = super::super::focused_document_consolidation(
        state,
        &mut rerank_stage.retrieval.bundle,
        query_ir,
        question,
        request.top_k,
    )
    .await;
    let consolidation_elapsed_ms = consolidation_started.elapsed().as_millis();
    let plan_keywords = rerank_stage.retrieval.planning.plan.keywords.clone();
    log_canonical_head_filter(
        library_id,
        super::super::retain_current_document_head_chunks(
            &mut rerank_stage.retrieval.bundle.chunks,
            &document_index,
        ),
        "focused-document consolidation",
    );
    let source_context_started = std::time::Instant::now();
    let source_context = super::super::augment_structured_source_context(
        super::super::StructuredSourceContextAugmentRequest {
            state,
            library_id,
            question,
            query_ir: Some(query_ir),
            document_index: &document_index,
            plan_keywords: &plan_keywords,
            graph_evidence_source_document_ids: &rerank_stage
                .retrieval
                .graph_evidence_source_document_ids,
            chunks: &mut rerank_stage.retrieval.bundle.chunks,
        },
    )
    .await?;
    tracing::info!(
        stage = "answer.source_context_done",
        library_id = %library_id,
        elapsed_ms = source_context_started.elapsed().as_millis(),
        "augment_structured_source_context complete"
    );
    hydrate_runtime_document_index(
        state,
        &mut document_index,
        &rerank_stage.retrieval.bundle.chunks,
    )
    .await?;
    // Temporal hard-filter on the bundle AFTER source-context augmentation.
    // The companion paths (focused-match, source profile, neighbor expansion,
    // library source profile) bypass the retrieval temporal filter and pull
    // chunks regardless of `occurred_at`. When the user explicitly scoped
    // the question to a date range, drop any chunk whose underlying
    // `KnowledgeChunkRow.occurred_at` is null OR falls outside the bounds.
    // Verified necessary on stage 2026-05-03: image-OCR chunks (no
    // occurred_at) were leaking into "messages in March 2026" answers via
    // the prepared-segment / source-context path. Single knowledge-store read
    // via `list_chunks_by_ids`; no per-chunk lookup.
    apply_bundle_temporal_filter(
        state,
        library_id,
        query_ir,
        &mut rerank_stage.retrieval.bundle.chunks,
    )
    .await?;
    let declaration_context_chunks =
        super::super::structured_query_pipeline::take_focused_declaration_context_chunks(
            &mut rerank_stage.retrieval.bundle.chunks,
        );
    log_structured_source_context(library_id, &source_context);
    log_canonical_head_filter(
        library_id,
        super::super::retain_current_document_head_chunks(
            &mut rerank_stage.retrieval.bundle.chunks,
            &document_index,
        ),
        "structured source context",
    );
    let topical_prune = super::super::prune_non_topical_document_tail(
        &mut rerank_stage.retrieval.bundle,
        question,
        Some(query_ir),
        query_requests_latest_versions(query_ir),
    );
    log_topical_prune(library_id, &topical_prune);

    // Context assembly runs AFTER consolidation so the assembled
    // `context_text` reflects the reshuffled bundle. The winner
    // document_id is threaded in so `load_retrieved_document_briefs`
    // can build the winner preview out of the anchor-window chunks
    // already in the bundle (rather than re-fetching intro chunks
    // that consolidation deliberately demoted).
    rerank_stage.retrieval.bundle.chunks.extend(declaration_context_chunks);
    let structured =
        super::super::finalize_structured_query(super::super::StructuredQueryAssemblyRequest {
            state,
            question,
            query_ir,
            rerank_stage,
            include_debug: request.include_debug,
            focused_document_id: consolidation.focused_document_id,
            declaration_inventory_candidates,
        })
        .await?;
    Ok(FinalizedBundle {
        structured,
        document_index,
        plan_keywords,
        consolidation_elapsed_ms,
        consolidation_reason: consolidation.focus_reason.as_str(),
        consolidation_winner_chunks: consolidation.winner_chunk_count,
        consolidation_tangential_chunks: consolidation.tangential_chunk_count,
        topical_pruned_chunks: topical_prune.removed_chunk_count,
    })
}

/// Loads the dominant document's own chunk rows as inventory candidates when
/// the query asks for a declaration inventory, falling back to the retrieved
/// bundle otherwise. Bounded by [`DECLARATION_INVENTORY_DOCUMENT_CHUNK_LIMIT`].
async fn declaration_inventory_candidates(
    state: &AppState,
    query_ir: &QueryIR,
    retrieved: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    if !super::super::context::query_requests_declaration_inventory(query_ir) {
        return Vec::new();
    }
    load_declaration_inventory_document_chunks(state, query_ir, retrieved)
        .await
        .unwrap_or_else(|| retrieved.to_vec())
}

/// Assembles the answer context and runs the compare/technical evidence probes
/// against it. Mutates `structured` in place so the proven chunks become part
/// of the final result.
async fn assemble_prepared_answer_context(
    request: &PrepareAnswerQueryRequest<'_>,
    query_ir: &QueryIR,
    structured: &mut super::super::RuntimeStructuredQueryResult,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<String> {
    let state = request.state;
    let library_id = request.library_id;
    // Stage 2: library summary is answer evidence; graph community
    // summaries are intentionally excluded from the final answer prompt
    // because they are broad topology hints, not cited evidence.
    let library_context = match load_query_execution_library_context(state, library_id).await {
        Ok(context) => Some(context),
        Err(error) => {
            log_library_context_skipped(library_id, &error);
            None
        }
    };
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
                should_prioritize_retrieved_context_for_query(query_ir, &structured.context_text),
            )
        },
    );
    let probe_context = EvidenceProbeContext {
        state,
        library_id,
        ir: query_ir,
        question: &request.question,
        document_index,
        plan_keywords,
    };
    let compare_probe =
        augment_partial_compare_context(&probe_context, &mut answer_context, structured).await?;
    if compare_probe.attempted {
        log_compare_context_probe(library_id, &compare_probe);
    }
    let technical_probe =
        augment_technical_focus_context(&probe_context, &mut answer_context, structured).await?;
    if technical_probe.attempted {
        log_technical_focus_probe(library_id, &technical_probe);
    }
    Ok(answer_context)
}

/// Runs the NL->IR compiler for the current question + conversation history.
/// Provider failures are logged as operator-visible errors, then downgraded to
/// a neutral provider-free IR so the `grounded_answer` tool can still
/// retrieve and verify evidence instead of forcing parent agents onto weaker
/// ad hoc search/read fallbacks.
/// Load the dominant document's own chunk rows as inventory candidates.
/// Bounded: a document beyond this many chunks contributes only its head,
/// which still covers every top-level declaration a bounded inventory can
/// show. Returns None when nothing dominates or the load fails, so the
/// caller falls back to the retrieved bundle.
const DECLARATION_INVENTORY_DOCUMENT_CHUNK_LIMIT: usize = 400;

async fn load_declaration_inventory_document_chunks(
    state: &AppState,
    query_ir: &crate::domains::query_ir::QueryIR,
    retrieved: &[super::super::RuntimeMatchedChunk],
) -> Option<Vec<super::super::RuntimeMatchedChunk>> {
    let document_id =
        super::super::context::declaration_inventory_document_id(query_ir, retrieved, None)?;
    let seed = retrieved.iter().find(|chunk| chunk.document_id == document_id)?;
    let revision_id = seed.revision_id;
    let document_label = seed.document_label.clone();
    let rows = match state
        .document_store
        .list_head_chunks_by_revision(revision_id, DECLARATION_INVENTORY_DOCUMENT_CHUNK_LIMIT)
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(
                stage = "answer.declaration_inventory",
                %document_id,
                %revision_id,
                error = %error,
                "document chunk load failed; falling back to retrieved bundle"
            );
            return None;
        }
    };
    (!rows.is_empty()).then(|| {
        rows.into_iter()
            .map(|row| super::super::RuntimeMatchedChunk {
                chunk_id: row.chunk_id,
                document_id: row.document_id,
                revision_id: row.revision_id,
                chunk_index: row.chunk_index,
                chunk_kind: row.chunk_kind.clone(),
                document_label: document_label.clone(),
                excerpt: String::new(),
                score_kind:
                    crate::services::query::execution::RuntimeChunkScoreKind::FocusedDocument,
                score: None,
                source_text: row.content_text,
            })
            .collect()
    })
}

pub(crate) async fn compile_query_ir(
    state: &AppState,
    execution_context: crate::services::query::provider_billing::QueryProviderExecutionContext,
    question: &str,
    conversation_history: &[ExternalConversationTurn],
) -> Result<QueryIR, ApiError> {
    let library_id = execution_context.library_id;
    let started_at = std::time::Instant::now();
    let history = compile_history_turns(conversation_history);
    match QueryCompilerService
        .compile(
            state,
            CompileQueryCommand {
                library_id,
                execution_context,
                question: question.to_string(),
                history,
            },
        )
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
                provider_kind = %outcome.provider_kind,
                model_name = %outcome.model_name,
                "query.compile.ir"
            );
            Ok(outcome.ir)
        }
        Err(error) => compile_failure_fallback(library_id, question, error),
    }
}

/// Maps conversation turns onto the compiler's history contract, dropping the
/// non-conversational turn kinds.
fn compile_history_turns(
    conversation_history: &[ExternalConversationTurn],
) -> Vec<CompileHistoryTurn> {
    conversation_history
        .iter()
        .filter_map(|turn| match &turn.turn_kind {
            crate::domains::query::QueryTurnKind::User => Some(CompileHistoryTurn {
                role: "user".to_string(),
                content: turn.content_text.clone(),
            }),
            crate::domains::query::QueryTurnKind::Assistant => Some(CompileHistoryTurn {
                role: "assistant".to_string(),
                content: turn.content_text.clone(),
            }),
            crate::domains::query::QueryTurnKind::System
            | crate::domains::query::QueryTurnKind::Tool => None,
        })
        .collect()
}

/// Downgrades a provider failure to a provider-free fallback IR so retrieval
/// and verification still run; other compile errors propagate unchanged.
fn compile_failure_fallback(
    library_id: Uuid,
    question: &str,
    error: ApiError,
) -> Result<QueryIR, ApiError> {
    tracing::error!(
        %library_id,
        ?error,
        "query compile failed"
    );
    if matches!(error, ApiError::ProviderFailure(_)) {
        let fallback_ir = provider_free_fallback_query_ir(question);
        log_compile_provider_fallback(library_id, &fallback_ir);
        Ok(fallback_ir)
    } else {
        Err(error)
    }
}

pub(super) fn structural_question_tokens(question: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in question.chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':') {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(take_structural_question_token(&current));
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(take_structural_question_token(&current));
    }
    tokens
}

pub(super) fn take_structural_question_token(value: &str) -> String {
    value.chars().take(STRUCTURAL_QUESTION_TOKEN_MAX_CHARS).collect()
}
