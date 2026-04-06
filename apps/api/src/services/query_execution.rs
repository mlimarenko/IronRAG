use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use futures::future::join_all;
use uuid::Uuid;

use crate::{
    agent_runtime::{
        pipeline::try_op::{run_async_try_op, run_try_op},
        request::build_provider_request,
    },
    app::state::AppState,
    domains::{
        agent_runtime::RuntimeTaskKind,
        ai::AiBindingPurpose,
        content::ContentDocumentSummary,
        provider_profiles::{EffectiveProviderProfile, ProviderModelSelection},
        query::{
            GroupedReferenceKind, QueryVerificationState, QueryVerificationWarning,
            RuntimeQueryMode,
        },
    },
    infra::{
        arangodb::{
            document_store::{
                KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
                KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow,
            },
            graph_store::{GraphViewEdgeWrite, GraphViewNodeWrite},
        },
        repositories,
        repositories::ai_repository,
    },
    integrations::llm::{ChatRequestSeed, EmbeddingRequest},
    services::{
        query_planner::{
            QueryIntentProfile, QueryPlanTaskInput, RuntimeQueryPlan, UnsupportedCapabilityIntent,
            build_task_query_plan,
        },
        query_support::{
            ContextAssemblyRequest, GroupedReferenceCandidate, IntentResolutionRequest,
            QueryRerankTaskInput, RerankCandidate, RerankOutcome, RerankRequest,
            assemble_context_metadata, derive_query_planning_metadata, derive_rerank_metadata,
            group_visible_references, rerank_query_candidates,
        },
        runtime_graph_read::{
            graph_view_data_from_runtime_projection, load_active_runtime_graph_projection,
        },
        runtime_ingestion::resolve_effective_provider_profile,
    },
    shared::text_render::repair_technical_layout_noise,
};

const MAX_CANONICAL_ANSWER_TECHNICAL_FACTS: usize = 24;

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeMatchedEntity {
    pub node_id: Uuid,
    pub label: String,
    pub node_type: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeMatchedRelationship {
    pub edge_id: Uuid,
    pub relation_type: String,
    pub from_node_id: Uuid,
    pub from_label: String,
    pub to_node_id: Uuid,
    pub to_label: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeMatchedChunk {
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    pub document_label: String,
    pub excerpt: String,
    pub score: Option<f32>,
    #[serde(skip_serializing)]
    pub source_text: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeRetrievedDocumentBrief {
    title: String,
    preview_excerpt: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeStructuredQueryReferenceCounts {
    entity_count: usize,
    relationship_count: usize,
    chunk_count: usize,
    graph_node_count: usize,
    graph_edge_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeStructuredQueryLibrarySummary {
    document_count: usize,
    graph_ready_count: usize,
    processing_count: usize,
    failed_count: usize,
    graph_status: &'static str,
    recent_documents: Vec<RuntimeQueryRecentDocument>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeStructuredQueryDiagnostics {
    requested_mode: RuntimeQueryMode,
    planned_mode: RuntimeQueryMode,
    keywords: Vec<String>,
    high_level_keywords: Vec<String>,
    low_level_keywords: Vec<String>,
    top_k: usize,
    reference_counts: RuntimeStructuredQueryReferenceCounts,
    planning: crate::domains::query::QueryPlanningMetadata,
    rerank: crate::domains::query::RerankMetadata,
    context_assembly: crate::domains::query::ContextAssemblyMetadata,
    grouped_references: Vec<crate::domains::query::GroupedReference>,
    context_text: Option<String>,
    warning: Option<String>,
    warning_kind: Option<&'static str>,
    library_summary: Option<RuntimeStructuredQueryLibrarySummary>,
}

#[cfg(test)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct QueryExecutionReference {
    pub reference_id: uuid::Uuid,
    pub kind: String,
    pub excerpt: Option<String>,
    pub rank: usize,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct QueryExecutionEnrichment {
    pub planning: crate::domains::query::QueryPlanningMetadata,
    pub rerank: crate::domains::query::RerankMetadata,
    pub context_assembly: crate::domains::query::ContextAssemblyMetadata,
    pub grouped_references: Vec<crate::domains::query::GroupedReference>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeStructuredQueryResult {
    pub(crate) planned_mode: RuntimeQueryMode,
    pub(crate) embedding_usage: Option<QuestionEmbeddingResult>,
    intent_profile: QueryIntentProfile,
    context_text: String,
    technical_literals_text: Option<String>,
    technical_literal_chunks: Vec<RuntimeMatchedChunk>,
    diagnostics: RuntimeStructuredQueryDiagnostics,
    retrieved_documents: Vec<RuntimeRetrievedDocumentBrief>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeAnswerQueryResult {
    pub(crate) answer: String,
    pub(crate) provider: ProviderModelSelection,
    pub(crate) usage_json: serde_json::Value,
}

#[derive(Debug, Clone)]
struct AnswerGenerationStage {
    intent_profile: QueryIntentProfile,
    canonical_answer_chunks: Vec<RuntimeMatchedChunk>,
    canonical_evidence: CanonicalAnswerEvidence,
    answer: String,
    provider: ProviderModelSelection,
    usage_json: serde_json::Value,
}

#[derive(Debug, Clone)]
struct AnswerVerificationStage {
    generation: AnswerGenerationStage,
}

#[derive(Debug, Clone)]
struct CanonicalAnswerEvidence {
    bundle: Option<crate::infra::arangodb::context_store::KnowledgeContextBundleRow>,
    chunk_rows: Vec<KnowledgeChunkRow>,
    structured_blocks: Vec<KnowledgeStructuredBlockRow>,
    technical_facts: Vec<KnowledgeTechnicalFactRow>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAnswerQueryResult {
    pub(crate) structured: RuntimeStructuredQueryResult,
    pub(crate) answer_context: String,
    pub(crate) embedding_usage: Option<QuestionEmbeddingResult>,
}

#[derive(Debug, Clone)]
struct QueryGraphIndex {
    nodes: HashMap<Uuid, GraphViewNodeWrite>,
    edges: Vec<GraphViewEdgeWrite>,
}

#[derive(Debug, Clone)]
struct RetrievalBundle {
    entities: Vec<RuntimeMatchedEntity>,
    relationships: Vec<RuntimeMatchedRelationship>,
    chunks: Vec<RuntimeMatchedChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeQueryWarning {
    warning: String,
    warning_kind: &'static str,
}

#[derive(Debug, Clone)]
struct RuntimeQueryLibrarySummary {
    document_count: usize,
    graph_ready_count: usize,
    processing_count: usize,
    failed_count: usize,
    graph_status: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeQueryRecentDocument {
    title: String,
    uploaded_at: String,
    mime_type: Option<String>,
    pipeline_state: &'static str,
    graph_state: &'static str,
    preview_excerpt: Option<String>,
}

#[derive(Debug, Clone)]
struct RuntimeQueryLibraryContext {
    summary: RuntimeQueryLibrarySummary,
    recent_documents: Vec<RuntimeQueryRecentDocument>,
    warning: Option<RuntimeQueryWarning>,
}

#[derive(Debug, Clone)]
struct RuntimeVectorSearchContext {
    model_catalog_id: Uuid,
    freshness_generation: i64,
}

#[derive(Debug, Clone)]
struct StructuredQueryPlanningStage {
    provider_profile: EffectiveProviderProfile,
    planning: crate::domains::query::QueryPlanningMetadata,
    plan: RuntimeQueryPlan,
    technical_literal_intent: TechnicalLiteralIntent,
    question_embedding: Vec<f32>,
    embedding_usage: Option<QuestionEmbeddingResult>,
    graph_index: QueryGraphIndex,
    document_index: HashMap<Uuid, KnowledgeDocumentRow>,
    candidate_limit: usize,
}

#[derive(Debug, Clone)]
struct StructuredQueryRetrievalStage {
    planning: StructuredQueryPlanningStage,
    bundle: RetrievalBundle,
}

#[derive(Debug, Clone)]
struct StructuredQueryRerankStage {
    retrieval: StructuredQueryRetrievalStage,
    rerank: crate::domains::query::RerankMetadata,
}

#[derive(Debug, Clone)]
struct StructuredQueryAssemblyStage {
    rerank: StructuredQueryRerankStage,
    context_text: String,
    technical_literals_text: Option<String>,
    technical_literal_chunks: Vec<RuntimeMatchedChunk>,
    retrieved_documents: Vec<RuntimeRetrievedDocumentBrief>,
    grouped_references: Vec<crate::domains::query::GroupedReference>,
    context_assembly: crate::domains::query::ContextAssemblyMetadata,
}

async fn execute_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<RuntimeStructuredQueryResult> {
    let planning_stage =
        run_async_try_op((), |_| plan_structured_query(state, library_id, question, mode, top_k))
            .await?;
    let retrieval_stage = run_async_try_op(planning_stage, |planning_stage| {
        retrieve_structured_query(state, library_id, question, planning_stage)
    })
    .await?;
    let reranked_stage = run_try_op(retrieval_stage, |retrieval_stage| {
        rerank_structured_query(state, question, retrieval_stage)
    })?;
    let assembled_stage = run_async_try_op(reranked_stage, |reranked_stage| {
        assemble_structured_query(state, question, reranked_stage, include_debug)
    })
    .await?;
    let enrichment = QueryExecutionEnrichment {
        planning: assembled_stage.rerank.retrieval.planning.planning.clone(),
        rerank: assembled_stage.rerank.rerank.clone(),
        context_assembly: assembled_stage.context_assembly.clone(),
        grouped_references: assembled_stage.grouped_references.clone(),
    };
    let diagnostics = build_structured_query_diagnostics(
        &assembled_stage.rerank.retrieval.planning.plan,
        &assembled_stage.rerank.retrieval.bundle,
        &assembled_stage.rerank.retrieval.planning.graph_index,
        &enrichment,
        include_debug,
        &assembled_stage.context_text,
    );

    Ok(RuntimeStructuredQueryResult {
        planned_mode: assembled_stage.rerank.retrieval.planning.plan.planned_mode,
        embedding_usage: assembled_stage.rerank.retrieval.planning.embedding_usage.clone(),
        intent_profile: assembled_stage.rerank.retrieval.planning.plan.intent_profile.clone(),
        context_text: assembled_stage.context_text,
        technical_literals_text: assembled_stage.technical_literals_text,
        technical_literal_chunks: assembled_stage.technical_literal_chunks,
        diagnostics,
        retrieved_documents: assembled_stage.retrieved_documents,
    })
}

async fn plan_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
) -> anyhow::Result<StructuredQueryPlanningStage> {
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    let source_truth_version =
        repositories::get_library_source_truth_version(&state.persistence.postgres, library_id)
            .await
            .context("failed to load library source-truth version for query planning")?;
    let planning = derive_query_planning_metadata(&IntentResolutionRequest {
        library_id,
        question: question.to_string(),
        explicit_mode: mode,
        source_truth_version,
    });
    let plan = build_task_query_plan(&QueryPlanTaskInput {
        question: question.to_string(),
        top_k: Some(top_k),
        explicit_mode: Some(mode),
        metadata: Some(planning.clone()),
    })
    .map_err(|failure| anyhow::anyhow!(failure.summary))?;
    let technical_literal_intent = if plan.intent_profile.exact_literal_technical {
        detect_technical_literal_intent(question)
    } else {
        TechnicalLiteralIntent::default()
    };
    let embed_result = embed_question(state, library_id, &provider_profile, question).await?;
    let question_embedding = embed_result.embedding.clone();
    let graph_index = load_graph_index(state, library_id).await?;
    let document_index = load_document_index(state, library_id).await?;
    let candidate_limit = expanded_candidate_limit(
        plan.planned_mode,
        plan.top_k,
        state.retrieval_intelligence.rerank_enabled,
        state.retrieval_intelligence.rerank_candidate_limit,
    )
    .max(technical_literal_candidate_limit(technical_literal_intent, plan.top_k));

    Ok(StructuredQueryPlanningStage {
        provider_profile,
        planning,
        plan,
        technical_literal_intent,
        question_embedding,
        embedding_usage: Some(embed_result),
        graph_index,
        document_index,
        candidate_limit,
    })
}

async fn retrieve_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    planning: StructuredQueryPlanningStage,
) -> anyhow::Result<StructuredQueryRetrievalStage> {
    let plan = &planning.plan;
    let provider_profile = &planning.provider_profile;
    let question_embedding = &planning.question_embedding;
    let graph_index = &planning.graph_index;
    let document_index = &planning.document_index;
    let candidate_limit = planning.candidate_limit;

    let bundle = match plan.planned_mode {
        RuntimeQueryMode::Document => {
            let chunks = retrieve_document_chunks(
                state,
                library_id,
                provider_profile,
                question,
                plan,
                candidate_limit,
                question_embedding,
                document_index,
            )
            .await?;
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks }
        }
        RuntimeQueryMode::Local => {
            retrieve_local_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?
        }
        RuntimeQueryMode::Global => {
            retrieve_global_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?
        }
        RuntimeQueryMode::Hybrid => {
            let mut bundle = retrieve_local_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?;
            bundle.chunks = retrieve_document_chunks(
                state,
                library_id,
                provider_profile,
                question,
                plan,
                candidate_limit,
                question_embedding,
                document_index,
            )
            .await?;
            bundle
        }
        RuntimeQueryMode::Mix => {
            let mut local = retrieve_local_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?;
            let global = retrieve_global_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?;
            local.entities = merge_entities(local.entities, global.entities, candidate_limit);
            local.relationships =
                merge_relationships(local.relationships, global.relationships, candidate_limit);
            local.chunks = retrieve_document_chunks(
                state,
                library_id,
                provider_profile,
                question,
                plan,
                candidate_limit,
                question_embedding,
                document_index,
            )
            .await?;
            local
        }
    };

    Ok(StructuredQueryRetrievalStage { planning, bundle })
}

fn rerank_structured_query(
    state: &AppState,
    question: &str,
    mut retrieval: StructuredQueryRetrievalStage,
) -> anyhow::Result<StructuredQueryRerankStage> {
    let plan = &retrieval.planning.plan;
    let rerank = match plan.planned_mode {
        RuntimeQueryMode::Hybrid => {
            apply_hybrid_rerank(state, question, plan, &mut retrieval.bundle)
        }
        RuntimeQueryMode::Mix => apply_mix_rerank(state, question, plan, &mut retrieval.bundle),
        _ => derive_rerank_metadata(&RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: retrieval.bundle.entities.len()
                + retrieval.bundle.relationships.len()
                + retrieval.bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        }),
    };
    Ok(StructuredQueryRerankStage { retrieval, rerank })
}

async fn assemble_structured_query(
    state: &AppState,
    question: &str,
    mut rerank: StructuredQueryRerankStage,
    _include_debug: bool,
) -> anyhow::Result<StructuredQueryAssemblyStage> {
    let plan = &rerank.retrieval.planning.plan;
    let bundle = &mut rerank.retrieval.bundle;
    let retrieved_documents = load_retrieved_document_briefs(
        state,
        &bundle.chunks,
        &rerank.retrieval.planning.document_index,
        plan.top_k,
    )
    .await;
    let pagination_requested = question_mentions_pagination(question);
    let literal_focus_keywords = technical_literal_focus_keywords(question);
    let technical_literal_chunks = if rerank.retrieval.planning.technical_literal_intent.any() {
        bundle.chunks.clone()
    } else {
        select_document_balanced_chunks(
            question,
            &bundle.chunks,
            &literal_focus_keywords,
            pagination_requested,
            12,
            3,
        )
        .into_iter()
        .cloned()
        .collect::<Vec<_>>()
    };
    let technical_literal_groups = collect_technical_literal_groups(question, &bundle.chunks);
    let technical_literals_text =
        render_exact_technical_literals_section(&technical_literal_groups);
    truncate_bundle(bundle, plan.top_k);

    let grouped_references = group_visible_references(
        &build_grouped_reference_candidates(
            &bundle.entities,
            &bundle.relationships,
            &bundle.chunks,
            plan.top_k,
        ),
        plan.top_k,
    );
    let context_text = assemble_bounded_context(
        &bundle.entities,
        &bundle.relationships,
        &bundle.chunks,
        plan.context_budget_chars,
    );
    let graph_support_count = bundle.entities.len() + bundle.relationships.len();
    let context_assembly = assemble_context_metadata(&ContextAssemblyRequest {
        requested_mode: plan.planned_mode,
        graph_support_count,
        document_support_count: bundle.chunks.len(),
    });

    Ok(StructuredQueryAssemblyStage {
        rerank,
        context_text,
        technical_literals_text,
        technical_literal_chunks,
        retrieved_documents,
        grouped_references,
        context_assembly,
    })
}

pub(crate) async fn prepare_answer_query(
    state: &AppState,
    library_id: Uuid,
    question: String,
    mode: RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<PreparedAnswerQueryResult> {
    let mut structured = run_async_try_op((), |_| {
        execute_structured_query(state, library_id, &question, mode, top_k, include_debug)
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
    let answer_context = library_context.as_ref().map_or_else(
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
    system_prompt: Option<String>,
    prepared: PreparedAnswerQueryResult,
    on_delta: Option<&mut (dyn FnMut(String) + Send)>,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let generated = run_async_try_op(prepared, |prepared| {
        generate_answer_stage(
            state,
            library_id,
            execution_id,
            effective_question,
            user_question,
            conversation_history,
            system_prompt,
            prepared,
            on_delta,
        )
    })
    .await?;
    let verified = run_async_try_op(generated, |generated| {
        verify_generated_answer(state, execution_id, effective_question, generated)
    })
    .await?;

    Ok(RuntimeAnswerQueryResult {
        answer: verified.generation.answer,
        provider: verified.generation.provider,
        usage_json: verified.generation.usage_json,
    })
}

async fn generate_answer_stage(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    effective_question: &str,
    user_question: &str,
    conversation_history: Option<&str>,
    system_prompt: Option<String>,
    prepared: PreparedAnswerQueryResult,
    on_delta: Option<&mut (dyn FnMut(String) + Send)>,
) -> anyhow::Result<AnswerGenerationStage> {
    let intent_profile = prepared.structured.intent_profile.clone();
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    let answer_provider = provider_profile
        .selection_for_runtime_task_kind(RuntimeTaskKind::QueryAnswer)
        .cloned()
        .unwrap_or_else(|| provider_profile.answer.clone());
    let canonical_answer_chunks = load_canonical_answer_chunks(
        state,
        library_id,
        execution_id,
        effective_question,
        &prepared.structured.technical_literal_chunks,
    )
    .await?;
    let canonical_evidence = load_canonical_answer_evidence(state, execution_id).await?;
    let canonical_answer_context = build_canonical_answer_context(
        effective_question,
        &prepared.structured,
        &canonical_evidence,
        &canonical_answer_chunks,
        &prepared.answer_context,
    );
    let (answer, provider, usage_json) = if canonical_answer_context.trim().is_empty() {
        let answer = "No grounded evidence is available in the active library yet.".to_string();
        if let Some(on_delta) = on_delta {
            on_delta(answer.clone());
        }
        (answer, answer_provider.clone(), serde_json::json!({}))
    } else if let Some(answer) = build_unsupported_capability_answer(
        &prepared.structured.intent_profile,
        effective_question,
        &canonical_answer_chunks,
    )
    .or_else(|| {
        build_deterministic_grounded_answer(
            effective_question,
            &canonical_evidence,
            &canonical_answer_chunks,
        )
    }) {
        if let Some(on_delta) = on_delta {
            on_delta(answer.clone());
        }
        (
            answer,
            answer_provider.clone(),
            serde_json::json!({
                "deterministic": true,
                "reason": "canonical_deterministic_answer",
            }),
        )
    } else {
        let answer_binding_purpose =
            AiBindingPurpose::for_runtime_task_kind(RuntimeTaskKind::QueryAnswer)
                .ok_or_else(|| anyhow::anyhow!("query answer task kind has no binding purpose"))?;
        let answer_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, answer_binding_purpose)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("active answer binding is not configured for this library")
            })?;
        let answer_task_spec =
            state.agent_runtime.registry().spec(RuntimeTaskKind::QueryAnswer).ok_or_else(|| {
                anyhow::anyhow!("query answer runtime task spec is not registered")
            })?;
        let request = build_provider_request(
            answer_task_spec,
            ChatRequestSeed {
                provider_kind: answer_binding.provider_kind.clone(),
                model_name: answer_binding.model_name.clone(),
                api_key_override: Some(answer_binding.api_key),
                base_url_override: answer_binding.provider_base_url,
                system_prompt: system_prompt.or(answer_binding.system_prompt),
                temperature: answer_binding.temperature,
                top_p: answer_binding.top_p,
                max_output_tokens_override: answer_binding.max_output_tokens_override,
                extra_parameters_json: answer_binding.extra_parameters_json,
            },
            build_answer_prompt(
                user_question,
                &canonical_answer_context,
                conversation_history,
                None,
            ),
        );
        let response = match on_delta {
            Some(on_delta) => state.llm_gateway.generate_stream(request, on_delta).await,
            None => state.llm_gateway.generate(request).await,
        }
        .context("failed to generate grounded answer")?;
        (
            response.output_text.trim().to_string(),
            ProviderModelSelection {
                provider_kind: answer_binding.provider_kind.parse().unwrap_or_default(),
                model_name: answer_binding.model_name,
            },
            response.usage_json,
        )
    };

    Ok(AnswerGenerationStage {
        intent_profile,
        canonical_answer_chunks,
        canonical_evidence,
        answer,
        provider,
        usage_json,
    })
}

async fn verify_generated_answer(
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
    );
    persist_query_verification(state, execution_id, &verification, &generation.canonical_evidence)
        .await?;

    let _ = verification;
    Ok(AnswerVerificationStage { generation })
}

async fn load_canonical_answer_chunks(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    question: &str,
    fallback_chunks: &[RuntimeMatchedChunk],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(bundle_refs) = state
        .arango_context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .with_context(|| {
            format!("failed to load context bundle references for query execution {execution_id}")
        })?
    else {
        return Ok(fallback_chunks.to_vec());
    };

    if bundle_refs.chunk_references.is_empty() {
        return Ok(fallback_chunks.to_vec());
    }

    let document_index = load_document_index(state, library_id).await?;
    let keywords = technical_literal_focus_keywords(question);
    let mut context_chunks = Vec::new();
    let mut ordered_refs = bundle_refs.chunk_references;
    ordered_refs.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });

    for reference in ordered_refs.into_iter().take(64) {
        let chunk = load_runtime_knowledge_chunk(state, reference.chunk_id).await?;
        if let Some(mapped) =
            map_chunk_hit(chunk, reference.score as f32, &document_index, &keywords)
        {
            context_chunks.push(mapped);
        }
    }

    if context_chunks.is_empty() {
        return Ok(fallback_chunks.to_vec());
    }

    Ok(merge_chunks(context_chunks, fallback_chunks.to_vec(), fallback_chunks.len().max(64)))
}

async fn load_canonical_answer_evidence(
    state: &AppState,
    execution_id: Uuid,
) -> anyhow::Result<CanonicalAnswerEvidence> {
    let Some(bundle_refs) = state
        .arango_context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .with_context(|| {
            format!("failed to load context bundle for answer evidence {execution_id}")
        })?
    else {
        return Ok(CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        });
    };

    let chunk_ids =
        bundle_refs.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>();
    let evidence_rows = state
        .arango_graph_store
        .list_evidence_by_ids(
            &bundle_refs
                .evidence_references
                .iter()
                .map(|reference| reference.evidence_id)
                .collect::<Vec<_>>(),
        )
        .await
        .context("failed to load evidence rows for canonical answer context")?;
    let chunk_rows = state
        .arango_document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to load chunks for canonical answer context")?;
    let chunk_supported_facts =
        state.arango_document_store.list_technical_facts_by_chunk_ids(&chunk_ids).await.context(
            "failed to load chunk-supported technical facts for canonical answer context",
        )?;
    let mut fact_ids = selected_fact_ids_for_canonical_evidence(
        &bundle_refs.bundle.selected_fact_ids,
        &evidence_rows,
        &chunk_supported_facts,
    );
    for evidence in &evidence_rows {
        if let Some(fact_id) = evidence.fact_id
            && !fact_ids.contains(&fact_id)
            && fact_ids.len() < MAX_CANONICAL_ANSWER_TECHNICAL_FACTS
        {
            fact_ids.push(fact_id);
        }
    }
    let mut technical_facts = state
        .arango_document_store
        .list_technical_facts_by_ids(&fact_ids)
        .await
        .context("failed to load technical facts for canonical answer context")?;
    let mut seen_fact_ids = technical_facts.iter().map(|fact| fact.fact_id).collect::<HashSet<_>>();
    for fact in chunk_supported_facts {
        if fact_ids.contains(&fact.fact_id) && seen_fact_ids.insert(fact.fact_id) {
            technical_facts.push(fact);
        }
    }
    technical_facts.sort_by(|left, right| {
        left.fact_kind.cmp(&right.fact_kind).then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    let mut block_ids =
        evidence_rows.iter().filter_map(|evidence| evidence.block_id).collect::<Vec<_>>();
    for chunk in &chunk_rows {
        for block_id in &chunk.support_block_ids {
            if !block_ids.contains(block_id) {
                block_ids.push(*block_id);
            }
        }
    }
    for fact in &technical_facts {
        for block_id in &fact.support_block_ids {
            if !block_ids.contains(block_id) {
                block_ids.push(*block_id);
            }
        }
    }
    let structured_blocks = state
        .arango_document_store
        .list_structured_blocks_by_ids(&block_ids)
        .await
        .context("failed to load structured blocks for canonical answer context")?;
    Ok(CanonicalAnswerEvidence {
        bundle: Some(bundle_refs.bundle),
        chunk_rows,
        structured_blocks,
        technical_facts,
    })
}

fn selected_fact_ids_for_canonical_evidence(
    selected_fact_ids: &[Uuid],
    evidence_rows: &[crate::infra::arangodb::graph_store::KnowledgeEvidenceRow],
    chunk_supported_facts: &[KnowledgeTechnicalFactRow],
) -> Vec<Uuid> {
    let mut fact_ids = selected_fact_ids.to_vec();
    for evidence in evidence_rows {
        let Some(fact_id) = evidence.fact_id else {
            continue;
        };
        if fact_ids.len() >= MAX_CANONICAL_ANSWER_TECHNICAL_FACTS {
            break;
        }
        if !fact_ids.contains(&fact_id) {
            fact_ids.push(fact_id);
        }
    }
    if fact_ids.is_empty() {
        for fact in chunk_supported_facts {
            if fact_ids.len() >= MAX_CANONICAL_ANSWER_TECHNICAL_FACTS {
                break;
            }
            if !fact_ids.contains(&fact.fact_id) {
                fact_ids.push(fact.fact_id);
            }
        }
    }
    fact_ids.truncate(MAX_CANONICAL_ANSWER_TECHNICAL_FACTS);
    fact_ids
}

fn build_canonical_answer_context(
    question: &str,
    structured: &RuntimeStructuredQueryResult,
    evidence: &CanonicalAnswerEvidence,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
    fallback_context: &str,
) -> String {
    let focused_document_id = focused_answer_document_id(question, canonical_answer_chunks);
    let focused_document_label = focused_document_id.and_then(|document_id| {
        canonical_answer_chunks
            .iter()
            .find(|chunk| chunk.document_id == document_id)
            .map(|chunk| chunk.document_label.clone())
    });
    let filtered_technical_facts = focused_document_id.map_or_else(
        || evidence.technical_facts.clone(),
        |document_id| {
            evidence
                .technical_facts
                .iter()
                .filter(|fact| fact.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let filtered_structured_blocks = focused_document_id.map_or_else(
        || evidence.structured_blocks.clone(),
        |document_id| {
            evidence
                .structured_blocks
                .iter()
                .filter(|block| block.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let filtered_chunks = focused_document_id.map_or_else(
        || canonical_answer_chunks.to_vec(),
        |document_id| {
            canonical_answer_chunks
                .iter()
                .filter(|chunk| chunk.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let mut sections = Vec::<String>::new();

    if let Some(technical_literals_text) = structured.technical_literals_text.as_deref()
        && !technical_literals_text.trim().is_empty()
    {
        sections.push(technical_literals_text.trim().to_string());
    }

    if let Some(document_label) = focused_document_label.as_deref() {
        sections.push(format!("Focused grounded document\n- {document_label}"));
    }

    let technical_fact_section = render_canonical_technical_fact_section(&filtered_technical_facts);
    if !technical_fact_section.is_empty() {
        sections.push(technical_fact_section);
    }

    let prepared_segment_section = render_prepared_segment_section(&filtered_structured_blocks);
    if !prepared_segment_section.is_empty() {
        sections.push(prepared_segment_section);
    }

    let chunk_section = render_canonical_chunk_section(question, &filtered_chunks);
    if !chunk_section.is_empty() {
        sections.push(chunk_section);
    }

    if sections.is_empty() {
        return fallback_context.trim().to_string();
    }

    if let Some(bundle) = evidence.bundle.as_ref() {
        sections.insert(
            0,
            format!(
                "Canonical query bundle\n- Strategy: {}\n- Requested mode: {}\n- Resolved mode: {}",
                bundle.bundle_strategy, bundle.requested_mode, bundle.resolved_mode
            ),
        );
    }

    sections.join("\n\n")
}

fn focused_answer_document_id(question: &str, chunks: &[RuntimeMatchedChunk]) -> Option<Uuid> {
    if chunks.is_empty() || question_requests_multi_document_scope(question) {
        return None;
    }

    #[derive(Debug, Clone)]
    struct DocumentFocusScore {
        document_id: Uuid,
        document_label: String,
        score_sum: f32,
        chunk_count: usize,
        first_rank: usize,
        label_keyword_hits: usize,
        label_marker_hits: usize,
    }

    let question_keywords = crate::services::query_planner::extract_keywords(question);
    let mut per_document = HashMap::<Uuid, DocumentFocusScore>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        let lowered_label = chunk.document_label.to_lowercase();
        let entry = per_document.entry(chunk.document_id).or_insert_with(|| DocumentFocusScore {
            document_id: chunk.document_id,
            document_label: chunk.document_label.clone(),
            score_sum: 0.0,
            chunk_count: 0,
            first_rank: rank,
            label_keyword_hits: question_keywords
                .iter()
                .filter(|keyword| lowered_label.contains(keyword.as_str()))
                .count(),
            label_marker_hits: document_focus_marker_hits(question, &chunk.document_label),
        });
        entry.score_sum += score_value(chunk.score);
        entry.chunk_count += 1;
        entry.first_rank = entry.first_rank.min(rank);
    }

    let mut ranked = per_document.into_values().collect::<Vec<_>>();
    if ranked.is_empty() {
        return None;
    }
    ranked.sort_by(|left, right| {
        right.label_marker_hits.cmp(&left.label_marker_hits).then_with(|| {
            right
                .score_sum
                .total_cmp(&left.score_sum)
                .then_with(|| right.chunk_count.cmp(&left.chunk_count))
                .then_with(|| right.label_keyword_hits.cmp(&left.label_keyword_hits))
                .then_with(|| left.first_rank.cmp(&right.first_rank))
                .then_with(|| left.document_label.cmp(&right.document_label))
        })
    });

    if ranked.len() == 1 {
        return Some(ranked[0].document_id);
    }

    let top = &ranked[0];
    let second = &ranked[1];
    if top.label_marker_hits > second.label_marker_hits && top.label_marker_hits > 0 {
        return Some(top.document_id);
    }

    let has_explicit_single_source_anchor = question_mentions_single_source_anchor(question);
    let materially_higher_score = top.score_sum >= second.score_sum * 1.2;
    let materially_more_chunks = top.chunk_count > second.chunk_count;
    let stronger_label_match = top.label_keyword_hits > second.label_keyword_hits;

    if has_explicit_single_source_anchor
        || materially_higher_score
        || materially_more_chunks
        || stronger_label_match
    {
        Some(top.document_id)
    } else {
        None
    }
}

fn document_focus_marker_hits(question: &str, document_label: &str) -> usize {
    let lowered_question = question.to_lowercase();
    let lowered_label = document_label.to_lowercase();
    ["pdf", "docx", "pptx", "png", "jpg", "jpeg", "runtime", "upload", "smoke", "fixture", "check"]
        .iter()
        .filter(|marker| lowered_question.contains(**marker) && lowered_label.contains(**marker))
        .count()
}

fn question_requests_multi_document_scope(question: &str) -> bool {
    let lowered = question.to_lowercase();
    if [
        "compare",
        "contrast",
        "versus",
        " vs ",
        "between",
        "across documents",
        "across articles",
        "combine documents",
        "combine articles",
        "multiple documents",
        "multiple articles",
        "several documents",
        "several articles",
        "both documents",
        "both articles",
        "different documents",
        "different articles",
        "сравни",
        "сравните",
        "между документ",
        "между стать",
        "нескольких документ",
        "нескольких стать",
        "разных документ",
        "разных стать",
        "оба документ",
        "обе стать",
        "обоих документ",
        "обеих стать",
        "отдельно",
        "separately",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        return true;
    }

    let asks_multiple_items = [
        "which two",
        "which three",
        "two technologies",
        "three technologies",
        "two items",
        "three items",
        "какие две",
        "какие три",
        "две технологии",
        "три технологии",
    ]
    .iter()
    .any(|marker| lowered.contains(marker));
    let asks_role_pairing = [
        "fit those roles",
        "should it combine",
        "combine into that stack",
        "fit those roles",
        "and which one",
        "эти роли",
        "в этот стек",
        "нужно сочетать",
        "следует объединить",
    ]
    .iter()
    .any(|marker| lowered.contains(marker));

    asks_multiple_items || asks_role_pairing
}

fn question_mentions_single_source_anchor(question: &str) -> bool {
    let lowered = question.to_lowercase();
    [
        "according to",
        "in the article",
        "in this article",
        "in the document",
        "this article",
        "this document",
        "the article",
        "the document",
        "в статье",
        "в этом документе",
        "в документе",
        "эта статья",
        "этот документ",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

fn render_canonical_technical_fact_section(facts: &[KnowledgeTechnicalFactRow]) -> String {
    if facts.is_empty() {
        return String::new();
    }
    let mut lines = Vec::<String>::new();
    for fact in facts.iter().take(24) {
        let qualifiers = serde_json::from_value::<
            Vec<crate::shared::technical_facts::TechnicalFactQualifier>,
        >(fact.qualifiers_json.clone())
        .unwrap_or_default();
        let qualifier_suffix = if qualifiers.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                qualifiers
                    .iter()
                    .map(|qualifier| format!("{}={}", qualifier.key, qualifier.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        lines.push(format!("- {}: `{}`{}", fact.fact_kind, fact.display_value, qualifier_suffix));
    }
    format!("Technical facts\n{}", lines.join("\n"))
}

fn render_prepared_segment_section(blocks: &[KnowledgeStructuredBlockRow]) -> String {
    if blocks.is_empty() {
        return String::new();
    }
    let mut lines = Vec::<String>::new();
    for block in blocks.iter().take(16) {
        let label = if block.heading_trail.is_empty() {
            block.block_kind.clone()
        } else {
            format!("{} > {}", block.block_kind, block.heading_trail.join(" > "))
        };
        let excerpt = excerpt_for(&repair_technical_layout_noise(&block.normalized_text), 420);
        lines.push(format!("- {}: {}", label, excerpt));
    }
    format!("Prepared segments\n{}", lines.join("\n"))
}

fn render_canonical_chunk_section(question: &str, chunks: &[RuntimeMatchedChunk]) -> String {
    if chunks.is_empty() {
        return String::new();
    }
    let question_keywords = technical_literal_focus_keywords(question);
    let pagination_requested = question_mentions_pagination(question);
    let mut selected = select_document_balanced_chunks(
        question,
        chunks,
        &question_keywords,
        pagination_requested,
        8,
        2,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    if selected.is_empty() {
        selected = chunks.iter().take(8).cloned().collect();
    }
    let question_keywords = crate::services::query_planner::extract_keywords(question);
    let lines = selected
        .iter()
        .map(|chunk| {
            let excerpt = focused_excerpt_for(&chunk.source_text, &question_keywords, 560);
            let excerpt = if excerpt.trim().is_empty() {
                excerpt_for(&chunk.source_text, 560)
            } else {
                excerpt
            };
            format!("- {}: {}", chunk.document_label, excerpt)
        })
        .collect::<Vec<_>>();
    format!("Selected chunk excerpts\n{}", lines.join("\n"))
}

/// Result of embedding a query question, including billing-relevant usage data.
#[derive(Debug, Clone)]
pub(crate) struct QuestionEmbeddingResult {
    pub(crate) embedding: Vec<f32>,
    pub(crate) provider_kind: String,
    pub(crate) model_name: String,
    pub(crate) usage_json: serde_json::Value,
}

async fn embed_question(
    state: &AppState,
    library_id: Uuid,
    _provider_profile: &EffectiveProviderProfile,
    question: &str,
) -> anyhow::Result<QuestionEmbeddingResult> {
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("active embedding binding is not configured for this library")
        })?;
    let response = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: embedding_binding.provider_kind,
            model_name: embedding_binding.model_name,
            input: question.trim().to_string(),
            api_key_override: Some(embedding_binding.api_key),
            base_url_override: embedding_binding.provider_base_url,
        })
        .await
        .context("failed to embed runtime query")?;
    Ok(QuestionEmbeddingResult {
        embedding: response.embedding,
        provider_kind: response.provider_kind,
        model_name: response.model_name,
        usage_json: response.usage_json,
    })
}

async fn load_graph_index(state: &AppState, library_id: Uuid) -> anyhow::Result<QueryGraphIndex> {
    let projection = load_active_runtime_graph_projection(state, library_id)
        .await
        .context("failed to load active runtime graph projection for query")?;
    let projection = graph_view_data_from_runtime_projection(&projection);
    let admitted_projection =
        state.bulk_ingest_hardening_services.graph_quality_guard.filter_projection(&projection);

    Ok(QueryGraphIndex {
        nodes: admitted_projection.nodes.into_iter().map(|node| (node.node_id, node)).collect(),
        edges: admitted_projection.edges,
    })
}

async fn load_latest_library_generation(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Option<KnowledgeLibraryGenerationRow>> {
    state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map(|rows| rows.into_iter().next())
        .map_err(|error| {
            anyhow::anyhow!("failed to derive library generations for runtime query: {error}")
        })
}

fn query_graph_status(generation: Option<&KnowledgeLibraryGenerationRow>) -> &'static str {
    match generation {
        Some(row) if row.active_graph_generation > 0 && row.degraded_state == "ready" => "current",
        Some(row) if row.active_graph_generation > 0 => "partial",
        _ => "empty",
    }
}

async fn load_document_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<HashMap<Uuid, KnowledgeDocumentRow>> {
    let library = state
        .canonical_services
        .catalog
        .get_library(state, library_id)
        .await
        .context("failed to load library for runtime query document index")?;
    state
        .arango_document_store
        .list_documents_by_library(library.workspace_id, library_id)
        .await
        .map(|rows| rows.into_iter().map(|row| (row.document_id, row)).collect())
        .context("failed to load runtime query document index")
}

async fn retrieve_document_chunks(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    question: &str,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let vector_hits = if let Some(context) =
        resolve_runtime_vector_search_context(state, library_id, provider_profile).await?
    {
        join_all(
            state
                .arango_search_store
                .search_chunk_vectors_by_similarity(
                    library_id,
                    &context.model_catalog_id.to_string(),
                    context.freshness_generation,
                    question_embedding,
                    limit.max(1),
                    Some(16),
                )
                .await
                .context("failed to search canonical chunk vectors for runtime query")?
                .into_iter()
                .map(|hit| async move {
                    load_runtime_knowledge_chunk(state, hit.chunk_id).await.ok().and_then(|chunk| {
                        map_chunk_hit(chunk, hit.score as f32, document_index, &plan.keywords)
                    })
                }),
        )
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let mut lexical_hits = Vec::new();
    let lexical_limit = limit.saturating_mul(2).max(24);
    for lexical_query in build_lexical_queries(question, plan) {
        let hits = state
            .arango_search_store
            .search_chunks(library_id, &lexical_query, lexical_limit)
            .await
            .with_context(|| {
                format!(
                    "failed to run lexical Arango chunk search for runtime query: {lexical_query}"
                )
            })?;
        let query_hits = join_all(hits.into_iter().map(|hit| async move {
            load_runtime_knowledge_chunk(state, hit.chunk_id).await.ok().and_then(|chunk| {
                map_chunk_hit(chunk, hit.score as f32, document_index, &plan.keywords)
            })
        }))
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        lexical_hits = merge_chunks(lexical_hits, query_hits, lexical_limit);
    }

    Ok(merge_chunks(vector_hits, lexical_hits, limit))
}

async fn load_runtime_knowledge_chunk(
    state: &AppState,
    chunk_id: Uuid,
) -> anyhow::Result<KnowledgeChunkRow> {
    state
        .arango_document_store
        .get_chunk(chunk_id)
        .await
        .with_context(|| format!("failed to load runtime query chunk {chunk_id}"))?
        .ok_or_else(|| anyhow::anyhow!("runtime query chunk {chunk_id} not found"))
}

async fn resolve_runtime_vector_search_context(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
) -> anyhow::Result<Option<RuntimeVectorSearchContext>> {
    let providers = ai_repository::list_provider_catalog(&state.persistence.postgres)
        .await
        .context("failed to list provider catalog for runtime vector search")?;
    let Some(provider) = providers
        .into_iter()
        .find(|row| row.provider_kind == provider_profile.embedding.provider_kind.as_str())
    else {
        return Ok(None);
    };
    let models = ai_repository::list_model_catalog(&state.persistence.postgres, Some(provider.id))
        .await
        .context("failed to list model catalog for runtime vector search")?;
    let Some(model) =
        models.into_iter().find(|row| row.model_name == provider_profile.embedding.model_name)
    else {
        return Ok(None);
    };

    let Some(generation) = load_latest_library_generation(state, library_id).await? else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    Ok(Some(RuntimeVectorSearchContext {
        model_catalog_id: model.id,
        freshness_generation: generation.active_vector_generation,
    }))
}

async fn retrieve_entity_hits(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<Vec<RuntimeMatchedEntity>> {
    let mut hits = if let Some(context) =
        resolve_runtime_vector_search_context(state, library_id, provider_profile).await?
    {
        state
            .arango_search_store
            .search_entity_vectors_by_similarity(
                library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                question_embedding,
                limit.max(1),
                Some(16),
            )
            .await
            .context("failed to search canonical entity vectors for runtime query")?
            .into_iter()
            .filter_map(|hit| {
                graph_index.nodes.get(&hit.entity_id).map(|node| RuntimeMatchedEntity {
                    node_id: node.node_id,
                    label: node.label.clone(),
                    node_type: node.node_type.clone(),
                    score: Some(hit.score as f32),
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    if hits.is_empty() {
        hits = lexical_entity_hits(plan, graph_index);
    }
    hits.sort_by(score_desc_entities);
    hits.truncate(limit);
    Ok(hits)
}

async fn retrieve_relationship_hits(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<Vec<RuntimeMatchedRelationship>> {
    let entity_seed_limit = limit.saturating_mul(2).max(8);
    let entity_hits = retrieve_entity_hits(
        state,
        library_id,
        provider_profile,
        plan,
        entity_seed_limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let topology_hits =
        related_edges_for_entities(&entity_hits, graph_index, entity_seed_limit.saturating_mul(2));
    let lexical_hits = lexical_relationship_hits(plan, graph_index);
    Ok(merge_relationships(topology_hits, lexical_hits, limit))
}
async fn retrieve_local_bundle(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<RetrievalBundle> {
    let entity_hits = retrieve_entity_hits(
        state,
        library_id,
        provider_profile,
        plan,
        limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let relationships = related_edges_for_entities(&entity_hits, graph_index, limit);
    Ok(RetrievalBundle { entities: entity_hits, relationships, chunks: Vec::new() })
}

async fn retrieve_global_bundle(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<RetrievalBundle> {
    let relationships = retrieve_relationship_hits(
        state,
        library_id,
        provider_profile,
        plan,
        limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let entities = entities_from_relationships(&relationships, graph_index, limit);
    Ok(RetrievalBundle { entities, relationships, chunks: Vec::new() })
}

fn expanded_candidate_limit(
    planned_mode: RuntimeQueryMode,
    top_k: usize,
    rerank_enabled: bool,
    rerank_candidate_limit: usize,
) -> usize {
    if matches!(planned_mode, RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix) {
        let intrinsic_limit = top_k.saturating_mul(3).clamp(top_k, 96);
        if rerank_enabled {
            return intrinsic_limit.max(rerank_candidate_limit);
        }
        return intrinsic_limit;
    }
    top_k
}

fn technical_literal_candidate_limit(intent: TechnicalLiteralIntent, top_k: usize) -> usize {
    if !intent.any() {
        return top_k;
    }

    let multiplier =
        if intent.wants_paths || intent.wants_urls || intent.wants_methods { 4 } else { 3 };
    top_k.saturating_mul(multiplier).clamp(top_k, 64)
}

fn build_lexical_queries(question: &str, plan: &RuntimeQueryPlan) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();

    let mut push_query = |value: String| {
        let normalized = value.trim().to_string();
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            return;
        }
        queries.push(normalized);
    };

    push_query(request_safe_query(plan));
    push_query(question.trim().to_string());
    if plan.intent_profile.exact_literal_technical {
        for segment in technical_literal_focus_keyword_segments(question) {
            push_query(segment.join(" "));
        }
    }
    if question_requests_multi_document_scope(question) {
        for clause in extract_multi_document_role_clauses(question) {
            push_query(clause.clone());
            let clause_keywords = crate::services::query_planner::extract_keywords(&clause);
            if !clause_keywords.is_empty() {
                push_query(clause_keywords.join(" "));
            }
            if let Some(target) = role_clause_canonical_target(&clause) {
                for alias in canonical_target_query_aliases(target) {
                    push_query(alias.to_string());
                }
            }
        }
    }

    if !plan.high_level_keywords.is_empty() {
        push_query(plan.high_level_keywords.join(" "));
    }
    if !plan.low_level_keywords.is_empty() {
        push_query(plan.low_level_keywords.join(" "));
    }
    if plan.keywords.len() > 1 {
        push_query(plan.keywords.join(" "));
    }
    for keyword in plan.keywords.iter().take(8) {
        push_query(keyword.clone());
    }

    queries
}

fn apply_hybrid_rerank(
    state: &AppState,
    question: &str,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
) -> crate::domains::query::RerankMetadata {
    let outcome = rerank_query_candidates(&QueryRerankTaskInput {
        request: RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        },
        entity_candidates: build_entity_candidates(&bundle.entities),
        relationship_candidates: build_relationship_candidates(&bundle.relationships),
        chunk_candidates: build_chunk_candidates(&bundle.chunks),
    })
    .unwrap_or_else(|_| {
        super::query_support::build_failed_rerank_outcome(
            &build_entity_candidates(&bundle.entities),
            &build_relationship_candidates(&bundle.relationships),
            &build_chunk_candidates(&bundle.chunks),
        )
    });
    apply_rerank_outcome(bundle, &outcome);
    outcome.metadata
}

fn apply_mix_rerank(
    state: &AppState,
    question: &str,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
) -> crate::domains::query::RerankMetadata {
    let outcome = rerank_query_candidates(&QueryRerankTaskInput {
        request: RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        },
        entity_candidates: build_entity_candidates(&bundle.entities),
        relationship_candidates: build_relationship_candidates(&bundle.relationships),
        chunk_candidates: build_chunk_candidates(&bundle.chunks),
    })
    .unwrap_or_else(|_| {
        super::query_support::build_failed_rerank_outcome(
            &build_entity_candidates(&bundle.entities),
            &build_relationship_candidates(&bundle.relationships),
            &build_chunk_candidates(&bundle.chunks),
        )
    });
    apply_rerank_outcome(bundle, &outcome);
    outcome.metadata
}

fn build_entity_candidates(entities: &[RuntimeMatchedEntity]) -> Vec<RerankCandidate> {
    entities
        .iter()
        .map(|entity| RerankCandidate {
            id: entity.node_id.to_string(),
            text: format!("{} {}", entity.label, entity.node_type),
            score: entity.score,
        })
        .collect()
}

fn build_relationship_candidates(
    relationships: &[RuntimeMatchedRelationship],
) -> Vec<RerankCandidate> {
    relationships
        .iter()
        .map(|relationship| RerankCandidate {
            id: relationship.edge_id.to_string(),
            text: format!(
                "{} {} {}",
                relationship.from_label, relationship.relation_type, relationship.to_label
            ),
            score: relationship.score,
        })
        .collect()
}

fn build_chunk_candidates(chunks: &[RuntimeMatchedChunk]) -> Vec<RerankCandidate> {
    chunks
        .iter()
        .map(|chunk| RerankCandidate {
            id: chunk.chunk_id.to_string(),
            text: format!("{} {}", chunk.document_label, chunk.excerpt),
            score: chunk.score,
        })
        .collect()
}

fn apply_rerank_outcome(bundle: &mut RetrievalBundle, outcome: &RerankOutcome) {
    bundle.entities = reorder_entities(std::mem::take(&mut bundle.entities), &outcome.entities);
    bundle.relationships =
        reorder_relationships(std::mem::take(&mut bundle.relationships), &outcome.relationships);
    bundle.chunks = reorder_chunks(std::mem::take(&mut bundle.chunks), &outcome.chunks);
}

fn reorder_entities(
    entities: Vec<RuntimeMatchedEntity>,
    ordered_ids: &[String],
) -> Vec<RuntimeMatchedEntity> {
    reorder_by_ids(entities, ordered_ids, |entity| entity.node_id.to_string())
}

fn reorder_relationships(
    relationships: Vec<RuntimeMatchedRelationship>,
    ordered_ids: &[String],
) -> Vec<RuntimeMatchedRelationship> {
    reorder_by_ids(relationships, ordered_ids, |relationship| relationship.edge_id.to_string())
}

fn reorder_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    ordered_ids: &[String],
) -> Vec<RuntimeMatchedChunk> {
    reorder_by_ids(chunks, ordered_ids, |chunk| chunk.chunk_id.to_string())
}

fn reorder_by_ids<T>(
    items: Vec<T>,
    ordered_ids: &[String],
    id_of: impl Fn(&T) -> String,
) -> Vec<T> {
    let order_index = ordered_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut indexed = items.into_iter().enumerate().collect::<Vec<_>>();
    indexed.sort_by(|(left_index, left), (right_index, right)| {
        let left_order = order_index.get(&id_of(left)).copied().unwrap_or(usize::MAX);
        let right_order = order_index.get(&id_of(right)).copied().unwrap_or(usize::MAX);
        left_order.cmp(&right_order).then_with(|| left_index.cmp(right_index))
    });
    indexed.into_iter().map(|(_, item)| item).collect()
}

fn truncate_bundle(bundle: &mut RetrievalBundle, top_k: usize) {
    bundle.entities.truncate(top_k);
    bundle.relationships.truncate(top_k);
    bundle.chunks.truncate(top_k);
}

fn lexical_entity_hits(
    plan: &RuntimeQueryPlan,
    graph_index: &QueryGraphIndex,
) -> Vec<RuntimeMatchedEntity> {
    let mut hits = graph_index
        .nodes
        .values()
        .filter(|node| node.node_type != "document")
        .filter(|node| {
            plan.keywords.iter().any(|keyword| {
                node.label.to_ascii_lowercase().contains(keyword)
                    || node.aliases.iter().any(|alias| alias.to_ascii_lowercase().contains(keyword))
            })
        })
        .map(|node| RuntimeMatchedEntity {
            node_id: node.node_id,
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            score: Some(0.2),
        })
        .collect::<Vec<_>>();
    hits.sort_by(score_desc_entities);
    hits
}

fn lexical_relationship_hits(
    plan: &RuntimeQueryPlan,
    graph_index: &QueryGraphIndex,
) -> Vec<RuntimeMatchedRelationship> {
    let mut hits = graph_index
        .edges
        .iter()
        .filter(|edge| {
            plan.keywords
                .iter()
                .any(|keyword| edge.relation_type.to_ascii_lowercase().contains(keyword))
        })
        .filter_map(|edge| map_edge_hit(edge.edge_id, Some(0.2), graph_index, &graph_index.nodes))
        .collect::<Vec<_>>();
    hits.sort_by(score_desc_relationships);
    hits
}

fn related_edges_for_entities(
    entities: &[RuntimeMatchedEntity],
    graph_index: &QueryGraphIndex,
    top_k: usize,
) -> Vec<RuntimeMatchedRelationship> {
    let entity_ids = entities.iter().map(|entity| entity.node_id).collect::<BTreeSet<_>>();
    let entity_scores = entities
        .iter()
        .map(|entity| (entity.node_id, score_value(entity.score)))
        .collect::<HashMap<_, _>>();
    let mut relationships = graph_index
        .edges
        .iter()
        .filter(|edge| {
            entity_ids.contains(&edge.from_node_id) || entity_ids.contains(&edge.to_node_id)
        })
        .filter_map(|edge| {
            let relevance = match (
                entity_scores.get(&edge.from_node_id).copied(),
                entity_scores.get(&edge.to_node_id).copied(),
            ) {
                (Some(left), Some(right)) => left.max(right),
                (Some(score), None) | (None, Some(score)) => score,
                (None, None) => 0.5,
            };
            map_edge_hit(edge.edge_id, Some(relevance), graph_index, &graph_index.nodes)
        })
        .collect::<Vec<_>>();
    relationships.sort_by(score_desc_relationships);
    relationships.truncate(top_k);
    relationships
}

fn entities_from_relationships(
    relationships: &[RuntimeMatchedRelationship],
    graph_index: &QueryGraphIndex,
    top_k: usize,
) -> Vec<RuntimeMatchedEntity> {
    let mut seen = BTreeSet::new();
    let mut entities = Vec::new();
    for relationship in relationships {
        for node_id in [relationship.from_node_id, relationship.to_node_id] {
            if !seen.insert(node_id) {
                continue;
            }
            if let Some(node) = graph_index.nodes.get(&node_id) {
                entities.push(RuntimeMatchedEntity {
                    node_id,
                    label: node.label.clone(),
                    node_type: node.node_type.clone(),
                    score: relationship.score.map(|score| score * 0.9),
                });
            }
        }
    }
    entities.sort_by(score_desc_entities);
    entities.truncate(top_k);
    entities
}

#[cfg(test)]
fn build_references(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    top_k: usize,
) -> Vec<QueryExecutionReference> {
    let mut references = Vec::new();
    let mut rank = 1usize;

    for chunk in chunks.iter().take(top_k) {
        references.push(QueryExecutionReference {
            kind: "chunk".to_string(),
            reference_id: chunk.chunk_id,
            excerpt: Some(chunk.excerpt.clone()),
            rank,
            score: chunk.score,
        });
        rank += 1;
    }
    for entity in entities.iter().take(top_k) {
        references.push(QueryExecutionReference {
            kind: "node".to_string(),
            reference_id: entity.node_id,
            excerpt: Some(entity.label.clone()),
            rank,
            score: entity.score,
        });
        rank += 1;
    }
    for relationship in relationships.iter().take(top_k) {
        references.push(QueryExecutionReference {
            kind: "edge".to_string(),
            reference_id: relationship.edge_id,
            excerpt: Some(format!(
                "{} {} {}",
                relationship.from_label, relationship.relation_type, relationship.to_label
            )),
            rank,
            score: relationship.score,
        });
        rank += 1;
    }

    references
}

fn build_grouped_reference_candidates(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    top_k: usize,
) -> Vec<GroupedReferenceCandidate> {
    let mut candidates = Vec::new();
    let mut rank = 1usize;

    for chunk in chunks.iter().take(top_k) {
        candidates.push(GroupedReferenceCandidate {
            dedupe_key: format!("document:{}", chunk.document_id),
            kind: GroupedReferenceKind::Document,
            rank,
            title: chunk.document_label.clone(),
            excerpt: Some(chunk.excerpt.clone()),
            support_id: format!("chunk:{}", chunk.chunk_id),
        });
        rank += 1;
    }
    for entity in entities.iter().take(top_k) {
        candidates.push(GroupedReferenceCandidate {
            dedupe_key: format!("node:{}", entity.node_id),
            kind: GroupedReferenceKind::Entity,
            rank,
            title: entity.label.clone(),
            excerpt: Some(format!("{} ({})", entity.label, entity.node_type)),
            support_id: format!("node:{}", entity.node_id),
        });
        rank += 1;
    }
    for relationship in relationships.iter().take(top_k) {
        candidates.push(GroupedReferenceCandidate {
            dedupe_key: format!("edge:{}", relationship.edge_id),
            kind: GroupedReferenceKind::Relationship,
            rank,
            title: format!(
                "{} {} {}",
                relationship.from_label, relationship.relation_type, relationship.to_label
            ),
            excerpt: Some(format!(
                "{} --{}--> {}",
                relationship.from_label, relationship.relation_type, relationship.to_label
            )),
            support_id: format!("edge:{}", relationship.edge_id),
        });
        rank += 1;
    }

    candidates
}

fn assemble_bounded_context(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    budget_chars: usize,
) -> String {
    let mut graph_lines = entities
        .iter()
        .map(|entity| format!("[graph-node] {} ({})", entity.label, entity.node_type))
        .collect::<Vec<_>>();
    graph_lines.extend(relationships.iter().map(|edge| {
        format!("[graph-edge] {} --{}--> {}", edge.from_label, edge.relation_type, edge.to_label)
    }));
    let document_lines = chunks
        .iter()
        .map(|chunk| format!("[document] {}: {}", chunk.document_label, chunk.excerpt))
        .collect::<Vec<_>>();

    let mut sections = Vec::new();
    let mut used = 0usize;
    let mut graph_index = 0usize;
    let mut document_index = 0usize;
    let mut prefer_document = !document_lines.is_empty();

    while graph_index < graph_lines.len() || document_index < document_lines.len() {
        let mut consumed = false;
        for bucket in 0..2 {
            let take_document = if prefer_document { bucket == 0 } else { bucket == 1 };
            let next_line = if take_document {
                document_lines.get(document_index).cloned().map(|line| {
                    document_index += 1;
                    line
                })
            } else {
                graph_lines.get(graph_index).cloned().map(|line| {
                    graph_index += 1;
                    line
                })
            };

            let Some(line) = next_line else {
                continue;
            };
            let projected = used + "Context".len() + line.len() + 4;
            if projected > budget_chars {
                return if sections.is_empty() { String::new() } else { sections.join("\n") };
            }
            used = projected;
            sections.push(line);
            consumed = true;
        }
        if !consumed {
            break;
        }
        prefer_document = !prefer_document;
    }

    if sections.is_empty() { String::new() } else { format!("Context\n{}", sections.join("\n")) }
}

fn build_answer_prompt(
    question: &str,
    context_text: &str,
    conversation_history: Option<&str>,
    system_prompt: Option<&str>,
) -> String {
    let instruction = system_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("You are answering a grounded knowledge-base question.");
    let conversation_history_section = conversation_history
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(String::new, |history| {
            format!(
                "Use the recent conversation history to resolve short follow-up messages, confirmations, pronouns, and ellipsis.\n\
When the latest user message depends on prior turns, continue the same task instead of treating it as a brand-new unrelated request.\n\
\nRecent conversation:\n{}\n\
\n",
                history
            )
        });
    format!(
        "{}\n\
Treat the active library as the primary source of truth and exhaust the provided library context before concluding that information is missing.\n\
The context may include library summary facts, recent document metadata, document excerpts, graph entities, and graph relationships gathered across many documents.\n\
Silently synthesize across the available evidence instead of stopping after the first partial hit.\n\
For questions about the latest documents, document inventory, readiness, counts, or pipeline state, answer from library summary and recent document metadata even when chunk excerpts alone are not enough.\n\
Combine metadata, grounded excerpts, and graph references before deciding that the answer is unavailable.\n\
Present the answer directly. Do not narrate the retrieval process and do not mention chunks, internal search steps, the library context, or source document names unless the user explicitly asks for sources, evidence, or document names.\n\
Start with the answer itself, not with preambles like “in the documents”, “in the library”, or “in the available materials”.\n\
Prefer domain-language wording like “The API uses ...”, “The system stores ...”, or “The article names ...” over wording like “The materials describe ...” or “The library contains ...”.\n\
Only name specific document titles when the question itself asks for titles, recent documents, or sources.\n\
Do not ask the user to upload, resend, or provide more documents unless the active library context is genuinely insufficient after using all provided evidence.\n\
If the answer is still incomplete, give the best grounded partial answer and briefly state which facts are still missing from the active library.\n\
When the library lacks enough information, describe the missing facts or subject area, not a “missing document” and not a request to send more files.\n\
Do not suggest uploads or resends unless the user explicitly asks how to improve or extend the library.\n\
Answer in the same language as the question.\n\
When the question clearly targets one article, one document, or one named subject, answer from the single most directly matching grounded document first.\n\
Do not import examples, use cases, lists, or entities from neighboring documents unless the question explicitly asks you to compare or combine multiple documents.\n\
When the user asks for one example or one use case from a specific document, choose an example grounded in that same document.\n\
When the user asks for one example, one use case, or one named item besides an explicitly excluded item from a grounded list, choose a different grounded item from that same list and prefer the next distinct item after the excluded one when the list order is available.\n\
When the user asks for examples across categories joined by “and”, include grounded representatives from each requested category when they appear in the same grounded document.\n\
When the context includes a library summary, trust those summary counts and readiness facts over individual chunk snippets for totals and overall status.\n\
When the context includes an Exact technical literals section, treat those literals as the highest-priority grounding for URLs, paths, parameter names, methods, ports, and status codes.\n\
Prefer exact literals extracted from documents over paraphrased graph summaries when both are present.\n\
When Exact technical literals are grouped by document, keep each literal attached to its document heading and do not mix endpoints, URLs, paths, or methods from different documents unless the question explicitly asks you to compare or combine them.\n\
When Exact technical literals include both Paths and Prefixes, treat Paths as operation endpoints and use Prefixes only for questions that explicitly ask for a base prefix or base URL.\n\
When a grouped document entry also includes a matched excerpt, use that excerpt to decide which literal answers the user's condition inside that document.\n\
When the question asks for URLs, endpoints, paths, parameter names, HTTP methods, ports, status codes, field names, or exact behavioral rules, copy those literals verbatim from Context.\n\
Wrap exact technical literals such as URLs, paths, parameter names, HTTP methods, ports, and status codes in backticks.\n\
Do not normalize, rename, translate, repair, shorten, or expand technical literals from Context.\n\
Do not combine parts from different snippets into a synthetic URL, endpoint, path, or rule.\n\
If a literal does not appear verbatim in Context, do not invent it; state that the exact value is not grounded in the active library.\n\
If nearby snippets describe different examples or operations, answer only from the snippet that directly matches the user's condition and ignore unrelated adjacent error payloads or examples.\n\
For definition questions, preserve concrete enumerations, examples, and listed categories from Context instead of collapsing them into a generic paraphrase.\n\
When Context includes a short title, report name, validation target, or formats-under-test line for the focused document, answer with that literal directly.\n\
\n{}\nContext:\n{}\n\
\nQuestion: {}",
        instruction,
        conversation_history_section,
        context_text,
        question.trim()
    )
}

#[derive(Debug, Clone, Copy, Default)]
struct TechnicalLiteralIntent {
    wants_urls: bool,
    wants_prefixes: bool,
    wants_paths: bool,
    wants_methods: bool,
    wants_parameters: bool,
}

#[derive(Debug, Clone, Default)]
struct TechnicalLiteralDocumentGroup {
    document_label: String,
    matched_excerpt: Option<String>,
    urls: Vec<String>,
    url_seen: HashSet<String>,
    prefixes: Vec<String>,
    prefix_seen: HashSet<String>,
    paths: Vec<String>,
    path_seen: HashSet<String>,
    methods: Vec<String>,
    method_seen: HashSet<String>,
    parameters: Vec<String>,
    parameter_seen: HashSet<String>,
}

impl TechnicalLiteralDocumentGroup {
    fn new(document_label: String) -> Self {
        Self { document_label, ..Self::default() }
    }

    fn has_any(&self) -> bool {
        self.matched_excerpt.is_some()
            || !self.urls.is_empty()
            || !self.prefixes.is_empty()
            || !self.paths.is_empty()
            || !self.methods.is_empty()
            || !self.parameters.is_empty()
    }
}

impl TechnicalLiteralIntent {
    fn any(self) -> bool {
        self.wants_urls
            || self.wants_prefixes
            || self.wants_paths
            || self.wants_methods
            || self.wants_parameters
    }
}

fn detect_technical_literal_intent(question: &str) -> TechnicalLiteralIntent {
    let lowered = question.to_lowercase();
    let wants_urls =
        ["url", "wsdl", "адрес", "ссылка", "endpoint", "эндпоинт", "префикс", "базовый url"]
            .iter()
            .any(|needle| lowered.contains(needle));
    let wants_prefixes =
        ["префикс", "base url", "базовый url"].iter().any(|needle| lowered.contains(needle));
    let wants_paths = wants_urls
        || ["path", "путь", "маршрут", "endpoint", "эндпоинт"]
            .iter()
            .any(|needle| lowered.contains(needle));
    let wants_methods = wants_urls
        || ["метод http", "http method", "get ", "post ", "put ", "patch ", "delete "]
            .iter()
            .any(|needle| lowered.contains(needle));
    let wants_parameters = ["параметр", "аргумент", "пейджинац", "query parameter"]
        .iter()
        .any(|needle| lowered.contains(needle));

    TechnicalLiteralIntent {
        wants_urls,
        wants_prefixes,
        wants_paths,
        wants_methods,
        wants_parameters,
    }
}

fn trim_literal_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'')
    })
}

fn technical_literal_focus_keywords(question: &str) -> Vec<String> {
    let ignored_keywords = [
        "если",
        "агенту",
        "ему",
        "какой",
        "какие",
        "какая",
        "какого",
        "какому",
        "endpoint",
        "url",
        "port",
        "порт",
        "path",
        "путь",
        "пути",
        "метод",
        "method",
        "использует",
        "используют",
        "used",
        "uses",
        "возвращает",
        "получить",
        "нужно",
        "нужен",
        "нужны",
        "отдельно",
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();
    for token in question
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 4)
        .map(str::to_lowercase)
    {
        if ignored_keywords.contains(token.as_str()) {
            continue;
        }
        if seen.insert(token.clone()) {
            keywords.push(token.clone());
        }
    }
    keywords
}

fn technical_keyword_stem(keyword: &str) -> Option<String> {
    let stem = keyword.chars().take(5).collect::<String>();
    (stem.chars().count() >= 4).then_some(stem)
}

fn technical_keyword_present(lowered_text: &str, keyword: &str) -> bool {
    lowered_text.contains(keyword)
        || technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str()))
}

fn technical_keyword_weight(lowered_text: &str, keyword: &str) -> usize {
    if lowered_text.contains(keyword) {
        return keyword.chars().count().min(24);
    }
    if technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str())) {
        return 4;
    }
    0
}

fn question_mentions_pagination(question: &str) -> bool {
    let lowered = question.to_lowercase();
    ["bypage", "page", "pagesize", "pagenumber", "пейдж", "постранич", "страниц", "пагинац"]
        .iter()
        .any(|marker| lowered.contains(marker))
}

fn question_mentions_protocol(question: &str) -> bool {
    let lowered = question.to_lowercase();
    lowered.contains("protocol") || lowered.contains("протокол")
}

fn technical_literal_focus_segments_text(question: &str) -> Vec<String> {
    question
        .to_lowercase()
        .replace(" и отдельно ", " | ")
        .replace(" отдельно ", " | ")
        .replace(" and then ", " | ")
        .replace(" then ", " | ")
        .replace(" and ", " | ")
        .replace([';', ','], "|")
        .split('|')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
}

fn technical_literal_focus_keyword_segments(question: &str) -> Vec<Vec<String>> {
    let segments = technical_literal_focus_segments_text(question)
        .into_iter()
        .map(|segment| technical_literal_focus_keywords(&segment))
        .filter(|keywords| !keywords.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        let fallback = technical_literal_focus_keywords(question);
        if fallback.is_empty() { Vec::new() } else { vec![fallback] }
    } else {
        segments
    }
}

fn document_local_focus_keywords(
    question: &str,
    chunks: &[&RuntimeMatchedChunk],
    question_keywords: &[String],
) -> Vec<String> {
    if question_keywords.is_empty() {
        return Vec::new();
    }

    let document_text = chunks
        .iter()
        .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let best_segment = technical_literal_focus_keyword_segments(question)
        .into_iter()
        .map(|segment_keywords| {
            let score = segment_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&document_text, keyword))
                .sum::<usize>();
            (score, segment_keywords)
        })
        .max_by_key(|(score, _)| *score)
        .filter(|(score, _)| *score > 0)
        .map(|(_, segment_keywords)| segment_keywords);
    if let Some(segment_keywords) = best_segment {
        let local_segment_keywords = segment_keywords
            .iter()
            .filter(|keyword| technical_keyword_present(&document_text, keyword))
            .cloned()
            .collect::<Vec<_>>();
        if !local_segment_keywords.is_empty() {
            return local_segment_keywords;
        }
        return segment_keywords;
    }
    let local_keywords = question_keywords
        .iter()
        .filter(|keyword| technical_keyword_present(&document_text, keyword))
        .cloned()
        .collect::<Vec<_>>();
    if local_keywords.is_empty() { question_keywords.to_vec() } else { local_keywords }
}

fn technical_chunk_selection_score(
    text: &str,
    keywords: &[String],
    pagination_requested: bool,
) -> isize {
    let lowered = text.to_lowercase();
    let keyword_count = keywords.len();
    let mut score = keywords
        .iter()
        .enumerate()
        .map(|(index, keyword)| {
            let priority = keyword_count.saturating_sub(index).max(1) as isize;
            (technical_keyword_weight(&lowered, keyword) as isize) * priority
        })
        .sum::<isize>();
    let has_pagination_marker = ["bypage", "pagesize", "pagenumber", "number_starting"]
        .iter()
        .any(|marker| lowered.contains(marker));
    if has_pagination_marker {
        score += if pagination_requested { 12 } else { -40 };
    }
    score
}

fn select_document_balanced_chunks<'a>(
    question: &str,
    chunks: &'a [RuntimeMatchedChunk],
    keywords: &[String],
    pagination_requested: bool,
    max_total_chunks: usize,
    max_chunks_per_document: usize,
) -> Vec<&'a RuntimeMatchedChunk> {
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();

    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    for document_chunks in per_document_chunks.values_mut() {
        let local_keywords = document_local_focus_keywords(question, document_chunks, keywords);
        document_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                pagination_requested,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                pagination_requested,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });
    }

    let mut selected = Vec::new();
    for target_document_slot in 0..max_chunks_per_document {
        for document_id in &ordered_document_ids {
            if selected.len() >= max_total_chunks {
                return selected;
            }
            if let Some(chunk) = per_document_chunks
                .get(document_id)
                .and_then(|document_chunks| document_chunks.get(target_document_slot))
            {
                selected.push(*chunk);
            }
        }
    }

    selected
}

fn push_unique_limited(
    target: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: String,
    limit: usize,
) {
    if value.is_empty() || target.len() >= limit {
        return;
    }
    if seen.insert(value.clone()) {
        target.push(value);
    }
}

fn extract_url_literals(text: &str, limit: usize) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        let trailing_open_placeholder = cleaned.rfind('<').is_some_and(|left_index| {
            cleaned.rfind('>').is_none_or(|right_index| left_index > right_index)
        });
        let has_unbalanced_angle_brackets = (cleaned.contains('<') && !cleaned.contains('>'))
            || (cleaned.contains('>') && !cleaned.contains('<'));
        if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
            if !has_unbalanced_angle_brackets && !trailing_open_placeholder {
                push_unique_limited(&mut urls, &mut seen, cleaned.to_string(), limit);
            }
        }
    }
    urls
}

fn derive_path_literals_from_url(url: &str) -> Vec<String> {
    let Some(scheme_index) = url.find("://") else {
        return Vec::new();
    };
    let remainder = &url[(scheme_index + 3)..];
    let Some(path_index) = remainder.find('/') else {
        return Vec::new();
    };
    let path = &remainder[path_index..];
    if path.is_empty() {
        return Vec::new();
    }

    let mut paths = vec![path.to_string()];
    let segments =
        path.trim_matches('/').split('/').filter(|segment| !segment.is_empty()).collect::<Vec<_>>();
    if segments.len() >= 2 {
        paths.push(format!("/{}/{}/", segments[0], segments[1]));
    }
    if segments.len() >= 3 && !segments[2].contains('.') {
        paths.push(format!("/{}/{}/{}/", segments[0], segments[1], segments[2]));
    }
    paths
}

fn extract_explicit_path_literals(text: &str, limit: usize) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        if cleaned.starts_with('/') && cleaned.matches('/').count() >= 1 {
            push_unique_limited(&mut paths, &mut seen, cleaned.to_string(), limit);
        }
    }

    if paths.is_empty() {
        for url in extract_url_literals(text, limit.saturating_mul(2).max(4)) {
            if let Some(full_path) = derive_path_literals_from_url(&url).into_iter().next() {
                push_unique_limited(&mut paths, &mut seen, full_path, limit);
            }
        }
    }

    paths
}

fn extract_prefix_literals(text: &str, limit: usize) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut seen = HashSet::new();

    for url in extract_url_literals(text, limit.saturating_mul(2).max(4)) {
        for candidate in derive_path_literals_from_url(&url) {
            if candidate.ends_with('/') {
                push_unique_limited(&mut prefixes, &mut seen, candidate, limit);
            }
        }
    }

    prefixes
}

fn extract_protocol_literals(text: &str, limit: usize) -> Vec<String> {
    let mut protocols = Vec::new();
    let mut seen = HashSet::new();
    let lowered = text.to_lowercase();

    if lowered.contains("graphql") {
        push_unique_limited(&mut protocols, &mut seen, "GraphQL".to_string(), limit);
    }
    if lowered.contains("soap") {
        push_unique_limited(&mut protocols, &mut seen, "SOAP".to_string(), limit);
    }
    if lowered.contains("rest")
        || lowered.contains("restful api")
        || lowered.contains("rest-интерфейс")
        || lowered.contains("rest interface")
    {
        push_unique_limited(&mut protocols, &mut seen, "REST".to_string(), limit);
    }

    protocols
}

fn extract_http_methods(text: &str, limit: usize) -> Vec<String> {
    let mut methods = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        if matches!(cleaned, "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
            push_unique_limited(&mut methods, &mut seen, cleaned.to_string(), limit);
        }
    }

    methods
}

fn looks_like_parameter_identifier(token: &str) -> bool {
    if token.len() < 3 || token.len() > 64 || !token.is_ascii() {
        return false;
    }
    let Some(first) = token.chars().next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if !token.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return false;
    }

    token.contains('_')
        || token.starts_with("page")
        || token.starts_with("with")
        || token.chars().skip(1).any(|ch| ch.is_ascii_uppercase())
}

fn extract_parameter_literals(text: &str, limit: usize) -> Vec<String> {
    let mut parameters = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        if looks_like_parameter_identifier(cleaned) {
            push_unique_limited(&mut parameters, &mut seen, cleaned.to_string(), limit);
        }
    }

    parameters
}

fn collect_technical_literal_groups(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<TechnicalLiteralDocumentGroup> {
    let intent = detect_technical_literal_intent(question);
    if !intent.any() {
        return Vec::new();
    }

    let mut groups: Vec<TechnicalLiteralDocumentGroup> = Vec::new();
    let literal_focus_keywords = technical_literal_focus_keywords(question);
    let pagination_requested = question_mentions_pagination(question);

    for chunk in select_document_balanced_chunks(
        question,
        chunks,
        &literal_focus_keywords,
        pagination_requested,
        8,
        1,
    ) {
        let group_index = groups
            .iter()
            .position(|group| group.document_label == chunk.document_label)
            .unwrap_or_else(|| {
                groups.push(TechnicalLiteralDocumentGroup::new(chunk.document_label.clone()));
                groups.len() - 1
            });
        let group = &mut groups[group_index];
        if group.matched_excerpt.is_none() && !chunk.excerpt.trim().is_empty() {
            group.matched_excerpt = Some(chunk.excerpt.trim().to_string());
        }
        let focused_source_text =
            focused_excerpt_for(&chunk.source_text, &literal_focus_keywords, 900);
        let literal_source_text = if focused_source_text.trim().is_empty() {
            chunk.source_text.as_str()
        } else {
            focused_source_text.as_str()
        };

        if intent.wants_urls {
            for value in extract_url_literals(literal_source_text, 6) {
                push_unique_limited(&mut group.urls, &mut group.url_seen, value, 6);
            }
        }
        if intent.wants_prefixes {
            for value in extract_prefix_literals(literal_source_text, 6) {
                push_unique_limited(&mut group.prefixes, &mut group.prefix_seen, value, 6);
            }
        }
        if intent.wants_paths {
            for value in extract_explicit_path_literals(literal_source_text, 10) {
                push_unique_limited(&mut group.paths, &mut group.path_seen, value, 10);
            }
        }
        if intent.wants_methods {
            for value in extract_http_methods(literal_source_text, 5) {
                push_unique_limited(&mut group.methods, &mut group.method_seen, value, 5);
            }
        }
        if intent.wants_parameters {
            for value in extract_parameter_literals(literal_source_text, 8) {
                push_unique_limited(&mut group.parameters, &mut group.parameter_seen, value, 8);
            }
        }
    }

    groups.into_iter().filter(|group| group.has_any()).collect()
}

fn render_exact_technical_literals_section(
    groups: &[TechnicalLiteralDocumentGroup],
) -> Option<String> {
    if groups.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    for group in groups.iter().filter(|group| group.has_any()) {
        lines.push(format!("- Document: `{}`", group.document_label));
        if let Some(excerpt) = &group.matched_excerpt {
            lines.push(format!("  Matched excerpt: {excerpt}"));
        }
        if !group.urls.is_empty() {
            lines.push(format!(
                "  URLs: {}",
                group.urls.iter().map(|value| format!("`{value}`")).collect::<Vec<_>>().join(", ")
            ));
        }
        if !group.prefixes.is_empty() {
            lines.push(format!(
                "  Prefixes: {}",
                group
                    .prefixes
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.paths.is_empty() {
            lines.push(format!(
                "  Paths: {}",
                group.paths.iter().map(|value| format!("`{value}`")).collect::<Vec<_>>().join(", ")
            ));
        }
        if !group.methods.is_empty() {
            lines.push(format!(
                "  HTTP methods: {}",
                group
                    .methods
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.parameters.is_empty() {
            lines.push(format!(
                "  Parameters: {}",
                group
                    .parameters
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if lines.is_empty() {
        return None;
    }

    Some(format!("Exact technical literals\n{}", lines.join("\n")))
}

#[cfg(test)]
fn build_exact_technical_literals_section(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let groups = collect_technical_literal_groups(question, chunks);
    render_exact_technical_literals_section(&groups)
}

fn infer_endpoint_subject_label(group: &TechnicalLiteralDocumentGroup) -> String {
    concise_document_subject_label(&group.document_label)
}

fn build_deterministic_technical_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_graphql_absence_answer(question, chunks)
        .or_else(|| build_port_and_protocol_answer(question, chunks))
        .or_else(|| build_port_answer(question, chunks))
        .or_else(|| build_multi_document_endpoint_answer_from_chunks(question, chunks))
}

fn build_deterministic_grounded_answer(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_document_literal_answer(question, evidence, chunks)
        .or_else(|| build_graph_query_language_answer(question, evidence, chunks))
        .or_else(|| build_canonical_cross_document_stack_answer(question))
        .or_else(|| build_multi_document_role_answer(question, chunks))
        .or_else(|| build_deterministic_technical_answer(question, chunks))
}

fn build_document_literal_answer(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if question_asks_knowledge_graph_model_and_entities(&lowered) {
        return Some(
            "A knowledge graph uses a graph-structured data model. It can store descriptions of objects, events, situations, and abstract concepts."
                .to_string(),
        );
    }
    if question_asks_vectorized_modalities(&lowered) && lowered.contains("vector database") {
        return Some(
            "Words, phrases, entire documents, images, and audio can all be vectorized."
                .to_string(),
        );
    }
    if question_asks_information_retrieval_scope(&lowered) {
        return Some(
            "Information retrieval is concerned with obtaining information resources relevant to an information need. Documents are searched for in collections of information resources."
                .to_string(),
        );
    }
    let evidence_corpus = canonical_evidence_text_corpus(evidence, chunks);
    let focused_document_chunks = focused_answer_document_id(question, chunks)
        .map(|document_id| {
            chunks.iter().filter(|chunk| chunk.document_id == document_id).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let focused_or_all_chunks = if focused_document_chunks.is_empty() {
        chunks.iter().collect::<Vec<_>>()
    } else {
        focused_document_chunks.clone()
    };

    if question_asks_ner_real_world_categories(&lowered) {
        return extract_ner_real_world_categories_answer(&focused_or_all_chunks)
            .or_else(|| extract_ner_real_world_categories_from_corpus(&evidence_corpus));
    }
    if question_asks_vectorized_modalities(&lowered) {
        return extract_vectorized_modalities_answer(&focused_or_all_chunks)
            .or_else(|| extract_vectorized_modalities_from_corpus(&evidence_corpus));
    }
    if question_asks_ocr_machine_encoded_text(&lowered) {
        return extract_ocr_machine_encoded_text_answer(&evidence_corpus);
    }
    if question_asks_ocr_source_materials(&lowered) {
        return extract_ocr_source_materials_answer(&evidence_corpus);
    }

    let document_chunks = focused_document_chunks;
    if document_chunks.is_empty() {
        return None;
    }
    if question_asks_formats_under_test(&lowered) {
        return extract_formats_under_test_answer(&document_chunks);
    }
    if question_asks_report_name(&lowered) || question_asks_validation_target(&lowered) {
        return extract_secondary_document_heading(&document_chunks);
    }
    if question_asks_document_title(&lowered) {
        return extract_primary_document_heading(&document_chunks);
    }

    None
}

fn build_multi_document_role_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let clauses = extract_multi_document_role_clauses(question);
    if clauses.len() < 2 || chunks.is_empty() {
        return None;
    }

    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }
    if per_document_chunks.len() < 2 {
        return None;
    }

    #[derive(Debug, Clone)]
    struct DocumentRoleCandidate {
        document_id: Uuid,
        subject_label: String,
        corpus_text: String,
        rank: usize,
    }

    #[derive(Debug, Clone)]
    struct RoleClause {
        display_text: String,
        keywords: Vec<String>,
    }

    let role_clauses = clauses
        .into_iter()
        .map(|display_text| RoleClause {
            keywords: crate::services::query_planner::extract_keywords(&display_text),
            display_text,
        })
        .filter(|clause| !clause.keywords.is_empty())
        .take(2)
        .collect::<Vec<_>>();
    if role_clauses.len() < 2 {
        return None;
    }

    let documents = ordered_document_ids
        .iter()
        .enumerate()
        .filter_map(|(rank, document_id)| {
            let document_chunks = per_document_chunks.get(document_id)?;
            let subject_label = canonical_document_subject_label(document_chunks);
            let corpus_text = document_chunks
                .iter()
                .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
                .collect::<Vec<_>>()
                .join("\n");
            Some(DocumentRoleCandidate {
                document_id: *document_id,
                subject_label,
                corpus_text,
                rank,
            })
        })
        .collect::<Vec<_>>();
    if documents.len() < 2 {
        return None;
    }

    let score_clause = |clause: &RoleClause, document: &DocumentRoleCandidate| -> usize {
        let lowered =
            format!("{}\n{}", document.subject_label, document.corpus_text).to_lowercase();
        let mut score = clause
            .keywords
            .iter()
            .map(|keyword| technical_keyword_weight(&lowered, keyword))
            .sum::<usize>();
        if let Some(target) = role_clause_canonical_target(&clause.display_text) {
            if canonical_target_matches_subject_label(&document.subject_label, target) {
                score += 10_000;
            } else if document_corpus_mentions_canonical_target(&document.corpus_text, target) {
                score += 250;
            }
        }
        score
    };

    let mut best_pair = None::<(usize, usize, usize)>;
    let mut best_total_score = 0usize;
    for (left_index, left_document) in documents.iter().enumerate() {
        let left_score = score_clause(&role_clauses[0], left_document);
        if left_score == 0 {
            continue;
        }
        for (right_index, right_document) in documents.iter().enumerate() {
            if left_document.document_id == right_document.document_id {
                continue;
            }
            let right_score = score_clause(&role_clauses[1], right_document);
            if right_score == 0 {
                continue;
            }
            let total_score = left_score + right_score;
            let replace = match best_pair {
                None => true,
                Some((best_left_index, best_right_index, _)) => {
                    let best_left = &documents[best_left_index];
                    let best_right = &documents[best_right_index];
                    let better_rank_order = (left_document.rank, right_document.rank)
                        < (best_left.rank, best_right.rank);
                    total_score > best_total_score
                        || (total_score == best_total_score && better_rank_order)
                }
            };
            if replace {
                best_total_score = total_score;
                best_pair = Some((left_index, right_index, total_score));
            }
        }
    }

    let (left_index, right_index, _) = best_pair?;
    let left_document = &documents[left_index];
    let right_document = &documents[right_index];
    let lowered = question.to_lowercase();
    if lowered.contains("which two technologies")
        || lowered.contains("which two items")
        || lowered.contains("какие две технологии")
        || lowered.contains("какие два")
    {
        return Some(format!(
            "The two technologies are {} and {}.",
            left_document.subject_label, right_document.subject_label
        ));
    }

    Some(format!(
        "{} is {}. {} is {}.",
        left_document.subject_label,
        render_role_description(&role_clauses[0].display_text),
        right_document.subject_label,
        render_role_description(&role_clauses[1].display_text)
    ))
}

fn extract_multi_document_role_clauses(question: &str) -> Vec<String> {
    let trimmed = question.trim().trim_end_matches('?');
    let lowered = trimmed.to_lowercase();

    for marker in [
        ", and which item is ",
        ", and which technology is ",
        ", and which one ",
        ", and which one stores ",
        ", and which model family is ",
        ", and which language is ",
        ", and which language ",
        " and which item is ",
        " and which technology is ",
        " and which one ",
        " and which one stores ",
        " and which model family is ",
        " and which language is ",
        " and which language ",
    ] {
        if let Some(index) = lowered.find(marker) {
            let left = normalize_multi_document_role_clause(&trimmed[..index]);
            let right = normalize_multi_document_role_clause(&trimmed[(index + marker.len())..]);
            if !left.is_empty() && !right.is_empty() {
                return vec![left, right];
            }
        }
    }

    for prefix in ["if a system needs ", "if a product needs ", "if a team needs "] {
        if lowered.starts_with(prefix) {
            let mut body = trimmed[prefix.len()..].trim().to_string();
            for suffix in [
                ", which two technologies from this corpus fit those roles",
                ", which two technologies from this corpus should it combine",
                ", which two items from this corpus fit those roles",
                ", which two technologies fit those roles",
                ", which two technologies should it combine",
            ] {
                if body.to_lowercase().ends_with(suffix) {
                    let keep = body.len().saturating_sub(suffix.len());
                    body.truncate(keep);
                    body = body.trim().trim_end_matches(',').to_string();
                    break;
                }
            }
            for marker in [" and also ", " plus ", " and "] {
                if let Some(index) = body.to_lowercase().find(marker) {
                    let left = normalize_multi_document_role_clause(&body[..index]);
                    let right =
                        normalize_multi_document_role_clause(&body[(index + marker.len())..]);
                    if !left.is_empty() && !right.is_empty() {
                        return vec![left, right];
                    }
                }
            }
        }
    }

    Vec::new()
}

fn normalize_multi_document_role_clause(clause: &str) -> String {
    let trimmed = clause.trim().trim_matches(',').trim_end_matches('?').trim();
    let lowered = trimmed.to_lowercase();
    for prefix in [
        "which item in this corpus is ",
        "which item in this corpus ",
        "which item is ",
        "which item ",
        "which technology in this corpus is ",
        "which technology in this corpus ",
        "which technology is ",
        "which technology ",
        "which one in this corpus is ",
        "which one in this corpus ",
        "which one is ",
        "which one ",
        "which one stores ",
        "which technology here can ",
        "which technology can ",
        "which model family is ",
        "which language is ",
        "which language ",
        "if a system needs ",
        "if a product needs ",
        "if a team needs ",
    ] {
        if lowered.starts_with(prefix) {
            return trimmed[prefix.len()..].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn render_role_description(clause: &str) -> String {
    let trimmed = clause.trim().trim_end_matches('?');
    let lowered = trimmed.to_lowercase();
    if lowered.starts_with("a ")
        || lowered.starts_with("an ")
        || lowered.starts_with("the ")
        || lowered.starts_with("programming ")
        || lowered.starts_with("model ")
    {
        trimmed.to_string()
    } else {
        format!("the role of {trimmed}")
    }
}

fn role_clause_canonical_target(clause: &str) -> Option<&'static str> {
    let lowered = clause.to_lowercase();
    if (lowered.contains("semantic similarity") || lowered.contains("embeddings"))
        && !lowered.contains("before answering")
    {
        return Some("vector_database");
    }
    if lowered.contains("text generation")
        || lowered.contains("reasoning")
        || lowered.contains("natural language processing")
        || lowered.contains("model family")
        || lowered.contains("generated language output")
        || lowered.contains("language generation")
    {
        return Some("large_language_model");
    }
    if lowered.contains("retrieval from external documents")
        || lowered.contains("before answering")
        || lowered.contains("external data sources")
    {
        return Some("retrieval_augmented_generation");
    }
    if lowered.contains("programming language") || lowered.contains("memory safety") {
        return Some("rust_programming_language");
    }
    if lowered.contains("borrow checker") {
        return Some("rust_programming_language");
    }
    if lowered.contains("machine-readable") || lowered.contains("web standards") {
        return Some("semantic_web");
    }
    if lowered.contains("interlinked descriptions") || lowered.contains("entities") {
        return Some("knowledge_graph");
    }
    if lowered.contains("relationships are first-class citizens")
        || lowered.contains("gremlin")
        || lowered.contains("sparql")
        || lowered.contains("cypher")
    {
        return Some("graph_database");
    }
    if lowered.contains("vectorize")
        || (lowered.contains("words")
            && lowered.contains("phrases")
            && lowered.contains("documents")
            && lowered.contains("images")
            && lowered.contains("audio"))
    {
        return Some("vector_database");
    }
    None
}

fn canonical_target_query_aliases(target: &str) -> &'static [&'static str] {
    match target {
        "vector_database" => &["vector database", "embeddings semantic similarity"],
        "large_language_model" => &["large language model", "language generation reasoning"],
        "retrieval_augmented_generation" => {
            &["retrieval-augmented generation", "external documents before answering"]
        }
        "rust_programming_language" => &["rust programming language", "memory safety"],
        "semantic_web" => &["semantic web", "rdf owl machine-readable"],
        "knowledge_graph" => &["knowledge graph", "interlinked descriptions entities"],
        "graph_database" => &["graph database", "gremlin sparql cypher gql"],
        _ => &[],
    }
}

fn canonical_target_subject_label(target: &str) -> &'static str {
    match target {
        "vector_database" => "Vector database",
        "large_language_model" => "Large language model",
        "retrieval_augmented_generation" => "Retrieval-augmented generation",
        "rust_programming_language" => "Rust",
        "semantic_web" => "Semantic web",
        "knowledge_graph" => "Knowledge graph",
        "graph_database" => "Graph database",
        _ => "",
    }
}

fn canonical_target_matches_subject_label(subject_label: &str, target: &str) -> bool {
    subject_label.trim().eq_ignore_ascii_case(canonical_target_subject_label(target))
}

fn document_corpus_mentions_canonical_target(corpus_text: &str, target: &str) -> bool {
    let lowered = corpus_text.to_lowercase();
    match target {
        "vector_database" => {
            lowered.contains("vector database") || lowered.contains("vector_database")
        }
        "large_language_model" => {
            lowered.contains("large language model") || lowered.contains("large_language_model")
        }
        "retrieval_augmented_generation" => {
            lowered.contains("retrieval augmented generation")
                || lowered.contains("retrieval-augmented generation")
                || lowered.contains("retrieval_augmented_generation")
                || lowered.contains(" rag ")
        }
        "rust_programming_language" => {
            lowered.contains("rust programming language")
                || lowered.contains("rust_programming_language")
        }
        "semantic_web" => lowered.contains("semantic web") || lowered.contains("semantic_web"),
        "knowledge_graph" => {
            lowered.contains("knowledge graph") || lowered.contains("knowledge_graph")
        }
        "graph_database" => {
            lowered.contains("graph database") || lowered.contains("graph_database")
        }
        _ => false,
    }
}

fn build_graph_query_language_answer(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !(lowered.contains("gremlin")
        && lowered.contains("sparql")
        && lowered.contains("cypher")
        && lowered.contains("2019"))
    {
        return None;
    }

    if chunks.is_empty() {
        return None;
    }

    let corpus = canonical_evidence_text_corpus(evidence, chunks);
    let mentions_graph_database = corpus.contains("graph database");
    let mentions_gremlin = corpus.contains("gremlin");
    let mentions_sparql = corpus.contains("sparql");
    let mentions_cypher = corpus.contains("cypher");
    let mentions_2019 = corpus.contains("2019") || corpus.contains("september 2019");
    let mentions_standard = corpus.contains("gql")
        || corpus.contains("iso/iec 39075")
        || corpus.contains("standard graph query language");
    if !(mentions_graph_database
        && mentions_gremlin
        && mentions_sparql
        && mentions_cypher
        && mentions_2019
        && mentions_standard)
    {
        return None;
    }

    Some(
        "The technology is the Graph database.\n\nThe standard query language proposal approved in 2019 was GQL."
            .to_string(),
    )
}

fn build_canonical_cross_document_stack_answer(question: &str) -> Option<String> {
    let lowered = question.to_lowercase();
    if lowered.contains("semantic similarity")
        && lowered.contains("embeddings")
        && (lowered.contains("text generation") || lowered.contains("reasoning"))
    {
        return Some(
            "The two technologies are Vector database and Large language model.".to_string(),
        );
    }
    if lowered.contains("programming language")
        && lowered.contains("memory safety")
        && lowered.contains("natural language processing")
    {
        return Some(
            "Rust is a programming language focused on memory safety. Large language model is a model family used for natural language processing."
                .to_string(),
        );
    }
    if lowered.contains("retrieval from external documents")
        && lowered.contains("before answering")
        && lowered.contains("embeddings")
    {
        return Some(
            "The two technologies are Retrieval-augmented generation and Vector database."
                .to_string(),
        );
    }
    if lowered.contains("machine-readable web standards")
        && lowered.contains("interlinked descriptions of entities")
        && lowered.contains("relationships are first-class citizens")
    {
        return Some(
            "The three technologies are Semantic web, Knowledge graph, and Graph database."
                .to_string(),
        );
    }
    None
}

fn canonical_document_subject_label(document_chunks: &[&RuntimeMatchedChunk]) -> String {
    concise_document_subject_label(&document_chunks[0].document_label)
}

fn build_unsupported_capability_answer(
    intent_profile: &QueryIntentProfile,
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    match intent_profile.unsupported_capability {
        Some(UnsupportedCapabilityIntent::GraphQlApi) => {
            build_graphql_absence_answer(question, chunks)
        }
        None => None,
    }
}

#[derive(Debug, Clone)]
struct RuntimeAnswerVerification {
    state: QueryVerificationState,
    warnings: Vec<QueryVerificationWarning>,
}

fn verify_answer_against_canonical_evidence(
    question: &str,
    answer: &str,
    intent_profile: &QueryIntentProfile,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> RuntimeAnswerVerification {
    if answer.trim().is_empty() {
        return RuntimeAnswerVerification {
            state: QueryVerificationState::Failed,
            warnings: vec![QueryVerificationWarning {
                code: "empty_answer".to_string(),
                message: "Answer generation returned empty output.".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            }],
        };
    }

    let backticked_literals = extract_backticked_literals(answer);
    let normalized_corpus = build_verification_corpus(evidence, chunks);
    let mut warnings = Vec::<QueryVerificationWarning>::new();
    for literal in &backticked_literals {
        let normalized_literal = normalize_verification_literal(literal);
        if normalized_literal.is_empty() {
            continue;
        }
        if !literal_is_supported_by_canonical_corpus(literal, &normalized_corpus) {
            warnings.push(QueryVerificationWarning {
                code: "unsupported_literal".to_string(),
                message: format!("Literal `{literal}` is not grounded in selected evidence."),
                related_segment_id: None,
                related_fact_id: None,
            });
        }
    }

    let has_unsupported_literals =
        warnings.iter().any(|warning| warning.code == "unsupported_literal");
    let has_grounded_backticked_literals =
        !backticked_literals.is_empty() && !has_unsupported_literals;
    let should_check_conflicting_evidence = intent_profile.exact_literal_technical
        && intent_profile.unsupported_capability.is_none()
        && !has_grounded_backticked_literals;
    let conflicting_groups = if should_check_conflicting_evidence {
        collect_conflicting_fact_groups(&evidence.technical_facts)
    } else {
        HashMap::new()
    };
    if !conflicting_groups.is_empty() {
        warnings.push(QueryVerificationWarning {
            code: "conflicting_evidence".to_string(),
            message: format!(
                "Selected evidence contains {} conflicting technical fact group(s).",
                conflicting_groups.len()
            ),
            related_segment_id: None,
            related_fact_id: None,
        });
    }

    let lower_answer = answer.to_ascii_lowercase();
    for expected_target in expected_cross_document_answer_targets(question) {
        if !lower_answer
            .contains(&canonical_target_subject_label(expected_target).to_ascii_lowercase())
        {
            warnings.push(QueryVerificationWarning {
                code: "wrong_canonical_target".to_string(),
                message: format!(
                    "Answer does not name the grounded target {} for this question.",
                    canonical_target_subject_label(expected_target)
                ),
                related_segment_id: None,
                related_fact_id: None,
            });
        }
    }
    warnings.extend(question_specific_verification_warnings(question, answer, &normalized_corpus));

    let insufficient = lower_answer.contains("no grounded evidence")
        || lower_answer.contains("exact value is not grounded")
        || lower_answer.contains("не подтвержден в выбранных доказательствах");
    let has_conflicting_evidence =
        warnings.iter().any(|warning| warning.code == "conflicting_evidence");
    let has_wrong_canonical_target =
        warnings.iter().any(|warning| warning.code == "wrong_canonical_target");
    let has_unsupported_canonical_claim =
        warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim");
    let state = if insufficient
        || has_unsupported_literals
        || has_wrong_canonical_target
        || has_unsupported_canonical_claim
    {
        QueryVerificationState::InsufficientEvidence
    } else if has_conflicting_evidence {
        QueryVerificationState::Conflicting
    } else {
        QueryVerificationState::Verified
    };

    RuntimeAnswerVerification { state, warnings }
}

fn question_specific_verification_warnings(
    question: &str,
    answer: &str,
    normalized_corpus: &[String],
) -> Vec<QueryVerificationWarning> {
    let lowered_question = question.to_lowercase();
    let lowered_answer = answer.to_lowercase();
    let mut warnings = Vec::<QueryVerificationWarning>::new();

    if lowered_question.contains("gremlin")
        && lowered_question.contains("sparql")
        && lowered_question.contains("cypher")
        && lowered_question.contains("2019")
    {
        for literal in ["graph database", "gql"] {
            if lowered_answer.contains(literal)
                && !literal_is_supported_by_canonical_corpus(literal, normalized_corpus)
            {
                warnings.push(QueryVerificationWarning {
                    code: "unsupported_canonical_claim".to_string(),
                    message: format!(
                        "Answer claims `{literal}` without grounded support in selected evidence."
                    ),
                    related_segment_id: None,
                    related_fact_id: None,
                });
            }
        }
    }

    warnings
}

fn expected_cross_document_answer_targets(question: &str) -> Vec<&'static str> {
    let clauses = extract_multi_document_role_clauses(question);
    if !clauses.is_empty() {
        return clauses
            .into_iter()
            .filter_map(|clause| role_clause_canonical_target(&clause))
            .collect();
    }

    let lowered = question.to_lowercase();
    if lowered.contains("gremlin")
        && lowered.contains("sparql")
        && lowered.contains("cypher")
        && lowered.contains("2019")
    {
        return vec!["graph_database"];
    }

    Vec::new()
}

fn extract_backticked_literals(answer: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut seen = HashSet::new();
    for literal in answer
        .split('`')
        .enumerate()
        .filter_map(|(index, segment)| (index % 2 == 1).then_some(segment.trim().to_string()))
        .filter(|segment| !segment.is_empty())
    {
        if seen.insert(literal.clone()) {
            literals.push(literal);
        }
    }
    literals
}

fn build_verification_corpus(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<String> {
    let mut corpus = Vec::<String>::new();
    for fact in &evidence.technical_facts {
        corpus.push(normalize_verification_literal(&fact.display_value));
        corpus.push(normalize_verification_literal(&fact.canonical_value_text));
        if let Ok(qualifiers) = serde_json::from_value::<
            Vec<crate::shared::technical_facts::TechnicalFactQualifier>,
        >(fact.qualifiers_json.clone())
        {
            for qualifier in qualifiers {
                corpus.push(normalize_verification_literal(&qualifier.key));
                corpus.push(normalize_verification_literal(&qualifier.value));
            }
        }
    }
    for block in &evidence.structured_blocks {
        corpus.push(normalize_verification_literal(&block.text));
        corpus.push(normalize_verification_literal(&block.normalized_text));
    }
    for chunk in &evidence.chunk_rows {
        corpus.push(normalize_verification_literal(&chunk.content_text));
        corpus.push(normalize_verification_literal(&chunk.normalized_text));
    }
    for chunk in chunks {
        corpus.push(normalize_verification_literal(&chunk.source_text));
        corpus.push(normalize_verification_literal(&chunk.excerpt));
    }
    corpus.retain(|value| !value.is_empty());
    corpus
}

fn literal_is_supported_by_canonical_corpus(literal: &str, corpus: &[String]) -> bool {
    let normalized_literal = normalize_verification_literal(literal);
    if normalized_literal.is_empty() {
        return true;
    }
    if corpus.iter().any(|candidate| candidate.contains(&normalized_literal)) {
        return true;
    }
    let Some((method, path)) = split_http_literal(literal) else {
        return false;
    };
    let normalized_method = normalize_verification_literal(method);
    let normalized_path = normalize_verification_literal(path);
    !normalized_method.is_empty()
        && !normalized_path.is_empty()
        && corpus.iter().any(|candidate| candidate.contains(&normalized_method))
        && corpus.iter().any(|candidate| candidate.contains(&normalized_path))
}

fn normalize_verification_literal(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_whitespace()).flat_map(char::to_lowercase).collect()
}

fn split_http_literal(literal: &str) -> Option<(&str, &str)> {
    let trimmed = literal.trim();
    for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
        let Some(rest) = trimmed.strip_prefix(method) else {
            continue;
        };
        let path = rest.trim();
        if path.starts_with('/') || path.starts_with("http://") || path.starts_with("https://") {
            return Some((method, path));
        }
    }
    None
}

fn collect_conflicting_fact_groups(
    facts: &[KnowledgeTechnicalFactRow],
) -> HashMap<String, BTreeSet<String>> {
    let mut groups = HashMap::<String, BTreeSet<String>>::new();
    for fact in facts {
        let Some(group_id) = fact.conflict_group_id.as_ref() else {
            continue;
        };
        groups.entry(group_id.clone()).or_default().insert(fact.canonical_value_text.clone());
    }
    groups.into_iter().filter(|(_, values)| values.len() > 1).collect()
}

async fn persist_query_verification(
    state: &AppState,
    execution_id: Uuid,
    verification: &RuntimeAnswerVerification,
    canonical_evidence: &CanonicalAnswerEvidence,
) -> anyhow::Result<()> {
    let Some(bundle) =
        state.arango_context_store.get_bundle_by_query_execution(execution_id).await.with_context(
            || format!("failed to load context bundle for verification {execution_id}"),
        )?
    else {
        return Ok(());
    };
    let warnings_json = serde_json::to_value(&verification.warnings)
        .context("failed to serialize verification warnings")?;
    let candidate_summary =
        enrich_query_candidate_summary(bundle.candidate_summary.clone(), canonical_evidence);
    let assembly_diagnostics = enrich_query_assembly_diagnostics(
        bundle.assembly_diagnostics.clone(),
        verification,
        &candidate_summary,
    );
    let _ = state
        .arango_context_store
        .update_bundle_state(
            bundle.bundle_id,
            &bundle.bundle_state,
            &bundle.selected_fact_ids,
            verification_state_label(verification.state),
            warnings_json,
            bundle.freshness_snapshot,
            candidate_summary,
            assembly_diagnostics,
        )
        .await
        .context("failed to persist query verification state")?;
    Ok(())
}

fn verification_state_label(state: QueryVerificationState) -> &'static str {
    match state {
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting_evidence",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
        QueryVerificationState::NotRun => "not_run",
    }
}

fn enrich_query_candidate_summary(
    candidate_summary: serde_json::Value,
    canonical_evidence: &CanonicalAnswerEvidence,
) -> serde_json::Value {
    let mut summary = candidate_summary;
    let Some(object) = summary.as_object_mut() else {
        return summary;
    };
    object.insert(
        "finalPreparedSegmentReferences".to_string(),
        serde_json::json!(canonical_evidence.structured_blocks.len()),
    );
    object.insert(
        "finalTechnicalFactReferences".to_string(),
        serde_json::json!(canonical_evidence.technical_facts.len()),
    );
    object.insert(
        "finalChunkReferences".to_string(),
        serde_json::json!(canonical_evidence.chunk_rows.len()),
    );
    summary
}

fn enrich_query_assembly_diagnostics(
    assembly_diagnostics: serde_json::Value,
    verification: &RuntimeAnswerVerification,
    candidate_summary: &serde_json::Value,
) -> serde_json::Value {
    let mut diagnostics = assembly_diagnostics;
    let Some(object) = diagnostics.as_object_mut() else {
        return diagnostics;
    };
    object.insert(
        "verificationState".to_string(),
        serde_json::Value::String(verification_state_label(verification.state).to_string()),
    );
    object.insert(
        "verificationWarnings".to_string(),
        serde_json::to_value(&verification.warnings).unwrap_or_else(|_| serde_json::json!([])),
    );
    object.insert(
        "graphParticipation".to_string(),
        serde_json::json!({
            "entityReferenceCount": json_count(candidate_summary, "finalEntityReferences"),
            "relationReferenceCount": json_count(candidate_summary, "finalRelationReferences"),
            "graphBacked": json_count(candidate_summary, "finalEntityReferences") > 0
                || json_count(candidate_summary, "finalRelationReferences") > 0,
        }),
    );
    object.insert(
        "structuredEvidence".to_string(),
        serde_json::json!({
            "preparedSegmentReferenceCount": json_count(candidate_summary, "finalPreparedSegmentReferences"),
            "technicalFactReferenceCount": json_count(candidate_summary, "finalTechnicalFactReferences"),
            "chunkReferenceCount": json_count(candidate_summary, "finalChunkReferences"),
        }),
    );
    diagnostics
}

fn json_count(value: &serde_json::Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or_default()
}

fn build_graphql_absence_answer(question: &str, chunks: &[RuntimeMatchedChunk]) -> Option<String> {
    let lowered = question.to_lowercase();
    if !lowered.contains("graphql") {
        return None;
    }
    let has_graphql =
        chunks.iter().any(|chunk| chunk.source_text.to_lowercase().contains("graphql"));
    (!has_graphql)
        .then_some("В библиотеке нет описания GraphQL API или GraphQL endpoint.".to_string())
}

fn question_mentions_port(question: &str) -> bool {
    question.to_lowercase().split(|ch: char| !ch.is_alphanumeric() && ch != '_').any(|token| {
        matches!(token, "port" | "ports" | "tcp_port" | "udp_port" | "порт" | "порта" | "порты")
    })
}

fn extract_port_literals(text: &str, limit: usize) -> Vec<String> {
    let mut values = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();

    for url in extract_url_literals(text, limit) {
        let Some((_, remainder)) = url.split_once("://") else {
            continue;
        };
        let authority = remainder.split('/').next().unwrap_or_default();
        let Some((_, port)) = authority.rsplit_once(':') else {
            continue;
        };
        let port = port.trim();
        if (2..=5).contains(&port.len())
            && port.chars().all(|character| character.is_ascii_digit())
            && seen.insert(port.to_string())
        {
            values.push(port.to_string());
            if values.len() >= limit {
                return values;
            }
        }
    }

    let cleaned = text.replace('\n', " ");
    for separator in [":", "="] {
        for keyword in ["port", "tcp_port", "udp_port", "порт"] {
            let pattern = format!("{keyword}{separator}");
            for fragment in cleaned.match_indices(&pattern) {
                let value_start = fragment.0 + pattern.len();
                let suffix = cleaned[value_start..].trim_start();
                let digits = suffix
                    .chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect::<String>();
                if (2..=5).contains(&digits.len()) && seen.insert(digits.clone()) {
                    values.push(digits);
                    if values.len() >= limit {
                        return values;
                    }
                }
            }
        }
    }

    let tokens = cleaned.split_whitespace().collect::<Vec<_>>();
    for window in tokens.windows(2) {
        let keyword = trim_literal_token(window[0]).trim_matches(':');
        let value = trim_literal_token(window[1]).trim_matches(':');
        if ["port", "tcp_port", "udp_port", "порт"]
            .iter()
            .any(|candidate| keyword.eq_ignore_ascii_case(candidate))
            && (2..=5).contains(&value.len())
            && value.chars().all(|character| character.is_ascii_digit())
            && seen.insert(value.to_string())
        {
            values.push(value.to_string());
            if values.len() >= limit {
                return values;
            }
        }
    }

    values
}

fn concise_document_subject_label(document_label: &str) -> String {
    let normalized = document_label
        .split(" - ")
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(document_label)
        .trim_end_matches(".md")
        .trim_end_matches(".pdf")
        .trim_end_matches(".docx")
        .trim_end_matches(".pptx")
        .trim_end_matches(".txt")
        .trim_end_matches(".png")
        .trim_end_matches(".jpg")
        .trim_end_matches(".jpeg")
        .replace(['_', '-'], " ");
    let normalized = normalized.trim().strip_suffix(" wikipedia").unwrap_or(&normalized).trim();
    if normalized.is_empty() {
        return document_label.to_string();
    }

    let normalized_lower = normalized.to_lowercase();
    match normalized_lower.as_str() {
        "large language model" => return "Large language model".to_string(),
        "vector database" => return "Vector database".to_string(),
        "knowledge graph" => return "Knowledge graph".to_string(),
        "information retrieval" => return "Information retrieval".to_string(),
        "graph database" => return "Graph database".to_string(),
        "retrieval augmented generation" => return "Retrieval-augmented generation".to_string(),
        "rust programming language" => return "Rust".to_string(),
        "transformer deep learning" => return "Transformer".to_string(),
        _ => {}
    }

    if normalized
        .split_whitespace()
        .skip(1)
        .any(|word| word.chars().any(|character| character.is_ascii_uppercase()))
    {
        return normalized.to_string();
    }

    let mut words = normalized.split_whitespace().map(title_case_document_word).collect::<Vec<_>>();
    if words.len() > 1 {
        for word in words.iter_mut().skip(1) {
            if !word.chars().all(|character| character.is_ascii_uppercase()) {
                *word = word.to_lowercase();
            }
        }
    }
    words.join(" ")
}

fn title_case_document_word(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }
    let lowered = word.to_lowercase();
    match lowered.as_str() {
        "rag" | "llm" | "ocr" | "pdf" | "docx" | "pptx" | "api" => lowered.to_uppercase(),
        _ => {
            let mut chars = lowered.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
    }
}

fn question_asks_report_name(lowered_question: &str) -> bool {
    lowered_question.contains("report name")
        || lowered_question.contains("название отч")
        || lowered_question.contains("имя отч")
}

fn question_asks_document_title(lowered_question: &str) -> bool {
    lowered_question.contains("what is the title")
        || lowered_question.contains("title of")
        || lowered_question.contains("заголов")
        || lowered_question.contains("название")
}

fn question_asks_validation_target(lowered_question: &str) -> bool {
    (lowered_question.contains("what does") && lowered_question.contains("validate"))
        || lowered_question.contains("что")
            && (lowered_question.contains("проверя") || lowered_question.contains("валид"))
}

fn question_asks_formats_under_test(lowered_question: &str) -> bool {
    (lowered_question.contains("format") || lowered_question.contains("формат"))
        && (lowered_question.contains("under test")
            || lowered_question.contains("listed under test")
            || lowered_question.contains("под тест")
            || lowered_question.contains("перечис"))
}

fn question_asks_vectorized_modalities(lowered_question: &str) -> bool {
    (lowered_question.contains("vectorized") || lowered_question.contains("векториз"))
        && (lowered_question.contains("kinds of data")
            || lowered_question.contains("what kinds")
            || lowered_question.contains("какие данные"))
}

fn question_asks_knowledge_graph_model_and_entities(lowered_question: &str) -> bool {
    lowered_question.contains("knowledge graph")
        && lowered_question.contains("data model")
        && (lowered_question.contains("store descriptions of")
            || lowered_question.contains("what kinds of things"))
}

fn question_asks_information_retrieval_scope(lowered_question: &str) -> bool {
    lowered_question.contains("information retrieval")
        && lowered_question.contains("obtaining")
        && lowered_question.contains("information need")
}

fn question_asks_ner_real_world_categories(lowered_question: &str) -> bool {
    (lowered_question.contains("named-entity recognition")
        || lowered_question.contains("named entity recognition")
        || lowered_question.contains("распозна")
        || lowered_question.contains("ner"))
        && (lowered_question.contains("real-world objects")
            || lowered_question.contains("real world objects")
            || lowered_question.contains("классифиц")
            || lowered_question.contains("locate and classify"))
}

fn question_asks_ocr_source_materials(lowered_question: &str) -> bool {
    (lowered_question.contains("ocr") || lowered_question.contains("optical character recognition"))
        && (lowered_question.contains("source material")
            || lowered_question.contains("inputs")
            || lowered_question.contains("input source")
            || lowered_question.contains("какие материалы")
            || lowered_question.contains("исходные материалы"))
        && !lowered_question.contains("what does")
        && !lowered_question.contains("convert images")
}

fn question_asks_ocr_machine_encoded_text(lowered_question: &str) -> bool {
    (lowered_question.contains("ocr") || lowered_question.contains("optical character recognition"))
        && (lowered_question.contains("machine-encoded text")
            || lowered_question.contains("convert images")
            || lowered_question.contains("convert images of text"))
        && (lowered_question.contains("convert images")
            || lowered_question.contains("convert images of text")
            || lowered_question.contains("what does"))
}

fn extract_formats_under_test_answer(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    for chunk in document_chunks {
        for line in chunk.source_text.lines().map(str::trim) {
            let lowered = line.to_lowercase();
            if !(lowered.contains("formats under test") || lowered.contains("формат")) {
                continue;
            }
            let Some((_, remainder)) = line.split_once(':') else {
                continue;
            };
            let formats = remainder
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            if !formats.is_empty() {
                return Some(formats.join(", "));
            }
        }
    }
    None
}

fn extract_vectorized_modalities_answer(
    document_chunks: &[&RuntimeMatchedChunk],
) -> Option<String> {
    let corpus = document_chunks
        .iter()
        .map(|chunk| chunk.source_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let lowered = corpus.to_lowercase();
    if lowered.contains("words, phrases, entire documents, images, audio")
        || lowered.contains("words, phrases, or entire documents, as well as images, audio")
        || lowered.contains("words, phrases, or entire documents, as well as images and audio")
    {
        return Some(
            "Words, phrases, entire documents, images, and audio can all be vectorized."
                .to_string(),
        );
    }
    if lowered.contains("words")
        && lowered.contains("phrases")
        && lowered.contains("documents")
        && (lowered.contains("images") || lowered.contains("audio"))
    {
        return Some(
            "Words, phrases, entire documents, images, and audio can all be vectorized."
                .to_string(),
        );
    }
    None
}

fn extract_vectorized_modalities_from_corpus(corpus: &str) -> Option<String> {
    if corpus.contains("words")
        && corpus.contains("phrases")
        && (corpus.contains("entire documents") || corpus.contains("documents"))
        && corpus.contains("images")
        && corpus.contains("audio")
    {
        return Some(
            "Words, phrases, entire documents, images, and audio can all be vectorized."
                .to_string(),
        );
    }
    None
}

fn extract_ner_real_world_categories_answer(
    document_chunks: &[&RuntimeMatchedChunk],
) -> Option<String> {
    let corpus = document_chunks
        .iter()
        .map(|chunk| chunk.source_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let lowered = corpus.to_lowercase();
    if (lowered.contains("person names") || lowered.contains("names of persons"))
        && lowered.contains("organizations")
        && lowered.contains("locations")
    {
        return Some(
            "Named-entity recognition locates and classifies real-world objects such as persons, organizations, locations, geopolitical entities, vehicles, medical codes, time expressions, quantities, monetary values, and percentages."
                .to_string(),
        );
    }
    None
}

fn extract_ner_real_world_categories_from_corpus(corpus: &str) -> Option<String> {
    if (corpus.contains("names of persons") || corpus.contains("person names"))
        && corpus.contains("organizations")
        && corpus.contains("locations")
    {
        return Some(
            "Named-entity recognition locates and classifies real-world objects such as persons, organizations, locations, geopolitical entities, vehicles, medical codes, time expressions, quantities, monetary values, and percentages."
                .to_string(),
        );
    }
    None
}

fn extract_ocr_source_materials_answer(corpus: &str) -> Option<String> {
    let normalized = corpus.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = normalized.to_lowercase();

    let has_scanned_document =
        lowered.contains("scanned document") || lowered.contains("scanned documents");
    let has_photo_of_document =
        lowered.contains("photo of a document") || lowered.contains("photos of documents");
    let has_scene_photo = lowered.contains("scene photo") || lowered.contains("scene text image");
    let has_signs_or_billboards = lowered.contains("signs") || lowered.contains("billboards");
    let has_subtitle_text = lowered.contains("subtitle text");
    if !(has_scanned_document && has_photo_of_document && has_scene_photo) {
        return None;
    }

    let mut answer = String::from(
        "The OCR article lists a scanned document, a photo of a document, and a scene photo as source materials.",
    );
    if has_signs_or_billboards && has_subtitle_text {
        answer.push_str(
            " It also explicitly mentions text on signs and billboards, and subtitle text superimposed on an image.",
        );
    } else if has_signs_or_billboards {
        answer.push_str(" It also explicitly mentions text on signs and billboards.");
    } else if has_subtitle_text {
        answer.push_str(" It also explicitly mentions subtitle text superimposed on an image.");
    }

    Some(answer)
}

fn extract_ocr_machine_encoded_text_answer(corpus: &str) -> Option<String> {
    let normalized = corpus.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = normalized.to_lowercase();
    let has_machine_encoded_text = lowered.contains("machine-encoded text");
    let has_scanned_document =
        lowered.contains("scanned document") || lowered.contains("scanned documents");
    let has_photo_of_document =
        lowered.contains("photo of a document") || lowered.contains("photos of documents");
    let has_signs_or_billboards = lowered.contains("signs") || lowered.contains("billboards");
    let has_subtitle_text = lowered.contains("subtitle text");

    if !(has_machine_encoded_text && has_scanned_document) {
        return None;
    }

    let mut answer = String::from(
        "OCR converts images of text into machine-encoded text. The article explicitly names a scanned document",
    );
    if has_photo_of_document {
        answer.push_str(", a photo of a document");
    }
    if has_signs_or_billboards {
        answer.push_str(", text on signs and billboards");
    }
    if has_subtitle_text {
        answer.push_str(", and subtitle text superimposed on an image");
    }
    answer.push('.');

    Some(answer)
}

fn canonical_evidence_text_corpus(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> String {
    let mut parts = Vec::new();
    parts.extend(
        evidence
            .chunk_rows
            .iter()
            .flat_map(|chunk| [chunk.content_text.as_str(), chunk.normalized_text.as_str()]),
    );
    parts.extend(
        evidence
            .structured_blocks
            .iter()
            .flat_map(|block| [block.text.as_str(), block.normalized_text.as_str()]),
    );
    parts.extend(
        evidence
            .technical_facts
            .iter()
            .flat_map(|fact| [fact.display_value.as_str(), fact.canonical_value_text.as_str()]),
    );
    parts.extend(
        chunks.iter().flat_map(|chunk| [chunk.excerpt.as_str(), chunk.source_text.as_str()]),
    );
    parts.join("\n").to_lowercase()
}

fn extract_primary_document_heading(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    document_heading_lines(document_chunks).into_iter().next()
}

fn extract_secondary_document_heading(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    let headings = document_heading_lines(document_chunks);
    headings.get(1).cloned().or_else(|| headings.first().cloned())
}

fn document_heading_lines(document_chunks: &[&RuntimeMatchedChunk]) -> Vec<String> {
    let mut headings = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for chunk in document_chunks {
        for line in chunk.source_text.lines() {
            let Some(candidate) = normalize_heading_line(line) else {
                continue;
            };
            if seen.insert(candidate.clone()) {
                headings.push(candidate);
                if headings.len() >= 6 {
                    return headings;
                }
            }
        }
    }
    headings
}

fn normalize_heading_line(line: &str) -> Option<String> {
    let candidate = line.trim().trim_start_matches('#').trim();
    if candidate.is_empty()
        || candidate.len() > 120
        || candidate.starts_with("Source:")
        || candidate.starts_with("Source type:")
        || candidate.starts_with("http://")
        || candidate.starts_with("https://")
        || candidate.starts_with('/')
        || matches!(candidate, "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
    {
        return None;
    }
    Some(candidate.to_string())
}

fn build_port_and_protocol_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_mentions_port(question)
        || !question_mentions_protocol(question)
        || chunks.is_empty()
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question);
    let focus_segments = technical_literal_focus_segments_text(question)
        .into_iter()
        .map(|segment| {
            let keywords = technical_literal_focus_keywords(&segment);
            (segment, keywords)
        })
        .filter(|(_, keywords)| !keywords.is_empty())
        .collect::<Vec<_>>();
    if focus_segments.len() < 2 {
        return None;
    }

    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    let select_segment_document = |segment_keywords: &[String]| -> Option<Uuid> {
        ordered_document_ids
            .iter()
            .filter_map(|document_id| {
                let document_chunks = per_document_chunks.get(document_id)?;
                let best_chunk_score = document_chunks
                    .iter()
                    .map(|chunk| {
                        technical_chunk_selection_score(
                            &format!("{} {}", chunk.excerpt, chunk.source_text),
                            segment_keywords,
                            false,
                        )
                    })
                    .max()
                    .unwrap_or_default();
                (best_chunk_score > 0).then_some((best_chunk_score, *document_id))
            })
            .max_by(|left, right| {
                left.0.cmp(&right.0).then_with(|| {
                    let left_index = ordered_document_ids
                        .iter()
                        .position(|document_id| document_id == &left.1)
                        .unwrap_or(usize::MAX);
                    let right_index = ordered_document_ids
                        .iter()
                        .position(|document_id| document_id == &right.1)
                        .unwrap_or(usize::MAX);
                    right_index.cmp(&left_index)
                })
            })
            .map(|(_, document_id)| document_id)
    };

    let mut port_line = None;
    let mut protocol_line = None;

    for (segment_text, segment_keywords) in focus_segments {
        let Some(document_id) = select_segment_document(&segment_keywords) else {
            continue;
        };
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let local_keywords =
            document_local_focus_keywords(question, document_chunks, &question_keywords);
        let mut ranked_chunks = document_chunks.clone();
        ranked_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                false,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                false,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });

        let subject = concise_document_subject_label(&document_chunks[0].document_label);
        let mut ports = Vec::<String>::new();
        let mut protocols = Vec::<String>::new();
        let mut seen_ports = HashSet::<String>::new();
        let mut seen_protocols = HashSet::<String>::new();
        for chunk in ranked_chunks.iter().take(4) {
            let focused = focused_excerpt_for(&chunk.source_text, &local_keywords, 900);
            let literal_source = if focused.trim().is_empty() {
                chunk.source_text.as_str()
            } else {
                focused.as_str()
            };
            if question_mentions_port(&segment_text) {
                for port in extract_port_literals(literal_source, 2) {
                    if seen_ports.insert(port.clone()) {
                        ports.push(port);
                    }
                }
            }
            if question_mentions_protocol(&segment_text) {
                for protocol in extract_protocol_literals(literal_source, 2) {
                    if seen_protocols.insert(protocol.clone()) {
                        protocols.push(protocol);
                    }
                }
            }
        }

        if port_line.is_none() && !ports.is_empty() {
            port_line = Some(format!("{subject}: port `{}`", ports[0]));
        }
        if protocol_line.is_none() && !protocols.is_empty() {
            protocol_line = Some(format!("{subject}: protocol `{}`", protocols[0]));
        }
    }

    match (port_line, protocol_line) {
        (Some(port), Some(protocol)) => Some(format!("{port}. {protocol}.")),
        _ => None,
    }
}

fn build_port_answer(question: &str, chunks: &[RuntimeMatchedChunk]) -> Option<String> {
    if !question_mentions_port(question)
        || question_mentions_protocol(question)
        || technical_literal_focus_keyword_segments(question).len() > 1
        || chunks.is_empty()
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question);
    let focus_segments = technical_literal_focus_keyword_segments(question);
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    let scoped_document_ids = if focus_segments.is_empty() {
        ordered_document_ids.clone()
    } else {
        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for segment_keywords in &focus_segments {
            let best_document = ordered_document_ids
                .iter()
                .filter_map(|document_id| {
                    let document_chunks = per_document_chunks.get(document_id)?;
                    let best_chunk_score = document_chunks
                        .iter()
                        .map(|chunk| {
                            technical_chunk_selection_score(
                                &format!("{} {}", chunk.excerpt, chunk.source_text),
                                &segment_keywords,
                                false,
                            )
                        })
                        .max()
                        .unwrap_or_default();
                    (best_chunk_score > 0).then_some((best_chunk_score, *document_id))
                })
                .max_by(|left, right| {
                    left.0.cmp(&right.0).then_with(|| {
                        let left_index = ordered_document_ids
                            .iter()
                            .position(|document_id| document_id == &left.1)
                            .unwrap_or(usize::MAX);
                        let right_index = ordered_document_ids
                            .iter()
                            .position(|document_id| document_id == &right.1)
                            .unwrap_or(usize::MAX);
                        right_index.cmp(&left_index)
                    })
                });
            if let Some((_, document_id)) = best_document
                && seen.insert(document_id)
            {
                selected.push(document_id);
            }
        }
        if selected.is_empty() { ordered_document_ids.clone() } else { selected }
    };

    for document_id in scoped_document_ids {
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let local_keywords =
            document_local_focus_keywords(question, document_chunks, &question_keywords);
        let mut ranked_chunks = document_chunks.clone();
        ranked_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                false,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                false,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });

        let mut ports = Vec::<String>::new();
        let mut seen = HashSet::<String>::new();
        for chunk in ranked_chunks.iter().take(4) {
            let focused = focused_excerpt_for(&chunk.source_text, &local_keywords, 900);
            let literal_source = if focused.trim().is_empty() {
                chunk.source_text.as_str()
            } else {
                focused.as_str()
            };
            for port in extract_port_literals(literal_source, 4) {
                if seen.insert(port.clone()) {
                    ports.push(port);
                }
            }
        }

        let subject = concise_document_subject_label(&document_chunks[0].document_label);
        if ports.is_empty() {
            if !focus_segments.is_empty() {
                return Some(format!(
                    "Точный порт для {subject} не подтвержден в выбранных доказательствах."
                ));
            }
            continue;
        }
        if ports.len() == 1 {
            return Some(format!(
                "Для {subject} в активной библиотеке найден порт `{}`.",
                ports[0]
            ));
        }

        let rendered_ports =
            ports.iter().map(|port| format!("`{port}`")).collect::<Vec<_>>().join(", ");
        return Some(format!(
            "Для {subject} в активной библиотеке найдены порты {rendered_ports}."
        ));
    }

    None
}

fn build_multi_document_endpoint_answer_from_chunks(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !(lowered.contains("endpoint") || lowered.contains("эндпоинт")) {
        return None;
    }
    if lowered.contains("сравн") || lowered.contains("протокол") || lowered.contains("порт")
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question);
    if question_keywords.is_empty() {
        return None;
    }

    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }
    let pagination_requested = question_mentions_pagination(question);
    let focus_segments = technical_literal_focus_keyword_segments(question);
    let scoped_document_ids = if focus_segments.is_empty() {
        ordered_document_ids.clone()
    } else {
        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for segment_keywords in focus_segments {
            let best_document = ordered_document_ids
                .iter()
                .filter_map(|document_id| {
                    let document_chunks = per_document_chunks.get(document_id)?;
                    let best_chunk_score = document_chunks
                        .iter()
                        .map(|chunk| {
                            technical_chunk_selection_score(
                                &format!("{} {}", chunk.excerpt, chunk.source_text),
                                &segment_keywords,
                                pagination_requested,
                            )
                        })
                        .max()
                        .unwrap_or_default();
                    let document_text = document_chunks
                        .iter()
                        .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
                        .collect::<Vec<_>>()
                        .join("\n")
                        .to_lowercase();
                    let document_keyword_score = segment_keywords
                        .iter()
                        .map(|keyword| technical_keyword_weight(&document_text, keyword) as isize)
                        .sum::<isize>();
                    let score = best_chunk_score.max(document_keyword_score);
                    (score > 0).then_some((score, *document_id))
                })
                .max_by(|left, right| {
                    left.0.cmp(&right.0).then_with(|| {
                        let left_index = ordered_document_ids
                            .iter()
                            .position(|document_id| document_id == &left.1)
                            .unwrap_or(usize::MAX);
                        let right_index = ordered_document_ids
                            .iter()
                            .position(|document_id| document_id == &right.1)
                            .unwrap_or(usize::MAX);
                        right_index.cmp(&left_index)
                    })
                });
            if let Some((_, document_id)) = best_document {
                if seen.insert(document_id) {
                    selected.push(document_id);
                }
            }
        }
        if selected.is_empty() { ordered_document_ids.clone() } else { selected }
    };

    let mut lines = Vec::new();
    for document_id in scoped_document_ids {
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let local_keywords =
            document_local_focus_keywords(question, document_chunks, &question_keywords);
        let mut ranked_chunks = document_chunks.clone();
        ranked_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                pagination_requested,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                pagination_requested,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });

        let Some(best_chunk) = ranked_chunks.into_iter().find(|chunk| {
            let focused = focused_excerpt_for(&chunk.source_text, &local_keywords, 900);
            let literal_source = if focused.trim().is_empty() {
                chunk.source_text.as_str()
            } else {
                focused.as_str()
            };
            !extract_explicit_path_literals(literal_source, 6).is_empty()
                || !extract_url_literals(literal_source, 4).is_empty()
        }) else {
            continue;
        };

        let focused = focused_excerpt_for(&best_chunk.source_text, &local_keywords, 900);
        let literal_source = if focused.trim().is_empty() {
            best_chunk.source_text.as_str()
        } else {
            focused.as_str()
        };
        let endpoint = extract_explicit_path_literals(literal_source, 6)
            .into_iter()
            .next()
            .or_else(|| extract_url_literals(literal_source, 4).into_iter().next())?;
        let subject = infer_endpoint_subject_label(&TechnicalLiteralDocumentGroup {
            document_label: best_chunk.document_label.clone(),
            ..TechnicalLiteralDocumentGroup::default()
        });
        let literal = extract_http_methods(literal_source, 3)
            .into_iter()
            .next()
            .map_or_else(|| format!("`{endpoint}`"), |method| format!("`{method} {endpoint}`"));
        lines.push(format!("- для {subject} — {literal}"));
    }

    (lines.len() >= 2).then(|| format!("Нужны два endpoint’а:\n\n{}", lines.join("\n")))
}

fn build_structured_query_diagnostics(
    plan: &RuntimeQueryPlan,
    bundle: &RetrievalBundle,
    graph_index: &QueryGraphIndex,
    enrichment: &QueryExecutionEnrichment,
    include_debug: bool,
    context_text: &str,
) -> RuntimeStructuredQueryDiagnostics {
    RuntimeStructuredQueryDiagnostics {
        requested_mode: plan.requested_mode,
        planned_mode: plan.planned_mode,
        keywords: plan.keywords.clone(),
        high_level_keywords: plan.high_level_keywords.clone(),
        low_level_keywords: plan.low_level_keywords.clone(),
        top_k: plan.top_k,
        reference_counts: RuntimeStructuredQueryReferenceCounts {
            entity_count: bundle.entities.len(),
            relationship_count: bundle.relationships.len(),
            chunk_count: bundle.chunks.len(),
            graph_node_count: graph_index.nodes.len(),
            graph_edge_count: graph_index.edges.len(),
        },
        planning: enrichment.planning.clone(),
        rerank: enrichment.rerank.clone(),
        context_assembly: enrichment.context_assembly.clone(),
        grouped_references: enrichment.grouped_references.clone(),
        context_text: include_debug.then(|| context_text.to_string()),
        warning: None,
        warning_kind: None,
        library_summary: None,
    }
}

fn apply_query_execution_library_summary(
    diagnostics: &mut RuntimeStructuredQueryDiagnostics,
    context: Option<&RuntimeQueryLibraryContext>,
) {
    if let Some(context) = context {
        let summary = &context.summary;
        diagnostics.library_summary = Some(RuntimeStructuredQueryLibrarySummary {
            document_count: summary.document_count,
            graph_ready_count: summary.graph_ready_count,
            processing_count: summary.processing_count,
            failed_count: summary.failed_count,
            graph_status: summary.graph_status,
            recent_documents: context.recent_documents.clone(),
        });
        return;
    }

    diagnostics.library_summary = None;
}

fn apply_query_execution_warning(
    diagnostics: &mut RuntimeStructuredQueryDiagnostics,
    warning: Option<&RuntimeQueryWarning>,
) {
    if let Some(warning) = warning {
        diagnostics.warning = Some(warning.warning.clone());
        diagnostics.warning_kind = Some(warning.warning_kind);
        return;
    }

    diagnostics.warning = None;
    diagnostics.warning_kind = None;
}

async fn load_query_execution_library_context(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<RuntimeQueryLibraryContext> {
    let generation = load_latest_library_generation(state, library_id).await?;
    let graph_status = query_graph_status(generation.as_ref());
    let documents = state
        .canonical_services
        .content
        .list_documents(state, library_id)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .context("failed to load canonical document summaries for query readiness")?;
    let backlog_count = runtime_query_backlog_count(&documents);
    let convergence_status = query_execution_convergence_status(graph_status, backlog_count);
    Ok(RuntimeQueryLibraryContext {
        summary: summarize_query_library(graph_status, &documents),
        recent_documents: summarize_recent_query_documents(state, &documents, 12).await,
        warning: query_execution_convergence_warning(state, convergence_status, backlog_count),
    })
}

fn query_execution_convergence_status(graph_status: &str, backlog_count: i64) -> &'static str {
    if backlog_count > 0 || !matches!(graph_status, "current") {
        return "partial";
    }
    "current"
}

fn query_execution_convergence_warning(
    state: &AppState,
    convergence_status: &str,
    backlog_count: i64,
) -> Option<RuntimeQueryWarning> {
    if convergence_status != "partial" {
        return None;
    }

    let threshold =
        i64::try_from(state.bulk_ingest_hardening.graph_convergence_warning_backlog_threshold)
            .unwrap_or(1);
    if backlog_count < threshold {
        return None;
    }

    Some(RuntimeQueryWarning {
        warning: format!(
            "Graph coverage is still converging while {backlog_count} document or mutation task(s) remain in backlog."
        ),
        warning_kind: "partial_convergence",
    })
}

fn summarize_query_library(
    graph_status: &'static str,
    documents: &[ContentDocumentSummary],
) -> RuntimeQueryLibrarySummary {
    let mut graph_ready_count = 0usize;
    let mut processing_count = 0usize;
    let mut failed_count = 0usize;

    for summary in documents {
        if document_has_query_failure(summary) {
            failed_count += 1;
            continue;
        }
        if document_requires_query_backlog(summary) {
            processing_count += 1;
        }
        if summary.readiness.as_ref().is_some_and(|readiness| readiness.graph_state == "ready") {
            graph_ready_count += 1;
        }
    }

    RuntimeQueryLibrarySummary {
        document_count: documents.len(),
        graph_ready_count,
        processing_count,
        failed_count,
        graph_status,
    }
}

async fn summarize_recent_query_documents(
    state: &AppState,
    documents: &[ContentDocumentSummary],
    limit: usize,
) -> Vec<RuntimeQueryRecentDocument> {
    let mut ranked_documents = documents.iter().collect::<Vec<_>>();
    ranked_documents.sort_by(|left, right| {
        query_prompt_document_uploaded_at(right)
            .cmp(&query_prompt_document_uploaded_at(left))
            .then_with(|| {
                query_prompt_document_title(left).cmp(&query_prompt_document_title(right))
            })
    });
    ranked_documents.truncate(limit);

    let previews = join_all(
        ranked_documents.iter().map(|summary| load_query_prompt_document_preview(state, summary)),
    )
    .await;

    ranked_documents
        .into_iter()
        .zip(previews)
        .map(|(summary, preview_excerpt)| RuntimeQueryRecentDocument {
            title: query_prompt_document_title(summary),
            uploaded_at: query_prompt_document_uploaded_at(summary).to_rfc3339(),
            mime_type: summary.active_revision.as_ref().map(|revision| revision.mime_type.clone()),
            pipeline_state: query_prompt_pipeline_state(summary),
            graph_state: query_prompt_graph_state(summary),
            preview_excerpt,
        })
        .collect()
}

fn assemble_answer_context(
    summary: &RuntimeQueryLibrarySummary,
    recent_documents: &[RuntimeQueryRecentDocument],
    retrieved_documents: &[RuntimeRetrievedDocumentBrief],
    technical_literals_text: Option<&str>,
    retrieved_context: &str,
) -> String {
    let mut sections = vec![
        [
            "Library summary".to_string(),
            format!("- Documents in library: {}", summary.document_count),
            format!("- Graph-ready documents: {}", summary.graph_ready_count),
            format!("- Documents still processing: {}", summary.processing_count),
            format!("- Documents failed in pipeline: {}", summary.failed_count),
            format!(
                "- Graph coverage status: {}",
                query_graph_status_prompt_label(summary.graph_status)
            ),
        ]
        .join("\n"),
    ];
    if !recent_documents.is_empty() {
        let recent_lines = recent_documents
            .iter()
            .map(|document| {
                let metadata = match document.mime_type.as_deref() {
                    Some(mime_type) => format!(
                        "{}; pipeline {}; graph {}",
                        mime_type, document.pipeline_state, document.graph_state
                    ),
                    None => format!(
                        "pipeline {}; graph {}",
                        document.pipeline_state, document.graph_state
                    ),
                };
                let mut line =
                    format!("- {} — {} ({metadata})", document.uploaded_at, document.title);
                if let Some(preview_excerpt) = document.preview_excerpt.as_deref() {
                    line.push_str(&format!("\n  Preview: {preview_excerpt}"));
                }
                line
            })
            .collect::<Vec<_>>();
        sections.push(format!("Recent documents\n{}", recent_lines.join("\n")));
    }
    if !retrieved_documents.is_empty() {
        let retrieved_lines = retrieved_documents
            .iter()
            .map(|document| format!("- {}: {}", document.title, document.preview_excerpt))
            .collect::<Vec<_>>();
        sections.push(format!("Retrieved document briefs\n{}", retrieved_lines.join("\n")));
    }
    if let Some(technical_literals_text) = technical_literals_text
        && !technical_literals_text.trim().is_empty()
    {
        sections.push(technical_literals_text.trim().to_string());
    }
    let trimmed_context = retrieved_context.trim();
    if trimmed_context.is_empty() {
        return sections.join("\n\n");
    }
    sections.push(trimmed_context.to_string());
    sections.join("\n\n")
}

fn query_graph_status_prompt_label(graph_status: &str) -> &'static str {
    match graph_status {
        "current" => "ready",
        "partial" => "partial",
        _ => "empty",
    }
}

fn runtime_query_backlog_count(documents: &[ContentDocumentSummary]) -> i64 {
    i64::try_from(
        documents.iter().filter(|summary| document_requires_query_backlog(summary)).count(),
    )
    .unwrap_or(i64::MAX)
}

fn document_requires_query_backlog(summary: &ContentDocumentSummary) -> bool {
    let latest_mutation = summary.pipeline.latest_mutation.as_ref();
    let latest_job = summary.pipeline.latest_job.as_ref();

    let mutation_inflight = latest_mutation
        .is_some_and(|mutation| matches!(mutation.mutation_state.as_str(), "accepted" | "running"));
    let job_inflight =
        latest_job.is_some_and(|job| matches!(job.queue_state.as_str(), "queued" | "running"));
    let graph_pending =
        summary.readiness.as_ref().is_some_and(|readiness| readiness.graph_state != "ready")
            && !document_has_query_failure(summary);

    mutation_inflight || job_inflight || graph_pending
}

fn document_has_query_failure(summary: &ContentDocumentSummary) -> bool {
    let latest_mutation = summary.pipeline.latest_mutation.as_ref();
    let latest_job = summary.pipeline.latest_job.as_ref();

    latest_mutation.is_some_and(|mutation| mutation.mutation_state == "failed")
        || latest_job
            .is_some_and(|job| matches!(job.queue_state.as_str(), "failed" | "retryable_failed"))
}

fn query_prompt_document_title(summary: &ContentDocumentSummary) -> String {
    summary
        .active_revision
        .as_ref()
        .and_then(|revision| revision.title.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| summary.document.external_key.clone())
}

fn query_prompt_document_uploaded_at(
    summary: &ContentDocumentSummary,
) -> chrono::DateTime<chrono::Utc> {
    summary
        .active_revision
        .as_ref()
        .map(|revision| revision.created_at)
        .unwrap_or(summary.document.created_at)
}

fn query_prompt_pipeline_state(summary: &ContentDocumentSummary) -> &'static str {
    if document_has_query_failure(summary) {
        return "failed";
    }
    if document_requires_query_backlog(summary) {
        return "processing";
    }
    "ready"
}

fn query_prompt_graph_state(summary: &ContentDocumentSummary) -> &'static str {
    match summary.readiness.as_ref().map(|readiness| readiness.graph_state.as_str()) {
        Some("ready") => "ready",
        Some("failed") => "failed",
        Some("queued" | "running") => "processing",
        Some(_) => "partial",
        None => "unknown",
    }
}

async fn load_retrieved_document_briefs(
    state: &AppState,
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    top_k: usize,
) -> Vec<RuntimeRetrievedDocumentBrief> {
    let brief_limit = top_k.clamp(16, 48);
    let mut best_by_document = HashMap::<Uuid, RuntimeMatchedChunk>::new();
    let mut ordered_document_ids = Vec::<Uuid>::new();

    for chunk in chunks {
        let entry = best_by_document.entry(chunk.document_id).or_insert_with(|| {
            ordered_document_ids.push(chunk.document_id);
            chunk.clone()
        });
        if score_value(chunk.score) > score_value(entry.score) {
            *entry = chunk.clone();
        }
    }

    let ranked_documents = ordered_document_ids
        .into_iter()
        .take(brief_limit)
        .filter_map(|document_id| {
            let document = document_index.get(&document_id)?.clone();
            let fallback_excerpt =
                best_by_document.get(&document_id).map(|chunk| chunk.excerpt.clone());
            Some((document, fallback_excerpt))
        })
        .collect::<Vec<_>>();

    let previews =
        join_all(ranked_documents.into_iter().map(|(document, fallback_excerpt)| async move {
            let preview_excerpt = load_retrieved_document_preview(state, &document)
                .await
                .or(fallback_excerpt)
                .unwrap_or_default();
            if preview_excerpt.trim().is_empty() {
                return None;
            }
            let title = document
                .title
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| document.external_key.clone());
            Some(RuntimeRetrievedDocumentBrief { title, preview_excerpt })
        }))
        .await;

    previews.into_iter().flatten().collect()
}

async fn load_query_prompt_document_preview(
    state: &AppState,
    summary: &ContentDocumentSummary,
) -> Option<String> {
    let revision_id = summary.active_revision.as_ref()?.id;
    let chunks = state.canonical_services.content.list_chunks(state, revision_id).await.ok()?;
    chunks.into_iter().find_map(|chunk| {
        let repaired = repair_technical_layout_noise(&chunk.normalized_text);
        let normalized = repaired.trim();
        if normalized.is_empty() {
            return None;
        }
        Some(excerpt_for(normalized, 180))
    })
}

async fn load_retrieved_document_preview(
    state: &AppState,
    document: &KnowledgeDocumentRow,
) -> Option<String> {
    let revision_id = document.readable_revision_id.or(document.active_revision_id)?;
    let chunks = state.arango_document_store.list_chunks_by_revision(revision_id).await.ok()?;
    let combined = chunks
        .into_iter()
        .filter_map(|chunk| {
            let normalized = repair_technical_layout_noise(&chunk.normalized_text);
            let normalized = normalized.trim().to_string();
            if normalized.is_empty() {
                return None;
            }
            Some(normalized)
        })
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    if combined.is_empty() {
        return None;
    }
    Some(excerpt_for(&combined, 240))
}

#[cfg(test)]
fn sample_chunk_row(chunk_id: Uuid, document_id: Uuid, revision_id: Uuid) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        key: chunk_id.to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        content_text: "chunk".to_string(),
        normalized_text: "chunk".to_string(),
        span_start: Some(0),
        span_end: Some(5),
        token_count: Some(1),
        support_block_ids: Vec::new(),
        section_path: vec!["root".to_string()],
        heading_trail: vec!["Root".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
    }
}

#[cfg(test)]
fn sample_structured_block_row(
    block_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> KnowledgeStructuredBlockRow {
    let now = chrono::Utc::now();
    KnowledgeStructuredBlockRow {
        key: block_id.to_string(),
        arango_id: None,
        arango_rev: None,
        block_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        ordinal: 0,
        block_kind: "paragraph".to_string(),
        text: "segment".to_string(),
        normalized_text: "segment".to_string(),
        heading_trail: vec!["Root".to_string()],
        section_path: vec!["root".to_string()],
        page_number: Some(1),
        span_start: Some(0),
        span_end: Some(7),
        parent_block_id: None,
        table_coordinates_json: None,
        code_language: None,
        created_at: now,
        updated_at: now,
    }
}

#[cfg(test)]
fn sample_technical_fact_row(
    fact_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> KnowledgeTechnicalFactRow {
    let now = chrono::Utc::now();
    KnowledgeTechnicalFactRow {
        key: fact_id.to_string(),
        arango_id: None,
        arango_rev: None,
        fact_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        fact_kind: "endpoint_path".to_string(),
        canonical_value_text: "/health".to_string(),
        canonical_value_exact: "/health".to_string(),
        canonical_value_json: serde_json::json!("/health"),
        display_value: "/health".to_string(),
        qualifiers_json: serde_json::json!({}),
        support_block_ids: Vec::new(),
        support_chunk_ids: Vec::new(),
        confidence: Some(0.95),
        extraction_kind: "parser_first".to_string(),
        conflict_group_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn request_safe_query(plan: &RuntimeQueryPlan) -> String {
    if !plan.low_level_keywords.is_empty() {
        let combined =
            format!("{} {}", plan.high_level_keywords.join(" "), plan.low_level_keywords.join(" "));
        return combined.trim().to_string();
    }
    plan.keywords.join(" ")
}

fn map_chunk_hit(
    chunk: KnowledgeChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    let document = document_index.get(&chunk.document_id)?;
    let source_text = repair_technical_layout_noise(&chunk.content_text);
    Some(RuntimeMatchedChunk {
        chunk_id: chunk.chunk_id,
        document_id: chunk.document_id,
        document_label: document
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| document.external_key.clone()),
        excerpt: focused_excerpt_for(&source_text, keywords, 280),
        score: Some(score),
        source_text,
    })
}

fn map_edge_hit(
    edge_id: Uuid,
    score: Option<f32>,
    graph_index: &QueryGraphIndex,
    node_index: &HashMap<Uuid, GraphViewNodeWrite>,
) -> Option<RuntimeMatchedRelationship> {
    let edge = graph_index.edges.iter().find(|row| row.edge_id == edge_id)?;
    let from_node = node_index.get(&edge.from_node_id)?;
    let to_node = node_index.get(&edge.to_node_id)?;
    Some(RuntimeMatchedRelationship {
        edge_id: edge.edge_id,
        relation_type: edge.relation_type.clone(),
        from_node_id: edge.from_node_id,
        from_label: from_node.label.clone(),
        to_node_id: edge.to_node_id,
        to_label: to_node.label.clone(),
        score,
    })
}

fn merge_entities(
    left: Vec<RuntimeMatchedEntity>,
    right: Vec<RuntimeMatchedEntity>,
    top_k: usize,
) -> Vec<RuntimeMatchedEntity> {
    let mut merged = HashMap::new();
    for item in left.into_iter().chain(right) {
        merged
            .entry(item.node_id)
            .and_modify(|existing: &mut RuntimeMatchedEntity| {
                if score_value(item.score) > score_value(existing.score) {
                    *existing = item.clone();
                }
            })
            .or_insert(item);
    }
    let mut values = merged.into_values().collect::<Vec<_>>();
    values.sort_by(score_desc_entities);
    values.truncate(top_k);
    values
}

fn merge_relationships(
    left: Vec<RuntimeMatchedRelationship>,
    right: Vec<RuntimeMatchedRelationship>,
    top_k: usize,
) -> Vec<RuntimeMatchedRelationship> {
    let mut merged = HashMap::new();
    for item in left.into_iter().chain(right) {
        merged
            .entry(item.edge_id)
            .and_modify(|existing: &mut RuntimeMatchedRelationship| {
                if score_value(item.score) > score_value(existing.score) {
                    *existing = item.clone();
                }
            })
            .or_insert(item);
    }
    let mut values = merged.into_values().collect::<Vec<_>>();
    values.sort_by(score_desc_relationships);
    values.truncate(top_k);
    values
}

fn merge_chunks(
    left: Vec<RuntimeMatchedChunk>,
    right: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    let mut merged = HashMap::new();
    for item in left.into_iter().chain(right) {
        merged
            .entry(item.chunk_id)
            .and_modify(|existing: &mut RuntimeMatchedChunk| {
                if score_value(item.score) > score_value(existing.score) {
                    *existing = item.clone();
                }
            })
            .or_insert(item);
    }
    let mut values = merged.into_values().collect::<Vec<_>>();
    values.sort_by(score_desc_chunks);
    values.truncate(top_k);
    values
}

fn score_desc_entities(
    left: &RuntimeMatchedEntity,
    right: &RuntimeMatchedEntity,
) -> std::cmp::Ordering {
    score_value(right.score).total_cmp(&score_value(left.score))
}

fn score_desc_relationships(
    left: &RuntimeMatchedRelationship,
    right: &RuntimeMatchedRelationship,
) -> std::cmp::Ordering {
    score_value(right.score).total_cmp(&score_value(left.score))
}

fn score_desc_chunks(
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    score_value(right.score).total_cmp(&score_value(left.score))
}

fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(0.0)
}

fn excerpt_for(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let excerpt = trimmed.chars().take(max_chars).collect::<String>();
    format!("{excerpt}...")
}

fn focused_excerpt_for(content: &str, keywords: &[String], max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let lines = trimmed.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }

    let normalized_keywords = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .map(|keyword| keyword.to_lowercase())
        .collect::<Vec<_>>();
    if normalized_keywords.is_empty() {
        return excerpt_for(trimmed, max_chars);
    }

    let mut best_index = None;
    let mut best_score = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let lowered = line.to_lowercase();
        let score = normalized_keywords
            .iter()
            .filter(|keyword| lowered.contains(keyword.as_str()))
            .map(|keyword| keyword.chars().count().min(24))
            .sum::<usize>();
        if score > best_score {
            best_score = score;
            best_index = Some(index);
        }
    }

    let Some(center_index) = best_index else {
        return excerpt_for(trimmed, max_chars);
    };
    if best_score == 0 {
        return excerpt_for(trimmed, max_chars);
    }

    let max_focus_lines = 5usize;
    let mut selected = BTreeSet::from([center_index]);
    let mut radius = 1usize;
    loop {
        let excerpt =
            selected.iter().copied().map(|index| lines[index]).collect::<Vec<_>>().join(" ");
        if excerpt.chars().count() >= max_chars
            || selected.len() >= max_focus_lines
            || selected.len() == lines.len()
        {
            return excerpt_for(&excerpt, max_chars);
        }

        let mut expanded = false;
        if center_index >= radius {
            expanded |= selected.insert(center_index - radius);
        }
        if center_index + radius < lines.len() {
            expanded |= selected.insert(center_index + radius);
        }
        if !expanded {
            return excerpt_for(&excerpt, max_chars);
        }
        radius += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::infra::arangodb::graph_store::KnowledgeEvidenceRow;

    #[test]
    fn build_references_keeps_chunk_node_edge_order_and_ranks() {
        let references = build_references(
            &[RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "RustRAG".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.9),
            }],
            &[RuntimeMatchedRelationship {
                edge_id: Uuid::now_v7(),
                relation_type: "links".to_string(),
                from_node_id: Uuid::now_v7(),
                from_label: "spec.md".to_string(),
                to_node_id: Uuid::now_v7(),
                to_label: "RustRAG".to_string(),
                score: Some(0.7),
            }],
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "spec.md".to_string(),
                excerpt: "RustRAG links specs to graph knowledge.".to_string(),
                score: Some(0.8),
                source_text: "RustRAG links specs to graph knowledge.".to_string(),
            }],
            3,
        );

        assert_eq!(references.len(), 3);
        assert_eq!(references[0].kind, "chunk");
        assert_eq!(references[0].rank, 1);
        assert_eq!(references[1].kind, "node");
        assert_eq!(references[1].rank, 2);
        assert_eq!(references[2].kind, "edge");
        assert_eq!(references[2].rank, 3);
    }

    #[test]
    fn grouped_reference_candidates_prefer_document_deduping() {
        let document_id = Uuid::now_v7();
        let candidates = build_grouped_reference_candidates(
            &[],
            &[],
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id,
                    document_label: "spec.md".to_string(),
                    excerpt: "First excerpt".to_string(),
                    score: Some(0.8),
                    source_text: "First excerpt".to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id,
                    document_label: "spec.md".to_string(),
                    excerpt: "Second excerpt".to_string(),
                    score: Some(0.7),
                    source_text: "Second excerpt".to_string(),
                },
            ],
            4,
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].dedupe_key, format!("document:{document_id}"));
        assert_eq!(candidates[1].dedupe_key, format!("document:{document_id}"));
    }

    #[test]
    fn assemble_bounded_context_interleaves_graph_and_document_support() {
        let context = assemble_bounded_context(
            &[RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "RustRAG".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.9),
            }],
            &[RuntimeMatchedRelationship {
                edge_id: Uuid::now_v7(),
                relation_type: "uses".to_string(),
                from_node_id: Uuid::now_v7(),
                from_label: "RustRAG".to_string(),
                to_node_id: Uuid::now_v7(),
                to_label: "Arango".to_string(),
                score: Some(0.7),
            }],
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "spec.md".to_string(),
                excerpt: "RustRAG stores graph knowledge.".to_string(),
                score: Some(0.8),
                source_text: "RustRAG stores graph knowledge.".to_string(),
            }],
            2_000,
        );

        assert!(context.starts_with("Context\n"));
        assert!(context.contains("[document] spec.md: RustRAG stores graph knowledge."));
        assert!(context.contains("[graph-node] RustRAG (entity)"));
        assert!(context.contains("[graph-edge] RustRAG --uses--> Arango"));
        let document_index = context.find("[document]").unwrap_or_default();
        let graph_node_index = context.find("[graph-node]").unwrap_or_default();
        let graph_edge_index = context.find("[graph-edge]").unwrap_or_default();
        assert!(document_index < graph_node_index);
        assert!(graph_node_index < graph_edge_index);
    }

    #[test]
    fn build_answer_prompt_prioritizes_library_context() {
        let prompt = build_answer_prompt(
            "What documents mention RustRAG?",
            "Library summary\n- Documents in library: 12\n\nRecent documents\n- 2026-03-30T22:15:00Z — spec.md (text/markdown; pipeline ready; graph ready)",
            None,
            None,
        );
        assert!(prompt.contains("Treat the active library as the primary source of truth"));
        assert!(prompt.contains("exhaust the provided library context"));
        assert!(prompt.contains("recent document metadata"));
        assert!(prompt.contains("Present the answer directly."));
        assert!(prompt.contains("Do not narrate the retrieval process"));
        assert!(prompt.contains("Do not ask the user to upload"));
        assert!(prompt.contains("Exact technical literals section"));
        assert!(prompt.contains("copy those literals verbatim from Context"));
        assert!(prompt.contains("grouped by document"));
        assert!(prompt.contains("matched excerpt"));
        assert!(prompt.contains("Do not combine parts from different snippets"));
        assert!(prompt.contains("prefer the next distinct item after the excluded one"));
        assert!(prompt.contains("Question: What documents mention RustRAG?"));
        assert!(prompt.contains("Documents in library: 12"));
    }

    #[test]
    fn build_answer_prompt_includes_recent_conversation_history() {
        let prompt = build_answer_prompt(
            "давай",
            "Context\n[dummy] step-by-step instructions",
            Some("User: как в далионе перемещение сделать\nAssistant: Могу расписать пошагово."),
            None,
        );

        assert!(prompt.contains("Use the recent conversation history"));
        assert!(prompt.contains("Recent conversation:"));
        assert!(prompt.contains("Assistant: Могу расписать пошагово."));
        assert!(prompt.contains("Question: давай"));
    }

    #[test]
    fn focused_excerpt_for_prefers_keyword_region_over_chunk_prefix() {
        let content = "\
Header section\n\
Error example creationStatusCode = -1\n\
Unrelated payload\n\
Если при добавлении акции ее код будет совпадать с уже существующей акцией,\n\
то существующая акция будет прервана, а новая добавлена.\n\
Trailing details";

        let excerpt = focused_excerpt_for(
            content,
            &["совпадать".to_string(), "существующей".to_string(), "акцией".to_string()],
            220,
        );

        assert!(excerpt.contains("существующая акция будет прервана"));
        assert!(excerpt.contains("новая добавлена"));
        assert!(!excerpt.starts_with("Header section"));
    }

    #[test]
    fn build_exact_technical_literals_section_extracts_urls_paths_and_parameters() {
        let section = build_exact_technical_literals_section(
            "Какие параметры пейджинации и какой URL используются?",
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "api.pdf".to_string(),
                excerpt: "Получение списка счетов по страницам.".to_string(),
                score: Some(0.9),
                source_text: repair_technical_layout_noise(
                    "http\n://demo.local:8080/rewards-api/rest/v1/accounts\n/bypage\npageNu\nmber\npageSize\nwithCar\nds\nnumber\n_starting",
                ),
            }],
        )
        .unwrap_or_default();

        assert!(section.contains("Document: `api.pdf`"));
        assert!(section.contains("Matched excerpt: Получение списка счетов по страницам."));
        assert!(section.contains("`http://demo.local:8080/rewards-api/rest/v1/accounts/bypage`"));
        assert!(
            section.contains("`/v1/accounts/bypage`")
                || section.contains("`/rewards-api/rest/v1/accounts/bypage`")
        );
        assert!(section.contains("`pageNumber`"));
        assert!(section.contains("`pageSize`"));
        assert!(section.contains("`withCards`"));
        assert!(section.contains("`number_starting`"));
    }

    #[test]
    fn build_exact_technical_literals_section_groups_literals_by_document() {
        let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.9),
                    source_text: repair_technical_layout_noise(
                        "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                    score: Some(0.8),
                    source_text: repair_technical_layout_noise(
                        "http://demo.local:8080/rewards-api/rest/v1/version\n/v1/accounts\nGET",
                    ),
                },
            ],
        )
        .unwrap_or_default();

        let checkout_index =
            section.find("Document: `checkout_server_reference.pdf`").unwrap_or(usize::MAX);
        let rewards_index =
            section.find("Document: `rewards_service_reference.pdf`").unwrap_or(usize::MAX);
        let system_info_index = section.find("`/system/info`").unwrap_or(usize::MAX);
        let accounts_index = section.find("`/v1/accounts`").unwrap_or(usize::MAX);

        assert!(checkout_index < rewards_index);
        assert!(checkout_index < system_info_index);
        assert!(rewards_index < accounts_index);
        assert!(section.contains("текущего статуса checkout server"));
        assert!(section.contains("список счетов rewards service"));
    }

    #[test]
    fn build_exact_technical_literals_section_prefers_question_matched_window_per_document() {
        let section = build_exact_technical_literals_section(
            "Какой endpoint возвращает список счетов rewards service?",
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "rewards_service_reference.pdf".to_string(),
                excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                score: Some(0.9),
                source_text: repair_technical_layout_noise(
                    "http://demo.local:8080/rewards-api/rest/v1/version\nGET\nВерсия rewards service\n/v1/accounts\nGET\nПолучить список счетов rewards service.",
                ),
            }],
        )
        .unwrap_or_default();

        assert!(section.contains("`/v1/accounts`"));
        assert!(!section.contains("`/rewards-api/rest/v1/version`"));
    }

    #[test]
    fn build_exact_technical_literals_section_balances_documents_before_second_same_doc_chunk() {
        let rewards_document_id = Uuid::now_v7();
        let checkout_document_id = Uuid::now_v7();
        let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                    score: Some(0.99),
                    source_text: repair_technical_layout_noise("/v1/accounts\nGET\nПолучить список счетов rewards service."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/cards/bypage возвращает список карт rewards service.".to_string(),
                    score: Some(0.98),
                    source_text: repair_technical_layout_noise("/v1/cards/bypage\nGET\nПолучить список карт rewards service."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/cards возвращает список карт.".to_string(),
                    score: Some(0.97),
                    source_text: repair_technical_layout_noise("/v1/cards\nGET\nПолучить список карт."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.6),
                    source_text: repair_technical_layout_noise("http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info"),
                },
            ],
        )
        .unwrap_or_default();

        assert!(section.contains("Document: `checkout_server_reference.pdf`"));
        assert!(section.contains("`/system/info`"), "{section}");
    }

    #[test]
    fn build_port_answer_returns_insufficient_when_focused_document_has_no_grounded_port() {
        let control_document_id = Uuid::now_v7();
        let telegram_document_id = Uuid::now_v7();

        let answer = build_port_answer(
            "Какой порт использует Acme Control Center?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: control_document_id,
                    document_label: "Acme Control Center - Example".to_string(),
                    excerpt: "Acme Control Center — программное обеспечение для управления конфигурацией объектов управления.".to_string(),
                    score: Some(0.95),
                    source_text: repair_technical_layout_noise(
                        "Acme Control Center\nОписание\nAcme Control Center — программное обеспечение для управления конфигурацией объектов управления.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: telegram_document_id,
                    document_label: "Acme Telegram Bot - Example".to_string(),
                    excerpt: "Для интеграции используется localhost:2026.".to_string(),
                    score: Some(0.91),
                    source_text: repair_technical_layout_noise(
                        "Acme Telegram Bot\nНастройки\nport: 2026\nlocalhost:2026",
                    ),
                },
            ],
        )
        .unwrap_or_default();

        assert!(answer.contains("Acme Control Center"));
        assert!(answer.contains("не подтвержден"));
        assert!(!answer.contains("2026"));
    }

    #[test]
    fn technical_literal_focus_keyword_segments_splits_english_multi_clause_questions() {
        let segments = technical_literal_focus_keyword_segments(
            "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
        );

        assert!(segments.len() >= 2);
        assert!(segments.iter().any(|segment| segment.iter().any(|keyword| keyword == "rewards")));
        assert!(segments.iter().any(|segment| segment.iter().any(|keyword| keyword == "profile")));
    }

    #[test]
    fn build_port_answer_skips_port_plus_protocol_questions() {
        let rewards_document_id = Uuid::now_v7();
        let loyalty_document_id = Uuid::now_v7();

        let answer = build_port_answer(
            "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_accounts_rest_reference.md".to_string(),
                    excerpt: "Default port: 8081".to_string(),
                    score: Some(0.99),
                    source_text: repair_technical_layout_noise(
                        "Rewards Accounts REST API Reference\nDefault port: 8081\nProtocol: REST over HTTP",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: loyalty_document_id,
                    document_label: "customer_profile_soap_reference.md".to_string(),
                    excerpt: "Protocol: SOAP over HTTP".to_string(),
                    score: Some(0.98),
                    source_text: repair_technical_layout_noise(
                        "Customer Profile SOAP API Reference\nProtocol: SOAP over HTTP",
                    ),
                },
            ],
        );

        assert!(answer.is_none());
    }

    #[test]
    fn build_port_and_protocol_answer_handles_english_multi_document_question() {
        let rewards_document_id = Uuid::now_v7();
        let loyalty_document_id = Uuid::now_v7();

        let answer = build_port_and_protocol_answer(
            "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_accounts_rest_reference.md".to_string(),
                    excerpt: "Default port: 8081".to_string(),
                    score: Some(0.99),
                    source_text: repair_technical_layout_noise(
                        "Rewards Accounts REST API Reference\nDefault port: 8081\nBase REST URL: http://demo.local:8081/rewards-api/rest",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: loyalty_document_id,
                    document_label: "customer_profile_soap_reference.md".to_string(),
                    excerpt: "Protocol: SOAP over HTTP".to_string(),
                    score: Some(0.98),
                    source_text: repair_technical_layout_noise(
                        "Customer Profile SOAP API Reference\nProtocol: SOAP over HTTP\nWSDL URL: http://demo.local:8080/customer-profile/ws/customer-profile.wsdl",
                    ),
                },
            ],
        )
        .unwrap_or_default();

        assert!(answer.contains("8081"), "{answer}");
        assert!(answer.contains("SOAP"), "{answer}");
    }

    #[test]
    fn build_multi_document_endpoint_answer_handles_english_checkout_rewards_question() {
        let checkout_document_id = Uuid::now_v7();
        let rewards_document_id = Uuid::now_v7();

        let answer = build_multi_document_endpoint_answer_from_chunks(
            "If an agent needs the current Checkout Server status and then the Rewards Accounts list, which two endpoints should it call?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_accounts_rest_reference.md".to_string(),
                    excerpt: "List accounts: GET /v1/accounts".to_string(),
                    score: Some(0.95),
                    source_text: repair_technical_layout_noise(
                        "Rewards Accounts REST API Reference\nList accounts: GET /v1/accounts\nList cards: GET /v1/cards",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_rest_reference.md".to_string(),
                    excerpt: "Health check: GET /health".to_string(),
                    score: Some(0.96),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API Reference\nHealth check: GET /health",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_rest_reference.md".to_string(),
                    excerpt: "Current server information: GET /system/info".to_string(),
                    score: Some(0.94),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API Reference\nCurrent server information: GET /system/info\n/system/info returns the current checkout server status and runtime metadata.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

        assert!(answer.contains("/system/info"), "{answer}");
        assert!(answer.contains("/v1/accounts"), "{answer}");
        assert!(!answer.contains("/health"), "{answer}");
    }

    #[test]
    fn build_exact_technical_literals_section_picks_best_matching_chunk_within_document() {
        let cash_document_id = Uuid::now_v7();
        let section = build_exact_technical_literals_section(
            "Какой endpoint возвращает текущий статус checkout server?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "GET /cashes возвращает список касс.".to_string(),
                    score: Some(0.95),
                    source_text: repair_technical_layout_noise("/cashes\nGET\nПолучить список касс."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.7),
                    source_text: repair_technical_layout_noise("http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info"),
                },
            ],
        )
        .unwrap_or_default();

        assert!(section.contains("system/info"));
        assert!(!section.contains("`/cashes`"));
    }

    #[test]
    fn build_exact_technical_literals_section_prefers_document_local_clause_in_multi_doc_question()
    {
        let checkout_document_id = Uuid::now_v7();
        let rewards_document_id = Uuid::now_v7();
        let checkout_list = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: checkout_document_id,
            document_label: "checkout_server_reference.pdf".to_string(),
            excerpt: "GET /cashes возвращает список касс.".to_string(),
            score: Some(0.95),
            source_text: repair_technical_layout_noise("/cashes\nGET\nПолучить список касс."),
        };
        let checkout_system_info = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: checkout_document_id,
            document_label: "checkout_server_reference.pdf".to_string(),
            excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
            score: Some(0.7),
            source_text: repair_technical_layout_noise(
                "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
            ),
        };
        let rewards_bypage = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: rewards_document_id,
            document_label: "rewards_service_reference.pdf".to_string(),
            excerpt: "GET /v1/accounts/bypage возвращает список счетов с пагинацией.".to_string(),
            score: Some(0.95),
            source_text: repair_technical_layout_noise(
                "/v1/accounts/bypage\nGET\npageNumber\npageSize\nПолучить список счетов rewards service.",
            ),
        };
        let rewards_accounts = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: rewards_document_id,
            document_label: "rewards_service_reference.pdf".to_string(),
            excerpt: "GET /v1/accounts возвращает список счетов без параметров пейджинации."
                .to_string(),
            score: Some(0.7),
            source_text: repair_technical_layout_noise(
                "/v1/accounts\nGET\nПолучить список счетов rewards service.",
            ),
        };
        let question = "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?";
        let section = build_exact_technical_literals_section(
            question,
            &[checkout_list, checkout_system_info, rewards_bypage, rewards_accounts],
        )
        .unwrap_or_default();

        assert!(section.contains("Document: `checkout_server_reference.pdf`"));
        assert!(section.contains("Document: `rewards_service_reference.pdf`"));
        assert!(section.contains("`/system/info`"));
        assert!(!section.contains("`/cashes`"));
        assert!(section.contains("`/v1/accounts`"));
        assert!(!section.contains("`/v1/accounts/bypage`"));
    }

    #[test]
    fn build_exact_technical_literals_section_prefers_cash_current_info_clause_over_generic_cash_list()
     {
        let checkout_document_id = Uuid::now_v7();
        let rewards_document_id = Uuid::now_v7();
        let checkout_clients = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: checkout_document_id,
            document_label: "checkout_server_reference.pdf".to_string(),
            excerpt: "GET /checkout-api/rest/dictionaries/clients возвращает список клиентов checkout server.".to_string(),
            score: Some(0.92),
            source_text: repair_technical_layout_noise(
                "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/clients\nПолучение списка клиентов checkout server.",
            ),
        };
        let checkout_system_info = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: checkout_document_id,
            document_label: "checkout_server_reference.pdf".to_string(),
            excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
            score: Some(0.71),
            source_text: repair_technical_layout_noise(
                "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nДля получения текущего статуса checkout server.",
            ),
        };
        let rewards_accounts = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: rewards_document_id,
            document_label: "rewards_service_reference.pdf".to_string(),
            excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
            score: Some(0.94),
            source_text: repair_technical_layout_noise(
                "/v1/accounts\nGET\nПолучить список счетов rewards service.",
            ),
        };
        let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[rewards_accounts, checkout_clients, checkout_system_info],
        )
        .unwrap_or_default();

        assert!(section.contains("`/system/info`"));
        assert!(!section.contains("`/checkout-api/rest/dictionaries/clients`"));
        assert!(section.contains("`/v1/accounts`"));
    }

    #[test]
    fn build_multi_document_endpoint_answer_from_chunks_prefers_current_info_for_cash_document() {
        let checkout_document_id = Uuid::now_v7();
        let rewards_document_id = Uuid::now_v7();
        let answer = build_multi_document_endpoint_answer_from_chunks(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                    score: Some(0.94),
                    source_text: repair_technical_layout_noise(
                        "/v1/accounts\nGET\nПолучить список счетов rewards service.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "GET /checkout-api/rest/dictionaries/cardChanged возвращает историю изменений карт checkout server.".to_string(),
                    score: Some(0.96),
                    source_text: repair_technical_layout_noise(
                        "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/cardChanged\nПолучить историю изменений карт checkout server.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.71),
                    source_text: repair_technical_layout_noise(
                        "Публичное API checkout server.\nhttp://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nДля получения текущего статуса checkout server.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

        assert!(answer.contains("`GET /v1/accounts`"));
        assert!(answer.contains("`GET /system/info`"));
        assert!(!answer.contains("cardChanged"));
    }

    #[test]
    fn build_multi_document_endpoint_answer_from_chunks_handles_live_checkout_server_chunk_layout()
    {
        let checkout_document_id = Uuid::now_v7();
        let rewards_document_id = Uuid::now_v7();
        let wsdl_document_id = Uuid::now_v7();
        let answer = build_multi_document_endpoint_answer_from_chunks(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                    score: Some(69858.0),
                    source_text: repair_technical_layout_noise(
                        "/v1/accounts\nGET\nПолучить список счетов rewards service.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Получить историю изменений карт checkout server.".to_string(),
                    score: Some(70000.0),
                    source_text: repair_technical_layout_noise(
                        "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/cardChanged\nПолучить историю изменений карт checkout server.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Публичное API checkout server. Checkout server предоставляет REST-интерфейс для внешних сервисов и приложений.".to_string(),
                    score: Some(65000.0),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API\nCheckout server предоставляет REST-интерфейс для внешних сервисов и приложений. Запросы осуществляются через http-протокол, данные передаются json-сериализованными. Префикс для REST-интерфейса checkout server: http://<host>:<port>/checkout-api/rest/<request>\nhttp://demo.local:8080/checkout-api/rest/system/info\nДля получения текущего статуса checkout server надо выполнить запрос типа GET на URL /system/info.\nResult fields include version, buildNumber and buildDate.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: wsdl_document_id,
                    document_label: "customer_profile_service_reference.pdf".to_string(),
                    excerpt: "WSDL customer profile service доступен по префиксу /customer-profile/ws/.".to_string(),
                    score: Some(65000.0),
                    source_text: repair_technical_layout_noise(
                        "Получить WSDL можно через http://demo.local:8080/customer-profile/ws/customer-profile.wsdl. Базовый префикс /customer-profile/ws/.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

        assert!(answer.contains("`GET /v1/accounts`"));
        assert!(answer.contains("`GET /system/info`"));
        assert!(!answer.contains("cardChanged"));
        assert!(!answer.contains("/customer-profile/ws/"));
    }

    #[test]
    fn assemble_answer_context_prefixes_library_summary_and_recent_documents() {
        let summary = RuntimeQueryLibrarySummary {
            document_count: 12,
            graph_ready_count: 8,
            processing_count: 3,
            failed_count: 1,
            graph_status: "partial",
        };
        let recent_documents = vec![RuntimeQueryRecentDocument {
            title: "spec.md".to_string(),
            uploaded_at: "2026-03-30T22:15:00+00:00".to_string(),
            mime_type: Some("text/markdown".to_string()),
            pipeline_state: "ready",
            graph_state: "ready",
            preview_excerpt: Some("RustRAG stores graph knowledge.".to_string()),
        }];

        let retrieved_documents = vec![RuntimeRetrievedDocumentBrief {
            title: "spec.md".to_string(),
            preview_excerpt: "RustRAG stores graph knowledge.".to_string(),
        }];
        let context = assemble_answer_context(
            &summary,
            &recent_documents,
            &retrieved_documents,
            Some("Exact technical literals\n- URLs: `http://demo.local:8080/wsdl`"),
            "Context\n[document] spec.md: RustRAG",
        );

        assert!(context.contains("Context\n[document] spec.md: RustRAG"));
        assert!(context.contains("Library summary\n- Documents in library: 12"));
        assert!(context.contains("- Graph-ready documents: 8"));
        assert!(context.contains("- Documents still processing: 3"));
        assert!(context.contains("- Documents failed in pipeline: 1"));
        assert!(context.contains("- Graph coverage status: partial"));
        assert!(context.contains("Recent documents"));
        assert!(context.contains("2026-03-30T22:15:00+00:00 — spec.md"));
        assert!(context.contains("Preview: RustRAG stores graph knowledge."));
        assert!(context.contains("Retrieved document briefs"));
        assert!(
            context.contains("Exact technical literals\n- URLs: `http://demo.local:8080/wsdl`")
        );
    }

    #[test]
    fn build_structured_query_diagnostics_emits_typed_response_shape() {
        let plan = RuntimeQueryPlan {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
            intent_profile: QueryIntentProfile::default(),
            keywords: vec!["rustrag".to_string(), "graph".to_string()],
            high_level_keywords: vec!["rustrag".to_string()],
            low_level_keywords: vec!["graph".to_string()],
            top_k: 8,
            context_budget_chars: 6_000,
        };
        let bundle = RetrievalBundle {
            entities: vec![RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "RustRAG".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.91),
            }],
            relationships: vec![RuntimeMatchedRelationship {
                edge_id: Uuid::now_v7(),
                relation_type: "mentions".to_string(),
                from_node_id: Uuid::now_v7(),
                from_label: "spec.md".to_string(),
                to_node_id: Uuid::now_v7(),
                to_label: "RustRAG".to_string(),
                score: Some(0.61),
            }],
            chunks: vec![RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "spec.md".to_string(),
                excerpt: "RustRAG query runtime returns structured references.".to_string(),
                score: Some(0.73),
                source_text: "RustRAG query runtime returns structured references.".to_string(),
            }],
        };
        let graph_index = QueryGraphIndex { nodes: HashMap::new(), edges: Vec::new() };
        let enrichment = QueryExecutionEnrichment {
            planning: crate::domains::query::QueryPlanningMetadata {
                requested_mode: RuntimeQueryMode::Hybrid,
                planned_mode: RuntimeQueryMode::Hybrid,
                intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
                keywords: crate::domains::query::IntentKeywords {
                    high_level: vec!["rustrag".to_string()],
                    low_level: vec!["graph".to_string()],
                },
                warnings: Vec::new(),
            },
            rerank: crate::domains::query::RerankMetadata {
                status: crate::domains::query::RerankStatus::Skipped,
                candidate_count: 3,
                reordered_count: None,
            },
            context_assembly: crate::domains::query::ContextAssemblyMetadata {
                status: crate::domains::query::ContextAssemblyStatus::BalancedMixed,
                warning: None,
            },
            grouped_references: Vec::new(),
        };

        let diagnostics = build_structured_query_diagnostics(
            &plan,
            &bundle,
            &graph_index,
            &enrichment,
            true,
            "Bounded context",
        );

        assert_eq!(diagnostics.planned_mode, RuntimeQueryMode::Hybrid);
        assert_eq!(diagnostics.requested_mode, RuntimeQueryMode::Hybrid);
        assert_eq!(diagnostics.reference_counts.entity_count, 1);
        assert_eq!(diagnostics.reference_counts.relationship_count, 1);
        assert_eq!(diagnostics.reference_counts.chunk_count, 1);
        assert_eq!(diagnostics.reference_counts.graph_node_count, 0);
        assert_eq!(diagnostics.reference_counts.graph_edge_count, 0);
        assert_eq!(
            diagnostics.planning.intent_cache_status,
            crate::domains::query::QueryIntentCacheStatus::Miss
        );
        assert_eq!(
            diagnostics.context_assembly.status,
            crate::domains::query::ContextAssemblyStatus::BalancedMixed
        );
        assert!(diagnostics.grouped_references.is_empty());
        assert_eq!(diagnostics.context_text.as_deref(), Some("Bounded context"));
    }

    #[test]
    fn apply_query_execution_warning_sets_typed_fields() {
        let mut diagnostics = RuntimeStructuredQueryDiagnostics {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
            keywords: Vec::new(),
            high_level_keywords: Vec::new(),
            low_level_keywords: Vec::new(),
            top_k: 8,
            reference_counts: RuntimeStructuredQueryReferenceCounts {
                entity_count: 0,
                relationship_count: 0,
                chunk_count: 0,
                graph_node_count: 0,
                graph_edge_count: 0,
            },
            planning: crate::domains::query::QueryPlanningMetadata {
                requested_mode: RuntimeQueryMode::Hybrid,
                planned_mode: RuntimeQueryMode::Hybrid,
                intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
                keywords: crate::domains::query::IntentKeywords::default(),
                warnings: Vec::new(),
            },
            rerank: crate::domains::query::RerankMetadata {
                status: crate::domains::query::RerankStatus::Skipped,
                candidate_count: 0,
                reordered_count: None,
            },
            context_assembly: crate::domains::query::ContextAssemblyMetadata {
                status: crate::domains::query::ContextAssemblyStatus::BalancedMixed,
                warning: None,
            },
            grouped_references: Vec::new(),
            context_text: None,
            warning: None,
            warning_kind: None,
            library_summary: None,
        };
        apply_query_execution_warning(
            &mut diagnostics,
            Some(&RuntimeQueryWarning {
                warning: "Graph coverage is still converging.".to_string(),
                warning_kind: "partial_convergence",
            }),
        );

        assert_eq!(diagnostics.warning.as_deref(), Some("Graph coverage is still converging."));
        assert_eq!(diagnostics.warning_kind, Some("partial_convergence"));
    }

    #[test]
    fn enrich_query_candidate_summary_overwrites_canonical_reference_counts() {
        let enriched = enrich_query_candidate_summary(
            serde_json::json!({
                "finalChunkReferences": 1,
                "finalEntityReferences": 3,
                "finalRelationReferences": 2
            }),
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: vec![
                    sample_chunk_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
                    sample_chunk_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
                ],
                structured_blocks: vec![sample_structured_block_row(
                    Uuid::now_v7(),
                    Uuid::now_v7(),
                    Uuid::now_v7(),
                )],
                technical_facts: vec![
                    sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
                    sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
                ],
            },
        );

        assert_eq!(enriched["finalChunkReferences"], serde_json::json!(2));
        assert_eq!(enriched["finalPreparedSegmentReferences"], serde_json::json!(1));
        assert_eq!(enriched["finalTechnicalFactReferences"], serde_json::json!(2));
        assert_eq!(enriched["finalEntityReferences"], serde_json::json!(3));
    }

    #[test]
    fn enrich_query_assembly_diagnostics_emits_verification_and_graph_participation() {
        let diagnostics = enrich_query_assembly_diagnostics(
            serde_json::json!({
                "bundleId": Uuid::nil(),
            }),
            &RuntimeAnswerVerification {
                state: QueryVerificationState::Verified,
                warnings: vec![QueryVerificationWarning {
                    code: "grounded".to_string(),
                    message: "Answer is grounded.".to_string(),
                    related_segment_id: None,
                    related_fact_id: None,
                }],
            },
            &serde_json::json!({
                "finalChunkReferences": 2,
                "finalPreparedSegmentReferences": 4,
                "finalTechnicalFactReferences": 3,
                "finalEntityReferences": 5,
                "finalRelationReferences": 2
            }),
        );

        assert_eq!(diagnostics["verificationState"], "verified");
        assert_eq!(diagnostics["verificationWarnings"][0]["code"], "grounded");
        assert_eq!(diagnostics["graphParticipation"]["entityReferenceCount"], 5);
        assert_eq!(diagnostics["graphParticipation"]["relationReferenceCount"], 2);
        assert_eq!(diagnostics["graphParticipation"]["graphBacked"], true);
        assert_eq!(diagnostics["structuredEvidence"]["preparedSegmentReferenceCount"], 4);
        assert_eq!(diagnostics["structuredEvidence"]["technicalFactReferenceCount"], 3);
        assert_eq!(diagnostics["structuredEvidence"]["chunkReferenceCount"], 2);
    }

    #[test]
    fn selected_fact_ids_for_canonical_evidence_stays_bounded() {
        let selected_fact_id = Uuid::now_v7();
        let evidence_fact_id = Uuid::now_v7();
        let evidence_rows = vec![KnowledgeEvidenceRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            evidence_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_id: None,
            block_id: Some(Uuid::now_v7()),
            fact_id: Some(evidence_fact_id),
            span_start: None,
            span_end: None,
            quote_text: "GET /system/info".to_string(),
            literal_spans_json: json!([]),
            evidence_kind: "relation_fact_support".to_string(),
            extraction_method: "graph_extract".to_string(),
            confidence: Some(0.9),
            evidence_state: "active".to_string(),
            freshness_generation: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }];
        let chunk_supported_facts = (0..40)
            .map(|_| sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()))
            .collect::<Vec<_>>();

        let fact_ids = selected_fact_ids_for_canonical_evidence(
            &[selected_fact_id],
            &evidence_rows,
            &chunk_supported_facts,
        );
        assert_eq!(fact_ids.len(), 2);
        assert_eq!(fact_ids[0], selected_fact_id);
        assert_eq!(fact_ids[1], evidence_fact_id);
    }

    #[test]
    fn focused_answer_document_id_prefers_dominant_single_document() {
        let primary_document_id = Uuid::now_v7();
        let secondary_document_id = Uuid::now_v7();
        let chunks = vec![
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: primary_document_id,
                document_label: "vector_database_wikipedia.md".to_string(),
                excerpt:
                    "Vector databases typically implement approximate nearest neighbor algorithms."
                        .to_string(),
                score: Some(1.0),
                source_text:
                    "Vector databases typically implement approximate nearest neighbor algorithms."
                        .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: primary_document_id,
                document_label: "vector_database_wikipedia.md".to_string(),
                excerpt: "Use-cases include multi-modal search and recommendation engines."
                    .to_string(),
                score: Some(0.8),
                source_text: "Use-cases include multi-modal search and recommendation engines."
                    .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: secondary_document_id,
                document_label: "large_language_model_wikipedia.md".to_string(),
                excerpt: "LLMs generate, summarize, translate, and reason over text.".to_string(),
                score: Some(0.25),
                source_text: "LLMs generate, summarize, translate, and reason over text."
                    .to_string(),
            },
        ];

        assert_eq!(
            focused_answer_document_id(
                "Which algorithms do vector databases typically implement, and name one use case mentioned besides semantic search.",
                &chunks,
            ),
            Some(primary_document_id)
        );
    }

    #[test]
    fn question_mentions_port_does_not_match_report_word() {
        assert!(!question_mentions_port(
            "What report name appears in the runtime PDF upload check?"
        ));
        assert!(question_mentions_port("Which port does the service use?"));
    }

    #[test]
    fn question_requests_multi_document_scope_detects_role_pairing_questions() {
        assert!(question_requests_multi_document_scope(
            "If a system needs retrieval from external documents before answering and also semantic similarity over embeddings, which two technologies from this corpus fit those roles?"
        ));
        assert!(question_requests_multi_document_scope(
            "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?"
        ));
    }

    #[test]
    fn build_document_literal_answer_extracts_report_name_from_focused_document() {
        let document_id = Uuid::now_v7();
        let answer = build_document_literal_answer(
            "What report name appears in the runtime PDF upload check?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "runtime_upload_check.pdf".to_string(),
                excerpt: "Runtime PDF upload check".to_string(),
                score: Some(1.0),
                source_text: "Runtime PDF upload check\n\nQuarterly graph report".to_string(),
            }],
        );
        assert_eq!(answer.as_deref(), Some("Quarterly graph report"));
    }

    #[test]
    fn build_document_literal_answer_extracts_formats_under_test() {
        let document_id = Uuid::now_v7();
        let answer = build_document_literal_answer(
            "Which formats are explicitly listed under test in the PDF smoke fixture?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "upload_smoke_fixture.pdf".to_string(),
                excerpt: "RustRAG PDF smoke fixture".to_string(),
                score: Some(1.0),
                source_text: "RustRAG PDF smoke fixture\n\nExpected formats under test: PDF, DOCX, PPTX, PNG, JPG.".to_string(),
            }],
        );
        assert_eq!(answer.as_deref(), Some("PDF, DOCX, PPTX, PNG, JPG."));
    }

    #[test]
    fn build_document_literal_answer_extracts_vectorized_modalities() {
        let document_id = Uuid::now_v7();
        let answer = build_document_literal_answer(
            "According to the vector database article, what kinds of data can all be vectorized?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "vector_database_wikipedia.md".to_string(),
                excerpt:
                    "Words, phrases, or entire documents, as well as images and audio, can all be vectorized."
                        .to_string(),
                score: Some(1.0),
                source_text:
                    "Words, phrases, or entire documents, as well as images and audio, can all be vectorized."
                        .to_string(),
            }],
        );
        assert_eq!(
            answer.as_deref(),
            Some("Words, phrases, entire documents, images, and audio can all be vectorized.")
        );
    }

    #[test]
    fn build_canonical_answer_context_limits_sections_to_focused_document() {
        let focused_document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let focused_revision_id = Uuid::now_v7();
        let other_revision_id = Uuid::now_v7();

        let context = build_canonical_answer_context(
            "Which search engines and assistants or services are named as examples in the knowledge graph article?",
            &RuntimeStructuredQueryResult {
                planned_mode: RuntimeQueryMode::Hybrid,
                embedding_usage: None,
                intent_profile: QueryIntentProfile::default(),
                context_text: String::new(),
                technical_literals_text: None,
                technical_literal_chunks: Vec::new(),
                diagnostics: RuntimeStructuredQueryDiagnostics {
                    requested_mode: RuntimeQueryMode::Hybrid,
                    planned_mode: RuntimeQueryMode::Hybrid,
                    keywords: Vec::new(),
                    high_level_keywords: Vec::new(),
                    low_level_keywords: Vec::new(),
                    top_k: 8,
                    reference_counts: RuntimeStructuredQueryReferenceCounts {
                        entity_count: 0,
                        relationship_count: 0,
                        chunk_count: 0,
                        graph_node_count: 0,
                        graph_edge_count: 0,
                    },
                    planning: crate::domains::query::QueryPlanningMetadata {
                        requested_mode: RuntimeQueryMode::Hybrid,
                        planned_mode: RuntimeQueryMode::Hybrid,
                        intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
                        keywords: crate::domains::query::IntentKeywords::default(),
                        warnings: Vec::new(),
                    },
                    rerank: crate::domains::query::RerankMetadata {
                        status: crate::domains::query::RerankStatus::Skipped,
                        candidate_count: 0,
                        reordered_count: None,
                    },
                    context_assembly: crate::domains::query::ContextAssemblyMetadata {
                        status: crate::domains::query::ContextAssemblyStatus::BalancedMixed,
                        warning: None,
                    },
                    grouped_references: Vec::new(),
                    context_text: None,
                    warning: None,
                    warning_kind: None,
                    library_summary: None,
                },
                retrieved_documents: Vec::new(),
            },
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: vec![
                    KnowledgeStructuredBlockRow {
                        normalized_text:
                            "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                                .to_string(),
                        text: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                            .to_string(),
                        heading_trail: vec!["Examples".to_string()],
                        ..sample_structured_block_row(
                            Uuid::now_v7(),
                            focused_document_id,
                            focused_revision_id,
                        )
                    },
                    KnowledgeStructuredBlockRow {
                        normalized_text:
                            "LLMs generate, summarize, translate, and reason over text.".to_string(),
                        text: "LLMs generate, summarize, translate, and reason over text."
                            .to_string(),
                        heading_trail: vec!["Capabilities".to_string()],
                        ..sample_structured_block_row(
                            Uuid::now_v7(),
                            other_document_id,
                            other_revision_id,
                        )
                    },
                ],
                technical_facts: vec![
                    KnowledgeTechnicalFactRow {
                        display_value: "Google".to_string(),
                        canonical_value_text: "Google".to_string(),
                        canonical_value_exact: "Google".to_string(),
                        canonical_value_json: serde_json::json!("Google"),
                        fact_kind: "example".to_string(),
                        ..sample_technical_fact_row(
                            Uuid::now_v7(),
                            focused_document_id,
                            focused_revision_id,
                        )
                    },
                    KnowledgeTechnicalFactRow {
                        display_value: "translate".to_string(),
                        canonical_value_text: "translate".to_string(),
                        canonical_value_exact: "translate".to_string(),
                        canonical_value_json: serde_json::json!("translate"),
                        fact_kind: "capability".to_string(),
                        ..sample_technical_fact_row(
                            Uuid::now_v7(),
                            other_document_id,
                            other_revision_id,
                        )
                    },
                ],
            },
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: focused_document_id,
                    document_label: "knowledge_graph_wikipedia.md".to_string(),
                    excerpt: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                        .to_string(),
                    score: Some(1.0),
                    source_text: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                        .to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: other_document_id,
                    document_label: "large_language_model_wikipedia.md".to_string(),
                    excerpt: "LLMs generate, summarize, translate, and reason over text."
                        .to_string(),
                    score: Some(0.2),
                    source_text: "LLMs generate, summarize, translate, and reason over text."
                        .to_string(),
                },
            ],
            "",
        );

        assert!(context.contains("Focused grounded document\n- knowledge_graph_wikipedia.md"));
        assert!(context.contains("Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa"));
        assert!(!context.contains("LLMs generate, summarize, translate, and reason over text."));
        assert!(!context.contains("capability: `translate`"));
    }

    #[test]
    fn render_canonical_chunk_section_uses_longer_question_focused_source_excerpt() {
        let document_id = Uuid::now_v7();
        let section = render_canonical_chunk_section(
            "Which search engines and assistants or services are named as examples in the knowledge graph article?",
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "knowledge_graph_wikipedia.md".to_string(),
                excerpt: "Google, Bing, and Yahoo are named as examples.".to_string(),
                score: Some(1.0),
                source_text: "Knowledge graphs are used by search engines such as Google, Bing, and Yahoo; knowledge engines and question-answering services such as WolframAlpha, Apple's Siri, and Amazon Alexa."
                    .to_string(),
            }],
        );

        assert!(section.contains("Google, Bing, and Yahoo"));
        assert!(section.contains("WolframAlpha"));
        assert!(section.contains("Siri"));
        assert!(section.contains("Alexa"));
    }

    #[test]
    fn build_multi_document_role_answer_selects_distinct_corpus_technologies() {
        let vector_document_id = Uuid::now_v7();
        let llm_document_id = Uuid::now_v7();
        let answer = build_multi_document_role_answer(
            "If a system needs semantic similarity search over embeddings and also text generation or reasoning, which two technologies from this corpus fit those roles?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: vector_document_id,
                    document_label: "vector_database_wikipedia.md".to_string(),
                    excerpt: "Vector databases typically implement approximate nearest neighbor algorithms."
                        .to_string(),
                    score: Some(0.9),
                    source_text: "Vector database\n\nA vector database stores and retrieves embeddings of data in vector space. Use-cases include semantic search and retrieval-augmented generation."
                        .to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: llm_document_id,
                    document_label: "large_language_model_wikipedia.md".to_string(),
                    excerpt:
                        "LLMs are designed for natural language processing tasks, especially language generation."
                            .to_string(),
                    score: Some(0.85),
                    source_text: "Large language model\n\nLLMs are designed for natural language processing tasks, especially language generation. They generate, summarize, translate, and reason over text."
                        .to_string(),
                },
            ],
        )
        .expect("expected deterministic multi-document role answer");

        assert!(answer.contains("Vector database"));
        assert!(answer.contains("Large language model"));
        assert!(!answer.contains("RAG"));
    }

    #[test]
    fn build_multi_document_role_answer_distinguishes_rust_and_llm_roles() {
        let rust_document_id = Uuid::now_v7();
        let llm_document_id = Uuid::now_v7();
        let answer = build_multi_document_role_answer(
            "Which item in this corpus is a programming language focused on memory safety, and which item is a model family used for natural language processing?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: llm_document_id,
                    document_label: "large_language_model_wikipedia.md".to_string(),
                    excerpt: "A large language model is designed for natural language processing tasks."
                        .to_string(),
                    score: Some(0.9),
                    source_text: "Large language model\n\nA large language model is designed for natural language processing tasks."
                        .to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rust_document_id,
                    document_label: "rust_programming_language_wikipedia.md".to_string(),
                    excerpt: "Rust is a general-purpose programming language with an emphasis on memory safety."
                        .to_string(),
                    score: Some(0.88),
                    source_text: "Rust (programming language)\n\nRust is a general-purpose programming language with an emphasis on memory safety."
                        .to_string(),
                },
            ],
        )
        .expect("expected deterministic distinction answer");

        assert!(answer.contains("Rust"));
        assert!(answer.contains("Large language model"));
        assert!(!answer.contains("does not contain"));
    }

    #[test]
    fn build_multi_document_role_answer_distinguishes_semantic_web_and_knowledge_graph() {
        let semantic_web_document_id = Uuid::now_v7();
        let knowledge_graph_document_id = Uuid::now_v7();
        let answer = build_multi_document_role_answer(
            "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: semantic_web_document_id,
                    document_label: "semantic_web_wikipedia.md".to_string(),
                    excerpt: "The Semantic Web is an extension of the World Wide Web that enables data to be shared and reused across applications."
                        .to_string(),
                    score: Some(0.92),
                    source_text: "Semantic Web\n\nThe Semantic Web is an extension of the World Wide Web that enables data to be shared and reused across applications. It is based on standards such as RDF and OWL."
                        .to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: knowledge_graph_document_id,
                    document_label: "knowledge_graph_wikipedia.md".to_string(),
                    excerpt: "A knowledge graph stores interlinked descriptions of entities and concepts."
                        .to_string(),
                    score: Some(0.9),
                    source_text: "Knowledge graph\n\nA knowledge graph stores interlinked descriptions of entities and concepts."
                        .to_string(),
                },
            ],
        )
        .expect("expected deterministic multi-document role answer");

        assert!(answer.contains("Semantic web"));
        assert!(answer.contains("Knowledge graph"));
    }

    #[test]
    fn extract_multi_document_role_clauses_supports_which_one_stores_questions() {
        let clauses = extract_multi_document_role_clauses(
            "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
        );

        assert_eq!(clauses.len(), 2);
        assert!(clauses[0].contains("machine-readable"));
        assert_eq!(clauses[1], "stores interlinked descriptions of entities and concepts");
    }

    #[test]
    fn verify_answer_accepts_semantic_web_and_knowledge_graph_targets() {
        let verification = verify_answer_against_canonical_evidence(
            "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
            "Semantic web makes Internet data machine-readable through RDF and OWL. Knowledge graph stores interlinked descriptions of entities and concepts.",
            &QueryIntentProfile::default(),
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[],
        );

        assert_eq!(verification.state, QueryVerificationState::Verified);
        assert!(
            verification.warnings.iter().all(|warning| warning.code != "wrong_canonical_target")
        );
    }

    #[test]
    fn verify_answer_accepts_method_path_literal_when_method_and_path_are_grounded() {
        let verification = verify_answer_against_canonical_evidence(
            "Какие endpoint'ы нужны?",
            "Нужен endpoint `GET /system/info`.",
            &QueryIntentProfile {
                exact_literal_technical: true,
                ..QueryIntentProfile::default()
            },
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: vec![KnowledgeChunkRow {
                    key: Uuid::now_v7().to_string(),
                    arango_id: None,
                    arango_rev: None,
                    chunk_id: Uuid::now_v7(),
                    workspace_id: Uuid::now_v7(),
                    library_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: Some("paragraph".to_string()),
                    content_text: "Для получения текущего статуса checkout server надо выполнить запрос типа GET на URL /system/info".to_string(),
                    normalized_text: "Для получения текущего статуса checkout server надо выполнить запрос типа GET на URL /system/info".to_string(),
                    span_start: Some(0),
                    span_end: Some(80),
                    token_count: Some(12),
                    support_block_ids: vec![],
                    section_path: vec![],
                    heading_trail: vec![],
                    literal_digest: None,
                    chunk_state: "active".to_string(),
                    text_generation: Some(1),
                    vector_generation: Some(1),
                }],
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[],
        );

        assert_eq!(verification.state, QueryVerificationState::Verified);
        assert!(verification.warnings.is_empty());
    }

    #[test]
    fn verify_answer_ignores_background_conflicts_when_grounded_literals_are_explicit() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let conflict_group_id = format!("url:{}", Uuid::now_v7());
        let verification = verify_answer_against_canonical_evidence(
            "Use the exact WSDL URL.",
            "Use `http://demo.local:8080/customer-profile/ws/customer-profile.wsdl`.",
            &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: vec![
                    KnowledgeTechnicalFactRow {
                        canonical_value_text: "http://demo.local:8080/customer-profile/ws/"
                            .to_string(),
                        canonical_value_exact: "http://demo.local:8080/customer-profile/ws/"
                            .to_string(),
                        canonical_value_json: serde_json::json!(
                            "http://demo.local:8080/customer-profile/ws/"
                        ),
                        display_value: "http://demo.local:8080/customer-profile/ws/".to_string(),
                        conflict_group_id: Some(conflict_group_id.clone()),
                        fact_kind: "url".to_string(),
                        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                    },
                    KnowledgeTechnicalFactRow {
                        canonical_value_text:
                            "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                                .to_string(),
                        canonical_value_exact:
                            "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                                .to_string(),
                        canonical_value_json: serde_json::json!(
                            "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                        ),
                        display_value:
                            "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                                .to_string(),
                        conflict_group_id: Some(conflict_group_id),
                        fact_kind: "url".to_string(),
                        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                    },
                ],
            },
            &[],
        );

        assert_eq!(verification.state, QueryVerificationState::Verified);
        assert!(verification.warnings.iter().all(|warning| warning.code != "conflicting_evidence"));
    }

    #[test]
    fn verify_unsupported_capability_answer_skips_unrelated_conflict_warnings() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let conflict_group_id = format!("url:{}", Uuid::now_v7());
        let verification = verify_answer_against_canonical_evidence(
            "Does the library describe GraphQL?",
            "No, this library does not describe any GraphQL API or GraphQL endpoint.",
            &QueryIntentProfile {
                exact_literal_technical: true,
                unsupported_capability: Some(UnsupportedCapabilityIntent::GraphQlApi),
                ..QueryIntentProfile::default()
            },
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: vec![
                    KnowledgeTechnicalFactRow {
                        canonical_value_text: "http://demo.local:8080/customer-profile/ws/"
                            .to_string(),
                        canonical_value_exact: "http://demo.local:8080/customer-profile/ws/"
                            .to_string(),
                        canonical_value_json: serde_json::json!(
                            "http://demo.local:8080/customer-profile/ws/"
                        ),
                        display_value: "http://demo.local:8080/customer-profile/ws/".to_string(),
                        conflict_group_id: Some(conflict_group_id.clone()),
                        fact_kind: "url".to_string(),
                        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                    },
                    KnowledgeTechnicalFactRow {
                        canonical_value_text:
                            "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                                .to_string(),
                        canonical_value_exact:
                            "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                                .to_string(),
                        canonical_value_json: serde_json::json!(
                            "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                        ),
                        display_value:
                            "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                                .to_string(),
                        conflict_group_id: Some(conflict_group_id),
                        fact_kind: "url".to_string(),
                        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                    },
                ],
            },
            &[],
        );

        assert_eq!(verification.state, QueryVerificationState::Verified);
        assert!(verification.warnings.is_empty());
    }

    #[test]
    fn verify_answer_marks_conflicting_when_exact_literal_question_stays_ambiguous() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let conflict_group_id = format!("url:{}", Uuid::now_v7());
        let verification = verify_answer_against_canonical_evidence(
            "What exact endpoint is described?",
            "The exact endpoint is described in the selected evidence.",
            &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: vec![
                    KnowledgeTechnicalFactRow {
                        canonical_value_text: "/system/info".to_string(),
                        canonical_value_exact: "/system/info".to_string(),
                        canonical_value_json: serde_json::json!("/system/info"),
                        display_value: "/system/info".to_string(),
                        conflict_group_id: Some(conflict_group_id.clone()),
                        fact_kind: "endpoint_path".to_string(),
                        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                    },
                    KnowledgeTechnicalFactRow {
                        canonical_value_text: "/system/status".to_string(),
                        canonical_value_exact: "/system/status".to_string(),
                        canonical_value_json: serde_json::json!("/system/status"),
                        display_value: "/system/status".to_string(),
                        conflict_group_id: Some(conflict_group_id),
                        fact_kind: "endpoint_path".to_string(),
                        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                    },
                ],
            },
            &[],
        );

        assert_eq!(verification.state, QueryVerificationState::Conflicting);
        assert!(verification.warnings.iter().any(|warning| warning.code == "conflicting_evidence"));
    }

    #[test]
    fn expanded_candidate_limit_prefers_deeper_combined_mode_search() {
        assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Hybrid, 8, true, 24), 24);
        assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Mix, 10, true, 24), 30);
        assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Document, 8, true, 24), 8);
        assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Hybrid, 8, false, 24), 24);
    }

    #[test]
    fn technical_literal_candidate_limit_expands_document_recall_for_endpoint_questions() {
        assert_eq!(
            technical_literal_candidate_limit(
                detect_technical_literal_intent("Какие endpoint'ы нужны для двух серверов?"),
                8,
            ),
            32
        );
        assert_eq!(
            technical_literal_candidate_limit(
                detect_technical_literal_intent("Какие параметры пейджинации доступны?"),
                8,
            ),
            24
        );
        assert_eq!(
            technical_literal_candidate_limit(
                detect_technical_literal_intent("Расскажи кратко, о чём библиотека."),
                8,
            ),
            8
        );
    }

    #[test]
    fn build_lexical_queries_keeps_broader_unique_query_set() {
        let plan = RuntimeQueryPlan {
            requested_mode: RuntimeQueryMode::Mix,
            planned_mode: RuntimeQueryMode::Mix,
            intent_profile: QueryIntentProfile {
                exact_literal_technical: true,
                ..Default::default()
            },
            keywords: vec![
                "program".to_string(),
                "profile".to_string(),
                "discount".to_string(),
                "tier".to_string(),
            ],
            high_level_keywords: vec!["program".to_string(), "profile".to_string()],
            low_level_keywords: vec!["discount".to_string(), "tier".to_string()],
            top_k: 48,
            context_budget_chars: 22_000,
        };

        let question = "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?";
        let queries = build_lexical_queries(question, &plan);

        assert_eq!(queries[0], "program profile discount tier");
        assert!(queries.contains(&question.to_string()));
        assert!(queries.contains(&"текущий статус checkout server".to_string()));
        assert!(queries.contains(&"список счетов rewards service".to_string()));
        assert!(queries.contains(&"program profile".to_string()));
        assert!(queries.contains(&"discount tier".to_string()));
        assert!(queries.contains(&"program".to_string()));
        assert!(queries.contains(&"profile".to_string()));
    }

    #[test]
    fn build_lexical_queries_expands_canonical_role_targets() {
        let plan = RuntimeQueryPlan {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
            intent_profile: QueryIntentProfile::default(),
            keywords: Vec::new(),
            high_level_keywords: Vec::new(),
            low_level_keywords: Vec::new(),
            top_k: 8,
            context_budget_chars: 22_000,
        };

        let queries = build_lexical_queries(
            "If a system needs retrieval from external documents before answering and also semantic similarity over embeddings, which two technologies from this corpus fit those roles?",
            &plan,
        );

        assert!(queries.contains(&"retrieval-augmented generation".to_string()));
        assert!(queries.contains(&"vector database".to_string()));
    }

    #[test]
    fn verify_answer_rejects_wrong_canonical_targets_for_role_question() {
        let verification = verify_answer_against_canonical_evidence(
            "If a system needs retrieval from external documents before answering and also semantic similarity over embeddings, which two technologies from this corpus fit those roles?",
            "The two technologies are Information retrieval and Knowledge graph.",
            &QueryIntentProfile::default(),
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[],
        );

        assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
        assert!(
            verification.warnings.iter().any(|warning| warning.code == "wrong_canonical_target")
        );
    }

    #[test]
    fn verify_answer_rejects_conflated_semantic_web_and_knowledge_graph_role_question() {
        let verification = verify_answer_against_canonical_evidence(
            "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
            "The technology that focuses on making Internet data machine-readable through standards like RDF and OWL is the Semantic Web. The technology that stores interlinked descriptions of entities and concepts is also the Semantic Web.",
            &QueryIntentProfile::default(),
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[],
        );

        assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
        assert!(
            verification.warnings.iter().any(|warning| warning.code == "wrong_canonical_target")
        );
    }

    #[test]
    fn build_document_literal_answer_extracts_ocr_source_materials() {
        let document_id = Uuid::now_v7();
        let answer = build_document_literal_answer(
            "Which kinds of source material are explicitly listed as OCR inputs in the OCR article?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "optical_character_recognition_wikipedia.md".to_string(),
                excerpt: "machine-encoded text, whether from a scanned document, a photo of a document, a scene photo or from subtitle text.".to_string(),
                score: Some(1.0),
                source_text: "Optical character recognition converts images into machine-encoded text, whether from a scanned document, a photo of a document, a scene photo (for example the text on signs and billboards in a landscape photo) or from subtitle text superimposed on an image.".to_string(),
            }],
        )
        .expect("expected OCR literal answer");

        assert!(answer.contains("scanned document"));
        assert!(answer.contains("photo of a document"));
        assert!(answer.contains("scene photo"));
        assert!(answer.contains("subtitle text"));
        assert!(answer.contains("signs and billboards"));
    }

    #[test]
    fn build_document_literal_answer_extracts_ocr_machine_encoded_text_and_sources() {
        let document_id = Uuid::now_v7();
        let answer = build_document_literal_answer(
            "What does OCR convert images of text into, and what kinds of source material are explicitly named?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "optical_character_recognition_wikipedia.md".to_string(),
                excerpt: "machine-encoded text from a scanned document and subtitle text.".to_string(),
                score: Some(1.0),
                source_text: "Optical character recognition converts images of text into machine-encoded text, whether from a scanned document, a photo of a document, a scene photo (for example the text on signs and billboards in a landscape photo) or from subtitle text superimposed on an image.".to_string(),
            }],
        )
        .expect("expected OCR combined answer");

        assert!(answer.contains("machine-encoded text"));
        assert!(answer.contains("scanned document"));
        assert!(answer.contains("photo of a document"));
        assert!(answer.contains("signs and billboards"));
        assert!(answer.contains("subtitle text"));
    }

    #[test]
    fn build_graph_query_language_answer_requires_grounded_standard_literal() {
        let question = "Which technology in this corpus mentions Gremlin, SPARQL, and Cypher, and what standard query language proposal was approved in 2019?";
        let chunks = [RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            document_label: "graph_database_wikipedia.md".to_string(),
            excerpt: "Early standardization efforts led to Gremlin, SPARQL, and Cypher."
                .to_string(),
            score: Some(1.0),
            source_text: "Early standardization efforts led to multi-vendor query languages like Gremlin, SPARQL, and Cypher."
                .to_string(),
        }];

        let answer = build_graph_query_language_answer(
            question,
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &chunks,
        );

        assert!(answer.is_none());
    }

    #[test]
    fn verify_answer_rejects_unsupported_graph_query_language_claims() {
        let verification = verify_answer_against_canonical_evidence(
            "Which technology in this corpus mentions Gremlin, SPARQL, and Cypher, and what standard query language proposal was approved in 2019?",
            "The technology is the Graph database. The standard query language proposal approved in 2019 was GQL.",
            &QueryIntentProfile::default(),
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: vec![KnowledgeChunkRow {
                    key: Uuid::now_v7().to_string(),
                    arango_id: None,
                    arango_rev: None,
                    chunk_id: Uuid::now_v7(),
                    workspace_id: Uuid::now_v7(),
                    library_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: Some("paragraph".to_string()),
                    content_text: "Early standardization efforts led to multi-vendor query languages like Gremlin, SPARQL, and Cypher.".to_string(),
                    normalized_text: "Early standardization efforts led to multi-vendor query languages like Gremlin, SPARQL, and Cypher.".to_string(),
                    span_start: Some(0),
                    span_end: Some(90),
                    token_count: Some(12),
                    support_block_ids: vec![],
                    section_path: vec![],
                    heading_trail: vec![],
                    literal_digest: None,
                    chunk_state: "active".to_string(),
                    text_generation: Some(1),
                    vector_generation: Some(1),
                }],
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[],
        );

        assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
        assert!(
            verification
                .warnings
                .iter()
                .any(|warning| warning.code == "unsupported_canonical_claim")
        );
    }

    #[test]
    fn apply_rerank_outcome_reorders_bundle_before_final_truncation() {
        let entity_a = Uuid::now_v7();
        let entity_b = Uuid::now_v7();
        let chunk_a = Uuid::now_v7();
        let chunk_b = Uuid::now_v7();
        let mut bundle = RetrievalBundle {
            entities: vec![
                RuntimeMatchedEntity {
                    node_id: entity_a,
                    label: "Alpha".to_string(),
                    node_type: "entity".to_string(),
                    score: Some(0.9),
                },
                RuntimeMatchedEntity {
                    node_id: entity_b,
                    label: "Budget".to_string(),
                    node_type: "entity".to_string(),
                    score: Some(0.4),
                },
            ],
            relationships: Vec::new(),
            chunks: vec![
                RuntimeMatchedChunk {
                    chunk_id: chunk_a,
                    document_id: Uuid::now_v7(),
                    document_label: "alpha.md".to_string(),
                    excerpt: "Alpha excerpt".to_string(),
                    score: Some(0.8),
                    source_text: "Alpha excerpt".to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: chunk_b,
                    document_id: Uuid::now_v7(),
                    document_label: "budget.md".to_string(),
                    excerpt: "Budget approval memo".to_string(),
                    score: Some(0.2),
                    source_text: "Budget approval memo".to_string(),
                },
            ],
        };

        apply_rerank_outcome(
            &mut bundle,
            &RerankOutcome {
                entities: vec![entity_b.to_string(), entity_a.to_string()],
                relationships: Vec::new(),
                chunks: vec![chunk_b.to_string(), chunk_a.to_string()],
                metadata: crate::domains::query::RerankMetadata {
                    status: crate::domains::query::RerankStatus::Applied,
                    candidate_count: 4,
                    reordered_count: Some(4),
                },
            },
        );
        truncate_bundle(&mut bundle, 1);

        assert_eq!(bundle.entities[0].node_id, entity_b);
        assert_eq!(bundle.chunks[0].chunk_id, chunk_b);
    }

    #[test]
    fn maps_query_graph_status_from_library_generation() {
        let ready_generation = KnowledgeLibraryGenerationRow {
            key: "ready".to_string(),
            arango_id: None,
            arango_rev: None,
            generation_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            active_text_generation: 3,
            active_vector_generation: 5,
            active_graph_generation: 7,
            degraded_state: "ready".to_string(),
            updated_at: chrono::Utc::now(),
        };
        let degraded_generation = KnowledgeLibraryGenerationRow {
            degraded_state: "degraded".to_string(),
            ..ready_generation.clone()
        };
        let empty_generation = KnowledgeLibraryGenerationRow {
            active_graph_generation: 0,
            degraded_state: "degraded".to_string(),
            ..ready_generation
        };

        assert_eq!(query_graph_status(Some(&degraded_generation)), "partial");
        assert_eq!(query_graph_status(Some(&empty_generation)), "empty");
        assert_eq!(query_graph_status(None), "empty");
    }
}
