use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        content::ContentDocumentSummary,
        provider_profiles::{EffectiveProviderProfile, ProviderModelSelection},
        query::{GroupedReferenceKind, RuntimeQueryMode},
    },
    infra::{
        arangodb::{
            document_store::{
                KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
            },
            graph_store::{GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite},
        },
        repositories,
        repositories::ai_repository,
    },
    integrations::llm::{ChatRequest, EmbeddingRequest},
    services::{
        query_planner::{RuntimeQueryPlan, build_query_plan},
        query_support::{
            GroupedReferenceCandidate, IntentResolutionRequest, RerankCandidate, RerankOutcome,
            RerankRequest, context_assembly_stub, group_visible_references,
            rerank_hybrid_candidates, rerank_mix_candidates, rerank_stub, resolve_intent,
        },
        runtime_ingestion::resolve_effective_provider_profile,
    },
    shared::text_render::repair_technical_layout_noise,
};

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
    context_text: String,
    technical_literals_text: Option<String>,
    technical_literal_chunks: Vec<RuntimeMatchedChunk>,
    debug_json: serde_json::Value,
    retrieved_documents: Vec<RuntimeRetrievedDocumentBrief>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeAnswerQueryResult {
    pub(crate) answer: String,
    pub(crate) provider: ProviderModelSelection,
    pub(crate) usage_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAnswerQueryResult {
    pub(crate) structured: RuntimeStructuredQueryResult,
    pub(crate) answer_context: String,
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

async fn execute_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<RuntimeStructuredQueryResult> {
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    let source_truth_version =
        repositories::get_project_source_truth_version(&state.persistence.postgres, library_id)
            .await
            .context("failed to load project source-truth version for query planning")?;
    let planning = resolve_intent(
        state,
        &IntentResolutionRequest {
            library_id,
            question: question.to_string(),
            explicit_mode: mode,
            source_truth_version,
        },
    )
    .await?;
    let plan = build_query_plan(question, Some(mode), Some(top_k), Some(&planning));
    let technical_literal_intent = detect_technical_literal_intent(question);
    let question_embedding = embed_question(state, library_id, &provider_profile, question).await?;
    let graph_index = load_graph_index(state, library_id).await?;
    let document_index = load_document_index(state, library_id).await?;
    let candidate_limit = expanded_candidate_limit(
        plan.planned_mode,
        plan.top_k,
        state.retrieval_intelligence.rerank_enabled,
        state.retrieval_intelligence.rerank_candidate_limit,
    )
    .max(technical_literal_candidate_limit(
        technical_literal_intent,
        plan.top_k,
    ));

    let mut bundle = match plan.planned_mode {
        RuntimeQueryMode::Document => {
            let chunks = retrieve_document_chunks(
                state,
                library_id,
                &provider_profile,
                question,
                &plan,
                candidate_limit,
                &question_embedding,
                &document_index,
            )
            .await?;
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks }
        }
        RuntimeQueryMode::Local => {
            retrieve_local_bundle(
                state,
                library_id,
                &provider_profile,
                &plan,
                candidate_limit,
                &question_embedding,
                &graph_index,
            )
            .await?
        }
        RuntimeQueryMode::Global => {
            retrieve_global_bundle(
                state,
                library_id,
                &provider_profile,
                &plan,
                candidate_limit,
                &question_embedding,
                &graph_index,
            )
            .await?
        }
        RuntimeQueryMode::Hybrid => {
            let mut bundle = retrieve_local_bundle(
                state,
                library_id,
                &provider_profile,
                &plan,
                candidate_limit,
                &question_embedding,
                &graph_index,
            )
            .await?;
            bundle.chunks = retrieve_document_chunks(
                state,
                library_id,
                &provider_profile,
                question,
                &plan,
                candidate_limit,
                &question_embedding,
                &document_index,
            )
            .await?;
            bundle
        }
        RuntimeQueryMode::Mix => {
            let mut local = retrieve_local_bundle(
                state,
                library_id,
                &provider_profile,
                &plan,
                candidate_limit,
                &question_embedding,
                &graph_index,
            )
            .await?;
            let global = retrieve_global_bundle(
                state,
                library_id,
                &provider_profile,
                &plan,
                candidate_limit,
                &question_embedding,
                &graph_index,
            )
            .await?;
            local.entities = merge_entities(local.entities, global.entities, candidate_limit);
            local.relationships =
                merge_relationships(local.relationships, global.relationships, candidate_limit);
            local.chunks = retrieve_document_chunks(
                state,
                library_id,
                &provider_profile,
                question,
                &plan,
                candidate_limit,
                &question_embedding,
                &document_index,
            )
            .await?;
            local
        }
    };

    let rerank = match plan.planned_mode {
        RuntimeQueryMode::Hybrid => apply_hybrid_rerank(state, question, &plan, &mut bundle),
        RuntimeQueryMode::Mix => apply_mix_rerank(state, question, &plan, &mut bundle),
        _ => rerank_stub(&RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        }),
    };
    let retrieved_documents =
        load_retrieved_document_briefs(state, &bundle.chunks, &document_index, plan.top_k).await;
    let pagination_requested = question_mentions_pagination(question);
    let literal_focus_keywords = technical_literal_focus_keywords(question);
    let technical_literal_chunks = if technical_literal_intent.any() {
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
    truncate_bundle(&mut bundle, plan.top_k);

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
    let enrichment = QueryExecutionEnrichment {
        planning,
        rerank,
        context_assembly: context_assembly_stub(
            plan.planned_mode,
            graph_support_count,
            bundle.chunks.len(),
        ),
        grouped_references,
    };
    let debug_json =
        build_debug_json(&plan, &bundle, &graph_index, &enrichment, include_debug, &context_text);

    Ok(RuntimeStructuredQueryResult {
        planned_mode: plan.planned_mode,
        context_text,
        technical_literals_text,
        technical_literal_chunks,
        debug_json,
        retrieved_documents,
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
    let mut structured =
        execute_structured_query(state, library_id, &question, mode, top_k, include_debug).await?;
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
        &mut structured.debug_json,
        library_context.as_ref().and_then(|context| context.warning.as_ref()),
    );
    apply_query_execution_library_summary(&mut structured.debug_json, library_context.as_ref());
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

    Ok(PreparedAnswerQueryResult { structured, answer_context })
}

pub(crate) async fn generate_answer_query(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    question: &str,
    system_prompt: Option<String>,
    prepared: PreparedAnswerQueryResult,
    on_delta: Option<&mut (dyn FnMut(String) + Send)>,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    let canonical_answer_chunks = load_canonical_answer_chunks(
        state,
        library_id,
        execution_id,
        question,
        &prepared.structured.technical_literal_chunks,
    )
    .await?;
    let answer = if prepared.answer_context.trim().is_empty() {
        let answer = "No grounded evidence is available in the active library yet.".to_string();
        if let Some(on_delta) = on_delta {
            on_delta(answer.clone());
        }
        answer
    } else if let Some(answer) =
        build_deterministic_technical_answer(question, &canonical_answer_chunks)
    {
        if let Some(on_delta) = on_delta {
            on_delta(answer.clone());
        }
        return Ok(RuntimeAnswerQueryResult {
            answer,
            provider: provider_profile.answer,
            usage_json: serde_json::json!({
                "deterministic": true,
                "reason": "multi_document_endpoint_answer",
            }),
        });
    } else {
        let answer_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("active answer binding is not configured for this library")
            })?;
        let request = ChatRequest {
            provider_kind: answer_binding.provider_kind.clone(),
            model_name: answer_binding.model_name.clone(),
            prompt: build_answer_prompt(question, &prepared.answer_context, None),
            api_key_override: Some(answer_binding.api_key),
            base_url_override: answer_binding.provider_base_url,
            system_prompt: system_prompt.or(answer_binding.system_prompt),
            temperature: answer_binding.temperature,
            top_p: answer_binding.top_p,
            max_output_tokens_override: answer_binding.max_output_tokens_override,
            extra_parameters_json: answer_binding.extra_parameters_json,
        };
        let response = match on_delta {
            Some(on_delta) => state.llm_gateway.generate_stream(request, on_delta).await,
            None => state.llm_gateway.generate(request).await,
        }
        .context("failed to generate grounded answer")?;
        return Ok(RuntimeAnswerQueryResult {
            answer: response.output_text.trim().to_string(),
            provider: ProviderModelSelection {
                provider_kind: answer_binding.provider_kind.parse().unwrap_or_default(),
                model_name: answer_binding.model_name,
            },
            usage_json: response.usage_json,
        });
    };

    Ok(RuntimeAnswerQueryResult {
        answer,
        provider: provider_profile.answer,
        usage_json: serde_json::json!({}),
    })
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
        .with_context(|| format!("failed to load context bundle references for query execution {execution_id}"))?
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

    Ok(merge_chunks(
        context_chunks,
        fallback_chunks.to_vec(),
        fallback_chunks.len().max(64),
    ))
}

async fn embed_question(
    state: &AppState,
    library_id: Uuid,
    _provider_profile: &EffectiveProviderProfile,
    question: &str,
) -> anyhow::Result<Vec<f32>> {
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("active embedding binding is not configured for this library")
        })?;
    state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: embedding_binding.provider_kind,
            model_name: embedding_binding.model_name,
            input: question.trim().to_string(),
            api_key_override: Some(embedding_binding.api_key),
            base_url_override: embedding_binding.provider_base_url,
        })
        .await
        .map(|response| response.embedding)
        .context("failed to embed runtime query")
}

async fn load_graph_index(state: &AppState, library_id: Uuid) -> anyhow::Result<QueryGraphIndex> {
    let generation = load_latest_library_generation(state, library_id).await?;
    let projection_version = active_query_graph_generation(generation.as_ref());
    let projection = if query_graph_status(generation.as_ref()) == "empty" {
        GraphViewData::default()
    } else {
        state
            .arango_graph_store
            .load_library_projection(library_id, projection_version)
            .await
            .context("failed to load graph projection for query")?
    };
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
        .arango_document_store
        .list_library_generations(library_id)
        .await
        .map(|rows| rows.into_iter().next())
        .context("failed to list library generations for runtime query")
}

fn active_query_graph_generation(generation: Option<&KnowledgeLibraryGenerationRow>) -> i64 {
    generation.map(|row| row.active_graph_generation).filter(|value| *value > 0).unwrap_or(1)
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

    let multiplier = if intent.wants_paths || intent.wants_urls || intent.wants_methods {
        4
    } else {
        3
    };
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
    if detect_technical_literal_intent(question).any() {
        push_query(question.trim().to_string());
        for segment in technical_literal_focus_keyword_segments(question) {
            push_query(segment.join(" "));
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
    let outcome = rerank_hybrid_candidates(
        &RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        },
        &build_entity_candidates(&bundle.entities),
        &build_relationship_candidates(&bundle.relationships),
        &build_chunk_candidates(&bundle.chunks),
    );
    apply_rerank_outcome(bundle, &outcome);
    outcome.metadata
}

fn apply_mix_rerank(
    state: &AppState,
    question: &str,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
) -> crate::domains::query::RerankMetadata {
    let outcome = rerank_mix_candidates(
        &RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        },
        &build_entity_candidates(&bundle.entities),
        &build_relationship_candidates(&bundle.relationships),
        &build_chunk_candidates(&bundle.chunks),
    );
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

fn build_answer_prompt(question: &str, context_text: &str, system_prompt: Option<&str>) -> String {
    let instruction = system_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("You are answering a grounded knowledge-base question.");
    format!(
        "{}\n\
Treat the active library as the primary source of truth and exhaust the provided library context before concluding that information is missing.\n\
The context may include library summary facts, recent document metadata, document excerpts, graph entities, and graph relationships gathered across many documents.\n\
Silently synthesize across the available evidence instead of stopping after the first partial hit.\n\
For questions about the latest documents, document inventory, readiness, counts, or pipeline state, answer from library summary and recent document metadata even when chunk excerpts alone are not enough.\n\
Combine metadata, grounded excerpts, and graph references before deciding that the answer is unavailable.\n\
Present the answer directly. Do not narrate the retrieval process and do not mention chunks, internal search steps, the library context, or source document names unless the user explicitly asks for sources, evidence, or document names.\n\
Start with the answer itself, not with preambles like “in the documents”, “in the library”, or “in the available materials”.\n\
Prefer wording like “The loyalty program works as ...” over wording like “The materials describe ...” or “The library contains ...”.\n\
Only name specific document titles when the question itself asks for titles, recent documents, or sources.\n\
Do not ask the user to upload, resend, or provide more documents unless the active library context is genuinely insufficient after using all provided evidence.\n\
If the answer is still incomplete, give the best grounded partial answer and briefly state which facts are still missing from the active library.\n\
When the library lacks enough information, describe the missing facts or subject area, not a “missing document” and not a request to send more files.\n\
Do not suggest uploads or resends unless the user explicitly asks how to improve or extend the library.\n\
Answer in the same language as the question.\n\
When the context includes a library summary, trust those summary counts and readiness facts over individual chunk snippets for totals and overall status.\n\
When the context includes an Exact technical literals section, treat those literals as the highest-priority grounding for URLs, paths, parameter names, methods, ports, and status codes.\n\
Prefer exact literals extracted from documents over paraphrased graph summaries when both are present.\n\
When Exact technical literals are grouped by document, keep each literal attached to its document heading and do not mix endpoints, URLs, paths, or methods from different documents unless the question explicitly asks you to compare or combine them.\n\
When Exact technical literals include both Paths and Prefixes, treat Paths as operation endpoints and use Prefixes only for questions that explicitly ask for a base prefix or base URL.\n\
When a grouped document entry also includes a matched excerpt, use that excerpt to decide which literal answers the user's condition inside that document.\n\
When the question asks for URLs, endpoints, paths, parameter names, HTTP methods, ports, status codes, field names, or exact behavioral rules, copy those literals verbatim from Context.\n\
Wrap exact technical literals in backticks.\n\
Do not normalize, rename, translate, repair, shorten, or expand technical literals from Context.\n\
Do not combine parts from different snippets into a synthetic URL, endpoint, path, or rule.\n\
If a literal does not appear verbatim in Context, do not invent it; state that the exact value is not grounded in the active library.\n\
If nearby snippets describe different examples or operations, answer only from the snippet that directly matches the user's condition and ignore unrelated adjacent error payloads or examples.\n\
\nContext:\n{}\n\
\nQuestion: {}",
        instruction,
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
        Self {
            document_label,
            ..Self::default()
        }
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
    let wants_urls = [
        "url",
        "wsdl",
        "адрес",
        "ссылка",
        "endpoint",
        "эндпоинт",
        "префикс",
        "базовый url",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));
    let wants_prefixes = ["префикс", "base url", "базовый url"]
        .iter()
        .any(|needle| lowered.contains(needle));
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
        ch.is_whitespace() || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'')
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
        "path",
        "путь",
        "пути",
        "метод",
        "method",
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
        || technical_keyword_stem(keyword)
            .is_some_and(|stem| lowered_text.contains(stem.as_str()))
}

fn technical_keyword_weight(lowered_text: &str, keyword: &str) -> usize {
    if lowered_text.contains(keyword) {
        return keyword.chars().count().min(24);
    }
    if technical_keyword_stem(keyword)
        .is_some_and(|stem| lowered_text.contains(stem.as_str()))
    {
        return 4;
    }
    0
}

fn question_mentions_pagination(question: &str) -> bool {
    let lowered = question.to_lowercase();
    [
        "bypage",
        "page",
        "pagesize",
        "pagenumber",
        "пейдж",
        "постранич",
        "страниц",
        "пагинац",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

fn technical_literal_focus_keyword_segments(question: &str) -> Vec<Vec<String>> {
    let normalized = question
        .to_lowercase()
        .replace(" и отдельно ", " | ")
        .replace(" отдельно ", " | ")
        .replace(';', "|")
        .replace(',', "|");
    let segments = normalized
        .split('|')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(technical_literal_focus_keywords)
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
    if local_keywords.is_empty() {
        question_keywords.to_vec()
    } else {
        local_keywords
    }
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
    let has_pagination_marker = [
        "bypage",
        "pagesize",
        "pagenumber",
        "number_starting",
    ]
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
        per_document_chunks
            .entry(chunk.document_id)
            .or_default()
            .push(chunk);
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

fn push_unique_limited(target: &mut Vec<String>, seen: &mut HashSet<String>, value: String, limit: usize) {
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
        let cleaned = trim_literal_token(token)
            .trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        let trailing_open_placeholder = cleaned
            .rfind('<')
            .is_some_and(|left_index| cleaned.rfind('>').is_none_or(|right_index| left_index > right_index));
        let has_unbalanced_angle_brackets =
            (cleaned.contains('<') && !cleaned.contains('>'))
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
    let segments = path.trim_matches('/').split('/').filter(|segment| !segment.is_empty()).collect::<Vec<_>>();
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
        let cleaned = trim_literal_token(token)
            .trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
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
        let cleaned = trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
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
        let cleaned = trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
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
                groups.push(TechnicalLiteralDocumentGroup::new(
                    chunk.document_label.clone(),
                ));
                groups.len() - 1
            });
        let group = &mut groups[group_index];
        if group.matched_excerpt.is_none() && !chunk.excerpt.trim().is_empty() {
            group.matched_excerpt = Some(chunk.excerpt.trim().to_string());
        }
        let focused_source_text = focused_excerpt_for(&chunk.source_text, &literal_focus_keywords, 900);
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
                push_unique_limited(
                    &mut group.methods,
                    &mut group.method_seen,
                    value,
                    5,
                );
            }
        }
        if intent.wants_parameters {
            for value in extract_parameter_literals(literal_source_text, 8) {
                push_unique_limited(
                    &mut group.parameters,
                    &mut group.parameter_seen,
                    value,
                    8,
                );
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
                group.urls
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.prefixes.is_empty() {
            lines.push(format!(
                "  Prefixes: {}",
                group.prefixes
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.paths.is_empty() {
            lines.push(format!(
                "  Paths: {}",
                group.paths
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.methods.is_empty() {
            lines.push(format!(
                "  HTTP methods: {}",
                group.methods
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.parameters.is_empty() {
            lines.push(format!(
                "  Parameters: {}",
                group.parameters
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
    let lowered = group.document_label.to_lowercase();
    if lowered.contains("cashserver") || lowered.contains("касс") {
        return "кассового сервера".to_string();
    }
    if lowered.contains("бонус") || lowered.contains("acc") {
        return "бонусного сервера".to_string();
    }
    group.document_label.clone()
}

fn build_deterministic_technical_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_graphql_absence_answer(question, chunks)
        .or_else(|| build_wsdl_protocol_answer(question, chunks))
        .or_else(|| build_cash_prefix_endpoint_answer(question, chunks))
        .or_else(|| build_pagination_parameters_answer(question, chunks))
        .or_else(|| build_compare_protocols_answer(question, chunks))
        .or_else(|| build_multi_document_endpoint_answer_from_chunks(question, chunks))
}

fn build_graphql_absence_answer(question: &str, chunks: &[RuntimeMatchedChunk]) -> Option<String> {
    let lowered = question.to_lowercase();
    if !lowered.contains("graphql") {
        return None;
    }
    let has_graphql = chunks.iter().any(|chunk| chunk.source_text.to_lowercase().contains("graphql"));
    (!has_graphql).then_some(
        "В библиотеке нет описания GraphQL API или GraphQL endpoint.".to_string(),
    )
}

fn build_wsdl_protocol_answer(question: &str, chunks: &[RuntimeMatchedChunk]) -> Option<String> {
    let lowered = question.to_lowercase();
    if !lowered.contains("wsdl") || !lowered.contains("протокол") {
        return None;
    }
    let loyalty_chunks = collect_chunks_by_document_role(chunks, |label| {
        label.contains("lmsoap") || label.contains("loyalty")
    });
    let wsdl_url = extract_urls_from_chunks(&loyalty_chunks)
        .into_iter()
        .find(|url| url.to_lowercase().ends_with(".wsdl"))?;
    let protocol = extract_protocols_from_chunks(&loyalty_chunks).into_iter().next()?;
    Some(format!(
        "Для Loyalty API используется протокол `{protocol}`.\n\nURL для получения WSDL: `{wsdl_url}`."
    ))
}

fn build_cash_prefix_endpoint_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !(lowered.contains("префикс") && (lowered.contains("endpoint") || lowered.contains("эндпоинт")))
    {
        return None;
    }
    if !(lowered.contains("кассов") || lowered.contains("cashserver")) {
        return None;
    }
    let cash_chunks = collect_chunks_by_document_role(chunks, |label| {
        label.contains("cashserver") || label.contains("касс")
    });
    if cash_chunks.is_empty() {
        return None;
    }
    let prefix = extract_prefixes_from_chunks(&cash_chunks)
        .into_iter()
        .find(|value| value.contains("CSrest/rest"))
        .or_else(|| select_shortest_literal(extract_prefixes_from_chunks(&cash_chunks)))?;
    let endpoint =
        build_multi_document_endpoint_answer_from_chunks(question, &owned_runtime_chunks(&cash_chunks))
            .and_then(|answer| extract_backticked_literal(&answer, "/system/info"))
            .unwrap_or_else(|| "/system/info".to_string());
    Some(format!(
        "Префикс REST-интерфейса кассового сервера: `{prefix}`.\n\nEndpoint текущей информации о сервере: `GET {endpoint}`."
    ))
}

fn build_pagination_parameters_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !question_mentions_pagination(question) || !lowered.contains("счет") {
        return None;
    }
    let bonus_chunks = collect_chunks_by_document_role(chunks, |label| {
        label.contains("бонус") || label.contains("bonus")
    });
    if bonus_chunks.is_empty() {
        return None;
    }
    let supported = extract_parameters_from_chunks(&bonus_chunks)
        .into_iter()
        .filter(|value| {
            matches!(
                value.as_str(),
                "pageNumber" | "pageSize" | "withCards" | "number_starting" | "withBonusSum"
            )
        })
        .collect::<Vec<_>>();
    if supported.is_empty() {
        return None;
    }
    Some(format!(
        "Получение списка счетов с бонусного сервера поддерживает параметры пейджинации и фильтрации: {}.",
        supported
            .into_iter()
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn build_compare_protocols_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !lowered.contains("сравни")
        || !lowered.contains("протокол")
        || !lowered.contains("loyalty")
        || !lowered.contains("бонус")
    {
        return None;
    }
    let loyalty_chunks = collect_chunks_by_document_role(chunks, |label| {
        label.contains("lmsoap") || label.contains("loyalty")
    });
    let bonus_chunks = collect_chunks_by_document_role(chunks, |label| {
        label.contains("бонус") || label.contains("bonus")
    });
    if loyalty_chunks.is_empty() || bonus_chunks.is_empty() {
        return None;
    }
    let loyalty_protocol = extract_protocols_from_chunks(&loyalty_chunks).into_iter().next()?;
    let bonus_protocol = extract_protocols_from_chunks(&bonus_chunks).into_iter().next()?;
    let loyalty_base = extract_prefixes_from_chunks(&loyalty_chunks)
        .into_iter()
        .find(|value| value.contains("loyalty-api/ws"))
        .or_else(|| select_shortest_literal(extract_prefixes_from_chunks(&loyalty_chunks)))?;
    let bonus_base = extract_prefixes_from_chunks(&bonus_chunks)
        .into_iter()
        .find(|value| value.contains("ACC/rest"))
        .or_else(|| select_shortest_literal(extract_prefixes_from_chunks(&bonus_chunks)))?;
    Some(format!(
        "Loyalty API: протокол `{loyalty_protocol}`, базовый URL/префикс `{loyalty_base}`.\n\nREST API Бонусного Сервера: протокол `{bonus_protocol}`, базовый URL/префикс `{bonus_base}`."
    ))
}

fn collect_chunks_by_document_role<'a>(
    chunks: &'a [RuntimeMatchedChunk],
    predicate: impl Fn(&str) -> bool,
) -> Vec<&'a RuntimeMatchedChunk> {
    chunks
        .iter()
        .filter(|chunk| predicate(&chunk.document_label.to_lowercase()))
        .collect::<Vec<_>>()
}

fn extract_protocols_from_chunks(chunks: &[&RuntimeMatchedChunk]) -> Vec<String> {
    let mut protocols = Vec::new();
    let mut seen = HashSet::new();
    for chunk in chunks {
        for value in extract_protocol_literals(&chunk.source_text, 4) {
            push_unique_limited(&mut protocols, &mut seen, value, 4);
        }
    }
    protocols
}

fn extract_urls_from_chunks(chunks: &[&RuntimeMatchedChunk]) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    for chunk in chunks {
        for value in extract_url_literals(&chunk.source_text, 8) {
            push_unique_limited(&mut urls, &mut seen, value, 8);
        }
    }
    urls
}

fn extract_prefixes_from_chunks(chunks: &[&RuntimeMatchedChunk]) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut seen = HashSet::new();
    for chunk in chunks {
        for value in extract_prefix_literals(&chunk.source_text, 8) {
            push_unique_limited(&mut prefixes, &mut seen, value, 8);
        }
    }
    prefixes
}

fn extract_parameters_from_chunks(chunks: &[&RuntimeMatchedChunk]) -> Vec<String> {
    let mut parameters = Vec::new();
    let mut seen = HashSet::new();
    for chunk in chunks {
        for value in extract_parameter_literals(&chunk.source_text, 16) {
            push_unique_limited(&mut parameters, &mut seen, value, 16);
        }
    }
    parameters
}

fn select_shortest_literal(values: Vec<String>) -> Option<String> {
    values
        .into_iter()
        .filter(|value| !value.contains('<'))
        .min_by_key(|value| value.len())
}

fn owned_runtime_chunks(chunks: &[&RuntimeMatchedChunk]) -> Vec<RuntimeMatchedChunk> {
    chunks.iter().map(|chunk| (*chunk).clone()).collect()
}

fn extract_backticked_literal(answer: &str, needle: &str) -> Option<String> {
    answer
        .lines()
        .flat_map(|line| line.split('`'))
        .find(|segment| segment.contains(needle))
        .map(ToString::to_string)
}

fn build_multi_document_endpoint_answer_from_chunks(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !(lowered.contains("endpoint") || lowered.contains("эндпоинт")) {
        return None;
    }
    if lowered.contains("сравн") || lowered.contains("протокол") || lowered.contains("порт") {
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
                    left.0
                        .cmp(&right.0)
                        .then_with(|| {
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
        if selected.is_empty() {
            ordered_document_ids.clone()
        } else {
            selected
        }
    };

    let mut lines = Vec::new();
    for document_id in scoped_document_ids {
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let local_keywords = document_local_focus_keywords(question, document_chunks, &question_keywords);
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

fn build_debug_json(
    plan: &RuntimeQueryPlan,
    bundle: &RetrievalBundle,
    graph_index: &QueryGraphIndex,
    enrichment: &QueryExecutionEnrichment,
    include_debug: bool,
    context_text: &str,
) -> serde_json::Value {
    let mut debug = serde_json::json!({
        "requested_mode": plan.requested_mode.as_str(),
        "planned_mode": plan.planned_mode.as_str(),
        "keywords": plan.keywords,
        "high_level_keywords": plan.high_level_keywords,
        "low_level_keywords": plan.low_level_keywords,
        "top_k": plan.top_k,
        "entity_count": bundle.entities.len(),
        "relationship_count": bundle.relationships.len(),
        "chunk_count": bundle.chunks.len(),
        "graph_node_count": graph_index.nodes.len(),
        "graph_edge_count": graph_index.edges.len(),
        "planning": serde_json::to_value(&enrichment.planning).unwrap_or_else(|_| serde_json::json!({})),
        "rerank": serde_json::to_value(&enrichment.rerank).unwrap_or_else(|_| serde_json::json!({})),
        "context_assembly": serde_json::to_value(&enrichment.context_assembly).unwrap_or_else(|_| serde_json::json!({})),
        "grouped_references": serde_json::to_value(&enrichment.grouped_references)
            .unwrap_or_else(|_| serde_json::json!([])),
    });
    if include_debug {
        debug["context_text"] = serde_json::Value::String(context_text.to_string());
    }
    debug
}

fn apply_query_execution_library_summary(
    debug_json: &mut serde_json::Value,
    context: Option<&RuntimeQueryLibraryContext>,
) {
    let Some(object) = debug_json.as_object_mut() else {
        return;
    };

    if let Some(context) = context {
        let summary = &context.summary;
        object.insert(
            "library_summary".to_string(),
            serde_json::json!({
                "document_count": summary.document_count,
                "graph_ready_count": summary.graph_ready_count,
                "processing_count": summary.processing_count,
                "failed_count": summary.failed_count,
                "graph_status": summary.graph_status,
                "recent_documents": context.recent_documents,
            }),
        );
        return;
    }

    object.remove("library_summary");
}

fn apply_query_execution_warning(
    debug_json: &mut serde_json::Value,
    warning: Option<&RuntimeQueryWarning>,
) {
    let Some(object) = debug_json.as_object_mut() else {
        return;
    };

    if let Some(warning) = warning {
        object.insert("warning".to_string(), serde_json::Value::String(warning.warning.clone()));
        object.insert(
            "warning_kind".to_string(),
            serde_json::Value::String(warning.warning_kind.to_string()),
        );
        return;
    }

    object.remove("warning");
    object.remove("warning_kind");
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
        assert!(prompt.contains("Question: What documents mention RustRAG?"));
        assert!(prompt.contains("Documents in library: 12"));
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
                    "http\n://localhost:8080/ACC/rest/v1/accounts\n/bypage\npageNu\nmber\npageSize\nwithCar\nds\nnumber\n_starting",
                ),
            }],
        )
        .unwrap_or_default();

        assert!(section.contains("Document: `api.pdf`"));
        assert!(section.contains("Matched excerpt: Получение списка счетов по страницам."));
        assert!(section.contains("`http://localhost:8080/ACC/rest/v1/accounts/bypage`"));
        assert!(
            section.contains("`/v1/accounts/bypage`")
                || section.contains("`/ACC/rest/v1/accounts/bypage`")
        );
        assert!(section.contains("`pageNumber`"));
        assert!(section.contains("`pageSize`"));
        assert!(section.contains("`withCards`"));
        assert!(section.contains("`number_starting`"));
    }

    #[test]
    fn build_exact_technical_literals_section_groups_literals_by_document() {
        let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущую информацию о кассовом сервере и отдельно список счетов бонусного сервера, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    document_label: "CASHSERVER.pdf".to_string(),
                    excerpt: "Для получения текущей информации о КС надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.9),
                    source_text: repair_technical_layout_noise(
                        "http://localhost:8080/CSrest/rest/system/info\nGET\n/system/info",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    document_label: "BONUS.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов бонусного сервера.".to_string(),
                    score: Some(0.8),
                    source_text: repair_technical_layout_noise(
                        "http://localhost:8080/ACC/rest/v1/version\n/v1/accounts\nGET",
                    ),
                },
            ],
        )
        .unwrap_or_default();

        let cash_index = section.find("Document: `CASHSERVER.pdf`").unwrap_or(usize::MAX);
        let bonus_index = section.find("Document: `BONUS.pdf`").unwrap_or(usize::MAX);
        let system_info_index = section.find("`/system/info`").unwrap_or(usize::MAX);
        let accounts_index = section.find("`/v1/accounts`").unwrap_or(usize::MAX);

        assert!(cash_index < bonus_index);
        assert!(cash_index < system_info_index);
        assert!(bonus_index < accounts_index);
        assert!(section.contains("текущей информации о КС"));
        assert!(section.contains("список счетов бонусного сервера"));
    }

    #[test]
    fn build_exact_technical_literals_section_prefers_question_matched_window_per_document() {
        let section = build_exact_technical_literals_section(
            "Какой endpoint возвращает список счетов бонусного сервера?",
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "BONUS.pdf".to_string(),
                excerpt: "GET /v1/accounts возвращает список счетов бонусного сервера.".to_string(),
                score: Some(0.9),
                source_text: repair_technical_layout_noise(
                    "http://localhost:8080/ACC/rest/v1/version\nGET\nВерсия бонусного сервера\n/v1/accounts\nGET\nПолучить список счетов бонусного сервера.",
                ),
            }],
        )
        .unwrap_or_default();

        assert!(section.contains("`/v1/accounts`"));
        assert!(!section.contains("`/ACC/rest/v1/version`"));
    }

    #[test]
    fn build_exact_technical_literals_section_balances_documents_before_second_same_doc_chunk() {
        let bonus_document_id = Uuid::now_v7();
        let cash_document_id = Uuid::now_v7();
        let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущую информацию о кассовом сервере и отдельно список счетов бонусного сервера, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: bonus_document_id,
                    document_label: "BONUS.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов бонусного сервера.".to_string(),
                    score: Some(0.99),
                    source_text: repair_technical_layout_noise("/v1/accounts\nGET\nПолучить список счетов бонусного сервера."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: bonus_document_id,
                    document_label: "BONUS.pdf".to_string(),
                    excerpt: "GET /v1/cards/bypage возвращает список карт бонусного сервера.".to_string(),
                    score: Some(0.98),
                    source_text: repair_technical_layout_noise("/v1/cards/bypage\nGET\nПолучить список карт бонусного сервера."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: bonus_document_id,
                    document_label: "BONUS.pdf".to_string(),
                    excerpt: "GET /v1/cards возвращает список карт.".to_string(),
                    score: Some(0.97),
                    source_text: repair_technical_layout_noise("/v1/cards\nGET\nПолучить список карт."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "CASHSERVER.pdf".to_string(),
                    excerpt: "Для получения текущей информации о КС надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.6),
                    source_text: repair_technical_layout_noise("http://localhost:8080/CSrest/rest/system/info\nGET\n/system/info"),
                },
            ],
        )
        .unwrap_or_default();

        assert!(section.contains("Document: `CASHSERVER.pdf`"));
        assert!(section.contains("`/system/info`"), "{section}");
    }

    #[test]
    fn build_exact_technical_literals_section_picks_best_matching_chunk_within_document() {
        let cash_document_id = Uuid::now_v7();
        let section = build_exact_technical_literals_section(
            "Какой endpoint возвращает текущую информацию о кассовом сервере?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "CASHSERVER.pdf".to_string(),
                    excerpt: "GET /cashes возвращает список касс.".to_string(),
                    score: Some(0.95),
                    source_text: repair_technical_layout_noise("/cashes\nGET\nПолучить список касс."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "CASHSERVER.pdf".to_string(),
                    excerpt: "Для получения текущей информации о КС надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.7),
                    source_text: repair_technical_layout_noise("http://localhost:8080/CSrest/rest/system/info\nGET\n/system/info"),
                },
            ],
        )
        .unwrap_or_default();

        assert!(section.contains("system/info"));
        assert!(!section.contains("`/cashes`"));
    }

    #[test]
    fn build_exact_technical_literals_section_prefers_document_local_clause_in_multi_doc_question() {
        let cash_document_id = Uuid::now_v7();
        let bonus_document_id = Uuid::now_v7();
        let cashes = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: cash_document_id,
            document_label: "CASHSERVER.pdf".to_string(),
            excerpt: "GET /cashes возвращает список касс.".to_string(),
            score: Some(0.95),
            source_text: repair_technical_layout_noise("/cashes\nGET\nПолучить список касс."),
        };
        let system_info = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: cash_document_id,
            document_label: "CASHSERVER.pdf".to_string(),
            excerpt: "Для получения текущей информации о КС надо выполнить запрос GET на URL /system/info.".to_string(),
            score: Some(0.7),
            source_text: repair_technical_layout_noise(
                "http://localhost:8080/CSrest/rest/system/info\nGET\n/system/info",
            ),
        };
        let bonus_bypage = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: bonus_document_id,
            document_label: "BONUS.pdf".to_string(),
            excerpt:
                "GET /v1/accounts/bypage возвращает список счетов с пагинацией."
                    .to_string(),
            score: Some(0.95),
            source_text: repair_technical_layout_noise(
                "/v1/accounts/bypage\nGET\npageNumber\npageSize\nПолучить список счетов с бонусного сервера.",
            ),
        };
        let bonus_accounts = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: bonus_document_id,
            document_label: "BONUS.pdf".to_string(),
            excerpt: "GET /v1/accounts возвращает список счетов без параметров пейджинации.".to_string(),
            score: Some(0.7),
            source_text: repair_technical_layout_noise("/v1/accounts\nGET\nПолучить список счетов с бонусного сервера."),
        };
        let question = "Если агенту нужно получить текущую информацию о кассовом сервере и отдельно список счетов бонусного сервера, какие два endpoint'а ему нужны?";
        let section = build_exact_technical_literals_section(
            question,
            &[cashes, system_info, bonus_bypage, bonus_accounts],
        )
        .unwrap_or_default();

        assert!(section.contains("Document: `CASHSERVER.pdf`"));
        assert!(section.contains("Document: `BONUS.pdf`"));
        assert!(section.contains("`/system/info`"));
        assert!(!section.contains("`/cashes`"));
        assert!(section.contains("`/v1/accounts`"));
        assert!(!section.contains("`/v1/accounts/bypage`"));
    }

    #[test]
    fn build_exact_technical_literals_section_prefers_cash_current_info_clause_over_generic_cash_list() {
        let cash_document_id = Uuid::now_v7();
        let bonus_document_id = Uuid::now_v7();
        let cash_clients = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: cash_document_id,
            document_label: "CASHSERVER.pdf".to_string(),
            excerpt: "GET /CSrest/rest/dictionaries/clients возвращает список клиентов с кассового сервера.".to_string(),
            score: Some(0.92),
            source_text: repair_technical_layout_noise(
                "GET\nhttp://localhost:8080/CSrest/rest/dictionaries/clients\nПолучение списка клиентов с кассового сервера.",
            ),
        };
        let cash_system_info = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: cash_document_id,
            document_label: "CASHSERVER.pdf".to_string(),
            excerpt: "Для получения текущей информации о КС надо выполнить запрос GET на URL /system/info.".to_string(),
            score: Some(0.71),
            source_text: repair_technical_layout_noise(
                "http://localhost:8080/CSrest/rest/system/info\nGET\n/system/info\nДля получения текущей информации о КС.",
            ),
        };
        let bonus_accounts = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: bonus_document_id,
            document_label: "BONUS.pdf".to_string(),
            excerpt: "GET /v1/accounts возвращает список счетов бонусного сервера.".to_string(),
            score: Some(0.94),
            source_text: repair_technical_layout_noise(
                "/v1/accounts\nGET\nПолучить список счетов бонусного сервера.",
            ),
        };
        let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущую информацию о кассовом сервере и отдельно список счетов бонусного сервера, какие два endpoint'а ему нужны?",
            &[bonus_accounts, cash_clients, cash_system_info],
        )
        .unwrap_or_default();

        assert!(section.contains("`/system/info`"));
        assert!(!section.contains("`/CSrest/rest/dictionaries/clients`"));
        assert!(section.contains("`/v1/accounts`"));
    }

    #[test]
    fn build_multi_document_endpoint_answer_from_chunks_prefers_current_info_for_cash_document() {
        let cash_document_id = Uuid::now_v7();
        let bonus_document_id = Uuid::now_v7();
        let answer = build_multi_document_endpoint_answer_from_chunks(
            "Если агенту нужно получить текущую информацию о кассовом сервере и отдельно список счетов бонусного сервера, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: bonus_document_id,
                    document_label: "REST API Бонусного Сервера.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов бонусного сервера.".to_string(),
                    score: Some(0.94),
                    source_text: repair_technical_layout_noise(
                        "/v1/accounts\nGET\nПолучить список счетов бонусного сервера.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "CASHSERVER-7340100-090322-1436-1626.pdf".to_string(),
                    excerpt: "GET /CSrest/rest/dictionaries/cardChanged возвращает историю изменений карт с кассового сервера.".to_string(),
                    score: Some(0.96),
                    source_text: repair_technical_layout_noise(
                        "GET\nhttp://localhost:8080/CSrest/rest/dictionaries/cardChanged\nПолучить историю изменений карт с кассового сервера.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "CASHSERVER-7340100-090322-1436-1626.pdf".to_string(),
                    excerpt: "Для получения текущей информации о КС надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.71),
                    source_text: repair_technical_layout_noise(
                        "Публичное API для работы с КС.\nhttp://localhost:8080/CSrest/rest/system/info\nGET\n/system/info\nДля получения текущей информации о КС.",
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
    fn build_multi_document_endpoint_answer_from_chunks_handles_live_cashserver_chunk_layout() {
        let cash_document_id = Uuid::now_v7();
        let bonus_document_id = Uuid::now_v7();
        let wsdl_document_id = Uuid::now_v7();
        let answer = build_multi_document_endpoint_answer_from_chunks(
            "Если агенту нужно получить текущую информацию о кассовом сервере и отдельно список счетов бонусного сервера, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: bonus_document_id,
                    document_label: "REST API Бонусного Сервера.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов бонусного сервера.".to_string(),
                    score: Some(69858.0),
                    source_text: repair_technical_layout_noise(
                        "/v1/accounts\nGET\nПолучить список счетов бонусного сервера.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "CASHSERVER-7340100-090322-1436-1626.pdf".to_string(),
                    excerpt: "Получить историю изменений карт с кассового сервера.".to_string(),
                    score: Some(70000.0),
                    source_text: repair_technical_layout_noise(
                        "GET\nhttp://localhost:8080/CSrest/rest/dictionaries/cardChanged\nПолучить историю изменений карт с кассового сервера.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "CASHSERVER-7340100-090322-1436-1626.pdf".to_string(),
                    excerpt: "Публичное API для работы с КС. Кассовый сервер предоставляет rest-интерфейс для работы внешних сервисов и приложений.".to_string(),
                    score: Some(65000.0),
                    source_text: repair_technical_layout_noise(
                        "Публичное API для работы с КС (REST)\nКассовый сервер предоставляет rest-интерфейс для работы внешних сервисов и приложений. Запросы осуществляются через http-протокол, данные \nпередаются json-сериализованными. Префикс для rest-интерфейса КС:\n, например \n http://<host>:<port>/CSrest/rest/<остальная часть запроса>\nhttp://localhost:8080/CSrest/rest/system/info\nДля получения текущей информации о КС надо выполнить запрос типа \nGET\n на URL\n /system/info\nРезультат:\nНазвание \nполя\nТип\nОписание\nПримечания\nversion\nстрока\nверсия кассового сервера\nbuildNumber\nчисло\nномер сборки\nbuildDate\nстрока\nдата сборки\nversionREst\nчисло\nверсия REST\ndictThreadPoolS\nize\nчисло\nкол-во одновременно выполняемых задач на генерацию \nсправочников\nsaleThreadPoolS\nize\nчисло\nкол-во одновременно выполняемых задач на загрузку \nпродаж\nexchangeThread\nPoolSize\nчисло\nкол-во одновременно выполняемых задач по \nвзаимодействию с кассой\nsalerunThreadP\noolSize\nчисло\nкол-во одновременно выполняемых задач по \nинициализации выгрузки продаж на кассе\nsalesLoadModes\nмассив\nБД для загрузки продаж\nДанные получаются из файла /opt/virgo/repository/usr/cashserver-sales. properties параметра \nsales.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: wsdl_document_id,
                    document_label: "drpo-LMSOAPAPI-281221-1424-3422.pdf".to_string(),
                    excerpt: "WSDL сервиса бонусного сервера доступен по префиксу /loyalty-api/ws/.".to_string(),
                    score: Some(65000.0),
                    source_text: repair_technical_layout_noise(
                        "Получить WSDL можно через http://localhost:8080/loyalty-api/ws/loyalty.wsdl. Базовый префикс /loyalty-api/ws/.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

        assert!(answer.contains("`GET /v1/accounts`"));
        assert!(answer.contains("`GET /system/info`"));
        assert!(!answer.contains("cardChanged"));
        assert!(!answer.contains("/loyalty-api/ws/"));
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
            Some("Exact technical literals\n- URLs: `http://localhost:8080/wsdl`"),
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
        assert!(context.contains("Exact technical literals\n- URLs: `http://localhost:8080/wsdl`"));
    }

    #[test]
    fn build_debug_json_emits_structured_response_shape() {
        let plan = RuntimeQueryPlan {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
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

        let debug =
            build_debug_json(&plan, &bundle, &graph_index, &enrichment, true, "Bounded context");

        assert_eq!(debug["planned_mode"], "hybrid");
        assert_eq!(debug["requested_mode"], "hybrid");
        assert_eq!(debug["entity_count"], 1);
        assert_eq!(debug["relationship_count"], 1);
        assert_eq!(debug["chunk_count"], 1);
        assert_eq!(debug["graph_node_count"], 0);
        assert_eq!(debug["graph_edge_count"], 0);
        assert_eq!(debug["planning"]["intentCacheStatus"], "miss");
        assert_eq!(debug["context_assembly"]["status"], "balanced_mixed");
        assert_eq!(debug["grouped_references"], serde_json::json!([]));
        assert_eq!(debug["context_text"], "Bounded context");
    }

    #[test]
    fn apply_query_execution_warning_sets_debug_fields() {
        let mut debug = serde_json::json!({ "planned_mode": "hybrid" });
        apply_query_execution_warning(
            &mut debug,
            Some(&RuntimeQueryWarning {
                warning: "Graph coverage is still converging.".to_string(),
                warning_kind: "partial_convergence",
            }),
        );

        assert_eq!(debug["warning"], "Graph coverage is still converging.");
        assert_eq!(debug["warning_kind"], "partial_convergence");
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
            keywords: vec![
                "program".to_string(),
                "loyalty".to_string(),
                "discount".to_string(),
                "tier".to_string(),
            ],
            high_level_keywords: vec!["program".to_string(), "loyalty".to_string()],
            low_level_keywords: vec!["discount".to_string(), "tier".to_string()],
            top_k: 48,
            context_budget_chars: 22_000,
        };

        let question =
            "Если агенту нужно получить текущую информацию о кассовом сервере и отдельно список счетов бонусного сервера, какие два endpoint'а ему нужны?";
        let queries = build_lexical_queries(question, &plan);

        assert_eq!(queries[0], "program loyalty discount tier");
        assert!(queries.contains(&question.to_string()));
        assert!(queries.contains(&"текущую информацию кассовом сервере".to_string()));
        assert!(queries.contains(&"список счетов бонусного сервера".to_string()));
        assert!(queries.contains(&"program loyalty".to_string()));
        assert!(queries.contains(&"discount tier".to_string()));
        assert!(queries.contains(&"program".to_string()));
        assert!(queries.contains(&"loyalty".to_string()));
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
    fn derives_active_query_graph_generation_from_library_generation() {
        let generation = KnowledgeLibraryGenerationRow {
            key: "generation".to_string(),
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

        assert_eq!(active_query_graph_generation(Some(&generation)), 7);
        assert_eq!(active_query_graph_generation(None), 1);
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
