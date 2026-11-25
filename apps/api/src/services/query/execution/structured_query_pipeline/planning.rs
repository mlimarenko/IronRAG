use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{query::RuntimeQueryMode, query_ir::QueryIR},
    services::query::{
        planner::{QueryPlanTaskInput, RuntimeQueryPlan, build_task_query_plan},
        provider_billing::QueryProviderExecutionContext,
        support::{
            IntentResolutionRequest, derive_query_planning_metadata,
            derive_query_planning_metadata_for_query_ir,
        },
        vector_dimensions::{
            validate_active_embedding_profile_key, validate_embedding_vector_dimensions,
        },
    },
};

use super::super::{
    embed::{
        EmbeddingProviderUnavailable, EmbeddingReservationFailure, QuestionEmbeddingResult,
        embed_question,
    },
    hyde::generate_hyde_passage,
    retrieve::{
        expanded_candidate_limit, load_document_index, load_graph_index,
        prepare_runtime_vector_search_context, validate_runtime_vector_search_context,
    },
    technical_literals::{TechnicalLiteralIntent, technical_literal_candidate_limit},
    types::{QueryVectorReadiness, RuntimeVectorSearchContext, StructuredQueryPlanningStage},
};

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
        Box::pin(compute_question_embeddings(state, execution_context, &plan, question, None))
            .await?;

    let graph_index_started = std::time::Instant::now();
    let (graph_index, graph_store_unavailable) = match load_graph_index(state, library_id).await {
        Ok(index) => (index, false),
        Err(error) if graph_projection_store_unavailable(&error) => {
            tracing::warn!(%library_id, "runtime graph projection store unavailable");
            (
                super::super::types::QueryGraphIndex::new(
                    std::sync::Arc::new(
                        crate::services::knowledge::runtime_read::ActiveRuntimeGraphProjection {
                            nodes: Vec::new(),
                            edges: Vec::new(),
                        },
                    ),
                    std::collections::HashMap::new(),
                    std::collections::HashMap::new(),
                ),
                true,
            )
        }
        Err(error) => return Err(error),
    };
    log_graph_index_loaded(
        library_id,
        graph_index_started.elapsed().as_millis(),
        graph_index.node_count(),
    );
    let document_index_started = std::time::Instant::now();
    let document_index = load_document_index(state, library_id).await?;
    log_document_index_loaded(
        library_id,
        document_index_started.elapsed().as_millis(),
        document_index.len(),
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
        vector_readiness: prepared_embeddings.vector_readiness,
        hyde_embedding: prepared_embeddings.hyde_embedding,
        vector_search_context: prepared_embeddings.vector_search_context,
        graph_index,
        graph_store_unavailable,
        document_index,
        candidate_limit,
    })
}

pub(super) fn graph_projection_store_unavailable(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<crate::services::knowledge::error::KnowledgeServiceError>(),
        Some(crate::services::knowledge::error::KnowledgeServiceError::Internal(inner))
            if inner.downcast_ref::<crate::services::knowledge::runtime_read::RuntimeGraphStoreUnavailable>().is_some()
    )
}

/// Embed the query string for the vector lane plus, when the plan asks for
/// it, a HyDE passage. Skipped entirely for exact-literal technical queries
/// where vector retrieval would only add noise.
struct PreparedQuestionEmbeddings {
    question_embedding: Vec<f32>,
    vector_readiness: QueryVectorReadiness,
    hyde_embedding: Option<Vec<f32>>,
    vector_search_context: Option<RuntimeVectorSearchContext>,
}

impl PreparedQuestionEmbeddings {
    fn empty_inventory() -> Self {
        Self {
            question_embedding: Vec::new(),
            vector_readiness: QueryVectorReadiness::EmptyInventory,
            hyde_embedding: None,
            vector_search_context: None,
        }
    }
}

fn log_graph_index_loaded(library_id: Uuid, elapsed_ms: u128, node_count: usize) {
    tracing::info!(
        stage = "plan.load_graph_index",
        library_id = %library_id,
        elapsed_ms,
        node_count,
        "graph index loaded for query planning"
    );
}

fn log_document_index_loaded(library_id: Uuid, elapsed_ms: u128, document_count: usize) {
    tracing::info!(
        stage = "plan.load_document_index",
        library_id = %library_id,
        elapsed_ms,
        document_count,
        "document index loaded for query planning"
    );
}

fn log_hyde_not_recommended() {
    tracing::debug!(
        stage = "hyde",
        hyde_recommended = false,
        "HyDE skipped — not recommended for this query intent"
    );
}

async fn compute_question_embeddings(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    plan: &RuntimeQueryPlan,
    question: &str,
    existing_vector_search_context: Option<&RuntimeVectorSearchContext>,
) -> anyhow::Result<PreparedQuestionEmbeddings> {
    let library_id = execution_context.library_id;
    let vector_search_context = match existing_vector_search_context {
        Some(context) => context.clone(),
        None => {
            let Some(context) = prepare_runtime_vector_search_context(state, library_id).await?
            else {
                return Ok(PreparedQuestionEmbeddings::empty_inventory());
            };
            context
        }
    };
    let embed_result =
        match Box::pin(embed_question(state, execution_context, question, &vector_search_context))
            .await
        {
            Ok(result) => result,
            Err(error) if error.downcast_ref::<EmbeddingProviderUnavailable>().is_some() => {
                validate_runtime_vector_search_context(state, library_id, &vector_search_context)
                    .await?;
                return Ok(PreparedQuestionEmbeddings {
                    question_embedding: Vec::new(),
                    vector_readiness: QueryVectorReadiness::ProviderUnavailable,
                    hyde_embedding: None,
                    vector_search_context: Some(vector_search_context),
                });
            }
            Err(error) => return Err(error),
        };
    validate_prepared_query_embedding(library_id, &vector_search_context, &embed_result)?;
    let question_embedding = embed_result.embedding.clone();
    let hyde_embedding = if plan.hyde_recommended {
        degrade_failed_hyde_embedding(
            Box::pin(compute_hyde_embedding(
                state,
                execution_context,
                question,
                &vector_search_context,
            ))
            .await,
        )?
    } else {
        log_hyde_not_recommended();
        None
    };
    Ok(PreparedQuestionEmbeddings {
        question_embedding,
        vector_readiness: QueryVectorReadiness::Ready,
        hyde_embedding,
        vector_search_context: Some(vector_search_context),
    })
}

/// HyDE is a retrieval enhancer, not a correctness dependency: when passage
/// generation or its embedding fails, the query must degrade to plain
/// question-embedding retrieval instead of failing the whole turn.
fn degrade_failed_hyde_embedding(
    result: anyhow::Result<Vec<f32>>,
) -> anyhow::Result<Option<Vec<f32>>> {
    match result {
        Ok(embedding) => Ok(Some(embedding)),
        Err(error) => {
            if error.downcast_ref::<EmbeddingReservationFailure>().is_some()
                || matches!(
                    error.downcast_ref::<crate::integrations::retry::ProviderCallError>(),
                    Some(crate::integrations::retry::ProviderCallError::Admission { .. })
                )
                || error
                    .downcast_ref::<crate::shared::provider_failure::ProviderFailure>()
                    .is_some_and(crate::shared::provider_failure::ProviderFailure::is_refusal)
            {
                return Err(error);
            }
            tracing::warn!(
                stage = "hyde",
                error = format!("{error:#}"),
                "HyDE embedding failed; continuing with question-only vector retrieval"
            );
            Ok(None)
        }
    }
}

pub(super) fn validate_prepared_query_embedding(
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

fn log_hyde_activated() {
    tracing::info!(stage = "hyde", hyde_recommended = true, "HyDE activated for this query");
}

fn log_hyde_passage(passage: &str) {
    tracing::debug!(stage = "hyde", passage_len = passage.len(), "HyDE passage generated");
}

fn log_hyde_passage_content(passage: &str) {
    tracing::trace!(stage = "hyde", passage = %passage, "HyDE passage content");
}

fn log_hyde_embedding_computed() {
    tracing::debug!(stage = "hyde_embed", "HyDE embedding computed");
}

async fn compute_hyde_embedding(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    question: &str,
    vector_search_context: &RuntimeVectorSearchContext,
) -> anyhow::Result<Vec<f32>> {
    let library_id = execution_context.library_id;
    log_hyde_activated();
    let passage = generate_hyde_passage(state, execution_context, question).await?;
    log_hyde_passage(&passage);
    log_hyde_passage_content(&passage);
    let hyde_result =
        Box::pin(embed_question(state, execution_context, &passage, vector_search_context))
            .await
            .context("failed to embed HyDE passage")?;
    validate_prepared_query_embedding(library_id, vector_search_context, &hyde_result)?;
    log_hyde_embedding_computed();
    Ok(hyde_result.embedding)
}

/// Inputs of the resolved standalone-retrieval-query replan.
pub(crate) struct ResolvedRetrievalReplanRequest<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) execution_context: QueryProviderExecutionContext,
    pub(crate) planning: &'a mut StructuredQueryPlanningStage,
    pub(crate) resolved_query: &'a str,
    pub(crate) mode: RuntimeQueryMode,
    pub(crate) top_k: usize,
    pub(crate) query_ir: &'a QueryIR,
}

/// Re-derive question-dependent planning for a resolved standalone retrieval query.
/// The loaded indexes are reused; metadata, plan, embeddings, and candidate limit
/// are recomputed for the resolved string.
pub(crate) async fn replan_for_resolved_retrieval_query(
    request: ResolvedRetrievalReplanRequest<'_>,
) -> anyhow::Result<()> {
    let ResolvedRetrievalReplanRequest {
        state,
        execution_context,
        planning,
        resolved_query,
        mode,
        top_k,
        query_ir,
    } = request;
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
    let prepared_embeddings = Box::pin(compute_question_embeddings(
        state,
        execution_context,
        &plan,
        resolved_query,
        existing_vector_search_context.as_ref(),
    ))
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
    planning.vector_readiness = prepared_embeddings.vector_readiness;
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

/// Inputs of the compiled-IR plan refresh, shared by the HyDE-free entry point
/// and the generic refresh helper.
pub(crate) struct CompiledIrPlanRefresh<'a> {
    pub(crate) planning: &'a mut StructuredQueryPlanningStage,
    pub(crate) retrieval_question: &'a str,
    pub(crate) mode: RuntimeQueryMode,
    pub(crate) top_k: usize,
    pub(crate) query_ir: &'a QueryIR,
    pub(crate) rerank_enabled: bool,
    pub(crate) rerank_candidate_limit: usize,
}

/// Finalize the IR-dependent part of the plan produced in parallel with query
/// compilation. The common path keeps the original question bytes, so its
/// base embedding and loaded indexes remain valid; only a newly enabled or
/// missing HyDE embedding may require provider work here.
pub(crate) async fn refresh_query_plan_for_compiled_ir(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    refresh: CompiledIrPlanRefresh<'_>,
) -> anyhow::Result<()> {
    let retrieval_question = refresh.retrieval_question;
    let vector_search_context = refresh.planning.vector_search_context.clone();
    refresh_query_plan_for_compiled_ir_with_hyde(refresh, move || async move {
        let context = vector_search_context.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "HyDE embedding requested without a ready request-scoped vector preflight"
            )
        })?;
        Box::pin(compute_hyde_embedding(state, execution_context, retrieval_question, context))
            .await
    })
    .await
}

pub(super) async fn refresh_query_plan_for_compiled_ir_with_hyde<F, Fut>(
    refresh: CompiledIrPlanRefresh<'_>,
    create_hyde_embedding: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<f32>>>,
{
    let CompiledIrPlanRefresh {
        planning,
        retrieval_question,
        mode,
        top_k,
        query_ir,
        rerank_enabled,
        rerank_candidate_limit,
    } = refresh;
    let (metadata, plan) = build_compiled_ir_query_plan(retrieval_question, mode, top_k, query_ir)?;
    let candidate_limit = expanded_candidate_limit(
        plan.planned_mode,
        plan.top_k,
        rerank_enabled,
        rerank_candidate_limit,
    )
    .max(technical_literal_candidate_limit(planning.technical_literal_intent, plan.top_k));
    let hyde_activation_changed = planning.plan.hyde_recommended != plan.hyde_recommended;
    let should_create_hyde = planning.vector_readiness == QueryVectorReadiness::Ready
        && planning.vector_search_context.is_some()
        && plan.hyde_recommended
        && (hyde_activation_changed || planning.hyde_embedding.is_none());
    let created_hyde_embedding = if should_create_hyde {
        degrade_failed_hyde_embedding(create_hyde_embedding().await)?
    } else {
        None
    };
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
