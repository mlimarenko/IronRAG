use std::collections::{HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    agent_runtime::pipeline::try_op::run_async_try_op,
    app::state::AppState,
    domains::{query::RuntimeQueryMode, query_ir::QueryIR},
    services::{
        query::latest_versions::query_requests_latest_versions,
        query::planner::{QueryPlanTaskInput, RuntimeQueryPlan, build_task_query_plan},
        query::provider_billing::QueryProviderExecutionContext,
        query::support::{
            IntentResolutionRequest, derive_query_planning_metadata,
            derive_query_planning_metadata_for_query_ir,
        },
        query::vector_dimensions::{
            validate_active_embedding_profile_key, validate_embedding_vector_dimensions,
        },
    },
};

use super::context::build_structured_query_diagnostics;
use super::embed::QuestionEmbeddingResult;
use super::retrieve::{prepare_runtime_vector_search_context, query_ir_promotes_graph_evidence};
#[cfg(test)]
use super::source_context::SOURCE_UNIT_CHUNK_KIND;
#[cfg(test)]
use super::types::QueryGraphIndex;
use super::{
    context::{
        assemble_bounded_context_for_query, assemble_context_metadata_for_query,
        build_grouped_reference_candidates, group_visible_references_for_query,
        load_retrieved_document_briefs, target_entity_context_lines,
    },
    document_target::resolve_scoped_target_document_ids,
    embed::embed_question,
    graph_retrieval::{
        graph_target_entity_profiles, retrieve_global_bundle, retrieve_local_bundle,
        retrieve_mixed_graph_bundle,
    },
    hyde::generate_hyde_passage,
    preflight::select_technical_literal_chunks,
    rerank::apply_configured_rerank,
    retrieve::{
        expanded_candidate_limit, graph_evidence_context_top_k, load_document_index,
        load_graph_evidence_chunks_for_bundle, load_graph_index, merge_graph_evidence_chunks,
        retain_canonical_document_head_chunks, retrieve_document_chunks, should_skip_vector_search,
        truncate_bundle_with_semantic_chunk_ranks,
    },
    source_context::{
        is_source_unit_runtime_chunk, source_slice_context_budget_chars,
        structured_source_context_top_k_for_chunks,
    },
    technical_literal_context::{
        collect_technical_literal_groups, render_exact_technical_literals_section,
    },
    technical_literal_focus::technical_literal_focus_keywords,
    technical_literals::{TechnicalLiteralIntent, technical_literal_candidate_limit},
    types::{
        QueryChunkReferenceSnapshot, QueryExecutionEnrichment, RetrievalBundle,
        RuntimeMatchedChunk, RuntimeStructuredQueryResult, RuntimeVectorSearchContext,
        SemanticRerankExecutionContext, StructuredQueryAssemblyStage, StructuredQueryPlanningStage,
        StructuredQueryRerankStage, StructuredQueryRetrievalStage,
    },
};

/// Finalize a reranked bundle into a `RuntimeStructuredQueryResult`
/// (context assembly + diagnostics). Runs AFTER the caller has had a
/// chance to mutate `rerank_stage.retrieval.bundle` (e.g. via
/// `focused_document_consolidation`) so the assembled context reflects
/// those edits.
pub(crate) async fn finalize_structured_query(
    state: &AppState,
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    rerank_stage: StructuredQueryRerankStage,
    include_debug: bool,
    focused_document_id: Option<Uuid>,
) -> anyhow::Result<RuntimeStructuredQueryResult> {
    let assemble_started = std::time::Instant::now();
    let assembly_stage = run_async_try_op(rerank_stage, |rerank_stage| {
        assemble_structured_query(
            state,
            question,
            query_ir,
            rerank_stage,
            include_debug,
            focused_document_id,
        )
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

pub(crate) async fn plan_structured_query(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
) -> anyhow::Result<StructuredQueryPlanningStage> {
    let library_id = execution_context.library_id;
    let planning = derive_query_planning_metadata(&IntentResolutionRequest {
        question: question.to_string(),
        explicit_mode: mode,
    });
    let plan = build_task_query_plan(&QueryPlanTaskInput {
        question: question.to_string(),
        top_k: Some(top_k),
        explicit_mode: Some(mode),
        metadata: Some(planning.clone()),
        query_ir: None,
    })
    .map_err(|failure| anyhow::anyhow!(failure.summary))?;
    let technical_literal_intent = TechnicalLiteralIntent::default();
    let prepared_embeddings =
        compute_question_embeddings(state, execution_context, &plan, question, None).await?;

    let graph_index_started = std::time::Instant::now();
    let graph_index = load_graph_index(state, library_id).await?;
    tracing::info!(
        stage = "plan.load_graph_index",
        library_id = %library_id,
        elapsed_ms = graph_index_started.elapsed().as_millis(),
        node_count = graph_index.node_count(),
        "graph index loaded for query planning"
    );
    let document_index_started = std::time::Instant::now();
    let document_index = load_document_index(state, library_id).await?;
    tracing::info!(
        stage = "plan.load_document_index",
        library_id = %library_id,
        elapsed_ms = document_index_started.elapsed().as_millis(),
        document_count = document_index.len(),
        "document index loaded for query planning"
    );
    let candidate_limit = expanded_candidate_limit(
        plan.planned_mode,
        plan.top_k,
        state.retrieval_intelligence.rerank_enabled,
        state.retrieval_intelligence.rerank_candidate_limit,
    )
    .max(technical_literal_candidate_limit(technical_literal_intent, plan.top_k));

    Ok(StructuredQueryPlanningStage {
        planning,
        plan,
        technical_literal_intent,
        question_embedding: prepared_embeddings.question_embedding,
        hyde_embedding: prepared_embeddings.hyde_embedding,
        vector_search_context: prepared_embeddings.vector_search_context,
        graph_index,
        document_index,
        candidate_limit,
    })
}

/// Embed the query string for the vector lane plus, when the plan asks for
/// it, a HyDE passage. Skipped entirely for exact-literal technical queries
/// where vector retrieval would only add noise.
struct PreparedQuestionEmbeddings {
    question_embedding: Vec<f32>,
    hyde_embedding: Option<Vec<f32>>,
    vector_search_context: Option<RuntimeVectorSearchContext>,
}

impl PreparedQuestionEmbeddings {
    fn skipped() -> Self {
        Self { question_embedding: Vec::new(), hyde_embedding: None, vector_search_context: None }
    }
}

async fn compute_question_embeddings(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    plan: &RuntimeQueryPlan,
    question: &str,
    existing_vector_search_context: Option<&RuntimeVectorSearchContext>,
) -> anyhow::Result<PreparedQuestionEmbeddings> {
    let library_id = execution_context.library_id;
    if should_skip_vector_search(plan) {
        tracing::info!(
            stage = "embed",
            exact_literal_technical = true,
            "vector retrieval skipped for exact technical literal query"
        );
        return Ok(PreparedQuestionEmbeddings::skipped());
    }
    let vector_search_context = match existing_vector_search_context {
        Some(context) => context.clone(),
        None => {
            let Some(context) = prepare_runtime_vector_search_context(state, library_id).await?
            else {
                return Ok(PreparedQuestionEmbeddings::skipped());
            };
            context
        }
    };
    let embed_result =
        embed_question(state, execution_context, question, &vector_search_context).await?;
    validate_prepared_query_embedding(library_id, &vector_search_context, &embed_result)?;
    let question_embedding = embed_result.embedding.clone();
    let hyde_embedding = if plan.hyde_recommended {
        Some(
            compute_hyde_embedding(state, execution_context, question, &vector_search_context)
                .await?,
        )
    } else {
        tracing::debug!(
            stage = "hyde",
            hyde_recommended = false,
            "HyDE skipped — not recommended for this query intent"
        );
        None
    };
    Ok(PreparedQuestionEmbeddings {
        question_embedding,
        hyde_embedding,
        vector_search_context: Some(vector_search_context),
    })
}

fn validate_prepared_query_embedding(
    library_id: Uuid,
    context: &RuntimeVectorSearchContext,
    embedding: &QuestionEmbeddingResult,
) -> anyhow::Result<()> {
    validate_active_embedding_profile_key(
        library_id,
        &context.embedding_profile_key,
        &embedding.embedding_profile_key,
    )?;
    validate_embedding_vector_dimensions(
        context.dimensions,
        &embedding.embedding,
        "prepared runtime query",
    )?;
    Ok(())
}

async fn compute_hyde_embedding(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    question: &str,
    vector_search_context: &RuntimeVectorSearchContext,
) -> anyhow::Result<Vec<f32>> {
    let library_id = execution_context.library_id;
    tracing::info!(stage = "hyde", hyde_recommended = true, "HyDE activated for this query");
    let passage = generate_hyde_passage(state, execution_context, question).await?;
    tracing::debug!(stage = "hyde", passage_len = passage.len(), "HyDE passage generated");
    tracing::trace!(stage = "hyde", passage = %passage, "HyDE passage content");
    let hyde_result = embed_question(state, execution_context, &passage, vector_search_context)
        .await
        .context("failed to embed HyDE passage")?;
    validate_prepared_query_embedding(library_id, vector_search_context, &hyde_result)?;
    tracing::debug!(stage = "hyde_embed", "HyDE embedding computed");
    Ok(hyde_result.embedding)
}

/// Re-derive question-dependent planning for a resolved standalone retrieval query.
/// The loaded indexes are reused; metadata, plan, embeddings, and candidate limit
/// are recomputed for the resolved string.
pub(crate) async fn replan_for_resolved_retrieval_query(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    planning: &mut StructuredQueryPlanningStage,
    resolved_query: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
    query_ir: &QueryIR,
) -> anyhow::Result<()> {
    let metadata = derive_query_planning_metadata_for_query_ir(
        &IntentResolutionRequest { question: resolved_query.to_string(), explicit_mode: mode },
        query_ir,
    );
    let plan = build_task_query_plan(&QueryPlanTaskInput {
        question: resolved_query.to_string(),
        top_k: Some(top_k),
        explicit_mode: Some(mode),
        metadata: Some(metadata.clone()),
        query_ir: Some(query_ir.clone()),
    })
    .map_err(|failure| anyhow::anyhow!(failure.summary))?;
    let existing_vector_search_context = planning.vector_search_context.clone();
    let prepared_embeddings = compute_question_embeddings(
        state,
        execution_context,
        &plan,
        resolved_query,
        existing_vector_search_context.as_ref(),
    )
    .await?;
    let candidate_limit = expanded_candidate_limit(
        plan.planned_mode,
        plan.top_k,
        state.retrieval_intelligence.rerank_enabled,
        state.retrieval_intelligence.rerank_candidate_limit,
    )
    .max(technical_literal_candidate_limit(planning.technical_literal_intent, plan.top_k));
    planning.planning = metadata;
    planning.plan = plan;
    planning.question_embedding = prepared_embeddings.question_embedding;
    planning.hyde_embedding = prepared_embeddings.hyde_embedding;
    planning.vector_search_context = prepared_embeddings.vector_search_context;
    planning.candidate_limit = candidate_limit;
    Ok(())
}

pub(crate) fn build_compiled_ir_query_plan(
    retrieval_question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
    query_ir: &QueryIR,
) -> anyhow::Result<(crate::domains::query::QueryPlanningMetadata, RuntimeQueryPlan)> {
    let metadata = derive_query_planning_metadata_for_query_ir(
        &IntentResolutionRequest { question: retrieval_question.to_string(), explicit_mode: mode },
        query_ir,
    );
    let plan = build_task_query_plan(&QueryPlanTaskInput {
        question: retrieval_question.to_string(),
        top_k: Some(top_k),
        explicit_mode: Some(mode),
        metadata: Some(metadata.clone()),
        query_ir: Some(query_ir.clone()),
    })
    .map_err(|failure| anyhow::anyhow!(failure.summary))?;
    Ok((metadata, plan))
}

/// Finalize the IR-dependent part of the plan produced in parallel with query
/// compilation. The common path keeps the original question bytes, so its
/// base embedding and loaded indexes remain valid; only a newly enabled or
/// missing HyDE embedding may require provider work here.
pub(crate) async fn refresh_query_plan_for_compiled_ir(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    planning: &mut StructuredQueryPlanningStage,
    retrieval_question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
    query_ir: &QueryIR,
    rerank_enabled: bool,
    rerank_candidate_limit: usize,
) -> anyhow::Result<()> {
    let vector_search_context = planning.vector_search_context.clone();
    refresh_query_plan_for_compiled_ir_with_hyde(
        planning,
        retrieval_question,
        mode,
        top_k,
        query_ir,
        rerank_enabled,
        rerank_candidate_limit,
        move || async move {
            let context = vector_search_context.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "HyDE embedding requested without a ready request-scoped vector preflight"
                )
            })?;
            compute_hyde_embedding(state, execution_context, retrieval_question, context).await
        },
    )
    .await
}

async fn refresh_query_plan_for_compiled_ir_with_hyde<F, Fut>(
    planning: &mut StructuredQueryPlanningStage,
    retrieval_question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
    query_ir: &QueryIR,
    rerank_enabled: bool,
    rerank_candidate_limit: usize,
    create_hyde_embedding: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<f32>>>,
{
    let (metadata, plan) = build_compiled_ir_query_plan(retrieval_question, mode, top_k, query_ir)?;
    let candidate_limit = expanded_candidate_limit(
        plan.planned_mode,
        plan.top_k,
        rerank_enabled,
        rerank_candidate_limit,
    )
    .max(technical_literal_candidate_limit(planning.technical_literal_intent, plan.top_k));
    let hyde_activation_changed = planning.plan.hyde_recommended != plan.hyde_recommended;
    let should_create_hyde = planning.vector_search_context.is_some()
        && plan.hyde_recommended
        && (hyde_activation_changed || planning.hyde_embedding.is_none());
    let created_hyde_embedding =
        if should_create_hyde { Some(create_hyde_embedding().await?) } else { None };
    let hyde_recommended = plan.hyde_recommended;

    planning.planning = metadata;
    planning.plan = plan;
    planning.candidate_limit = candidate_limit;
    if let Some(hyde_embedding) = created_hyde_embedding {
        planning.hyde_embedding = Some(hyde_embedding);
    } else if !hyde_recommended {
        planning.hyde_embedding = None;
    }
    Ok(())
}

pub(crate) async fn retrieve_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mut planning: StructuredQueryPlanningStage,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
) -> anyhow::Result<StructuredQueryRetrievalStage> {
    let technical_literal_intent =
        effective_technical_literal_intent(question, query_ir, planning.technical_literal_intent);
    planning.technical_literal_intent = technical_literal_intent;
    planning.candidate_limit = planning
        .candidate_limit
        .max(technical_literal_candidate_limit(technical_literal_intent, planning.plan.top_k));

    let plan = &planning.plan;
    let vector_search_embedding =
        planning.hyde_embedding.as_deref().unwrap_or(&planning.question_embedding);
    let question_embedding = vector_search_embedding;
    let vector_search_context = planning.vector_search_context.as_ref();
    let graph_index = &planning.graph_index;
    let document_index = &planning.document_index;
    let target_profile_started = std::time::Instant::now();
    let target_entity_profiles = graph_target_entity_profiles(query_ir, graph_index);
    tracing::info!(
        stage = "retrieval.graph_target_profiles",
        target_profile_count = target_entity_profiles.len(),
        elapsed_ms = target_profile_started.elapsed().as_millis(),
        "graph target entity profiles prepared for query execution",
    );
    let candidate_limit = planning.candidate_limit;
    let document_filter_ids =
        resolve_scoped_target_document_ids(question, query_ir, document_index);
    let locked_target_document_ids =
        (!document_filter_ids.is_empty()).then_some(&document_filter_ids);

    let bundle_retrieval_started = std::time::Instant::now();
    // For Hybrid/Mix the graph-only bundle (entities/relationships) is
    // built here, but the document-chunk leg is deferred so it can run
    // concurrently with graph-evidence hydration below — both depend
    // only on the graph bundle plus shared read-only inputs, never on
    // each other.
    let (mut bundle, needs_document_chunks) = match plan.planned_mode {
        RuntimeQueryMode::Document => {
            let chunks = Box::pin(retrieve_document_chunks(
                state,
                library_id,
                question,
                locked_target_document_ids,
                plan,
                candidate_limit,
                question_embedding,
                vector_search_context,
                document_index,
                query_ir,
            ))
            .await?;
            (RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks }, false)
        }
        RuntimeQueryMode::Local => (
            retrieve_local_bundle(
                state,
                library_id,
                plan,
                query_ir,
                &target_entity_profiles,
                candidate_limit,
                question_embedding,
                vector_search_context,
                graph_index,
            )
            .await?,
            false,
        ),
        RuntimeQueryMode::Global => (
            retrieve_global_bundle(
                state,
                library_id,
                plan,
                query_ir,
                &target_entity_profiles,
                candidate_limit,
                question_embedding,
                vector_search_context,
                graph_index,
            )
            .await?,
            false,
        ),
        RuntimeQueryMode::Hybrid => (
            retrieve_local_bundle(
                state,
                library_id,
                plan,
                query_ir,
                &target_entity_profiles,
                candidate_limit,
                question_embedding,
                vector_search_context,
                graph_index,
            )
            .await?,
            true,
        ),
        RuntimeQueryMode::Mix => (
            retrieve_mixed_graph_bundle(
                state,
                library_id,
                plan,
                query_ir,
                &target_entity_profiles,
                candidate_limit,
                question_embedding,
                vector_search_context,
                graph_index,
            )
            .await?,
            true,
        ),
    };
    tracing::info!(
        stage = "retrieval.bundle",
        library_id = %library_id,
        planned_mode = ?plan.planned_mode,
        elapsed_ms = bundle_retrieval_started.elapsed().as_millis(),
        chunk_count = bundle.chunks.len(),
        entity_count = bundle.entities.len(),
        relationship_count = bundle.relationships.len(),
        "bundle retrieval complete"
    );

    // Document-chunk retrieval (Hybrid/Mix) and graph-evidence hydration
    // are independent reads off the same graph bundle; overlap them so
    // the turn pays the slower of the two, not their sum. Output is
    // identical: `merge_graph_evidence_chunks` runs only after both
    // complete and consumes both result sets explicitly.
    let graph_evidence_started = std::time::Instant::now();
    let document_chunks_future = async {
        if needs_document_chunks {
            Box::pin(retrieve_document_chunks(
                state,
                library_id,
                question,
                locked_target_document_ids,
                plan,
                candidate_limit,
                question_embedding,
                vector_search_context,
                document_index,
                query_ir,
            ))
            .await
            .map(Some)
        } else {
            Ok(None)
        }
    };
    let graph_evidence_future = load_graph_evidence_chunks_for_bundle(
        state,
        library_id,
        question,
        &bundle.entities,
        &bundle.relationships,
        plan,
        query_ir,
        &target_entity_profiles,
        graph_index,
        document_index,
        &document_filter_ids,
        &plan.keywords,
    );
    let (document_chunks, graph_evidence) =
        tokio::try_join!(document_chunks_future, graph_evidence_future)?;
    if let Some(document_chunks) = document_chunks {
        bundle.chunks = document_chunks;
    }
    tracing::info!(
        stage = "retrieval.graph_evidence",
        library_id = %library_id,
        elapsed_ms = graph_evidence_started.elapsed().as_millis(),
        graph_chunk_count = graph_evidence.chunks.len(),
        document_chunk_count = bundle.chunks.len(),
        "graph evidence + document chunks loaded concurrently"
    );
    let promote_graph_evidence = query_ir.is_some_and(query_ir_promotes_graph_evidence);
    if !graph_evidence.chunks.is_empty() && promote_graph_evidence {
        bundle.chunks = merge_graph_evidence_chunks(
            std::mem::take(&mut bundle.chunks),
            graph_evidence.chunks,
            graph_evidence_context_top_k(candidate_limit),
        );
    } else if !graph_evidence.chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.graph_evidence_demoted",
            library_id = %library_id,
            graph_chunk_count = graph_evidence.chunks.len(),
            "graph evidence chunks omitted from answer-driving context for non-graph QueryIR"
        );
    }
    let stale_chunk_count =
        retain_canonical_document_head_chunks(&mut bundle.chunks, document_index);
    if stale_chunk_count > 0 {
        tracing::info!(
            stage = "retrieval.canonical_head_filter",
            library_id = %library_id,
            stale_chunk_count,
            "removed non-head revision chunks from retrieval bundle"
        );
    }

    Ok(StructuredQueryRetrievalStage {
        planning,
        bundle,
        graph_evidence_context_lines: graph_evidence.context_lines,
        graph_evidence_source_document_ids: graph_evidence.source_document_ids,
    })
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
    let rerank = apply_configured_rerank(
        state,
        library_id,
        semantic_rerank_context,
        question,
        plan,
        &mut retrieval.bundle,
        &mut semantic_chunk_ranks,
    )
    .await;

    Ok(StructuredQueryRerankStage { retrieval, rerank, semantic_chunk_ranks })
}

async fn assemble_structured_query(
    state: &AppState,
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    mut rerank: StructuredQueryRerankStage,
    _include_debug: bool,
    focused_document_id: Option<Uuid>,
) -> anyhow::Result<StructuredQueryAssemblyStage> {
    let technical_literal_intent = effective_technical_literal_intent(
        question,
        Some(query_ir),
        rerank.retrieval.planning.technical_literal_intent,
    );
    rerank.retrieval.planning.technical_literal_intent = technical_literal_intent;
    let plan = &rerank.retrieval.planning.plan;
    let semantic_chunk_ranks = &rerank.semantic_chunk_ranks;
    let bundle = &mut rerank.retrieval.bundle;
    let effective_top_k =
        structured_source_context_top_k_for_chunks(query_ir, plan.top_k, &bundle.chunks);
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
    let technical_literal_chunks = select_technical_literal_chunks(
        question,
        query_ir,
        &bundle.chunks,
        technical_literal_intent,
        effective_top_k,
        &literal_focus_keywords,
        &rerank.retrieval.graph_evidence_source_document_ids,
        pagination_requested,
    );
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
    let context_text = assemble_bounded_context_for_query(
        query_ir,
        question,
        &bundle.entities,
        &bundle.relationships,
        &bundle.chunks,
        &graph_evidence_lines,
        effective_context_budget,
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
mod tests {
    use crate::domains::query_ir::{
        QueryAct, QueryIR, QueryLanguage, QueryScope, SourceSliceDirection, SourceSliceFilter,
        SourceSliceSpec,
    };
    use uuid::Uuid;

    use super::*;

    fn runtime_chunk(kind: Option<&str>, score: f32) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: kind.map(str::to_string),
            document_label: "records.jsonl".to_string(),
            excerpt: "record".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(score),
            source_text: "record".to_string(),
        }
    }

    #[test]
    fn chunk_reference_snapshots_include_ordered_source_units() {
        let profile = runtime_chunk(Some("source_profile"), 4.0);
        let source_unit = runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0);
        let ordinary = runtime_chunk(Some("text"), 2.0);

        let snapshots = build_query_chunk_reference_snapshots(&[
            profile.clone(),
            source_unit.clone(),
            ordinary.clone(),
        ]);

        assert_eq!(snapshots.len(), 3);
        assert_eq!(snapshots[0].chunk_id, profile.chunk_id);
        assert_eq!(snapshots[0].rank, 1);
        assert_eq!(snapshots[1].chunk_id, source_unit.chunk_id);
        assert_eq!(snapshots[1].rank, 2);
        assert_eq!(snapshots[2].chunk_id, ordinary.chunk_id);
        assert_eq!(snapshots[2].rank, 3);
    }

    #[test]
    fn ordered_source_units_preserve_source_order() {
        let later = RuntimeMatchedChunk {
            chunk_index: 7,
            ..runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0)
        };
        let earlier = RuntimeMatchedChunk {
            chunk_index: 3,
            ..runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0)
        };
        let ordinary = runtime_chunk(Some("text"), 2.0);

        let units = collect_ordered_source_units(&[later.clone(), ordinary, earlier.clone()]);

        assert_eq!(units.len(), 2);
        assert_eq!(units[0].chunk_id, earlier.chunk_id);
        assert_eq!(units[1].chunk_id, later.chunk_id);
    }

    #[test]
    fn query_ir_target_types_expand_technical_literal_selection() {
        let question = "Which commands and settings configure scanning through RareProtocol?";
        let query_ir = QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![
                crate::domains::query_ir::QueryTargetKind::Protocol,
                crate::domains::query_ir::QueryTargetKind::Path,
                crate::domains::query_ir::QueryTargetKind::ConfigKey,
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.82,
        };
        let target_document_id = Uuid::now_v7();
        let target_chunk_id = Uuid::now_v7();
        let mut chunks = (0..14)
            .map(|index| RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: index,
                chunk_kind: None,
                document_label: format!("noisy-{index}.md"),
                excerpt: "General operations memo without command literals.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0 - (index as f32 * 0.01)),
                source_text: "General operations memo without command literals.".to_string(),
            })
            .collect::<Vec<_>>();
        chunks.push(RuntimeMatchedChunk {
            chunk_id: target_chunk_id,
            document_id: target_document_id,
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_label: "rare-protocol-scan-folder.md".to_string(),
            excerpt: "RareProtocol setup: create /srv/scans and set scan_share = writable."
                .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.42),
            source_text: "RareProtocol setup: create /srv/scans and set scan_share = writable."
                .to_string(),
        });
        let focus_keywords = technical_literal_focus_keywords(question, Some(&query_ir));

        let default_selected = select_technical_literal_chunks(
            question,
            &query_ir,
            &chunks,
            TechnicalLiteralIntent::default(),
            8,
            &focus_keywords,
            &[],
            false,
        );
        assert!(
            !default_selected.iter().any(|chunk| chunk.chunk_id == target_chunk_id),
            "default selection is capped before the later needle chunk"
        );

        let technical_intent = effective_technical_literal_intent(
            question,
            Some(&query_ir),
            TechnicalLiteralIntent::default(),
        );
        assert!(technical_intent.any());

        let expanded_selected = select_technical_literal_chunks(
            question,
            &query_ir,
            &chunks,
            technical_intent,
            8,
            &focus_keywords,
            &[],
            false,
        );

        assert!(
            expanded_selected.iter().any(|chunk| chunk.chunk_id == target_chunk_id),
            "QueryIR technical target types must keep later exact-literal evidence candidates"
        );
    }

    #[test]
    fn effective_technical_literal_intent_unions_planner_and_query_ir_signals() {
        let query_ir = QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![
                crate::domains::query_ir::QueryTargetKind::ConfigKey,
                crate::domains::query_ir::QueryTargetKind::Path,
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.8,
        };

        let intent = effective_technical_literal_intent(
            "Which settings should the client use?",
            Some(&query_ir),
            TechnicalLiteralIntent { wants_urls: true, ..TechnicalLiteralIntent::default() },
        );

        assert!(intent.wants_urls);
        assert!(intent.wants_paths);
        assert!(intent.wants_methods);
        assert!(intent.wants_parameters);
    }

    #[test]
    fn latest_version_queries_do_not_include_graph_context() {
        let query_ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::LibraryMeta,
            language: QueryLanguage::Auto,
            target_types: vec![
                crate::domains::query_ir::QueryTargetKind::Release,
                crate::domains::query_ir::QueryTargetKind::Version,
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: Some(SourceSliceSpec {
                direction: SourceSliceDirection::Tail,
                count: Some(10),
                filter: SourceSliceFilter::ReleaseMarker,
            }),
            retrieval_query: None,
            confidence: 0.9,
        };

        assert!(!include_graph_context_for_query(&query_ir));
    }
}
