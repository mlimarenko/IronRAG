use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap},
};

use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        provider_profiles::EffectiveProviderProfile,
        query_ir::{EntityRole, QueryAct, QueryIR, QueryScope},
    },
    services::query::{
        planner::RuntimeQueryPlan,
        text_match::{
            RelatedTokenCandidate, RelatedTokenSelection, build_related_token_candidates,
            near_token_overlap_count, normalized_alnum_token_sequence, normalized_alnum_tokens,
            select_related_overlap_tokens_from_candidates, token_sequence_exact_or_contains_tokens,
        },
        vector_dimensions::{
            library_vector_index_dimensions, validate_embedding_vector_dimensions,
        },
    },
    shared::text_tokens::literal_wildcard_prefixes,
};

use super::{
    QueryGraphIndex, RetrievalBundle, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    resolve_runtime_vector_search_context, score_value,
};

const ASSOCIATIVE_GRAPH_EXPANSION_HOPS: usize = 2;
const ASSOCIATIVE_GRAPH_MAX_CANDIDATE_EDGES: usize = 512;
const ASSOCIATIVE_GRAPH_MAX_FRONTIER_NODES: usize = 128;
const ASSOCIATIVE_GRAPH_MAX_EDGES_PER_FRONTIER_NODE: usize = 64;
const ASSOCIATIVE_GRAPH_RANK_ITERATIONS: usize = 8;
const ASSOCIATIVE_GRAPH_DAMPING: f32 = 0.85;
const ASSOCIATIVE_EDGE_SUPPORT_WEIGHT: f32 = 0.015;
const ASSOCIATIVE_EDGE_TEXT_RELEVANCE_WEIGHT: f32 = 16.0;

struct EntityRetrievalLanes {
    vector_hits: Vec<RuntimeMatchedEntity>,
    lexical_hits: Vec<RuntimeMatchedEntity>,
}

async fn retrieve_entity_hit_lanes_with_relevance(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    relevance_profile: &GraphQueryRelevanceProfile,
    target_entity_profiles: &[GraphTargetEntityProfile],
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<EntityRetrievalLanes> {
    let vector_hits = if question_embedding.is_empty() {
        Vec::new()
    } else if let Some(context) =
        resolve_runtime_vector_search_context(state, library_id, provider_profile).await?
    {
        let _vector_guard = state.canonical_services.search.vector_plane_read_guard(state).await?;
        let library_dim = library_vector_index_dimensions(state, library_id).await?;
        validate_embedding_vector_dimensions(
            library_dim,
            question_embedding,
            "runtime entity search",
        )?;
        let entity_vector_result = state
            .arango_search_store
            .search_entity_vectors_by_similarity(
                library_dim,
                library_id,
                &context.model_catalog_id.to_string(),
                question_embedding,
                limit.max(1),
                None,
            )
            .await;
        let raw_hits = match entity_vector_result {
            Ok(hits) => hits,
            Err(ref err) if is_arango_collection_not_found(err) => {
                tracing::info!(
                    library_id = %library_id,
                    "entity vector search: empty layer, returning no graph evidence"
                );
                Vec::new()
            }
            Err(err) => {
                return Err(
                    err.context("failed to search canonical entity vectors for runtime query")
                );
            }
        };
        raw_hits
            .into_iter()
            .filter_map(|hit| {
                let node = graph_index.node(hit.entity_id)?;
                if node.node_type.eq_ignore_ascii_case("document") {
                    return None;
                }
                Some(RuntimeMatchedEntity {
                    node_id: node.id,
                    label: node.label.clone(),
                    node_type: node.node_type.clone(),
                    summary: node.summary.clone(),
                    score: Some(hit.score as f32),
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let lexical_hits = query_relevant_entity_hits_with_relevance(
        relevance_profile,
        target_entity_profiles,
        graph_index,
        limit,
    );
    Ok(EntityRetrievalLanes { vector_hits, lexical_hits })
}

pub(crate) async fn retrieve_relationship_hits(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<Vec<RuntimeMatchedRelationship>> {
    let entity_seed_limit = limit.saturating_mul(2).max(8);
    let relevance_profile = graph_relevance_profile(plan, query_ir);
    let lanes = retrieve_entity_hit_lanes_with_relevance(
        state,
        library_id,
        provider_profile,
        &relevance_profile,
        target_entity_profiles,
        entity_seed_limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let entity_hits =
        merge_entity_retrieval_lanes(lanes.vector_hits, lanes.lexical_hits, entity_seed_limit);
    let topology_hits = associative_edges_for_entities_with_relevance(
        &entity_hits,
        graph_index,
        &relevance_profile,
        entity_seed_limit.saturating_mul(2),
    );
    let lexical_hits = lexical_relationship_hits(&relevance_profile, graph_index);
    Ok(merge_relationships(topology_hits, lexical_hits, limit))
}

pub(crate) async fn retrieve_local_bundle(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<RetrievalBundle> {
    let relevance_profile = graph_relevance_profile(plan, query_ir);
    let lanes = retrieve_entity_hit_lanes_with_relevance(
        state,
        library_id,
        provider_profile,
        &relevance_profile,
        target_entity_profiles,
        limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let entity_hits = merge_entity_retrieval_lanes(lanes.vector_hits, lanes.lexical_hits, limit);
    let relationships = associative_edges_for_entities_with_relevance(
        &entity_hits,
        graph_index,
        &relevance_profile,
        limit,
    );
    Ok(RetrievalBundle { entities: entity_hits, relationships, chunks: Vec::new() })
}

pub(crate) async fn retrieve_global_bundle(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<RetrievalBundle> {
    let relationships = retrieve_relationship_hits(
        state,
        library_id,
        provider_profile,
        plan,
        query_ir,
        target_entity_profiles,
        limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let entities = entities_from_relationships(&relationships, graph_index, limit);
    Ok(RetrievalBundle { entities, relationships, chunks: Vec::new() })
}

pub(crate) async fn retrieve_mixed_graph_bundle(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<RetrievalBundle> {
    let started = std::time::Instant::now();
    if limit == 0 {
        return Ok(RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: Vec::new(),
        });
    }

    let entity_seed_limit = limit.saturating_mul(2).max(8);
    let relevance_profile = graph_relevance_profile(plan, query_ir);
    let lanes = retrieve_entity_hit_lanes_with_relevance(
        state,
        library_id,
        provider_profile,
        &relevance_profile,
        target_entity_profiles,
        entity_seed_limit,
        question_embedding,
        graph_index,
    )
    .await?;

    let local_entities =
        merge_entity_retrieval_lane_slices(&lanes.vector_hits, &lanes.lexical_hits, limit);
    let entity_seed_hits =
        merge_entity_retrieval_lanes(lanes.vector_hits, lanes.lexical_hits, entity_seed_limit);
    let local_relationships = associative_edges_for_entities_with_relevance(
        &local_entities,
        graph_index,
        &relevance_profile,
        limit,
    );

    let global_topology_hits = associative_edges_for_entities_with_relevance(
        &entity_seed_hits,
        graph_index,
        &relevance_profile,
        entity_seed_limit.saturating_mul(2),
    );
    let global_relationships = merge_relationships(
        global_topology_hits,
        lexical_relationship_hits(&relevance_profile, graph_index),
        limit,
    );
    let global_entities = entities_from_relationships(&global_relationships, graph_index, limit);

    let entities = merge_primary_then_expanded_entities(local_entities, global_entities, limit);
    let relationships = merge_relationships(local_relationships, global_relationships, limit);
    tracing::info!(
        stage = "retrieval.mix_graph",
        library_id = %library_id,
        entity_seed_limit,
        entity_count = entities.len(),
        relationship_count = relationships.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "mixed graph retrieval completed"
    );

    Ok(RetrievalBundle { entities, relationships, chunks: Vec::new() })
}

pub(crate) fn map_edge_hit(
    edge_id: Uuid,
    score: Option<f32>,
    graph_index: &QueryGraphIndex,
) -> Option<RuntimeMatchedRelationship> {
    let edge = graph_index.edge(edge_id)?;
    let from_node = graph_index.node(edge.from_node_id)?;
    let to_node = graph_index.node(edge.to_node_id)?;
    Some(RuntimeMatchedRelationship {
        edge_id: edge.id,
        relation_type: edge.relation_type.clone(),
        from_node_id: edge.from_node_id,
        from_label: from_node.label.clone(),
        to_node_id: edge.to_node_id,
        to_label: to_node.label.clone(),
        summary: edge.summary.clone(),
        support_count: edge.support_count,
        score,
    })
}

pub(crate) fn merge_primary_then_expanded_entities(
    left: Vec<RuntimeMatchedEntity>,
    right: Vec<RuntimeMatchedEntity>,
    top_k: usize,
) -> Vec<RuntimeMatchedEntity> {
    if top_k == 0 {
        return Vec::new();
    }

    let mut positions = HashMap::<Uuid, usize>::new();
    let mut merged = Vec::<RuntimeMatchedEntity>::new();
    let mut record = |item: RuntimeMatchedEntity| {
        if let Some(position) = positions.get(&item.node_id).copied() {
            if score_value(item.score) > score_value(merged[position].score) {
                merged[position] = item;
            }
            return;
        }
        positions.insert(item.node_id, merged.len());
        merged.push(item);
    };

    for item in left {
        record(item);
    }
    for item in right {
        record(item);
    }

    merged.truncate(top_k);
    merged
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

fn merge_entity_retrieval_lanes(
    vector_hits: Vec<RuntimeMatchedEntity>,
    lexical_hits: Vec<RuntimeMatchedEntity>,
    top_k: usize,
) -> Vec<RuntimeMatchedEntity> {
    const RRF_K: f32 = 60.0;

    if top_k == 0 {
        return Vec::new();
    }

    let mut rrf_scores: HashMap<Uuid, f32> = HashMap::new();
    let mut raw_scores: HashMap<Uuid, f32> = HashMap::new();
    let mut lane_priorities: HashMap<Uuid, u8> = HashMap::new();
    let mut entities_by_id: HashMap<Uuid, RuntimeMatchedEntity> = HashMap::new();
    let mut record_hit = |rank: usize, entity: RuntimeMatchedEntity, lane_priority: u8| {
        let rrf_score = 1.0 / (RRF_K + rank as f32 + 1.0);
        *rrf_scores.entry(entity.node_id).or_default() += rrf_score;
        lane_priorities
            .entry(entity.node_id)
            .and_modify(|existing| *existing = (*existing).max(lane_priority))
            .or_insert(lane_priority);
        let raw_score = score_value(entity.score);
        if raw_score.is_finite() {
            raw_scores
                .entry(entity.node_id)
                .and_modify(|existing| {
                    if raw_score > *existing {
                        *existing = raw_score;
                    }
                })
                .or_insert(raw_score);
        }
        entities_by_id
            .entry(entity.node_id)
            .and_modify(|existing| {
                if raw_score > score_value(existing.score) {
                    *existing = entity.clone();
                }
            })
            .or_insert(entity);
    };

    for (rank, entity) in vector_hits.into_iter().enumerate() {
        record_hit(rank, entity, 0);
    }
    for (rank, entity) in lexical_hits.into_iter().enumerate() {
        record_hit(rank, entity, 1);
    }

    let mut values = entities_by_id
        .into_values()
        .map(|mut entity| {
            entity.score = rrf_scores.get(&entity.node_id).copied();
            entity
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        let left_rrf = rrf_scores.get(&left.node_id).copied().unwrap_or_default();
        let right_rrf = rrf_scores.get(&right.node_id).copied().unwrap_or_default();
        let left_lane = lane_priorities.get(&left.node_id).copied().unwrap_or_default();
        let right_lane = lane_priorities.get(&right.node_id).copied().unwrap_or_default();
        let left_raw = raw_scores.get(&left.node_id).copied().unwrap_or_default();
        let right_raw = raw_scores.get(&right.node_id).copied().unwrap_or_default();
        right_rrf
            .total_cmp(&left_rrf)
            .then_with(|| right_lane.cmp(&left_lane))
            .then_with(|| right_raw.total_cmp(&left_raw))
            .then_with(|| left.node_type.cmp(&right.node_type))
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    values.truncate(top_k);
    values
}

fn merge_entity_retrieval_lane_slices(
    vector_hits: &[RuntimeMatchedEntity],
    lexical_hits: &[RuntimeMatchedEntity],
    top_k: usize,
) -> Vec<RuntimeMatchedEntity> {
    merge_entity_retrieval_lanes(
        vector_hits.iter().take(top_k).cloned().collect(),
        lexical_hits.iter().take(top_k).cloned().collect(),
        top_k,
    )
}

pub(crate) fn score_desc_entities(
    left: &RuntimeMatchedEntity,
    right: &RuntimeMatchedEntity,
) -> Ordering {
    score_value(right.score)
        .total_cmp(&score_value(left.score))
        .then_with(|| left.node_type.cmp(&right.node_type))
        .then_with(|| left.label.cmp(&right.label))
        .then_with(|| left.node_id.cmp(&right.node_id))
}

pub(crate) fn score_desc_relationships(
    left: &RuntimeMatchedRelationship,
    right: &RuntimeMatchedRelationship,
) -> Ordering {
    score_value(right.score)
        .total_cmp(&score_value(left.score))
        .then_with(|| right.support_count.cmp(&left.support_count))
        .then_with(|| left.relation_type.cmp(&right.relation_type))
        .then_with(|| left.from_label.cmp(&right.from_label))
        .then_with(|| left.to_label.cmp(&right.to_label))
        .then_with(|| left.edge_id.cmp(&right.edge_id))
}

#[derive(Debug, Clone)]
struct GraphQueryRelevanceProfile {
    keywords: Vec<String>,
    keyword_tokens: BTreeSet<String>,
    relationship_keywords: Vec<String>,
    target_types: BTreeSet<String>,
    inventory_support_fallback: bool,
}

fn graph_relevance_profile(
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
) -> GraphQueryRelevanceProfile {
    let keywords = graph_relevance_keywords(plan, query_ir);
    let keyword_tokens = keywords.iter().cloned().collect::<BTreeSet<_>>();
    let relationship_keywords = plan.keywords.clone();
    let target_types = graph_target_types(query_ir);
    let inventory_support_fallback = should_use_inventory_support_fallback(query_ir);
    GraphQueryRelevanceProfile {
        keywords,
        keyword_tokens,
        relationship_keywords,
        target_types,
        inventory_support_fallback,
    }
}

#[cfg(test)]
fn lexical_entity_hits(
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
) -> Vec<RuntimeMatchedEntity> {
    let relevance_profile = graph_relevance_profile(plan, query_ir);
    lexical_entity_hits_with_relevance(&relevance_profile, target_entity_profiles, graph_index)
}

fn lexical_entity_hits_with_relevance(
    relevance_profile: &GraphQueryRelevanceProfile,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
) -> Vec<RuntimeMatchedEntity> {
    lexical_node_hits_with_relevance(relevance_profile, target_entity_profiles, graph_index, false)
}

fn lexical_node_hits_with_relevance(
    relevance_profile: &GraphQueryRelevanceProfile,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
    include_documents: bool,
) -> Vec<RuntimeMatchedEntity> {
    let mut hits = graph_index
        .nodes()
        .filter(|node| include_documents || node.node_type != "document")
        .filter_map(|node| graph_node_relevance(node, relevance_profile, &target_entity_profiles))
        .map(|relevance| RuntimeMatchedEntity {
            node_id: relevance.node.id,
            label: relevance.node.label.clone(),
            node_type: relevance.node.node_type.clone(),
            summary: relevance.node.summary.clone(),
            score: Some(relevance.score),
        })
        .collect::<Vec<_>>();
    hits.sort_by(score_desc_entities);
    hits
}

#[cfg(test)]
pub(crate) fn query_relevant_entity_hits(
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
    limit: usize,
) -> Vec<RuntimeMatchedEntity> {
    let relevance_profile = graph_relevance_profile(plan, query_ir);
    query_relevant_entity_hits_with_relevance(
        &relevance_profile,
        target_entity_profiles,
        graph_index,
        limit,
    )
}

fn query_relevant_entity_hits_with_relevance(
    relevance_profile: &GraphQueryRelevanceProfile,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
    limit: usize,
) -> Vec<RuntimeMatchedEntity> {
    let mut hits =
        lexical_entity_hits_with_relevance(relevance_profile, target_entity_profiles, graph_index);
    if hits.is_empty() && relevance_profile.inventory_support_fallback {
        hits = support_ranked_entity_hits(graph_index, limit);
    }
    hits.truncate(limit);
    hits
}

pub(crate) fn query_relevant_graph_evidence_target_hits(
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
    limit: usize,
) -> Vec<RuntimeMatchedEntity> {
    let relevance_profile = graph_relevance_profile(plan, query_ir);
    let mut hits = lexical_node_hits_with_relevance(
        &relevance_profile,
        target_entity_profiles,
        graph_index,
        true,
    );
    if hits.is_empty() && relevance_profile.inventory_support_fallback {
        hits = support_ranked_entity_hits(graph_index, limit);
    }
    hits.truncate(limit);
    hits
}

fn support_ranked_entity_hits(
    graph_index: &QueryGraphIndex,
    limit: usize,
) -> Vec<RuntimeMatchedEntity> {
    let mut hits = graph_index
        .nodes()
        .filter(|node| node.node_type != "document")
        .map(|node| RuntimeMatchedEntity {
            node_id: node.id,
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            summary: node.summary.clone(),
            score: Some(0.1 + node.support_count.max(0) as f32 * 0.001),
        })
        .collect::<Vec<_>>();
    hits.sort_by(score_desc_entities);
    hits.truncate(limit);
    hits
}

fn graph_relevance_keywords(plan: &RuntimeQueryPlan, query_ir: Option<&QueryIR>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut keywords = Vec::new();
    let mut push = |value: &str| {
        for token in normalized_alnum_tokens(value, 3) {
            if seen.insert(token.clone()) {
                keywords.push(token);
            }
        }
    };

    let primary_keywords =
        if plan.entity_keywords.is_empty() { &plan.keywords } else { &plan.entity_keywords };
    for keyword in primary_keywords {
        push(keyword);
    }
    for keyword in &plan.keywords {
        push(keyword);
    }
    if let Some(ir) = query_ir {
        if let Some(document_focus) = ir.document_focus.as_ref() {
            push(&document_focus.hint);
        }
        for mention in &ir.target_entities {
            push(&mention.label);
        }
    }
    keywords
}

struct GraphNodeRelevance<'a> {
    node: &'a crate::infra::repositories::RuntimeGraphQueryNodeRow,
    score: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphTargetEntityProfile {
    profile_key: String,
    target_label_tokens: Vec<String>,
    wildcard_prefixes: Vec<String>,
    related_tokens: RelatedTokenSelection,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum GraphTargetEntityCoverageFieldKind {
    Label,
    Alias,
    Summary,
    Evidence,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GraphTargetEntityCoverageField<'a> {
    pub(crate) text: &'a str,
    pub(crate) kind: GraphTargetEntityCoverageFieldKind,
}

pub(crate) fn graph_target_entity_profiles(
    query_ir: Option<&QueryIR>,
    graph_index: &QueryGraphIndex,
) -> Vec<GraphTargetEntityProfile> {
    let Some(ir) = query_ir else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let related_candidates =
        build_related_token_candidates(graph_index.nodes().map(|node| node.label.as_str()), 3);
    let mut profiles = graph_target_profiles_from_labels(
        graph_target_profile_labels(ir),
        &related_candidates,
        &mut seen,
    );
    if graph_target_profiles_need_entity_fallback(ir, &profiles, graph_index) {
        profiles.extend(graph_target_profiles_from_labels(
            graph_target_entity_profile_labels(ir),
            &related_candidates,
            &mut seen,
        ));
    }
    profiles
}

fn graph_target_profiles_from_labels(
    labels: Vec<String>,
    related_candidates: &[RelatedTokenCandidate],
    seen: &mut BTreeSet<String>,
) -> Vec<GraphTargetEntityProfile> {
    labels
        .into_iter()
        .filter_map(|label| {
            let label = label.trim();
            if label.is_empty() {
                return None;
            }
            let target_label_tokens = normalized_alnum_token_sequence(label, 3);
            let target_tokens = target_label_tokens.iter().cloned().collect::<BTreeSet<_>>();
            let wildcard_prefixes = literal_wildcard_prefixes(label, 2);
            if target_tokens.is_empty() && wildcard_prefixes.is_empty() {
                return None;
            }
            let profile_key = if wildcard_prefixes.is_empty() {
                target_tokens.iter().cloned().collect::<Vec<_>>().join("\u{0}")
            } else {
                format!("wildcard:{}", wildcard_prefixes.join("\u{0}"))
            };
            if !seen.insert(profile_key.clone()) {
                return None;
            }
            let related_tokens =
                select_related_overlap_tokens_from_candidates(label, related_candidates, 3);
            Some(GraphTargetEntityProfile {
                profile_key,
                target_label_tokens,
                wildcard_prefixes,
                related_tokens,
            })
        })
        .collect()
}

fn graph_target_profiles_need_entity_fallback(
    ir: &QueryIR,
    profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
) -> bool {
    graph_target_profiles_suppress_incomplete_compare_entities(ir)
        && !ir.target_entities.is_empty()
        && !profiles
            .iter()
            .any(|profile| graph_target_entity_profile_matches_graph(profile, graph_index))
}

fn graph_target_entity_profile_matches_graph(
    profile: &GraphTargetEntityProfile,
    graph_index: &QueryGraphIndex,
) -> bool {
    graph_index.nodes().any(|node| {
        let aliases = crate::shared::json_coercion::from_value_or_default::<Vec<String>>(
            "runtime_graph_node.aliases_json",
            &node.aliases_json,
        );
        let mut fields = vec![GraphTargetEntityCoverageField {
            text: node.label.as_str(),
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }];
        for alias in &aliases {
            fields.push(GraphTargetEntityCoverageField {
                text: alias.as_str(),
                kind: GraphTargetEntityCoverageFieldKind::Alias,
            });
        }
        if let Some(summary) = node.summary.as_deref() {
            fields.push(GraphTargetEntityCoverageField {
                text: summary,
                kind: GraphTargetEntityCoverageFieldKind::Summary,
            });
        }
        graph_target_entity_profile_field_score(&fields, profile).is_some()
    })
}

fn graph_target_profile_labels(ir: &QueryIR) -> Vec<String> {
    let mut labels = Vec::new();
    if let Some(document_focus) = ir.document_focus.as_ref() {
        push_unique_graph_target_profile_label(&mut labels, &document_focus.hint);
    }

    if !graph_target_profiles_suppress_incomplete_compare_entities(ir) {
        push_graph_target_entity_profile_labels(&mut labels, ir);
    }
    labels
}

fn graph_target_entity_profile_labels(ir: &QueryIR) -> Vec<String> {
    let mut labels = Vec::new();
    push_graph_target_entity_profile_labels(&mut labels, ir);
    labels
}

fn push_graph_target_entity_profile_labels(labels: &mut Vec<String>, ir: &QueryIR) {
    for mention in ir.target_entities.iter().filter(|mention| mention.role == EntityRole::Subject) {
        push_unique_graph_target_profile_label(labels, &mention.label);
    }

    if labels.is_empty() || matches!(ir.act, QueryAct::Compare) || ir.is_multi_document() {
        for mention in &ir.target_entities {
            push_unique_graph_target_profile_label(labels, &mention.label);
        }
    }
}

fn graph_target_profiles_suppress_incomplete_compare_entities(ir: &QueryIR) -> bool {
    matches!(ir.act, QueryAct::Compare)
        && ir.document_focus.is_some()
        && comparison_operand_count(ir) < 2
}

fn comparison_operand_count(ir: &QueryIR) -> usize {
    let Some(comparison) = &ir.comparison else {
        return 0;
    };
    [&comparison.a, &comparison.b]
        .into_iter()
        .filter_map(|value| value.as_deref())
        .filter(|value| !value.trim().is_empty())
        .count()
}

fn push_unique_graph_target_profile_label(labels: &mut Vec<String>, label: &str) {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return;
    }
    let normalized = trimmed.to_lowercase();
    if labels.iter().any(|existing| existing.to_lowercase() == normalized) {
        return;
    }
    labels.push(trimmed.to_string());
}

pub(crate) fn graph_target_entity_coverage_score(
    fields: &[GraphTargetEntityCoverageField<'_>],
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> usize {
    const SINGLE_PROFILE_BASE_SCORE: usize = 10_000;
    const MULTI_PROFILE_BASE_SCORE: usize = 50_000;
    const MULTI_PROFILE_STEP_SCORE: usize = 1_000;

    if fields.is_empty() || target_entity_profiles.is_empty() {
        return 0;
    }

    let mut matched_profile_count = 0usize;
    let mut matched_score = 0usize;
    let mut matched_profiles = BTreeSet::new();
    for profile in target_entity_profiles {
        let Some(profile_score) = graph_target_entity_profile_field_score(fields, profile) else {
            continue;
        };
        if matched_profiles.insert(profile.profile_key.as_str()) {
            matched_profile_count += 1;
            matched_score = matched_score.saturating_add(profile_score);
        }
    }
    if matched_profile_count == 0 {
        return 0;
    }

    let base = if matched_profile_count > 1 {
        MULTI_PROFILE_BASE_SCORE
            .saturating_add(matched_profile_count.saturating_mul(MULTI_PROFILE_STEP_SCORE))
    } else {
        SINGLE_PROFILE_BASE_SCORE
    };
    base.saturating_add(matched_score)
}

fn graph_target_entity_profile_field_score(
    fields: &[GraphTargetEntityCoverageField<'_>],
    profile: &GraphTargetEntityProfile,
) -> Option<usize> {
    if !profile.wildcard_prefixes.is_empty() {
        return fields
            .iter()
            .filter_map(|field| {
                if !matches!(
                    field.kind,
                    GraphTargetEntityCoverageFieldKind::Label
                        | GraphTargetEntityCoverageFieldKind::Alias
                ) {
                    return None;
                }
                let field_text = normalized_prefix_match_text(field.text);
                profile
                    .wildcard_prefixes
                    .iter()
                    .any(|prefix| field_text.starts_with(prefix))
                    .then_some(graph_target_entity_wildcard_field_score(field.kind))
            })
            .max();
    }

    fields
        .iter()
        .filter_map(|field| {
            let field_text = field.text.trim();
            if field_text.is_empty() {
                return None;
            }
            let field_tokens = normalized_alnum_token_sequence(field_text, 3);
            if token_sequence_exact_or_contains_tokens(&field_tokens, &profile.target_label_tokens)
            {
                return Some(graph_target_entity_exact_field_score(field.kind));
            }
            let field_token_set = field_tokens.iter().cloned().collect::<BTreeSet<_>>();
            if !profile.related_tokens.is_empty()
                && profile.related_tokens.matches_tokens(&field_token_set)
            {
                return Some(graph_target_entity_related_field_score(field.kind));
            }
            None
        })
        .max()
}

fn normalized_prefix_match_text(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect::<String>().trim().to_string()
}

const fn graph_target_entity_wildcard_field_score(
    kind: GraphTargetEntityCoverageFieldKind,
) -> usize {
    match kind {
        GraphTargetEntityCoverageFieldKind::Label => 180,
        GraphTargetEntityCoverageFieldKind::Alias => 160,
        GraphTargetEntityCoverageFieldKind::Evidence
        | GraphTargetEntityCoverageFieldKind::Summary => 0,
    }
}

const fn graph_target_entity_exact_field_score(kind: GraphTargetEntityCoverageFieldKind) -> usize {
    match kind {
        GraphTargetEntityCoverageFieldKind::Label => 160,
        GraphTargetEntityCoverageFieldKind::Alias => 140,
        GraphTargetEntityCoverageFieldKind::Evidence => 110,
        GraphTargetEntityCoverageFieldKind::Summary => 60,
    }
}

const fn graph_target_entity_related_field_score(
    kind: GraphTargetEntityCoverageFieldKind,
) -> usize {
    match kind {
        GraphTargetEntityCoverageFieldKind::Label => 80,
        GraphTargetEntityCoverageFieldKind::Alias => 70,
        GraphTargetEntityCoverageFieldKind::Evidence => 55,
        GraphTargetEntityCoverageFieldKind::Summary => 30,
    }
}

fn graph_target_types(query_ir: Option<&QueryIR>) -> BTreeSet<String> {
    let Some(ir) = query_ir else {
        return BTreeSet::new();
    };
    if !matches!(ir.act, QueryAct::Enumerate | QueryAct::Meta | QueryAct::RetrieveValue)
        && ir.scope != QueryScope::LibraryMeta
    {
        return BTreeSet::new();
    }
    if !ir.target_entities.is_empty() && !matches!(ir.act, QueryAct::Enumerate | QueryAct::Meta) {
        return BTreeSet::new();
    }
    ir.target_types
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn should_use_inventory_support_fallback(query_ir: Option<&QueryIR>) -> bool {
    let Some(ir) = query_ir else {
        return false;
    };
    matches!(ir.act, QueryAct::Enumerate | QueryAct::Meta)
        && ir.scope == QueryScope::LibraryMeta
        && ir.target_entities.is_empty()
        && ir.target_types.is_empty()
}

fn graph_node_relevance<'a>(
    node: &'a crate::infra::repositories::RuntimeGraphQueryNodeRow,
    relevance_profile: &GraphQueryRelevanceProfile,
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> Option<GraphNodeRelevance<'a>> {
    let label = node.label.to_lowercase();
    let node_type = node.node_type.to_lowercase();
    let summary = node.summary.as_deref().unwrap_or_default().to_lowercase();
    let aliases = crate::shared::json_coercion::from_value_or_default::<Vec<String>>(
        "runtime_graph_node.aliases_json",
        &node.aliases_json,
    )
    .into_iter()
    .map(|alias| alias.to_lowercase())
    .collect::<Vec<_>>();
    let label_tokens = normalized_alnum_tokens(&label, 3);

    let mut target_fields = vec![GraphTargetEntityCoverageField {
        text: &label,
        kind: GraphTargetEntityCoverageFieldKind::Label,
    }];
    for alias in &aliases {
        target_fields.push(GraphTargetEntityCoverageField {
            text: alias,
            kind: GraphTargetEntityCoverageFieldKind::Alias,
        });
    }
    if !summary.is_empty() {
        target_fields.push(GraphTargetEntityCoverageField {
            text: &summary,
            kind: GraphTargetEntityCoverageFieldKind::Summary,
        });
    }
    if let Some(score) = explicit_target_entity_relevance(&target_fields, target_entity_profiles) {
        return Some(GraphNodeRelevance { node, score });
    }

    let exact_match = relevance_profile.keywords.iter().any(|keyword| {
        label.contains(keyword.as_str())
            || summary.contains(keyword.as_str())
            || node_type.contains(keyword.as_str())
            || aliases.iter().any(|alias| alias.contains(keyword.as_str()))
    });
    let summary_tokens = normalized_alnum_tokens(&summary, 3);
    let node_type_tokens = normalized_alnum_tokens(&node_type, 3);
    let alias_tokens =
        aliases.iter().flat_map(|alias| normalized_alnum_tokens(alias, 3)).collect::<BTreeSet<_>>();
    let token_overlap = near_token_overlap_count(&relevance_profile.keyword_tokens, &label_tokens)
        + near_token_overlap_count(&relevance_profile.keyword_tokens, &summary_tokens)
        + near_token_overlap_count(&relevance_profile.keyword_tokens, &node_type_tokens)
        + near_token_overlap_count(&relevance_profile.keyword_tokens, &alias_tokens);

    if exact_match || token_overlap > 0 {
        let score = 0.22 + (token_overlap.min(8) as f32 * 0.02);
        return Some(GraphNodeRelevance { node, score });
    }

    if relevance_profile.target_types.contains(&node_type) {
        return Some(GraphNodeRelevance { node, score: 0.18 });
    }

    None
}

fn explicit_target_entity_relevance(
    fields: &[GraphTargetEntityCoverageField<'_>],
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> Option<f32> {
    let score = graph_target_entity_coverage_score(fields, target_entity_profiles);
    (score > 0).then_some(score as f32)
}

fn lexical_relationship_hits(
    relevance_profile: &GraphQueryRelevanceProfile,
    graph_index: &QueryGraphIndex,
) -> Vec<RuntimeMatchedRelationship> {
    let mut hits = graph_index
        .edges()
        .filter(|edge| {
            relevance_profile
                .relationship_keywords
                .iter()
                .any(|keyword| edge.relation_type.to_ascii_lowercase().contains(keyword))
        })
        .filter_map(|edge| map_edge_hit(edge.id, Some(0.2), graph_index))
        .collect::<Vec<_>>();
    hits.sort_by(score_desc_relationships);
    hits
}

pub(crate) fn associative_edges_for_entities(
    entities: &[RuntimeMatchedEntity],
    graph_index: &QueryGraphIndex,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    top_k: usize,
) -> Vec<RuntimeMatchedRelationship> {
    let relevance_profile = graph_relevance_profile(plan, query_ir);
    associative_edges_for_entities_with_relevance(entities, graph_index, &relevance_profile, top_k)
}

fn associative_edges_for_entities_with_relevance(
    entities: &[RuntimeMatchedEntity],
    graph_index: &QueryGraphIndex,
    relevance_profile: &GraphQueryRelevanceProfile,
    top_k: usize,
) -> Vec<RuntimeMatchedRelationship> {
    if top_k == 0 || entities.is_empty() {
        return Vec::new();
    }

    let mut seed_scores = associative_seed_scores(entities, graph_index, false);
    if seed_scores.is_empty() {
        seed_scores = associative_seed_scores(entities, graph_index, true);
    }
    if seed_scores.is_empty() {
        return Vec::new();
    }

    let candidate_edges =
        associative_candidate_edges(&seed_scores, graph_index, relevance_profile, top_k);
    if candidate_edges.is_empty() {
        return Vec::new();
    }

    let node_scores = propagate_associative_node_scores(&seed_scores, &candidate_edges);
    let mut relationships = candidate_edges
        .iter()
        .filter_map(|candidate| {
            let from_score = node_scores.get(&candidate.from_node_id).copied().unwrap_or_default();
            let to_score = node_scores.get(&candidate.to_node_id).copied().unwrap_or_default();
            let endpoint_score = from_score.max(to_score) + (from_score.min(to_score) * 0.5);
            let relevance = endpoint_score
                + (candidate.text_relevance * ASSOCIATIVE_EDGE_TEXT_RELEVANCE_WEIGHT)
                + candidate.support_bonus;
            map_edge_hit(candidate.edge_id, Some(relevance), graph_index)
        })
        .collect::<Vec<_>>();
    relationships.sort_by(score_desc_relationships);
    relationships.truncate(top_k);
    relationships
}

fn associative_seed_scores(
    entities: &[RuntimeMatchedEntity],
    graph_index: &QueryGraphIndex,
    include_documents: bool,
) -> BTreeMap<Uuid, f32> {
    entities
        .iter()
        .enumerate()
        .filter_map(|(rank, entity)| {
            let node = graph_index.node(entity.node_id)?;
            if !include_documents && node.node_type.eq_ignore_ascii_case("document") {
                return None;
            }
            Some((entity.node_id, associative_seed_score_for_rank(rank)))
        })
        .collect()
}

fn associative_seed_score_for_rank(rank: usize) -> f32 {
    1.0 + (1.0 / (rank as f32 + 1.0))
}

fn is_document_node(graph_index: &QueryGraphIndex, node_id: &Uuid) -> bool {
    graph_index.node(*node_id).is_some_and(|node| node.node_type.eq_ignore_ascii_case("document"))
}

#[derive(Debug, Clone)]
struct AssociativeCandidateEdge {
    edge_id: Uuid,
    from_node_id: Uuid,
    to_node_id: Uuid,
    text_relevance: f32,
    support_bonus: f32,
    walk_weight: f32,
    pre_score: f32,
}

fn associative_candidate_edges(
    seed_scores: &BTreeMap<Uuid, f32>,
    graph_index: &QueryGraphIndex,
    relevance_profile: &GraphQueryRelevanceProfile,
    top_k: usize,
) -> Vec<AssociativeCandidateEdge> {
    let max_candidate_edges =
        top_k.saturating_mul(16).clamp(64, ASSOCIATIVE_GRAPH_MAX_CANDIDATE_EDGES);
    let mut selected_edges = Vec::new();
    let mut selected_edge_ids = BTreeSet::new();
    let mut known_node_ids = seed_scores.keys().copied().collect::<BTreeSet<_>>();
    let mut frontier = known_node_ids.clone();

    for _ in 0..ASSOCIATIVE_GRAPH_EXPANSION_HOPS {
        if frontier.is_empty() || selected_edges.len() >= max_candidate_edges {
            break;
        }

        let mut depth_edge_ids = BTreeSet::new();
        let mut depth_edges = Vec::new();
        for node_id in frontier.iter().take(ASSOCIATIVE_GRAPH_MAX_FRONTIER_NODES) {
            let mut incident_edges = graph_index
                .incident_edges(*node_id)
                .filter(|edge| !selected_edge_ids.contains(&edge.id))
                .filter(|edge| depth_edge_ids.insert(edge.id))
                .filter_map(|edge| {
                    associative_candidate_edge(
                        edge,
                        graph_index,
                        relevance_profile,
                        seed_scores,
                        &known_node_ids,
                    )
                })
                .collect::<Vec<_>>();
            incident_edges.sort_by(|left, right| {
                right
                    .pre_score
                    .total_cmp(&left.pre_score)
                    .then_with(|| left.edge_id.cmp(&right.edge_id))
            });
            depth_edges.extend(
                incident_edges.into_iter().take(ASSOCIATIVE_GRAPH_MAX_EDGES_PER_FRONTIER_NODE),
            );
        }

        depth_edges.sort_by(|left, right| {
            right
                .pre_score
                .total_cmp(&left.pre_score)
                .then_with(|| left.edge_id.cmp(&right.edge_id))
        });

        let remaining = max_candidate_edges.saturating_sub(selected_edges.len());
        let mut next_frontier = BTreeSet::new();
        for edge in depth_edges.into_iter().take(remaining) {
            selected_edge_ids.insert(edge.edge_id);
            for node_id in [edge.from_node_id, edge.to_node_id] {
                if is_document_node(graph_index, &node_id) {
                    continue;
                }
                if known_node_ids.insert(node_id) {
                    next_frontier.insert(node_id);
                }
            }
            selected_edges.push(edge);
        }
        frontier = next_frontier;
    }

    selected_edges
}

fn associative_candidate_edge(
    edge: &crate::infra::repositories::RuntimeGraphQueryEdgeRow,
    graph_index: &QueryGraphIndex,
    relevance_profile: &GraphQueryRelevanceProfile,
    seed_scores: &BTreeMap<Uuid, f32>,
    known_node_ids: &BTreeSet<Uuid>,
) -> Option<AssociativeCandidateEdge> {
    if graph_index.node(edge.from_node_id).is_none() || graph_index.node(edge.to_node_id).is_none()
    {
        return None;
    }
    let text_relevance = graph_edge_text_relevance(edge, graph_index, relevance_profile);
    let support_bonus =
        (edge.support_count.max(1) as f32).ln_1p() * ASSOCIATIVE_EDGE_SUPPORT_WEIGHT;
    let seed_score = seed_scores
        .get(&edge.from_node_id)
        .copied()
        .unwrap_or_default()
        .max(seed_scores.get(&edge.to_node_id).copied().unwrap_or_default());
    let known_endpoint_bonus = if known_node_ids.contains(&edge.from_node_id)
        || known_node_ids.contains(&edge.to_node_id)
    {
        0.05
    } else {
        0.0
    };
    let stored_weight = edge
        .weight
        .map(|weight| weight as f32)
        .filter(|weight| weight.is_finite() && *weight > 0.0)
        .unwrap_or(1.0 + support_bonus)
        .min(10.0);
    let weighted_text_relevance = text_relevance * ASSOCIATIVE_EDGE_TEXT_RELEVANCE_WEIGHT;
    let pre_score = seed_score + weighted_text_relevance + support_bonus + known_endpoint_bonus;
    Some(AssociativeCandidateEdge {
        edge_id: edge.id,
        from_node_id: edge.from_node_id,
        to_node_id: edge.to_node_id,
        text_relevance,
        support_bonus,
        walk_weight: stored_weight + weighted_text_relevance,
        pre_score,
    })
}

fn propagate_associative_node_scores(
    seed_scores: &BTreeMap<Uuid, f32>,
    candidate_edges: &[AssociativeCandidateEdge],
) -> BTreeMap<Uuid, f32> {
    let seed_total = seed_scores.values().copied().sum::<f32>();
    if seed_total <= 0.0 {
        return BTreeMap::new();
    }

    let teleport = seed_scores
        .iter()
        .map(|(node_id, score)| (*node_id, *score / seed_total))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency = BTreeMap::<Uuid, Vec<(Uuid, f32)>>::new();
    for edge in candidate_edges {
        adjacency.entry(edge.from_node_id).or_default().push((edge.to_node_id, edge.walk_weight));
        adjacency.entry(edge.to_node_id).or_default().push((edge.from_node_id, edge.walk_weight));
    }

    let mut ranks = teleport.clone();
    for _ in 0..ASSOCIATIVE_GRAPH_RANK_ITERATIONS {
        let mut next = teleport
            .iter()
            .map(|(node_id, score)| (*node_id, score * (1.0 - ASSOCIATIVE_GRAPH_DAMPING)))
            .collect::<BTreeMap<_, _>>();
        let mut dangling_mass = 0.0;

        for (node_id, rank) in &ranks {
            let Some(neighbors) = adjacency.get(node_id) else {
                dangling_mass += *rank;
                continue;
            };
            let total_weight = neighbors.iter().map(|(_, weight)| *weight).sum::<f32>();
            if total_weight <= 0.0 {
                dangling_mass += *rank;
                continue;
            }
            for (neighbor_id, weight) in neighbors {
                let propagated = ASSOCIATIVE_GRAPH_DAMPING * *rank * (*weight / total_weight);
                *next.entry(*neighbor_id).or_default() += propagated;
            }
        }

        if dangling_mass > 0.0 {
            for (node_id, score) in &teleport {
                *next.entry(*node_id).or_default() +=
                    ASSOCIATIVE_GRAPH_DAMPING * dangling_mass * *score;
            }
        }
        ranks = next;
    }

    ranks
}

fn graph_edge_text_relevance(
    edge: &crate::infra::repositories::RuntimeGraphQueryEdgeRow,
    graph_index: &QueryGraphIndex,
    relevance_profile: &GraphQueryRelevanceProfile,
) -> f32 {
    if relevance_profile.keyword_tokens.is_empty() {
        return 0.0;
    }
    let Some(from_node) = graph_index.node(edge.from_node_id) else {
        return 0.0;
    };
    let Some(to_node) = graph_index.node(edge.to_node_id) else {
        return 0.0;
    };
    let mut edge_tokens = BTreeSet::new();
    for value in [
        edge.relation_type.as_str(),
        edge.summary.as_deref().unwrap_or_default(),
        from_node.label.as_str(),
        from_node.node_type.as_str(),
        from_node.summary.as_deref().unwrap_or_default(),
        to_node.label.as_str(),
        to_node.node_type.as_str(),
        to_node.summary.as_deref().unwrap_or_default(),
    ] {
        edge_tokens.extend(normalized_alnum_tokens(value, 3));
    }
    let overlap = near_token_overlap_count(&relevance_profile.keyword_tokens, &edge_tokens);
    (overlap.min(8) as f32) * 0.015
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
            if let Some(node) = graph_index.node(node_id) {
                if node.node_type.eq_ignore_ascii_case("document") {
                    continue;
                }
                entities.push(RuntimeMatchedEntity {
                    node_id,
                    label: node.label.clone(),
                    node_type: node.node_type.clone(),
                    summary: node.summary.clone(),
                    score: relationship.score.map(|score| score * 0.9),
                });
            }
        }
    }
    entities.sort_by(score_desc_entities);
    entities.truncate(top_k);
    entities
}

/// Returns `true` when an ArangoDB error indicates the target collection or
/// view does not exist (errorNum 1203). This is a valid no-data state for
/// libraries whose entity layer has not been extracted yet; callers should
/// treat it as an empty result rather than a fatal infrastructure failure.
fn is_arango_collection_not_found(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("1203")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind,
        associative_edges_for_entities, associative_seed_score_for_rank,
        entities_from_relationships, graph_relevance_profile, graph_target_entity_coverage_score,
        graph_target_entity_profiles, lexical_entity_hits, lexical_relationship_hits,
        merge_entity_retrieval_lane_slices, merge_entity_retrieval_lanes,
        merge_primary_then_expanded_entities, query_relevant_entity_hits, score_value,
    };
    use crate::{
        domains::query_ir::{
            ComparisonSpec, DocumentHint, EntityMention, EntityRole, QueryAct, QueryIR,
            QueryLanguage, QueryScope,
        },
        infra::repositories::{RuntimeGraphQueryEdgeRow, RuntimeGraphQueryNodeRow},
        services::{
            knowledge::runtime_read::ActiveRuntimeGraphProjection,
            query::{
                execution::{QueryGraphIndex, RuntimeMatchedEntity, RuntimeMatchedRelationship},
                planner::{RuntimeQueryPlan, build_query_plan},
            },
        },
    };

    fn graph_index_with_nodes(nodes: Vec<RuntimeGraphQueryNodeRow>) -> QueryGraphIndex {
        let positions =
            nodes.iter().enumerate().map(|(position, node)| (node.id, position)).collect();
        QueryGraphIndex::new(
            std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges: Vec::new() }),
            positions,
            Default::default(),
        )
    }

    fn graph_index_with_projection(
        nodes: Vec<RuntimeGraphQueryNodeRow>,
        edges: Vec<RuntimeGraphQueryEdgeRow>,
    ) -> QueryGraphIndex {
        let node_positions =
            nodes.iter().enumerate().rev().map(|(position, node)| (node.id, position)).collect();
        let edge_positions =
            edges.iter().enumerate().rev().map(|(position, edge)| (edge.id, position)).collect();
        QueryGraphIndex::new(
            std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges }),
            node_positions,
            edge_positions,
        )
    }

    fn node(label: &str, node_type: &str, summary: Option<&str>) -> RuntimeGraphQueryNodeRow {
        RuntimeGraphQueryNodeRow {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            canonical_key: format!("{node_type}:{}", label.to_lowercase()),
            label: label.to_string(),
            node_type: node_type.to_string(),
            aliases_json: json!([]),
            summary: summary.map(str::to_string),
            support_count: 1,
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn edge(
        from_node_id: Uuid,
        to_node_id: Uuid,
        relation_type: &str,
        summary: Option<&str>,
    ) -> RuntimeGraphQueryEdgeRow {
        RuntimeGraphQueryEdgeRow {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            from_node_id,
            to_node_id,
            relation_type: relation_type.to_string(),
            summary: summary.map(str::to_string),
            weight: None,
            support_count: 1,
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn matched_entity(label: &str) -> RuntimeMatchedEntity {
        RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: label.to_string(),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(1.0),
        }
    }

    fn inventory_ir(target_types: &[&str]) -> QueryIR {
        QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::LibraryMeta,
            language: QueryLanguage::Auto,
            target_types: target_types.iter().map(|value| (*value).to_string()).collect(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    fn wildcard_inventory_ir(target_label: &str) -> QueryIR {
        QueryIR {
            target_entities: vec![EntityMention {
                label: target_label.to_string(),
                role: EntityRole::Subject,
            }],
            ..inventory_ir(&[])
        }
    }

    fn configure_ir(target_label: &str, focus_hint: &str) -> QueryIR {
        QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["path".to_string(), "procedure".to_string()],
            target_entities: vec![EntityMention {
                label: target_label.to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: Some(DocumentHint { hint: focus_hint.to_string() }),
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    fn configure_ir_with_object_facets(focus_hint: &str, object_labels: &[&str]) -> QueryIR {
        QueryIR {
            target_entities: object_labels
                .iter()
                .map(|label| EntityMention {
                    label: (*label).to_string(),
                    role: EntityRole::Object,
                })
                .collect(),
            document_focus: Some(DocumentHint { hint: focus_hint.to_string() }),
            ..configure_ir(focus_hint, focus_hint)
        }
    }

    fn focused_compare_ir(
        focus_hint: &str,
        facet_labels: &[&str],
        second_operand: Option<&str>,
    ) -> QueryIR {
        QueryIR {
            act: QueryAct::Compare,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["document".to_string(), "concept".to_string()],
            target_entities: facet_labels
                .iter()
                .map(|label| EntityMention {
                    label: (*label).to_string(),
                    role: EntityRole::Subject,
                })
                .collect(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: Some(ComparisonSpec {
                a: Some("available variants".to_string()),
                b: second_operand.map(str::to_string),
                dimension: "facet coverage".to_string(),
            }),
            document_focus: Some(DocumentHint { hint: focus_hint.to_string() }),
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.86,
        }
    }

    fn describe_ir(target_label: &str) -> QueryIR {
        describe_ir_with_targets(&[target_label])
    }

    fn describe_ir_with_targets(target_labels: &[&str]) -> QueryIR {
        QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::LibraryMeta,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: target_labels
                .iter()
                .map(|label| EntityMention {
                    label: (*label).to_string(),
                    role: EntityRole::Subject,
                })
                .collect(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    #[test]
    fn lexical_entity_hits_match_node_types_from_query_ir() {
        let plan = build_query_plan("list graph inventory", None, Some(8), None);
        let ir = inventory_ir(&["event"]);
        let graph_index = graph_index_with_nodes(vec![
            node("[26.04.2026 22:36]", "event", Some("Timestamp marking a chat message")),
            node("setup guide", "artifact", None),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "[26.04.2026 22:36]");
        assert_eq!(hits[0].node_type, "event");
    }

    #[test]
    fn lexical_entity_hits_prioritize_wildcard_prefix_labels() {
        let plan = build_query_plan("list alpha-* modules", None, Some(8), None);
        let ir = wildcard_inventory_ir("alpha-*");
        let graph_index = graph_index_with_nodes(vec![
            node("Alpha Suite", "software_module", Some("Overview for alpha modules")),
            node("alpha-core", "software_module", None),
            node("alpha-desktop", "software_module", None),
            node("beta-core", "software_module", None),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let hits = query_relevant_entity_hits(&plan, Some(&ir), &profiles, &graph_index, 2);
        let labels = hits.iter().map(|hit| hit.label.as_str()).collect::<Vec<_>>();

        assert_eq!(labels, vec!["alpha-core", "alpha-desktop"]);
    }

    #[test]
    fn inventory_entity_hits_fall_back_to_supported_graph_nodes() {
        let plan = RuntimeQueryPlan {
            keywords: vec!["inventory".to_string()],
            entity_keywords: Vec::new(),
            ..build_query_plan("inventory", None, Some(8), None)
        };
        let ir = inventory_ir(&[]);
        let mut primary = node("Alpha Gateway", "software_module", None);
        primary.support_count = 9;
        let mut secondary = node("Beta Worker", "software_module", None);
        secondary.support_count = 4;
        let document = node("source.md", "document", None);
        let graph_index = graph_index_with_nodes(vec![secondary, document, primary]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let hits = query_relevant_entity_hits(&plan, Some(&ir), &profiles, &graph_index, 2);
        let labels = hits.iter().map(|hit| hit.label.as_str()).collect::<Vec<_>>();

        assert_eq!(labels, vec!["Alpha Gateway", "Beta Worker"]);
    }

    #[test]
    fn lexical_entity_hits_match_summary_and_node_type_not_only_label() {
        let plan = RuntimeQueryPlan {
            keywords: vec!["timestamp".to_string()],
            entity_keywords: Vec::new(),
            ..build_query_plan("timestamp inventory", None, Some(8), None)
        };
        let graph_index = graph_index_with_nodes(vec![node(
            "[2026-04-26 20:00]",
            "event",
            Some("Message timestamp extracted from a transcript"),
        )]);

        let hits = lexical_entity_hits(&plan, None, &[], &graph_index);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].node_type, "event");
    }

    #[test]
    fn lexical_entity_hits_use_query_ir_focus_terms() {
        let plan = build_query_plan("how configure connector?", None, Some(8), None);
        let ir = configure_ir("shared reports", "report archive path");
        let graph_index = graph_index_with_nodes(vec![
            node("/srv/reports/archive", "artifact", Some("Path to shared report archive")),
            node("/srv/cache", "artifact", Some("Runtime cache path")),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

        assert_eq!(hits[0].label, "/srv/reports/archive");
    }

    #[test]
    fn lexical_entity_hits_promote_clipped_target_prefix_match() {
        let plan = build_query_plan("how configure acmew?", None, Some(8), None);
        let ir = configure_ir("Acmew", "Acmew");
        let graph_index = graph_index_with_nodes(vec![
            node("Acmealpha payment processor", "artifact", Some("Acmealpha payment settings")),
            node("Beta payment processor", "artifact", Some("Beta payment settings")),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

        assert_eq!(hits[0].label, "Acmealpha payment processor");
        assert!(score_value(hits[0].score) > 100.0);
        assert!(
            hits.iter()
                .find(|hit| hit.label == "Beta payment processor")
                .is_none_or(|hit| score_value(hit.score) < score_value(hits[0].score))
        );
    }

    #[test]
    fn graph_target_profiles_learn_related_tokens_from_document_labels() {
        let ir = configure_ir("Acmew", "Acmew");
        let graph_index = graph_index_with_nodes(vec![
            node("Acmealpha setup guide", "document", Some("Configuration source")),
            node("Beta setup guide", "document", Some("Configuration source")),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let score = graph_target_entity_coverage_score(
            &[GraphTargetEntityCoverageField {
                text: "Acmealpha setup guide",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            }],
            &profiles,
        );

        assert!(score > 0);
    }

    #[test]
    fn graph_target_profiles_use_document_focus_instead_of_value_facets() {
        let ir = configure_ir_with_object_facets(
            "Acmew",
            &["installation", "configuration file", "parameters"],
        );
        let graph_index = graph_index_with_nodes(vec![
            node("Acmealpha setup guide", "document", Some("Configuration source")),
            node("Configuration file", "artifact", Some("Generic file reference")),
            node("Parameter catalog", "artifact", Some("Generic parameter reference")),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let focused_score = graph_target_entity_coverage_score(
            &[GraphTargetEntityCoverageField {
                text: "Acmealpha setup guide",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            }],
            &profiles,
        );
        let generic_score = graph_target_entity_coverage_score(
            &[GraphTargetEntityCoverageField {
                text: "Configuration file",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            }],
            &profiles,
        );

        assert!(focused_score > 0);
        assert_eq!(generic_score, 0);
    }

    #[test]
    fn graph_target_profiles_anchor_incomplete_focused_compare_to_document_focus() {
        let ir = focused_compare_ir(
            "Acmew",
            &["module options", "operation rules", "limit matrix"],
            None,
        );
        let graph_index = graph_index_with_nodes(vec![
            node("Acmealpha setup guide", "document", Some("Configuration source")),
            node("Module options", "artifact", Some("Generic module catalogue")),
            node("Operation rules", "artifact", Some("Generic operation catalogue")),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let focused_score = graph_target_entity_coverage_score(
            &[GraphTargetEntityCoverageField {
                text: "Acmealpha setup guide",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            }],
            &profiles,
        );
        let generic_score = graph_target_entity_coverage_score(
            &[GraphTargetEntityCoverageField {
                text: "Module options",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            }],
            &profiles,
        );

        assert!(focused_score > 0);
        assert_eq!(generic_score, 0);
    }

    #[test]
    fn graph_target_profiles_fallback_to_entities_when_focused_compare_document_is_absent() {
        let ir = focused_compare_ir("Missing setup source", &["Module options"], None);
        let graph_index = graph_index_with_nodes(vec![node(
            "Module options",
            "artifact",
            Some("Generic module catalogue"),
        )]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let entity_score = graph_target_entity_coverage_score(
            &[GraphTargetEntityCoverageField {
                text: "Module options",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            }],
            &profiles,
        );

        assert!(entity_score > 0);
    }

    #[test]
    fn graph_target_profiles_keep_complete_compare_operands() {
        let ir = focused_compare_ir("Acmew", &["Alpha Engine", "Beta Engine"], Some("Beta"));
        let graph_index = graph_index_with_nodes(vec![
            node("Acmealpha setup guide", "document", Some("Configuration source")),
            node("Alpha Engine", "artifact", Some("First compared option")),
            node("Beta Engine", "artifact", Some("Second compared option")),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let operand_score = graph_target_entity_coverage_score(
            &[GraphTargetEntityCoverageField {
                text: "Alpha Engine",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            }],
            &profiles,
        );

        assert!(operand_score > 0);
    }

    #[test]
    fn lexical_entity_hits_promote_explicit_target_and_rare_related_token() {
        let plan = build_query_plan("who is Alpha Omega?", None, Some(8), None);
        let ir = describe_ir("Alpha Omega");
        let graph_index = graph_index_with_nodes(vec![
            node("Alpha Omega", "person", None),
            node("Omega Delta", "person", None),
            node("Alpha Person", "person", None),
            node("Alpha Team", "person", None),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

        assert_eq!(hits[0].label, "Alpha Omega");
        let omega_index = hits.iter().position(|hit| hit.label == "Omega Delta").unwrap();
        let alpha_index = hits.iter().position(|hit| hit.label == "Alpha Person").unwrap();
        assert!(omega_index < alpha_index);
        assert!(score_value(hits[omega_index].score) > 100.0);
        assert!(score_value(hits[alpha_index].score) < 1.0);
    }

    #[test]
    fn lexical_entity_hits_promote_multi_target_nodes_above_single_anchor_nodes() {
        let plan = build_query_plan("find Beacon near Harbor Delta", None, Some(8), None);
        let ir = describe_ir_with_targets(&["Beacon", "Harbor Delta"]);
        let graph_index = graph_index_with_nodes(vec![
            node("Beacon", "artifact", None),
            node("Harbor Delta", "location", None),
            node("Harbor Delta archive", "artifact", None),
            node("Beacon moved through Harbor Delta", "event", None),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

        assert_eq!(hits[0].label, "Beacon moved through Harbor Delta");
        assert!(
            score_value(hits[0].score) > score_value(hits[1].score),
            "distinct target-entity coverage must outrank one-anchor matches"
        );
    }

    #[test]
    fn lexical_entity_hits_deduplicate_duplicate_target_entities() {
        let plan = build_query_plan("find Beacon", None, Some(8), None);
        let duplicate_ir = describe_ir_with_targets(&["Beacon", "Beacon"]);
        let single_ir = describe_ir("Beacon");
        let graph_index = graph_index_with_nodes(vec![node("Beacon", "artifact", None)]);

        let duplicate_profiles = graph_target_entity_profiles(Some(&duplicate_ir), &graph_index);
        let single_profiles = graph_target_entity_profiles(Some(&single_ir), &graph_index);
        let duplicate_hits =
            lexical_entity_hits(&plan, Some(&duplicate_ir), &duplicate_profiles, &graph_index);
        let single_hits =
            lexical_entity_hits(&plan, Some(&single_ir), &single_profiles, &graph_index);

        assert_eq!(score_value(duplicate_hits[0].score), score_value(single_hits[0].score));
    }

    #[test]
    fn lexical_entity_hits_do_not_promote_embedded_short_target_labels() {
        let plan = build_query_plan("who is Sasha Otoya?", None, Some(8), None);
        let ir = describe_ir("Sasha Otoya");
        let graph_index = graph_index_with_nodes(vec![
            node("OTO", "organization", None),
            node("Alex Otoya", "person", None),
            node("Sasha Otoya", "person", None),
        ]);

        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
        let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

        assert_eq!(hits[0].label, "Sasha Otoya");
        let embedded_index = hits.iter().position(|hit| hit.label == "OTO");
        let related_index = hits.iter().position(|hit| hit.label == "Alex Otoya").unwrap();
        assert!(
            embedded_index.is_none_or(|index| index > related_index),
            "embedded short label must not outrank token-overlap entity"
        );
    }

    #[test]
    fn lexical_entity_hits_do_not_return_document_nodes() {
        let plan = build_query_plan("list graph inventory", None, Some(8), None);
        let ir = inventory_ir(&["document"]);
        let graph_index = graph_index_with_nodes(vec![node("chat.txt", "document", None)]);
        let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);

        assert!(lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index).is_empty());
    }

    #[test]
    fn entity_retrieval_lane_merge_keeps_lexical_needle_under_vector_score_pressure() {
        let vector_one = node("Noisy Vector One", "concept", None);
        let vector_two = node("Noisy Vector Two", "concept", None);
        let needle = node("Needle Endpoint", "artifact", None);
        let vector_hits = vec![
            RuntimeMatchedEntity {
                node_id: vector_one.id,
                label: vector_one.label,
                node_type: vector_one.node_type,
                summary: None,
                score: Some(9_000.0),
            },
            RuntimeMatchedEntity {
                node_id: vector_two.id,
                label: vector_two.label,
                node_type: vector_two.node_type,
                summary: None,
                score: Some(8_000.0),
            },
        ];
        let lexical_hits = vec![RuntimeMatchedEntity {
            node_id: needle.id,
            label: needle.label,
            node_type: needle.node_type,
            summary: None,
            score: Some(0.24),
        }];

        let merged = merge_entity_retrieval_lanes(vector_hits, lexical_hits, 2);

        assert!(merged.iter().any(|entity| entity.node_id == needle.id));
    }

    #[test]
    fn entity_lane_slice_merge_preserves_narrow_local_ranking() {
        let cross_lane = matched_entity("Cross Lane Anchor");
        let vector_only = matched_entity("Vector Only Anchor");
        let lexical_only = matched_entity("Lexical Only Anchor");
        let vector_hits = vec![cross_lane.clone(), vector_only];
        let lexical_hits = vec![lexical_only.clone(), cross_lane.clone()];

        let sliced = merge_entity_retrieval_lane_slices(&vector_hits, &lexical_hits, 1);
        let broad = merge_entity_retrieval_lanes(vector_hits, lexical_hits, 2);

        assert_eq!(sliced[0].node_id, lexical_only.node_id);
        assert_eq!(broad[0].node_id, cross_lane.node_id);
    }

    #[test]
    fn entity_merge_keeps_query_anchors_before_expansion_scores() {
        let mut first_anchor = matched_entity("Prefix Alpha");
        first_anchor.score = Some(0.01);
        let mut second_anchor = matched_entity("Prefix Beta");
        second_anchor.score = Some(0.01);
        let mut expansion = matched_entity("Broad Expansion");
        expansion.score = Some(100.0);

        let merged = merge_primary_then_expanded_entities(
            vec![first_anchor, second_anchor],
            vec![expansion],
            2,
        );

        let labels = merged.iter().map(|entity| entity.label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels, vec!["Prefix Alpha", "Prefix Beta"]);
    }

    #[test]
    fn relationship_entities_skip_document_nodes() {
        let document = node("Reference Document", "document", None);
        let artifact = node("Canonical Artifact", "artifact", None);
        let relationship = RuntimeMatchedRelationship {
            edge_id: Uuid::now_v7(),
            relation_type: "mentions".to_string(),
            from_node_id: document.id,
            from_label: document.label.clone(),
            to_node_id: artifact.id,
            to_label: artifact.label.clone(),
            summary: None,
            support_count: 1,
            score: Some(10.0),
        };
        let graph_index = graph_index_with_projection(vec![document, artifact], Vec::new());

        let entities = entities_from_relationships(&[relationship], &graph_index, 4);

        let labels = entities.iter().map(|entity| entity.label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels, vec!["Canonical Artifact"]);
    }

    #[test]
    fn graph_index_iterators_follow_projection_order() {
        let first = node("first node", "process", None);
        let second = node("second node", "artifact", None);
        let first_edge = edge(first.id, second.id, "uses", Some("first edge"));
        let second_edge = edge(second.id, first.id, "mentions", Some("second edge"));
        let graph_index = graph_index_with_projection(
            vec![first.clone(), second.clone()],
            vec![first_edge.clone(), second_edge.clone()],
        );

        let node_labels = graph_index.nodes().map(|node| node.label.as_str()).collect::<Vec<_>>();
        let edge_summaries =
            graph_index.edges().filter_map(|edge| edge.summary.as_deref()).collect::<Vec<_>>();

        assert_eq!(node_labels, vec!["first node", "second node"]);
        assert_eq!(edge_summaries, vec!["first edge", "second edge"]);
    }

    #[test]
    fn lexical_relationship_hits_use_plan_keywords_only() {
        let source = node("source process", "process", None);
        let target = node("target artifact", "artifact", None);
        let keyword_edge = edge(source.id, target.id, "connects", Some("keyword edge"));
        let entity_keyword_edge =
            edge(source.id, target.id, "routes_to", Some("entity-keyword edge"));
        let graph_index = graph_index_with_projection(
            vec![source, target],
            vec![keyword_edge, entity_keyword_edge],
        );
        let plan = RuntimeQueryPlan {
            keywords: vec!["connects".to_string()],
            entity_keywords: vec!["routes".to_string()],
            ..build_query_plan("connect source", None, Some(8), None)
        };
        let ir = describe_ir("routes");
        let relevance_profile = graph_relevance_profile(&plan, Some(&ir));

        let hits = lexical_relationship_hits(&relevance_profile, &graph_index);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].relation_type, "connects");
    }

    #[test]
    fn associative_edges_rank_edge_text_relevance_before_stable_ties() {
        let source = node("source process", "process", None);
        let ordinary_target = node("ordinary artifact", "artifact", None);
        let needle_target = node("needle artifact", "artifact", None);
        let ordinary_edge =
            edge(source.id, ordinary_target.id, "produces", Some("ordinary output"));
        let needle_edge = edge(source.id, needle_target.id, "produces", Some("needle output"));
        let graph_index = graph_index_with_projection(
            vec![source.clone(), ordinary_target, needle_target],
            vec![ordinary_edge, needle_edge],
        );
        let plan = RuntimeQueryPlan {
            keywords: vec!["needle".to_string()],
            entity_keywords: Vec::new(),
            ..build_query_plan("needle", None, Some(8), None)
        };
        let entities = vec![RuntimeMatchedEntity {
            node_id: source.id,
            label: source.label,
            node_type: source.node_type,
            summary: None,
            score: Some(0.3),
        }];

        let hits = associative_edges_for_entities(&entities, &graph_index, &plan, None, 2);

        assert_eq!(hits[0].to_label, "needle artifact");
        assert!(score_value(hits[0].score) > score_value(hits[1].score));
    }

    #[test]
    fn associative_edges_preserve_fused_entity_rank_as_seed_strength() {
        let high_seed = node("high ranked seed", "process", None);
        let low_seed = node("low ranked seed", "process", None);
        let high_target = node("high target", "artifact", None);
        let low_target = node("low target", "artifact", None);
        let high_edge = edge(high_seed.id, high_target.id, "links", Some("neutral support"));
        let mut low_edge = edge(low_seed.id, low_target.id, "links", Some("neutral support"));
        low_edge.support_count = 20;
        let graph_index = graph_index_with_projection(
            vec![high_seed.clone(), low_seed.clone(), high_target, low_target],
            vec![high_edge, low_edge],
        );
        let plan = RuntimeQueryPlan {
            keywords: Vec::new(),
            entity_keywords: Vec::new(),
            high_level_keywords: Vec::new(),
            low_level_keywords: Vec::new(),
            concept_keywords: Vec::new(),
            ..build_query_plan("ranked seed expansion", None, Some(8), None)
        };
        let entities = vec![
            RuntimeMatchedEntity {
                node_id: high_seed.id,
                label: high_seed.label,
                node_type: high_seed.node_type,
                summary: None,
                score: Some(0.016_393),
            },
            RuntimeMatchedEntity {
                node_id: low_seed.id,
                label: low_seed.label,
                node_type: low_seed.node_type,
                summary: None,
                score: Some(0.016_129),
            },
        ];

        let hits = associative_edges_for_entities(&entities, &graph_index, &plan, None, 2);

        assert_eq!(hits[0].to_label, "high target");
    }

    #[test]
    fn associative_seed_score_is_rank_monotonic_and_bounded() {
        let first = associative_seed_score_for_rank(0);
        let second = associative_seed_score_for_rank(1);
        let distant = associative_seed_score_for_rank(99);

        assert!(first <= 2.0);
        assert!(first > second);
        assert!(second > distant);
        assert!(distant > 1.0);
    }

    #[test]
    fn associative_edges_promote_two_hop_bridge_over_one_hop_noise() {
        let source = node("Alpha Relay", "process", None);
        let bridge = node("Bridge Junction", "artifact", None);
        let endpoint = node("Gamma Endpoint", "artifact", None);
        let noise = node("Ordinary Artifact", "artifact", None);
        let noise_edge = edge(source.id, noise.id, "mentions", Some("ordinary output"));
        let bridge_edge = edge(source.id, bridge.id, "connects", Some("Alpha Relay bridge"));
        let endpoint_edge =
            edge(bridge.id, endpoint.id, "routes_to", Some("Bridge reaches Gamma Endpoint"));
        let graph_index = graph_index_with_projection(
            vec![source.clone(), bridge, endpoint, noise],
            vec![noise_edge, bridge_edge, endpoint_edge],
        );
        let plan = RuntimeQueryPlan {
            keywords: vec!["gamma".to_string(), "endpoint".to_string()],
            entity_keywords: Vec::new(),
            ..build_query_plan("which route reaches Gamma Endpoint?", None, Some(8), None)
        };
        let entities = vec![RuntimeMatchedEntity {
            node_id: source.id,
            label: source.label,
            node_type: source.node_type,
            summary: None,
            score: Some(0.3),
        }];

        let hits = associative_edges_for_entities(&entities, &graph_index, &plan, None, 3);

        assert_eq!(hits[0].to_label, "Gamma Endpoint");
        assert!(hits.iter().any(|hit| hit.to_label == "Ordinary Artifact"));
    }

    #[test]
    fn associative_edges_ignore_document_seed_noise() {
        let router = node("Router Hub", "artifact", None);
        let needle_artifact = node("Needle Artifact", "artifact", None);
        let ordinary_artifact = node("Ordinary Artifact", "artifact", None);
        let noise_artifact = node("Noise Artifact", "artifact", None);
        let source_document = node("random topology snapshot", "document", None);
        let noisy_document = node("reference guide", "document", None);
        let noise_edge = edge(
            source_document.id,
            noise_artifact.id,
            "mentions",
            Some("Document mentions extracted entity"),
        );
        let noisy_edge = edge(
            noisy_document.id,
            needle_artifact.id,
            "mentions",
            Some("Document mentions extracted entity"),
        );
        let guarded_route_edge = edge(
            router.id,
            needle_artifact.id,
            "selects",
            Some("Router Hub selects Needle Artifact through the guarded needle route"),
        );
        let ordinary_edge = edge(
            router.id,
            ordinary_artifact.id,
            "mentions",
            Some("Router Hub mentions Ordinary Artifact through the ordinary noise route"),
        );
        let graph_index = graph_index_with_projection(
            vec![
                router.clone(),
                needle_artifact.clone(),
                ordinary_artifact.clone(),
                noise_artifact.clone(),
                source_document.clone(),
                noisy_document.clone(),
            ],
            vec![noise_edge, noisy_edge, guarded_route_edge, ordinary_edge],
        );
        let plan = RuntimeQueryPlan {
            keywords: vec![
                "Router".into(),
                "Hub".into(),
                "guarded".into(),
                "needle".into(),
                "route".into(),
                "Ordinary".into(),
                "Artifact".into(),
            ],
            entity_keywords: Vec::new(),
            ..build_query_plan("Which route does Router Hub select?", None, Some(8), None)
        };
        let entities = vec![
            RuntimeMatchedEntity {
                node_id: router.id,
                label: router.label,
                node_type: router.node_type,
                summary: None,
                score: Some(0.8),
            },
            RuntimeMatchedEntity {
                node_id: source_document.id,
                label: source_document.label,
                node_type: source_document.node_type,
                summary: None,
                score: Some(12.0),
            },
            RuntimeMatchedEntity {
                node_id: noisy_document.id,
                label: noisy_document.label,
                node_type: noisy_document.node_type,
                summary: None,
                score: Some(11.0),
            },
        ];

        let hits = associative_edges_for_entities(&entities, &graph_index, &plan, None, 3);

        assert!(hits.iter().any(|hit| hit.to_label == "Needle Artifact"));
        assert!(hits.iter().any(|hit| hit.to_label == "Ordinary Artifact"));
        assert!(hits.iter().any(|hit| {
            hit.summary.as_deref().is_some_and(|summary| summary.contains("guarded needle route"))
        }));
    }

    #[test]
    fn associative_edges_return_empty_for_empty_seed_or_limit() {
        let source = node("Alpha Relay", "process", None);
        let target = node("Gamma Endpoint", "artifact", None);
        let edge = edge(source.id, target.id, "routes_to", Some("route"));
        let graph_index = graph_index_with_projection(vec![source.clone(), target], vec![edge]);
        let plan = build_query_plan("Gamma Endpoint", None, Some(8), None);
        let entities = vec![RuntimeMatchedEntity {
            node_id: source.id,
            label: source.label,
            node_type: source.node_type,
            summary: None,
            score: Some(0.3),
        }];

        assert!(associative_edges_for_entities(&[], &graph_index, &plan, None, 3).is_empty());
        assert!(associative_edges_for_entities(&entities, &graph_index, &plan, None, 0).is_empty());
    }
}
