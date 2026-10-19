use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{
    agent_runtime::pipeline::try_op::run_async_try_op, app::state::AppState,
    services::query::latest_versions::query_requests_latest_versions,
};

use super::context::build_structured_query_diagnostics;
use super::retrieve::query_ir_promotes_graph_evidence;
use super::{
    context::{
        BoundedContextInventoryQuery, assemble_bounded_context_for_query_with_inventory_candidates,
        assemble_context_metadata_for_query, build_grouped_reference_candidates,
        group_visible_references_for_query, load_retrieved_document_briefs,
        query_requests_declaration_inventory, target_entity_context_lines,
    },
    document_target::promote_comparison_target_chunks,
    preflight::{TechnicalLiteralSelection, select_technical_literal_chunks},
    rerank::{ApplyRerankRequest, apply_configured_rerank},
    retrieve::truncate_bundle_with_semantic_chunk_ranks,
    source_context::{
        is_focused_declaration_context_chunk, is_source_unit_runtime_chunk,
        source_slice_context_budget_chars, structured_source_context_top_k_for_chunks,
    },
    technical_literal_context::{
        collect_technical_literal_groups, render_exact_technical_literals_section,
    },
    technical_literal_focus::technical_literal_focus_keywords,
    technical_literals::TechnicalLiteralIntent,
    types::{
        QueryChunkReferenceSnapshot, QueryExecutionEnrichment, RuntimeMatchedChunk,
        RuntimeStructuredQueryResult, SemanticRerankExecutionContext, StructuredQueryAssemblyStage,
        StructuredQueryRerankStage, StructuredQueryRetrievalStage,
    },
};

mod planning;
mod retrieval;

pub(crate) use planning::{
    CompiledIrPlanRefresh, ResolvedRetrievalReplanRequest, plan_structured_query,
    refresh_query_plan_for_compiled_ir, replan_for_resolved_retrieval_query,
};
pub(crate) use retrieval::retrieve_structured_query;

/// Inputs of the structured-query finalize/assembly stages: the reranked stage
/// plus the query-side context the assembly renders from.
pub(crate) struct StructuredQueryAssemblyRequest<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) question: &'a str,
    pub(crate) query_ir: &'a crate::domains::query_ir::QueryIR,
    pub(crate) rerank_stage: StructuredQueryRerankStage,
    pub(crate) include_debug: bool,
    pub(crate) focused_document_id: Option<Uuid>,
    pub(crate) declaration_inventory_candidates: Vec<RuntimeMatchedChunk>,
}

/// Finalize a reranked bundle into a `RuntimeStructuredQueryResult`
/// (context assembly + diagnostics). Runs AFTER the caller has had a
/// chance to mutate `rerank_stage.retrieval.bundle` (e.g. via
/// `focused_document_consolidation`) so the assembled context reflects
/// those edits.
pub(crate) async fn finalize_structured_query(
    request: StructuredQueryAssemblyRequest<'_>,
) -> anyhow::Result<RuntimeStructuredQueryResult> {
    let StructuredQueryAssemblyRequest {
        state,
        question,
        query_ir,
        rerank_stage,
        include_debug,
        focused_document_id,
        declaration_inventory_candidates,
    } = request;
    let assemble_started = std::time::Instant::now();
    let assembly_stage = run_async_try_op(rerank_stage, |rerank_stage| {
        assemble_structured_query(StructuredQueryAssemblyRequest {
            state,
            question,
            query_ir,
            rerank_stage,
            include_debug,
            focused_document_id,
            declaration_inventory_candidates,
        })
    })
    .await?;
    let assemble_elapsed_ms = assemble_started.elapsed().as_millis();
    tracing::info!(
        stage = "retrieval.assemble",
        assemble_ms = assemble_elapsed_ms,
        "structured retrieval assemble stage"
    );

    let enrichment = QueryExecutionEnrichment {
        planning: assembly_stage.rerank.retrieval.planning.planning.clone(),
        rerank: assembly_stage.rerank.rerank.clone(),
        context_assembly: assembly_stage.context_assembly.clone(),
        grouped_references: assembly_stage.grouped_references.clone(),
    };
    let diagnostics = build_structured_query_diagnostics(
        &assembly_stage.rerank.retrieval.planning.plan,
        &assembly_stage.rerank.retrieval.bundle,
        &assembly_stage.rerank.retrieval.planning.graph_index,
        &enrichment,
        include_debug,
        &assembly_stage.context_text,
    );

    // Snapshot the final ranked chunks so the turn layer can write
    // `query_chunk_reference` audit rows keyed by the execution_id.
    // Rank is 1-based, score is f64 (f32 retrieval score widened) to
    // match the table definition.
    let retrieved_context_document_titles =
        distinct_context_document_titles(&assembly_stage.rerank.retrieval.bundle.chunks);
    let chunk_references =
        build_query_chunk_reference_snapshots(&assembly_stage.rerank.retrieval.bundle.chunks);
    let context_chunks = assembly_stage.rerank.retrieval.bundle.chunks.clone();
    let ordered_source_units =
        collect_ordered_source_units(&assembly_stage.rerank.retrieval.bundle.chunks);
    let graph_evidence_context_lines = assembly_stage.graph_evidence_context_lines.clone();
    let graph_entity_references = assembly_stage.rerank.retrieval.bundle.entities.clone();
    let graph_relation_references = assembly_stage.rerank.retrieval.bundle.relationships.clone();

    Ok(RuntimeStructuredQueryResult {
        planned_mode: assembly_stage.rerank.retrieval.planning.plan.planned_mode,
        intent_profile: assembly_stage.rerank.retrieval.planning.plan.intent_profile,
        context_text: assembly_stage.context_text,
        technical_literals_text: assembly_stage.technical_literals_text,
        technical_literal_chunks: assembly_stage.technical_literal_chunks,
        diagnostics,
        retrieved_documents: assembly_stage.retrieved_documents,
        retrieved_context_document_titles,
        chunk_references,
        context_chunks,
        ordered_source_units,
        graph_evidence_context_lines,
        graph_entity_references,
        graph_relation_references,
    })
}

fn distinct_context_document_titles(chunks: &[RuntimeMatchedChunk]) -> Vec<String> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut titles = Vec::new();
    for chunk in chunks {
        let title = chunk.document_label.trim();
        if title.is_empty() {
            continue;
        }
        let key = title.to_lowercase();
        if seen.insert(key) {
            titles.push(title.to_string());
        }
    }
    titles
}

fn build_query_chunk_reference_snapshots(
    chunks: &[RuntimeMatchedChunk],
) -> Vec<QueryChunkReferenceSnapshot> {
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| QueryChunkReferenceSnapshot {
            chunk_id: chunk.chunk_id,
            rank: (index as i32) + 1,
            score: chunk.score.unwrap_or(0.0) as f64,
        })
        .collect()
}

fn collect_ordered_source_units(chunks: &[RuntimeMatchedChunk]) -> Vec<RuntimeMatchedChunk> {
    let mut units = chunks
        .iter()
        .filter(|chunk| is_source_unit_runtime_chunk(chunk))
        .cloned()
        .collect::<Vec<_>>();
    units.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id));
    units
}

pub(crate) async fn rerank_structured_query(
    state: &AppState,
    library_id: Uuid,
    semantic_rerank_context: SemanticRerankExecutionContext,
    question: &str,
    mut retrieval: StructuredQueryRetrievalStage,
) -> anyhow::Result<StructuredQueryRerankStage> {
    let plan = &retrieval.planning.plan;
    let mut semantic_chunk_ranks = HashMap::new();
    let rerank = apply_configured_rerank(ApplyRerankRequest {
        state,
        library_id,
        semantic_rerank_context,
        question,
        plan,
        bundle: &mut retrieval.bundle,
        semantic_chunk_ranks: &mut semantic_chunk_ranks,
    })
    .await;

    Ok(StructuredQueryRerankStage { retrieval, rerank, semantic_chunk_ranks })
}

async fn assemble_structured_query(
    request: StructuredQueryAssemblyRequest<'_>,
) -> anyhow::Result<StructuredQueryAssemblyStage> {
    let StructuredQueryAssemblyRequest {
        state,
        question,
        query_ir,
        rerank_stage: mut rerank,
        include_debug: _include_debug,
        focused_document_id,
        mut declaration_inventory_candidates,
    } = request;
    let technical_literal_intent = effective_technical_literal_intent(
        question,
        Some(query_ir),
        rerank.retrieval.planning.technical_literal_intent,
    );
    rerank.retrieval.planning.technical_literal_intent = technical_literal_intent;
    let plan = &rerank.retrieval.planning.plan;
    let semantic_chunk_ranks = &rerank.semantic_chunk_ranks;
    let bundle = &mut rerank.retrieval.bundle;
    if query_requests_declaration_inventory(query_ir) {
        declaration_inventory_candidates.extend(bundle.chunks.iter().cloned());
    }
    let declaration_context_chunks = take_focused_declaration_context_chunks(&mut bundle.chunks);
    declaration_inventory_candidates.extend(declaration_context_chunks.iter().cloned());
    let effective_top_k =
        structured_source_context_top_k_for_chunks(query_ir, plan.top_k, &bundle.chunks);
    promote_comparison_target_chunks(query_ir, &mut bundle.chunks);
    let retrieved_documents = load_retrieved_document_briefs(
        state,
        &bundle.chunks,
        &rerank.retrieval.planning.document_index,
        effective_top_k,
        focused_document_id,
    )
    .await;
    let pagination_requested = false;
    let literal_focus_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let technical_literal_chunks = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir,
        chunks: &bundle.chunks,
        technical_literal_intent,
        top_k: effective_top_k,
        literal_focus_keywords: &literal_focus_keywords,
        preferred_document_ids: &rerank.retrieval.graph_evidence_source_document_ids,
        pagination_requested,
    });
    let technical_literal_groups =
        collect_technical_literal_groups(question, query_ir, &technical_literal_chunks);
    let technical_literals_text =
        render_exact_technical_literals_section(&technical_literal_groups);
    // Documents whose chunks are attached context of a parent page (image
    // attachments) are demoted below peer/primary content during truncation and
    // excluded from clarify variant grouping. The query's explicitly focused
    // document, if any, is never demoted so an exact attachment ask still
    // surfaces it. Derived from the typed `document_role` only — no
    // MIME/extension/filename signal at the retrieval layer.
    let demoted_document_ids: HashSet<Uuid> = rerank
        .retrieval
        .planning
        .document_index
        .values()
        .filter(|doc| crate::domains::content::role_is_attached_context(&doc.document_role))
        .filter(|doc| doc.parent_document_id.is_some())
        .map(|doc| doc.document_id)
        .filter(|id| Some(*id) != focused_document_id)
        .collect();

    truncate_bundle_with_semantic_chunk_ranks(
        bundle,
        effective_top_k,
        Some(query_ir),
        &demoted_document_ids,
        semantic_chunk_ranks,
    );

    let include_graph_context = include_graph_context_for_query(query_ir);
    if !include_graph_context {
        bundle.entities.clear();
        bundle.relationships.clear();
    }

    let grouped_references = group_visible_references_for_query(
        &build_grouped_reference_candidates(
            &bundle.entities,
            &bundle.relationships,
            &bundle.chunks,
            effective_top_k,
            &demoted_document_ids,
        ),
        effective_top_k,
    );
    let effective_context_budget =
        source_slice_context_budget_chars(query_ir, plan.context_budget_chars);
    let mut graph_evidence_lines = if include_graph_context {
        target_entity_context_lines(query_ir, &rerank.retrieval.planning.graph_index)
    } else {
        Vec::new()
    };
    if include_graph_context {
        graph_evidence_lines.extend(rerank.retrieval.graph_evidence_context_lines.clone());
    }
    let context_text = assemble_bounded_context_for_query_with_inventory_candidates(
        BoundedContextInventoryQuery {
            query_ir,
            question,
            entities: &bundle.entities,
            relationships: &bundle.relationships,
            chunks: &bundle.chunks,
            graph_evidence_lines: &graph_evidence_lines,
            inventory_candidates: &declaration_inventory_candidates,
            focused_document_id,
            fallback_declaration_chunks: &declaration_context_chunks,
            budget_chars: effective_context_budget,
        },
    );
    let graph_support_count =
        bundle.entities.len() + bundle.relationships.len() + graph_evidence_lines.len();
    let context_assembly = assemble_context_metadata_for_query(
        plan.planned_mode,
        graph_support_count,
        bundle.chunks.len(),
    );

    Ok(StructuredQueryAssemblyStage {
        rerank,
        context_text,
        graph_evidence_context_lines: graph_evidence_lines,
        technical_literals_text,
        technical_literal_chunks,
        retrieved_documents,
        grouped_references,
        context_assembly,
    })
}

pub(crate) fn take_focused_declaration_context_chunks(
    chunks: &mut Vec<RuntimeMatchedChunk>,
) -> Vec<RuntimeMatchedChunk> {
    if chunks.iter().any(is_focused_declaration_context_chunk) {
        let (baseline_chunks, declaration_chunks) = std::mem::take(chunks)
            .into_iter()
            .partition(|chunk| !is_focused_declaration_context_chunk(chunk));
        *chunks = baseline_chunks;
        declaration_chunks
    } else {
        Vec::new()
    }
}

fn effective_technical_literal_intent(
    question: &str,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
    fallback: TechnicalLiteralIntent,
) -> TechnicalLiteralIntent {
    let query_ir_intent = query_ir
        .map(|ir| {
            super::technical_literals::detect_technical_literal_intent_from_query_ir(question, ir)
        })
        .unwrap_or_default();
    merge_technical_literal_intent(fallback, query_ir_intent)
}

fn merge_technical_literal_intent(
    left: TechnicalLiteralIntent,
    right: TechnicalLiteralIntent,
) -> TechnicalLiteralIntent {
    TechnicalLiteralIntent {
        wants_urls: left.wants_urls || right.wants_urls,
        wants_prefixes: left.wants_prefixes || right.wants_prefixes,
        wants_paths: left.wants_paths || right.wants_paths,
        wants_methods: left.wants_methods || right.wants_methods,
        wants_parameters: left.wants_parameters || right.wants_parameters,
    }
}

fn include_graph_context_for_query(query_ir: &crate::domains::query_ir::QueryIR) -> bool {
    query_ir_promotes_graph_evidence(query_ir) && !query_requests_latest_versions(query_ir)
}

#[cfg(test)]
#[path = "structured_query_pipeline_plan_tests.rs"]
mod plan_tests;

#[cfg(test)]
mod tests;
