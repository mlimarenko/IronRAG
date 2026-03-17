use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        provider_profiles::{EffectiveProviderProfile, ProviderModelSelection},
        query_modes::RuntimeQueryMode,
        runtime_query::{GroundingStatus, RuntimeQueryEnrichment, RuntimeQueryReference},
    },
    infra::{
        graph_store::{GraphProjectionData, GraphProjectionEdgeWrite, GraphProjectionNodeWrite},
        repositories::{self, ChunkEmbeddingRow, ChunkRow, DocumentRow, RuntimeQueryExecutionRow},
        vector_search,
    },
    integrations::llm::{ChatRequest, EmbeddingRequest},
    services::{
        graph_projection::active_projection_version,
        query_intelligence::{
            IntentResolutionRequest, RerankCandidate, RerankOutcome, RerankRequest,
        },
        query_planner::{RuntimeQueryPlan, build_query_plan},
        runtime_ingestion::resolve_effective_provider_profile,
    },
};

const MAX_EMBEDDING_SCAN_ROWS: i64 = 10_000;
const MAX_CHUNK_SCAN_ROWS: i64 = 10_000;

#[derive(Debug, Clone)]
pub struct RuntimeQueryRequest {
    pub library_id: Uuid,
    pub question: String,
    pub mode: RuntimeQueryMode,
    pub top_k: usize,
    pub include_debug: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeMatchedEntity {
    pub node_id: Uuid,
    pub label: String,
    pub node_type: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeMatchedRelationship {
    pub edge_id: Uuid,
    pub relation_type: String,
    pub from_node_id: Uuid,
    pub from_label: String,
    pub to_node_id: Uuid,
    pub to_label: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeMatchedChunk {
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    pub document_label: String,
    pub excerpt: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct RuntimeStructuredQueryResult {
    pub mode: RuntimeQueryMode,
    pub planned_mode: RuntimeQueryMode,
    pub keywords: Vec<String>,
    pub grounding_status: GroundingStatus,
    pub entities: Vec<RuntimeMatchedEntity>,
    pub relationships: Vec<RuntimeMatchedRelationship>,
    pub chunks: Vec<RuntimeMatchedChunk>,
    pub references: Vec<RuntimeQueryReference>,
    pub provider: ProviderModelSelection,
    pub context_text: String,
    pub enrichment: RuntimeQueryEnrichment,
    pub debug_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct RuntimeAnswerQueryResult {
    pub structured: RuntimeStructuredQueryResult,
    pub answer: String,
    pub provider: ProviderModelSelection,
    pub usage_json: serde_json::Value,
    pub warning: Option<String>,
    pub warning_kind: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PersistedRuntimeQuery {
    pub execution: RuntimeQueryExecutionRow,
    pub references: Vec<repositories::RuntimeQueryReferenceRow>,
}

#[derive(Debug, Clone)]
struct QueryGraphIndex {
    nodes: HashMap<Uuid, GraphProjectionNodeWrite>,
    edges: Vec<GraphProjectionEdgeWrite>,
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

pub async fn execute_structured_query(
    state: &AppState,
    request: &RuntimeQueryRequest,
) -> anyhow::Result<RuntimeStructuredQueryResult> {
    let provider_profile = resolve_effective_provider_profile(state, request.library_id).await?;
    let source_truth_version = repositories::get_project_source_truth_version(
        &state.persistence.postgres,
        request.library_id,
    )
    .await
    .context("failed to load project source-truth version for query planning")?;
    let planning = state
        .retrieval_intelligence_services
        .query_intelligence
        .resolve_intent(
            state,
            &IntentResolutionRequest {
                library_id: request.library_id,
                question: request.question.clone(),
                explicit_mode: request.mode,
                source_truth_version,
            },
        )
        .await?;
    let plan = build_query_plan(
        &request.question,
        Some(request.mode),
        Some(request.top_k),
        Some(&planning),
    );
    let question_embedding = embed_question(state, &provider_profile, &request.question).await?;
    let graph_index = load_graph_index(state, request.library_id).await?;
    let document_index = load_document_index(state, request.library_id).await?;
    let candidate_limit = expanded_candidate_limit(
        plan.planned_mode,
        plan.top_k,
        state.retrieval_intelligence.rerank_enabled,
        state.retrieval_intelligence.rerank_candidate_limit,
    );

    let mut bundle = match plan.planned_mode {
        RuntimeQueryMode::Document => {
            let chunks = retrieve_document_chunks(
                state,
                request.library_id,
                &provider_profile,
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
                request.library_id,
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
                request.library_id,
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
                request.library_id,
                &provider_profile,
                &plan,
                candidate_limit,
                &question_embedding,
                &graph_index,
            )
            .await?;
            bundle.chunks = retrieve_document_chunks(
                state,
                request.library_id,
                &provider_profile,
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
                request.library_id,
                &provider_profile,
                &plan,
                candidate_limit,
                &question_embedding,
                &graph_index,
            )
            .await?;
            let global = retrieve_global_bundle(
                state,
                request.library_id,
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
                request.library_id,
                &provider_profile,
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
        RuntimeQueryMode::Hybrid => apply_hybrid_rerank(state, request, &plan, &mut bundle),
        RuntimeQueryMode::Mix => apply_mix_rerank(state, request, &plan, &mut bundle),
        _ => state.retrieval_intelligence_services.query_intelligence.rerank_stub(&RerankRequest {
            question: request.question.clone(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        }),
    };
    truncate_bundle(&mut bundle, plan.top_k);

    let grounding_status =
        classify_grounding_status(&bundle.entities, &bundle.relationships, &bundle.chunks);
    let references =
        build_references(&bundle.entities, &bundle.relationships, &bundle.chunks, plan.top_k);
    let context_text = assemble_bounded_context(
        &bundle.entities,
        &bundle.relationships,
        &bundle.chunks,
        plan.context_budget_chars,
    );
    let graph_support_count = bundle.entities.len() + bundle.relationships.len();
    let enrichment = RuntimeQueryEnrichment {
        planning,
        rerank,
        context_assembly: state
            .retrieval_intelligence_services
            .query_intelligence
            .context_assembly_stub(plan.planned_mode, graph_support_count, bundle.chunks.len()),
        grouped_references: Vec::new(),
    };
    let debug_json = build_debug_json(
        &plan,
        &bundle,
        &graph_index,
        &enrichment,
        request.include_debug,
        &context_text,
    );

    Ok(RuntimeStructuredQueryResult {
        mode: request.mode,
        planned_mode: plan.planned_mode,
        keywords: plan.keywords,
        grounding_status,
        entities: bundle.entities,
        relationships: bundle.relationships,
        chunks: bundle.chunks,
        references,
        provider: provider_profile.embedding.clone(),
        context_text,
        enrichment,
        debug_json,
    })
}

pub async fn execute_answer_query(
    state: &AppState,
    request: &RuntimeQueryRequest,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let provider_profile = resolve_effective_provider_profile(state, request.library_id).await?;
    let mut structured = execute_structured_query(state, request).await?;
    let readiness_warning = load_runtime_query_warning(state, request.library_id).await?;
    apply_runtime_query_warning(&mut structured.debug_json, readiness_warning.as_ref());
    let warning = readiness_warning.as_ref().map(|item| item.warning.clone());
    let warning_kind = readiness_warning.as_ref().map(|item| item.warning_kind.to_string());
    let answer = if structured.references.is_empty() {
        "No grounded evidence is available in the active library yet.".to_string()
    } else {
        let response = state
            .llm_gateway
            .generate(ChatRequest {
                provider_kind: provider_profile.answer.provider_kind.as_str().to_string(),
                model_name: provider_profile.answer.model_name.clone(),
                prompt: build_answer_prompt(
                    &request.question,
                    structured.planned_mode,
                    &structured.context_text,
                ),
            })
            .await
            .context("failed to generate grounded answer")?;
        return Ok(RuntimeAnswerQueryResult {
            structured,
            answer: response.output_text.trim().to_string(),
            provider: provider_profile.answer,
            usage_json: response.usage_json,
            warning,
            warning_kind,
        });
    };

    Ok(RuntimeAnswerQueryResult {
        structured,
        answer,
        provider: provider_profile.answer,
        usage_json: serde_json::json!({}),
        warning,
        warning_kind,
    })
}

pub async fn persist_structured_query_result(
    state: &AppState,
    request: &RuntimeQueryRequest,
    result: &RuntimeStructuredQueryResult,
) -> anyhow::Result<PersistedRuntimeQuery> {
    persist_query_execution(
        state,
        request.library_id,
        request.question.as_str(),
        result.mode,
        None,
        result.grounding_status.clone(),
        &result.provider,
        result.debug_json.clone(),
        &result.references,
    )
    .await
}

pub async fn persist_answer_query_result(
    state: &AppState,
    request: &RuntimeQueryRequest,
    result: &RuntimeAnswerQueryResult,
) -> anyhow::Result<PersistedRuntimeQuery> {
    persist_query_execution(
        state,
        request.library_id,
        request.question.as_str(),
        result.structured.mode,
        Some(result.answer.as_str()),
        result.structured.grounding_status.clone(),
        &result.provider,
        result.structured.debug_json.clone(),
        &result.structured.references,
    )
    .await
}

pub async fn load_persisted_query(
    state: &AppState,
    library_id: Uuid,
    query_id: Uuid,
) -> anyhow::Result<Option<PersistedRuntimeQuery>> {
    let Some(execution) =
        repositories::get_runtime_query_execution_by_id(&state.persistence.postgres, query_id)
            .await
            .context("failed to load runtime query execution")?
    else {
        return Ok(None);
    };
    if execution.project_id != library_id {
        return Ok(None);
    }
    let references = repositories::list_runtime_query_references_by_execution(
        &state.persistence.postgres,
        query_id,
    )
    .await
    .context("failed to load runtime query references")?;
    Ok(Some(PersistedRuntimeQuery { execution, references }))
}

async fn persist_query_execution(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mode: RuntimeQueryMode,
    answer_text: Option<&str>,
    grounding_status: GroundingStatus,
    provider: &ProviderModelSelection,
    debug_json: serde_json::Value,
    references: &[RuntimeQueryReference],
) -> anyhow::Result<PersistedRuntimeQuery> {
    let execution = repositories::create_runtime_query_execution(
        &state.persistence.postgres,
        library_id,
        mode.as_str(),
        question,
        "completed",
        answer_text,
        grounding_status.as_str(),
        provider.provider_kind.as_str(),
        &provider.model_name,
        debug_json,
    )
    .await
    .context("failed to create runtime query execution")?;

    let mut persisted_references = Vec::with_capacity(references.len());
    for reference in references {
        persisted_references.push(
            repositories::create_runtime_query_reference(
                &state.persistence.postgres,
                execution.id,
                &reference.kind,
                reference.reference_id,
                reference.excerpt.as_deref(),
                i32::try_from(reference.rank).unwrap_or(i32::MAX),
                reference.score.map(f64::from),
                serde_json::json!({}),
            )
            .await
            .with_context(|| {
                format!("failed to persist runtime query reference {}", reference.reference_id)
            })?,
        );
    }

    Ok(PersistedRuntimeQuery { execution, references: persisted_references })
}

async fn embed_question(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    question: &str,
) -> anyhow::Result<Vec<f32>> {
    state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: provider_profile.embedding.provider_kind.as_str().to_string(),
            model_name: provider_profile.embedding.model_name.clone(),
            input: question.trim().to_string(),
        })
        .await
        .map(|response| response.embedding)
        .context("failed to embed runtime query")
}

async fn load_graph_index(state: &AppState, library_id: Uuid) -> anyhow::Result<QueryGraphIndex> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load runtime graph snapshot for query")?;
    let projection_version = active_projection_version(snapshot.as_ref());
    let projection = if snapshot.as_ref().is_none_or(|row| row.graph_status == "empty") {
        GraphProjectionData::default()
    } else {
        state
            .graph_store
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

async fn load_document_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<HashMap<Uuid, DocumentRow>> {
    repositories::list_documents(&state.persistence.postgres, Some(library_id))
        .await
        .map(|rows| rows.into_iter().map(|row| (row.id, row)).collect())
        .context("failed to load runtime query document index")
}

async fn retrieve_document_chunks(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    document_index: &HashMap<Uuid, DocumentRow>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let chunk_rows = repositories::list_chunks_by_project(
        &state.persistence.postgres,
        library_id,
        MAX_CHUNK_SCAN_ROWS,
    )
    .await
    .context("failed to load chunks for runtime query")?;
    let chunk_index = chunk_rows.into_iter().map(|row| (row.id, row)).collect::<HashMap<_, _>>();
    let embeddings = repositories::list_chunk_embeddings_by_project(
        &state.persistence.postgres,
        library_id,
        MAX_EMBEDDING_SCAN_ROWS,
    )
    .await
    .context("failed to load chunk embeddings for runtime query")?;

    let mut scored = embeddings
        .iter()
        .filter(|row| {
            row.provider_kind == provider_profile.embedding.provider_kind.as_str()
                && row.model_name == provider_profile.embedding.model_name
        })
        .filter_map(|row| {
            score_chunk_embedding(row, question_embedding, &chunk_index, document_index)
        })
        .collect::<Vec<_>>();
    scored.sort_by(score_desc_chunks);
    scored.truncate(limit);

    if scored.is_empty() {
        let fallback_query = plan
            .keywords
            .first()
            .map_or_else(|| request_safe_query(plan), std::clone::Clone::clone);
        let lexical = repositories::search_chunks_by_project(
            &state.persistence.postgres,
            library_id,
            &fallback_query,
            i32::try_from(limit).unwrap_or(i32::MAX),
        )
        .await
        .context("failed to run lexical chunk fallback")?;
        scored = lexical
            .into_iter()
            .filter_map(|chunk| map_chunk_hit(chunk, 0.15, document_index))
            .collect();
    }

    Ok(scored)
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
    if matches!(planned_mode, RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix) && rerank_enabled {
        return top_k.max(rerank_candidate_limit);
    }
    top_k
}

fn apply_hybrid_rerank(
    state: &AppState,
    request: &RuntimeQueryRequest,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
) -> crate::domains::query_intelligence::RerankMetadata {
    let outcome =
        state.retrieval_intelligence_services.query_intelligence.rerank_hybrid_candidates(
            &RerankRequest {
                question: request.question.clone(),
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
    request: &RuntimeQueryRequest,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
) -> crate::domains::query_intelligence::RerankMetadata {
    let outcome = state.retrieval_intelligence_services.query_intelligence.rerank_mix_candidates(
        &RerankRequest {
            question: request.question.clone(),
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

async fn retrieve_entity_hits(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<Vec<RuntimeMatchedEntity>> {
    let mut hits = vector_search::search_runtime_vector_targets(
        &state.persistence.postgres,
        library_id,
        "entity",
        provider_profile.embedding.provider_kind.as_str(),
        &provider_profile.embedding.model_name,
        question_embedding,
        limit,
    )
    .await
    .context("failed to search entity vector targets")?
    .into_iter()
    .filter_map(|hit| {
        graph_index.nodes.get(&hit.row.target_id).map(|node| RuntimeMatchedEntity {
            node_id: node.node_id,
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            score: Some(hit.score),
        })
    })
    .collect::<Vec<_>>();

    if hits.is_empty() {
        hits = lexical_entity_hits(plan, graph_index);
    }
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
    let node_index = &graph_index.nodes;
    let mut hits = vector_search::search_runtime_vector_targets(
        &state.persistence.postgres,
        library_id,
        "relation",
        provider_profile.embedding.provider_kind.as_str(),
        &provider_profile.embedding.model_name,
        question_embedding,
        limit,
    )
    .await
    .context("failed to search relation vector targets")?
    .into_iter()
    .filter_map(|hit| map_edge_hit(hit.row.target_id, Some(hit.score), graph_index, node_index))
    .collect::<Vec<_>>();

    if hits.is_empty() {
        hits = lexical_relationship_hits(plan, graph_index);
    }
    hits.truncate(limit);
    Ok(hits)
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
    let mut relationships = graph_index
        .edges
        .iter()
        .filter(|edge| {
            entity_ids.contains(&edge.from_node_id) || entity_ids.contains(&edge.to_node_id)
        })
        .filter_map(|edge| map_edge_hit(edge.edge_id, Some(0.5), graph_index, &graph_index.nodes))
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

fn build_references(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    top_k: usize,
) -> Vec<RuntimeQueryReference> {
    let mut references = Vec::new();
    let mut rank = 1usize;

    for chunk in chunks.iter().take(top_k) {
        references.push(RuntimeQueryReference {
            kind: "chunk".to_string(),
            reference_id: chunk.chunk_id,
            excerpt: Some(chunk.excerpt.clone()),
            rank,
            score: chunk.score,
        });
        rank += 1;
    }
    for entity in entities.iter().take(top_k) {
        references.push(RuntimeQueryReference {
            kind: "node".to_string(),
            reference_id: entity.node_id,
            excerpt: Some(entity.label.clone()),
            rank,
            score: entity.score,
        });
        rank += 1;
    }
    for relationship in relationships.iter().take(top_k) {
        references.push(RuntimeQueryReference {
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

fn classify_grounding_status(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
) -> GroundingStatus {
    if !chunks.is_empty() && (!entities.is_empty() || !relationships.is_empty()) {
        GroundingStatus::Grounded
    } else if chunks.len() + entities.len() + relationships.len() >= 2 {
        GroundingStatus::Partial
    } else if !chunks.is_empty() || !entities.is_empty() || !relationships.is_empty() {
        GroundingStatus::Weak
    } else {
        GroundingStatus::None
    }
}

fn assemble_bounded_context(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    budget_chars: usize,
) -> String {
    let mut sections = Vec::new();
    let mut used = 0usize;

    append_context_section(
        &mut sections,
        &mut used,
        budget_chars,
        "Entities",
        entities.iter().map(|entity| format!("- {} ({})", entity.label, entity.node_type)),
    );
    append_context_section(
        &mut sections,
        &mut used,
        budget_chars,
        "Relationships",
        relationships.iter().map(|edge| {
            format!("- {} --{}--> {}", edge.from_label, edge.relation_type, edge.to_label)
        }),
    );
    append_context_section(
        &mut sections,
        &mut used,
        budget_chars,
        "Chunks",
        chunks.iter().map(|chunk| format!("- {}: {}", chunk.document_label, chunk.excerpt)),
    );

    sections.join("\n\n")
}

fn append_context_section(
    sections: &mut Vec<String>,
    used: &mut usize,
    budget_chars: usize,
    title: &str,
    lines: impl Iterator<Item = String>,
) {
    let mut section_lines = Vec::new();
    for line in lines {
        let projected = *used + title.len() + line.len() + 4;
        if projected > budget_chars {
            break;
        }
        *used = projected;
        section_lines.push(line);
    }
    if !section_lines.is_empty() {
        sections.push(format!("{title}\n{}", section_lines.join("\n")));
    }
}

fn build_answer_prompt(question: &str, mode: RuntimeQueryMode, context_text: &str) -> String {
    format!(
        "You are answering a grounded knowledge-base question.\n\
Use only the provided context. If the context is insufficient, say so plainly.\n\
Mode: {}\n\
\nContext:\n{}\n\
\nQuestion: {}",
        mode.as_str(),
        context_text,
        question.trim()
    )
}

fn build_debug_json(
    plan: &RuntimeQueryPlan,
    bundle: &RetrievalBundle,
    graph_index: &QueryGraphIndex,
    enrichment: &RuntimeQueryEnrichment,
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
    });
    if include_debug {
        debug["context_text"] = serde_json::Value::String(context_text.to_string());
    }
    debug
}

fn apply_runtime_query_warning(
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

async fn load_runtime_query_warning(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Option<RuntimeQueryWarning>> {
    let counters = repositories::load_runtime_graph_convergence_counters(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to load runtime graph convergence counters for query readiness")?;
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load runtime graph snapshot for query readiness")?;
    let graph_status = snapshot.as_ref().map(|row| row.graph_status.as_str()).unwrap_or("empty");
    let convergence_status = runtime_query_convergence_status(graph_status, &counters);

    Ok(runtime_query_convergence_warning(state, convergence_status, &counters))
}

fn runtime_query_convergence_status(
    graph_status: &str,
    counters: &repositories::RuntimeGraphConvergenceCountersRow,
) -> &'static str {
    if graph_status == "failed" {
        return "degraded";
    }
    if counters.queued_document_count > 0
        || counters.processing_document_count > 0
        || counters.ready_no_graph_count > 0
        || counters.pending_update_count > 0
        || counters.pending_delete_count > 0
        || matches!(graph_status, "building" | "empty" | "partial")
    {
        return "partial";
    }
    "current"
}

fn runtime_query_convergence_warning(
    state: &AppState,
    convergence_status: &str,
    counters: &repositories::RuntimeGraphConvergenceCountersRow,
) -> Option<RuntimeQueryWarning> {
    if convergence_status != "partial" {
        return None;
    }

    let backlog = counters.queued_document_count
        + counters.processing_document_count
        + counters.ready_no_graph_count
        + counters.pending_update_count
        + counters.pending_delete_count;
    let threshold =
        i64::try_from(state.bulk_ingest_hardening.graph_convergence_warning_backlog_threshold)
            .unwrap_or(1);
    if backlog < threshold {
        return None;
    }

    Some(RuntimeQueryWarning {
        warning: format!(
            "Graph coverage is still converging while {backlog} document or mutation task(s) remain in backlog."
        ),
        warning_kind: "partial_convergence",
    })
}

fn request_safe_query(plan: &RuntimeQueryPlan) -> String {
    if !plan.low_level_keywords.is_empty() {
        return plan.low_level_keywords.join(" ");
    }
    plan.keywords.join(" ")
}

#[must_use]
pub fn parse_runtime_query_enrichment(
    debug_json: &serde_json::Value,
    fallback_mode: RuntimeQueryMode,
) -> RuntimeQueryEnrichment {
    let planning = debug_json
        .get("planning")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(|| {
            let keywords = debug_json
                .get("keywords")
                .cloned()
                .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
                .unwrap_or_default();
            crate::domains::query_intelligence::QueryPlanningMetadata {
                requested_mode: fallback_mode,
                planned_mode: fallback_mode,
                intent_cache_status:
                    crate::domains::query_intelligence::QueryIntentCacheStatus::Miss,
                keywords: crate::domains::query_intelligence::IntentKeywords {
                    high_level: keywords.iter().take(3).cloned().collect(),
                    low_level: keywords.iter().skip(3).cloned().collect(),
                },
                warnings: Vec::new(),
            }
        });
    let rerank = debug_json
        .get("rerank")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(crate::domains::query_intelligence::RerankMetadata {
            status: crate::domains::query_intelligence::RerankStatus::NotApplicable,
            candidate_count: 0,
            reordered_count: None,
        });
    let context_assembly = debug_json
        .get("context_assembly")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(crate::domains::query_intelligence::ContextAssemblyMetadata {
            status: crate::domains::query_intelligence::ContextAssemblyStatus::DocumentOnly,
            warning: None,
        });

    RuntimeQueryEnrichment { planning, rerank, context_assembly, grouped_references: Vec::new() }
}

#[must_use]
pub fn parse_runtime_query_warning(
    debug_json: &serde_json::Value,
) -> (Option<String>, Option<String>) {
    let warning =
        debug_json.get("warning").and_then(serde_json::Value::as_str).map(ToOwned::to_owned);
    let warning_kind =
        debug_json.get("warning_kind").and_then(serde_json::Value::as_str).map(ToOwned::to_owned);
    (warning, warning_kind)
}

fn score_chunk_embedding(
    row: &ChunkEmbeddingRow,
    question_embedding: &[f32],
    chunk_index: &HashMap<Uuid, ChunkRow>,
    document_index: &HashMap<Uuid, DocumentRow>,
) -> Option<RuntimeMatchedChunk> {
    let candidate = serde_json::from_value::<Vec<f32>>(row.embedding_json.clone()).ok()?;
    let score = cosine_similarity(question_embedding, &candidate)?;
    let chunk = chunk_index.get(&row.chunk_id)?.clone();
    map_chunk_hit(chunk, score, document_index)
}

fn map_chunk_hit(
    chunk: ChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, DocumentRow>,
) -> Option<RuntimeMatchedChunk> {
    let document = document_index.get(&chunk.document_id)?;
    Some(RuntimeMatchedChunk {
        chunk_id: chunk.id,
        document_id: chunk.document_id,
        document_label: document
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| document.external_key.clone()),
        excerpt: excerpt_for(&chunk.content, 280),
        score: Some(score),
    })
}

fn map_edge_hit(
    edge_id: Uuid,
    score: Option<f32>,
    graph_index: &QueryGraphIndex,
    node_index: &HashMap<Uuid, GraphProjectionNodeWrite>,
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

fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.is_empty() || right.is_empty() || left.len() != right.len() {
        return None;
    }

    let mut dot = 0.0_f32;
    let mut left_norm = 0.0_f32;
    let mut right_norm = 0.0_f32;
    for (lhs, rhs) in left.iter().zip(right.iter()) {
        dot += lhs * rhs;
        left_norm += lhs * lhs;
        right_norm += rhs * rhs;
    }
    let denominator = left_norm.sqrt() * right_norm.sqrt();
    if denominator <= f32::EPSILON {
        return None;
    }
    Some(dot / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_grounding_prefers_grounded_when_graph_and_chunk_evidence_exist() {
        let grounding = classify_grounding_status(
            &[RuntimeMatchedEntity {
                node_id: Uuid::nil(),
                label: "OpenAI".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.8),
            }],
            &[],
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::nil(),
                document_id: Uuid::nil(),
                document_label: "spec.md".to_string(),
                excerpt: "OpenAI appears in the spec".to_string(),
                score: Some(0.7),
            }],
        );

        assert_eq!(grounding, GroundingStatus::Grounded);
    }

    #[test]
    fn classify_grounding_returns_partial_for_multiple_graph_hits_without_chunks() {
        let grounding = classify_grounding_status(
            &[RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Sarah Chen".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.55),
            }],
            &[RuntimeMatchedRelationship {
                edge_id: Uuid::now_v7(),
                relation_type: "mentions".to_string(),
                from_node_id: Uuid::now_v7(),
                from_label: "spec.md".to_string(),
                to_node_id: Uuid::now_v7(),
                to_label: "Sarah Chen".to_string(),
                score: Some(0.42),
            }],
            &[],
        );

        assert_eq!(grounding, GroundingStatus::Partial);
    }

    #[test]
    fn classify_grounding_returns_weak_for_single_evidence_type() {
        let grounding = classify_grounding_status(
            &[],
            &[],
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "quickstart.md".to_string(),
                excerpt: "The graph assistant supports hybrid mode.".to_string(),
                score: Some(0.22),
            }],
        );

        assert_eq!(grounding, GroundingStatus::Weak);
    }

    #[test]
    fn classify_grounding_returns_none_without_any_evidence() {
        assert_eq!(classify_grounding_status(&[], &[], &[]), GroundingStatus::None);
    }

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
    fn build_answer_prompt_mentions_every_runtime_mode() {
        for mode in [
            RuntimeQueryMode::Document,
            RuntimeQueryMode::Local,
            RuntimeQueryMode::Global,
            RuntimeQueryMode::Hybrid,
            RuntimeQueryMode::Mix,
        ] {
            let prompt =
                build_answer_prompt("What documents mention RustRAG?", mode, "Document context");
            assert!(prompt.contains(&format!("Mode: {}", mode.as_str())));
            assert!(prompt.contains("Question: What documents mention RustRAG?"));
        }
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
            }],
        };
        let graph_index = QueryGraphIndex { nodes: HashMap::new(), edges: Vec::new() };
        let enrichment = RuntimeQueryEnrichment {
            planning: crate::domains::query_intelligence::QueryPlanningMetadata {
                requested_mode: RuntimeQueryMode::Hybrid,
                planned_mode: RuntimeQueryMode::Hybrid,
                intent_cache_status:
                    crate::domains::query_intelligence::QueryIntentCacheStatus::Miss,
                keywords: crate::domains::query_intelligence::IntentKeywords {
                    high_level: vec!["rustrag".to_string()],
                    low_level: vec!["graph".to_string()],
                },
                warnings: Vec::new(),
            },
            rerank: crate::domains::query_intelligence::RerankMetadata {
                status: crate::domains::query_intelligence::RerankStatus::Skipped,
                candidate_count: 3,
                reordered_count: None,
            },
            context_assembly: crate::domains::query_intelligence::ContextAssemblyMetadata {
                status: crate::domains::query_intelligence::ContextAssemblyStatus::BalancedMixed,
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
        assert_eq!(debug["context_text"], "Bounded context");
    }

    #[test]
    fn apply_runtime_query_warning_sets_debug_fields() {
        let mut debug = serde_json::json!({ "planned_mode": "hybrid" });
        apply_runtime_query_warning(
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
    fn parse_runtime_query_warning_reads_persisted_warning_fields() {
        let (warning, warning_kind) = parse_runtime_query_warning(&serde_json::json!({
            "warning": "Graph coverage is still converging.",
            "warning_kind": "partial_convergence"
        }));

        assert_eq!(warning.as_deref(), Some("Graph coverage is still converging."));
        assert_eq!(warning_kind.as_deref(), Some("partial_convergence"));
    }

    #[test]
    fn expanded_candidate_limit_only_grows_for_combined_modes_when_enabled() {
        assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Hybrid, 8, true, 24), 24);
        assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Mix, 10, true, 24), 24);
        assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Document, 8, true, 24), 8);
        assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Hybrid, 8, false, 24), 8);
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
                },
                RuntimeMatchedChunk {
                    chunk_id: chunk_b,
                    document_id: Uuid::now_v7(),
                    document_label: "budget.md".to_string(),
                    excerpt: "Budget approval memo".to_string(),
                    score: Some(0.2),
                },
            ],
        };

        apply_rerank_outcome(
            &mut bundle,
            &RerankOutcome {
                entities: vec![entity_b.to_string(), entity_a.to_string()],
                relationships: Vec::new(),
                chunks: vec![chunk_b.to_string(), chunk_a.to_string()],
                metadata: crate::domains::query_intelligence::RerankMetadata {
                    status: crate::domains::query_intelligence::RerankStatus::Applied,
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
    fn cosine_similarity_rejects_dimension_mismatch() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), None);
    }
}
