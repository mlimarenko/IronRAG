#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{provider_profiles::EffectiveProviderProfile, query::RuntimeQueryMode},
    infra::{
        arangodb::document_store::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
        },
        repositories::ai_repository,
    },
    services::{
        knowledge::runtime_read::{
            graph_view_data_from_runtime_projection, load_active_runtime_graph_projection,
        },
        query::planner::RuntimeQueryPlan,
    },
    shared::extraction::table_summary::is_table_summary_text,
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::question_asks_table_aggregation;
use super::technical_literals::technical_literal_focus_keyword_segments;
use super::types::*;

const DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT: usize = 32;
const DIRECT_TABLE_AGGREGATION_ROW_LIMIT: usize = 24;
const DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT: usize = 32;

pub(crate) async fn load_graph_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<QueryGraphIndex> {
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

pub(crate) async fn load_latest_library_generation(
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

pub(crate) fn query_graph_status(
    generation: Option<&KnowledgeLibraryGenerationRow>,
) -> &'static str {
    match generation {
        Some(row) if row.active_graph_generation > 0 && row.degraded_state == "ready" => "current",
        Some(row) if row.active_graph_generation > 0 => "partial",
        _ => "empty",
    }
}

pub(crate) async fn load_document_index(
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
        .list_documents_by_library(library.workspace_id, library_id, false)
        .await
        .map(|rows| rows.into_iter().map(|row| (row.document_id, row)).collect())
        .context("failed to load runtime query document index")
}

pub(crate) async fn retrieve_document_chunks(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    question: &str,
    forced_target_document_ids: Option<&BTreeSet<Uuid>>,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let targeted_document_ids = forced_target_document_ids
        .filter(|ids| !ids.is_empty())
        .cloned()
        .unwrap_or_else(|| explicit_target_document_ids(question, document_index));
    let initial_table_row_count = requested_initial_table_row_count(question);
    let targeted_table_aggregation =
        question_asks_table_aggregation(question) && !targeted_document_ids.is_empty();
    let lexical_queries = build_lexical_queries(question, plan);
    let lexical_limit = limit.saturating_mul(2).max(24);
    let plan_keywords = &plan.keywords;

    let vector_future = async {
        let context =
            resolve_runtime_vector_search_context(state, library_id, provider_profile).await?;
        let Some(context) = context else {
            return Ok::<Vec<RuntimeMatchedChunk>, anyhow::Error>(Vec::new());
        };
        let raw_hits = state
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
            .context("failed to search canonical chunk vectors for runtime query")?;
        let hits = join_all(raw_hits.into_iter().map(|hit| async move {
            load_runtime_knowledge_chunk(state, hit.chunk_id).await.ok().and_then(|chunk| {
                map_chunk_hit(chunk, hit.score as f32, document_index, plan_keywords)
            })
        }))
        .await
        .into_iter()
        .flatten()
        .filter(|chunk| {
            targeted_document_ids.is_empty() || targeted_document_ids.contains(&chunk.document_id)
        })
        .collect::<Vec<_>>();
        Ok(hits)
    };

    let lexical_future = async {
        let mut lexical_hits = Vec::new();
        for lexical_query in lexical_queries {
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
                    map_chunk_hit(chunk, hit.score as f32, document_index, plan_keywords)
                })
            }))
            .await
            .into_iter()
            .flatten()
            .filter(|chunk| {
                targeted_document_ids.is_empty()
                    || targeted_document_ids.contains(&chunk.document_id)
            })
            .collect::<Vec<_>>();
            lexical_hits = merge_chunks(lexical_hits, query_hits, lexical_limit);
        }
        Ok::<Vec<RuntimeMatchedChunk>, anyhow::Error>(lexical_hits)
    };

    let (vector_hits, lexical_hits) = tokio::try_join!(vector_future, lexical_future)?;
    let mut chunks =
        merge_chunks(vector_hits, lexical_hits, limit.max(initial_table_row_count.unwrap_or(0)));
    if !targeted_document_ids.is_empty() {
        chunks.retain(|chunk| targeted_document_ids.contains(&chunk.document_id));
    }
    if let Some(row_count) = initial_table_row_count {
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            row_count,
            plan_keywords,
        )
        .await?;
        chunks = merge_chunks(chunks, initial_rows, limit.max(row_count));
    }
    if targeted_table_aggregation {
        let direct_summary_chunks = load_table_summary_chunks_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT,
            plan_keywords,
        )
        .await?;
        let direct_row_chunks = load_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            DIRECT_TABLE_AGGREGATION_ROW_LIMIT,
            plan_keywords,
        )
        .await?;
        chunks = merge_canonical_table_aggregation_chunks(
            chunks,
            direct_summary_chunks,
            direct_row_chunks,
            limit.max(DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT),
        );
    }

    Ok(chunks)
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

pub(crate) async fn resolve_runtime_vector_search_context(
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

pub(crate) async fn retrieve_entity_hits(
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

pub(crate) async fn retrieve_relationship_hits(
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

pub(crate) async fn retrieve_local_bundle(
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

pub(crate) async fn retrieve_global_bundle(
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

pub(crate) fn expanded_candidate_limit(
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

pub(crate) fn build_lexical_queries(question: &str, plan: &RuntimeQueryPlan) -> Vec<String> {
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
    if super::answer::question_requests_multi_document_scope(question) {
        for clause in super::answer::extract_multi_document_role_clauses(question) {
            push_query(clause.clone());
            let clause_keywords = crate::services::query::planner::extract_keywords(&clause);
            if !clause_keywords.is_empty() {
                push_query(clause_keywords.join(" "));
            }
            if let Some(target) = super::answer::role_clause_canonical_target(&clause) {
                for alias in super::answer::canonical_target_query_aliases(target) {
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
    // Use concept_keywords for broader text search when available.
    if !plan.concept_keywords.is_empty() {
        push_query(plan.concept_keywords.join(" "));
    }
    if plan.keywords.len() > 1 {
        push_query(plan.keywords.join(" "));
    }
    for keyword in plan.keywords.iter().take(8) {
        push_query(keyword.clone());
    }
    // Add expanded synonyms as additional search queries for broader recall.
    for expanded in plan.expanded_keywords.iter().take(12) {
        if !plan.keywords.contains(expanded) {
            push_query(expanded.clone());
        }
    }

    queries
}

pub(crate) fn request_safe_query(plan: &RuntimeQueryPlan) -> String {
    if !plan.low_level_keywords.is_empty() {
        let combined =
            format!("{} {}", plan.high_level_keywords.join(" "), plan.low_level_keywords.join(" "));
        return combined.trim().to_string();
    }
    plan.keywords.join(" ")
}

pub(crate) fn map_chunk_hit(
    chunk: KnowledgeChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    let document = document_index.get(&chunk.document_id)?;
    let canonical_revision_id = canonical_document_revision_id(document)?;
    if chunk.revision_id != canonical_revision_id {
        return None;
    }
    let source_text = chunk_answer_source_text(&chunk);
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

fn chunk_answer_source_text(chunk: &KnowledgeChunkRow) -> String {
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    if chunk.content_text.trim().is_empty() && !chunk.normalized_text.trim().is_empty() {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    repair_technical_layout_noise(&chunk.content_text)
}

fn explicit_target_document_ids(
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeSet<Uuid> {
    super::explicit_target_document_ids_from_values(
        question,
        document_index.values().flat_map(|document| {
            [
                document.file_name.as_deref(),
                document.title.as_deref(),
                Some(document.external_key.as_str()),
            ]
            .into_iter()
            .flatten()
            .map(move |value| (document.document_id, value))
        }),
    )
}

pub(crate) fn canonical_document_revision_id(document: &KnowledgeDocumentRow) -> Option<Uuid> {
    document.readable_revision_id.or(document.active_revision_id)
}

pub(crate) fn requested_initial_table_row_count(question: &str) -> Option<usize> {
    let lowered = question.to_lowercase();
    for marker in ["первые", "первых", "first"] {
        let Some(start) = lowered.find(marker) else {
            continue;
        };
        let tail = &lowered[start + marker.len()..];
        if !(tail.contains("строк") || tail.contains("строки") || tail.contains("rows"))
        {
            continue;
        }
        let count = tail
            .split(|ch: char| !ch.is_ascii_digit())
            .find_map(|token| (!token.is_empty()).then(|| token.parse::<usize>().ok()).flatten());
        if let Some(count) = count {
            return Some(count.clamp(1, 32));
        }
    }
    None
}

pub(crate) async fn load_initial_table_rows_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    row_count: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    load_table_rows_for_documents(state, document_index, targeted_document_ids, row_count, keywords)
        .await
}

pub(crate) async fn load_table_rows_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    limit_per_document: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() || limit_per_document == 0 {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for document_id in targeted_document_ids {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let rows = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .with_context(|| format!("failed to load table rows for document {document_id}"))?;
        let synthetic_base_score = 0.5_f32;
        chunks.extend(
            rows.into_iter()
                .filter(|chunk| chunk.chunk_kind.as_deref() == Some("table_row"))
                .take(limit_per_document)
                .enumerate()
                .filter_map(|(ordinal, chunk)| {
                    map_chunk_hit(
                        chunk,
                        synthetic_base_score - ordinal as f32 * 0.0001,
                        document_index,
                        keywords,
                    )
                }),
        );
    }

    Ok(chunks)
}

pub(crate) async fn load_table_summary_chunks_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    limit_per_document: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() || limit_per_document == 0 {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for document_id in targeted_document_ids {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let revision_chunks =
            state.arango_document_store.list_chunks_by_revision(revision_id).await.with_context(
                || format!("failed to load table summaries for document {document_id}"),
            )?;
        let synthetic_base_score = 0.01_f32;
        chunks.extend(
            revision_chunks
                .into_iter()
                .filter(|chunk| {
                    chunk.chunk_kind.as_deref() == Some("metadata_block")
                        && is_table_summary_text(&chunk.normalized_text)
                })
                .take(limit_per_document)
                .enumerate()
                .filter_map(|(ordinal, chunk)| {
                    map_chunk_hit(
                        chunk,
                        synthetic_base_score - ordinal as f32 * 0.0001,
                        document_index,
                        keywords,
                    )
                }),
        );
    }

    Ok(chunks)
}

pub(crate) fn is_table_analytics_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    let text = chunk.source_text.trim();
    is_table_summary_text(text) || (text.starts_with("Sheet: ") && text.contains(" | Row "))
}

pub(crate) fn merge_canonical_table_aggregation_chunks(
    existing_chunks: Vec<RuntimeMatchedChunk>,
    direct_summary_chunks: Vec<RuntimeMatchedChunk>,
    direct_row_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    if direct_summary_chunks.is_empty() && direct_row_chunks.is_empty() {
        return existing_chunks;
    }

    let direct_chunks = merge_chunks(direct_summary_chunks, direct_row_chunks, top_k);
    let mut merged = merge_chunks(direct_chunks, existing_chunks, top_k);
    if merged.iter().any(is_table_analytics_chunk) {
        merged.retain(is_table_analytics_chunk);
    }
    merged
}

pub(crate) fn map_edge_hit(
    edge_id: Uuid,
    score: Option<f32>,
    graph_index: &QueryGraphIndex,
    node_index: &HashMap<Uuid, crate::infra::arangodb::graph_store::GraphViewNodeWrite>,
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

pub(crate) fn merge_entities(
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

pub(crate) fn merge_relationships(
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        canonical_document_revision_id, chunk_answer_source_text, explicit_target_document_ids,
        is_table_analytics_chunk, map_chunk_hit, merge_canonical_table_aggregation_chunks,
        requested_initial_table_row_count,
    };
    use crate::infra::arangodb::document_store::{KnowledgeChunkRow, KnowledgeDocumentRow};
    use crate::services::query::execution::{
        RuntimeMatchedChunk, normalized_document_target_candidates,
    };

    #[test]
    fn table_row_answer_context_uses_semantic_row_text() {
        let chunk = KnowledgeChunkRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: Some("table_row".to_string()),
            content_text: "| 1 |".to_string(),
            normalized_text: "Sheet: test1 | Row 1 | col_1: 1".to_string(),
            span_start: Some(0),
            span_end: Some(5),
            token_count: Some(4),
            support_block_ids: Vec::new(),
            section_path: vec!["test1".to_string()],
            heading_trail: vec!["test1".to_string()],
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
        };

        assert_eq!(chunk_answer_source_text(&chunk), "Sheet: test1 | Row 1 | col_1: 1");
    }

    #[test]
    fn metadata_summary_answer_context_uses_normalized_text_when_content_is_empty() {
        let chunk = KnowledgeChunkRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: Some("metadata_block".to_string()),
            content_text: String::new(),
            normalized_text: "Table Summary | Sheet: products | Column: Stock | Value Kind: numeric | Value Shape: label | Aggregation Priority: 3 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Average: 20 | Min: 10 | Max: 30".to_string(),
            span_start: None,
            span_end: None,
            token_count: Some(16),
            support_block_ids: Vec::new(),
            section_path: vec!["products".to_string()],
            heading_trail: vec!["products".to_string()],
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
        };

        assert!(chunk_answer_source_text(&chunk).starts_with("Table Summary |"));
    }

    #[test]
    fn non_table_chunk_answer_context_preserves_raw_content_text() {
        let chunk = KnowledgeChunkRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("heading".to_string()),
            content_text: "test1".to_string(),
            normalized_text: "test1".to_string(),
            span_start: Some(0),
            span_end: Some(5),
            token_count: Some(1),
            support_block_ids: Vec::new(),
            section_path: vec!["test1".to_string()],
            heading_trail: vec!["test1".to_string()],
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
        };

        assert_eq!(chunk_answer_source_text(&chunk), "test1");
    }

    #[test]
    fn explicit_target_document_ids_match_exact_file_name() {
        let document = sample_document_row("people-100.csv", "people-100.csv");
        let document_index = HashMap::from([(document.document_id, document.clone())]);

        let targeted = explicit_target_document_ids(
            "В people-100.csv какая должность у Shelby Terrell?",
            &document_index,
        );

        assert_eq!(targeted, BTreeSet::from([document.document_id]));
    }

    #[test]
    fn document_target_candidates_include_extensionless_stem() {
        let document = sample_document_row("sample-heavy-1.xls", "sample-heavy-1.xls");

        let candidates = normalized_document_target_candidates(
            [
                document.file_name.as_deref(),
                document.title.as_deref(),
                Some(document.external_key.as_str()),
            ]
            .into_iter()
            .flatten(),
        );

        assert!(candidates.contains(&"sample-heavy-1.xls".to_string()));
        assert!(candidates.contains(&"sample-heavy-1".to_string()));
    }

    #[test]
    fn requested_initial_table_row_count_detects_russian_row_ranges() {
        assert_eq!(
            requested_initial_table_row_count(
                "Покажи значения из первых 5 строк sample-heavy-1.xls."
            ),
            Some(5)
        );
    }

    #[test]
    fn requested_initial_table_row_count_detects_english_row_ranges() {
        assert_eq!(
            requested_initial_table_row_count("Show the first 7 rows from people-100.csv."),
            Some(7)
        );
    }

    #[test]
    fn map_chunk_hit_skips_noncanonical_revision_chunks() {
        let document = sample_document_row("people-100.csv", "people-100.csv");
        let canonical_revision_id = canonical_document_revision_id(&document).unwrap();
        let stale_revision_id = Uuid::now_v7();
        assert_ne!(canonical_revision_id, stale_revision_id);
        let document_index = HashMap::from([(document.document_id, document.clone())]);
        let chunk = KnowledgeChunkRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id: Uuid::now_v7(),
            workspace_id: document.workspace_id,
            library_id: document.library_id,
            document_id: document.document_id,
            revision_id: stale_revision_id,
            chunk_index: 0,
            chunk_kind: Some("table_row".to_string()),
            content_text: "stale".to_string(),
            normalized_text: "Sheet: people | Row 1 | Name: Stale".to_string(),
            span_start: None,
            span_end: None,
            token_count: Some(4),
            support_block_ids: Vec::new(),
            section_path: vec!["people".to_string()],
            heading_trail: vec!["people".to_string()],
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
        };

        assert!(map_chunk_hit(chunk, 1.0, &document_index, &[]).is_none());
    }

    #[test]
    fn merge_canonical_table_aggregation_chunks_prefers_table_analytics() {
        let document_id = Uuid::now_v7();
        let heading = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "customers-100.xlsx".to_string(),
            excerpt: "customers-100".to_string(),
            score: Some(1.0),
            source_text: "customers-100".to_string(),
        };
        let summary = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "customers-100.xlsx".to_string(),
            excerpt: "City".to_string(),
            score: Some(1.0),
            source_text: "Table Summary | Sheet: customers-100 | Column: City | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 100 | Non-empty Count: 100 | Distinct Count: 100 | Most Frequent Count: 1 | Most Frequent Tie Count: 100".to_string(),
        };
        let row = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "customers-100.xlsx".to_string(),
            excerpt: "Row 1".to_string(),
            score: Some(1.0),
            source_text: "Sheet: customers-100 | Row 1 | City: Acevedoville".to_string(),
        };

        let merged = merge_canonical_table_aggregation_chunks(
            vec![heading],
            vec![summary.clone()],
            vec![row.clone()],
            8,
        );

        assert_eq!(merged.len(), 2);
        assert!(merged.iter().all(is_table_analytics_chunk));
        let merged_ids = merged.into_iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
        assert_eq!(merged_ids, BTreeSet::from([summary.chunk_id, row.chunk_id]));
    }

    #[test]
    fn merge_canonical_table_aggregation_chunks_keeps_existing_when_no_direct_analytics_exist() {
        let heading = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            document_label: "customers-100.xlsx".to_string(),
            excerpt: "customers-100".to_string(),
            score: Some(1.0),
            source_text: "customers-100".to_string(),
        };

        let merged = merge_canonical_table_aggregation_chunks(
            vec![heading.clone()],
            Vec::new(),
            Vec::new(),
            8,
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].chunk_id, heading.chunk_id);
    }

    fn sample_document_row(file_name: &str, title: &str) -> KnowledgeDocumentRow {
        let document_id = Uuid::now_v7();
        KnowledgeDocumentRow {
            key: document_id.to_string(),
            arango_id: None,
            arango_rev: None,
            document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: document_id.to_string(),
            file_name: Some(file_name.to_string()),
            title: Some(title.to_string()),
            document_state: "active".to_string(),
            active_revision_id: Some(Uuid::now_v7()),
            readable_revision_id: Some(Uuid::now_v7()),
            latest_revision_no: Some(1),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
        }
    }
}

pub(crate) fn merge_chunks(
    left: Vec<RuntimeMatchedChunk>,
    right: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(left, right, top_k)
}

/// Reciprocal Rank Fusion: merges two ranked lists into a single ranking.
/// Each document's score is `1/(k + rank_in_list)` summed across both lists.
/// This normalizes across different scoring scales (BM25 vs cosine similarity).
fn rrf_merge_chunks(
    vector_hits: Vec<RuntimeMatchedChunk>,
    lexical_hits: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    const RRF_K: f32 = 60.0;

    let mut rrf_scores: HashMap<Uuid, f32> = HashMap::new();
    let mut chunks_by_id: HashMap<Uuid, RuntimeMatchedChunk> = HashMap::new();

    // Score vector hits by their rank position
    for (rank, chunk) in vector_hits.into_iter().enumerate() {
        let rrf_score = 1.0 / (RRF_K + rank as f32 + 1.0);
        *rrf_scores.entry(chunk.chunk_id).or_default() += rrf_score;
        chunks_by_id.entry(chunk.chunk_id).or_insert(chunk);
    }

    // Score lexical hits by their rank position
    for (rank, chunk) in lexical_hits.into_iter().enumerate() {
        let rrf_score = 1.0 / (RRF_K + rank as f32 + 1.0);
        *rrf_scores.entry(chunk.chunk_id).or_default() += rrf_score;
        chunks_by_id.entry(chunk.chunk_id).or_insert(chunk);
    }

    // Apply RRF scores back to chunks
    let mut values: Vec<RuntimeMatchedChunk> = chunks_by_id
        .into_values()
        .map(|mut chunk| {
            chunk.score = rrf_scores.get(&chunk.chunk_id).copied();
            chunk
        })
        .collect();

    values.sort_by(score_desc_chunks);
    values.truncate(top_k);
    values
}

pub(crate) fn score_desc_entities(
    left: &RuntimeMatchedEntity,
    right: &RuntimeMatchedEntity,
) -> std::cmp::Ordering {
    score_value(right.score).total_cmp(&score_value(left.score))
}

pub(crate) fn score_desc_relationships(
    left: &RuntimeMatchedRelationship,
    right: &RuntimeMatchedRelationship,
) -> std::cmp::Ordering {
    score_value(right.score).total_cmp(&score_value(left.score))
}

pub(crate) fn score_desc_chunks(
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    score_value(right.score).total_cmp(&score_value(left.score))
}

pub(crate) fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(0.0)
}

pub(crate) fn truncate_bundle(bundle: &mut RetrievalBundle, top_k: usize) {
    bundle.entities.truncate(top_k);
    bundle.relationships.truncate(top_k);
    bundle.chunks.truncate(top_k);
}

fn lexical_entity_hits(
    plan: &RuntimeQueryPlan,
    graph_index: &QueryGraphIndex,
) -> Vec<RuntimeMatchedEntity> {
    // Prefer entity_keywords for entity search when available; fall back to all keywords.
    let search_keywords: &[String] =
        if plan.entity_keywords.is_empty() { &plan.keywords } else { &plan.entity_keywords };
    let mut hits = graph_index
        .nodes
        .values()
        .filter(|node| node.node_type != "document")
        .filter(|node| {
            search_keywords.iter().any(|keyword| {
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

pub(crate) fn related_edges_for_entities(
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

pub(crate) fn entities_from_relationships(
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

pub(crate) fn excerpt_for(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let excerpt = trimmed.chars().take(max_chars).collect::<String>();
    format!("{excerpt}...")
}

pub(crate) fn focused_excerpt_for(content: &str, keywords: &[String], max_chars: usize) -> String {
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
