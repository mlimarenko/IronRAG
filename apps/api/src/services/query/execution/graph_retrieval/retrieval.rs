use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
};

use uuid::Uuid;

use crate::{
    app::state::AppState, domains::query_ir::QueryIR, services::query::planner::RuntimeQueryPlan,
};

use super::super::{
    QueryGraphIndex, RetrievalBundle, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    associative_graph_retrieval::associative_edges_for_entities_with_relevance, score_value,
    types::RuntimeVectorSearchContext, vector_retrieval::retrieve_entity_vector_hits,
};
use super::{
    relevance::{
        GraphQueryRelevanceProfile, graph_relevance_profile, lexical_relationship_hits,
        query_relevant_entity_hits_with_relevance,
    },
    target_profiles::GraphTargetEntityProfile,
};

struct EntityRetrievalLanes {
    vector_hits: Vec<RuntimeMatchedEntity>,
    lexical_hits: Vec<RuntimeMatchedEntity>,
}

/// Read-only inputs shared by every graph bundle retrieval entry point.
#[derive(Clone, Copy)]
pub(crate) struct GraphRetrievalRequest<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) library_id: Uuid,
    pub(crate) plan: &'a RuntimeQueryPlan,
    pub(crate) query_ir: Option<&'a QueryIR>,
    pub(crate) target_entity_profiles: &'a [GraphTargetEntityProfile],
    pub(crate) limit: usize,
    pub(crate) question_embedding: &'a [f32],
    pub(crate) vector_search_context: Option<&'a RuntimeVectorSearchContext>,
    pub(crate) graph_index: &'a QueryGraphIndex,
}

/// Inputs for the per-lane entity retrieval helper.
struct EntityLaneRequest<'a> {
    state: &'a AppState,
    library_id: Uuid,
    relevance_profile: &'a GraphQueryRelevanceProfile,
    target_entity_profiles: &'a [GraphTargetEntityProfile],
    limit: usize,
    question_embedding: &'a [f32],
    vector_search_context: Option<&'a RuntimeVectorSearchContext>,
    graph_index: &'a QueryGraphIndex,
}

async fn retrieve_entity_hit_lanes_with_relevance(
    request: EntityLaneRequest<'_>,
) -> anyhow::Result<EntityRetrievalLanes> {
    let vector_hits = retrieve_entity_vector_hits(
        request.state,
        request.library_id,
        request.limit,
        request.question_embedding,
        request.vector_search_context,
        request.graph_index,
    )
    .await?;

    let lexical_hits = query_relevant_entity_hits_with_relevance(
        request.relevance_profile,
        request.target_entity_profiles,
        request.graph_index,
        request.limit,
    );
    Ok(EntityRetrievalLanes { vector_hits, lexical_hits })
}

pub(crate) async fn retrieve_relationship_hits(
    request: GraphRetrievalRequest<'_>,
) -> anyhow::Result<Vec<RuntimeMatchedRelationship>> {
    let entity_seed_limit = request.limit.saturating_mul(2).max(8);
    let relevance_profile = graph_relevance_profile(request.plan, request.query_ir);
    let lanes = retrieve_entity_hit_lanes_with_relevance(EntityLaneRequest {
        state: request.state,
        library_id: request.library_id,
        relevance_profile: &relevance_profile,
        target_entity_profiles: request.target_entity_profiles,
        limit: entity_seed_limit,
        question_embedding: request.question_embedding,
        vector_search_context: request.vector_search_context,
        graph_index: request.graph_index,
    })
    .await?;
    let entity_hits =
        merge_entity_retrieval_lanes(lanes.vector_hits, lanes.lexical_hits, entity_seed_limit);
    let topology_hits = associative_edges_for_entities_with_relevance(
        &entity_hits,
        request.graph_index,
        &relevance_profile,
        entity_seed_limit.saturating_mul(2),
    );
    let lexical_hits = lexical_relationship_hits(&relevance_profile, request.graph_index);
    Ok(merge_relationships(topology_hits, lexical_hits, request.limit))
}

pub(crate) async fn retrieve_local_bundle(
    request: GraphRetrievalRequest<'_>,
) -> anyhow::Result<RetrievalBundle> {
    let relevance_profile = graph_relevance_profile(request.plan, request.query_ir);
    let lanes = retrieve_entity_hit_lanes_with_relevance(EntityLaneRequest {
        state: request.state,
        library_id: request.library_id,
        relevance_profile: &relevance_profile,
        target_entity_profiles: request.target_entity_profiles,
        limit: request.limit,
        question_embedding: request.question_embedding,
        vector_search_context: request.vector_search_context,
        graph_index: request.graph_index,
    })
    .await?;
    let entity_hits =
        merge_entity_retrieval_lanes(lanes.vector_hits, lanes.lexical_hits, request.limit);
    let relationships = associative_edges_for_entities_with_relevance(
        &entity_hits,
        request.graph_index,
        &relevance_profile,
        request.limit,
    );
    Ok(RetrievalBundle { entities: entity_hits, relationships, chunks: Vec::new() })
}

pub(crate) async fn retrieve_global_bundle(
    request: GraphRetrievalRequest<'_>,
) -> anyhow::Result<RetrievalBundle> {
    let limit = request.limit;
    let graph_index = request.graph_index;
    let relationships = retrieve_relationship_hits(request).await?;
    let entities = entities_from_relationships(&relationships, graph_index, limit);
    Ok(RetrievalBundle { entities, relationships, chunks: Vec::new() })
}

pub(crate) async fn retrieve_mixed_graph_bundle(
    request: GraphRetrievalRequest<'_>,
) -> anyhow::Result<RetrievalBundle> {
    let started = std::time::Instant::now();
    if request.limit == 0 {
        return Ok(RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: Vec::new(),
        });
    }

    let entity_seed_limit = request.limit.saturating_mul(2).max(8);
    let relevance_profile = graph_relevance_profile(request.plan, request.query_ir);
    let lanes = retrieve_entity_hit_lanes_with_relevance(EntityLaneRequest {
        state: request.state,
        library_id: request.library_id,
        relevance_profile: &relevance_profile,
        target_entity_profiles: request.target_entity_profiles,
        limit: entity_seed_limit,
        question_embedding: request.question_embedding,
        vector_search_context: request.vector_search_context,
        graph_index: request.graph_index,
    })
    .await?;

    let local_entities =
        merge_entity_retrieval_lane_slices(&lanes.vector_hits, &lanes.lexical_hits, request.limit);
    let entity_seed_hits =
        merge_entity_retrieval_lanes(lanes.vector_hits, lanes.lexical_hits, entity_seed_limit);
    let local_relationships = associative_edges_for_entities_with_relevance(
        &local_entities,
        request.graph_index,
        &relevance_profile,
        request.limit,
    );

    let global_topology_hits = associative_edges_for_entities_with_relevance(
        &entity_seed_hits,
        request.graph_index,
        &relevance_profile,
        entity_seed_limit.saturating_mul(2),
    );
    let global_relationships = merge_relationships(
        global_topology_hits,
        lexical_relationship_hits(&relevance_profile, request.graph_index),
        request.limit,
    );
    let global_entities =
        entities_from_relationships(&global_relationships, request.graph_index, request.limit);

    let entities =
        merge_primary_then_expanded_entities(local_entities, global_entities, request.limit);
    let relationships =
        merge_relationships(local_relationships, global_relationships, request.limit);
    tracing::info!(
        stage = "retrieval.mix_graph",
        library_id = %request.library_id,
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

pub(super) fn merge_entity_retrieval_lanes(
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

pub(super) fn merge_entity_retrieval_lane_slices(
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
