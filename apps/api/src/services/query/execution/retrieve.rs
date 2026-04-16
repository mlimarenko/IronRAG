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
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::technical_literals::technical_literal_focus_keyword_segments;
use super::types::*;
use super::{
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_summary_chunks_for_documents, merge_canonical_table_aggregation_chunks,
    question_asks_table_aggregation, requested_initial_table_row_count,
};

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
        edges: admitted_projection.edges.into_iter().map(|e| (e.edge_id, e)).collect(),
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
        if question_embedding.is_empty() {
            return Ok::<Vec<RuntimeMatchedChunk>, anyhow::Error>(Vec::new());
        }
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

pub(crate) const fn should_skip_vector_search(plan: &RuntimeQueryPlan) -> bool {
    plan.intent_profile.exact_literal_technical
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
    if super::question_requests_multi_document_scope(question) {
        for clause in super::extract_multi_document_role_clauses(question) {
            push_query(clause.clone());
            let clause_keywords = crate::services::query::planner::extract_keywords(&clause);
            if !clause_keywords.is_empty() {
                push_query(clause_keywords.join(" "));
            }
            if let Some(target) = super::role_clause_canonical_target(&clause) {
                for alias in target.query_aliases() {
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

#[cfg(test)]
#[path = "retrieve_tests.rs"]
mod tests;
