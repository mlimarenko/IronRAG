use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use crate::{
    infra::repositories::RuntimeGraphQueryEdgeRow,
    services::query::text_match::{near_token_overlap_count, normalized_alnum_tokens},
};

use super::{
    QueryGraphIndex, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    graph_retrieval::{GraphQueryRelevanceProfile, map_edge_hit, score_desc_relationships},
};

const ASSOCIATIVE_GRAPH_EXPANSION_HOPS: usize = 2;
const ASSOCIATIVE_GRAPH_MAX_CANDIDATE_EDGES: usize = 512;
const ASSOCIATIVE_GRAPH_MAX_FRONTIER_NODES: usize = 128;
const ASSOCIATIVE_GRAPH_MAX_EDGES_PER_FRONTIER_NODE: usize = 64;
const ASSOCIATIVE_GRAPH_RANK_ITERATIONS: usize = 8;
const ASSOCIATIVE_GRAPH_DAMPING: f32 = 0.85;
const ASSOCIATIVE_EDGE_SUPPORT_WEIGHT: f32 = 0.015;
const ASSOCIATIVE_EDGE_TEXT_RELEVANCE_WEIGHT: f32 = 16.0;

pub(super) fn associative_edges_for_entities_with_relevance(
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

#[cfg(test)]
pub(super) fn associative_seed_score_for_rank(rank: usize) -> f32 {
    1.0 + (1.0 / (rank as f32 + 1.0))
}

#[cfg(not(test))]
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
        if should_stop_associative_expansion(&frontier, selected_edges.len(), max_candidate_edges) {
            break;
        }

        let depth_edges = associative_frontier_edges(
            &frontier,
            &selected_edge_ids,
            &known_node_ids,
            seed_scores,
            graph_index,
            relevance_profile,
        );
        let remaining = max_candidate_edges.saturating_sub(selected_edges.len());
        frontier = select_associative_depth_edges(
            depth_edges,
            remaining,
            &mut selected_edges,
            &mut selected_edge_ids,
            &mut known_node_ids,
            graph_index,
        );
    }

    selected_edges
}

fn should_stop_associative_expansion(
    frontier: &BTreeSet<Uuid>,
    selected_edge_count: usize,
    max_candidate_edges: usize,
) -> bool {
    frontier.is_empty() || selected_edge_count >= max_candidate_edges
}

fn associative_frontier_edges(
    frontier: &BTreeSet<Uuid>,
    selected_edge_ids: &BTreeSet<Uuid>,
    known_node_ids: &BTreeSet<Uuid>,
    seed_scores: &BTreeMap<Uuid, f32>,
    graph_index: &QueryGraphIndex,
    relevance_profile: &GraphQueryRelevanceProfile,
) -> Vec<AssociativeCandidateEdge> {
    let mut depth_edge_ids = BTreeSet::new();
    let mut depth_edges = frontier
        .iter()
        .take(ASSOCIATIVE_GRAPH_MAX_FRONTIER_NODES)
        .flat_map(|node_id| {
            associative_incident_edges(
                *node_id,
                selected_edge_ids,
                &mut depth_edge_ids,
                known_node_ids,
                seed_scores,
                graph_index,
                relevance_profile,
            )
        })
        .collect::<Vec<_>>();
    sort_associative_candidate_edges(&mut depth_edges);
    depth_edges
}

fn associative_incident_edges(
    node_id: Uuid,
    selected_edge_ids: &BTreeSet<Uuid>,
    depth_edge_ids: &mut BTreeSet<Uuid>,
    known_node_ids: &BTreeSet<Uuid>,
    seed_scores: &BTreeMap<Uuid, f32>,
    graph_index: &QueryGraphIndex,
    relevance_profile: &GraphQueryRelevanceProfile,
) -> Vec<AssociativeCandidateEdge> {
    let mut incident_edges = graph_index
        .incident_edges(node_id)
        .filter(|edge| !selected_edge_ids.contains(&edge.id))
        .filter(|edge| depth_edge_ids.insert(edge.id))
        .filter_map(|edge| {
            associative_candidate_edge(
                edge,
                graph_index,
                relevance_profile,
                seed_scores,
                known_node_ids,
            )
        })
        .collect::<Vec<_>>();
    sort_associative_candidate_edges(&mut incident_edges);
    incident_edges.truncate(ASSOCIATIVE_GRAPH_MAX_EDGES_PER_FRONTIER_NODE);
    incident_edges
}

fn sort_associative_candidate_edges(edges: &mut [AssociativeCandidateEdge]) {
    edges.sort_by(|left, right| {
        right.pre_score.total_cmp(&left.pre_score).then_with(|| left.edge_id.cmp(&right.edge_id))
    });
}

fn select_associative_depth_edges(
    depth_edges: Vec<AssociativeCandidateEdge>,
    remaining: usize,
    selected_edges: &mut Vec<AssociativeCandidateEdge>,
    selected_edge_ids: &mut BTreeSet<Uuid>,
    known_node_ids: &mut BTreeSet<Uuid>,
    graph_index: &QueryGraphIndex,
) -> BTreeSet<Uuid> {
    let mut next_frontier = BTreeSet::new();
    for edge in depth_edges.into_iter().take(remaining) {
        selected_edge_ids.insert(edge.edge_id);
        add_unknown_associative_endpoints(&edge, known_node_ids, &mut next_frontier, graph_index);
        selected_edges.push(edge);
    }
    next_frontier
}

fn add_unknown_associative_endpoints(
    edge: &AssociativeCandidateEdge,
    known_node_ids: &mut BTreeSet<Uuid>,
    next_frontier: &mut BTreeSet<Uuid>,
    graph_index: &QueryGraphIndex,
) {
    for node_id in [edge.from_node_id, edge.to_node_id] {
        if !is_document_node(graph_index, &node_id) && known_node_ids.insert(node_id) {
            next_frontier.insert(node_id);
        }
    }
}

fn associative_candidate_edge(
    edge: &RuntimeGraphQueryEdgeRow,
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
    let Some(teleport) = associative_teleport_distribution(seed_scores) else {
        return BTreeMap::new();
    };
    let adjacency = associative_adjacency(candidate_edges);
    let mut ranks = teleport.clone();

    for _ in 0..ASSOCIATIVE_GRAPH_RANK_ITERATIONS {
        ranks = propagate_associative_ranks(&ranks, &teleport, &adjacency);
    }

    ranks
}

fn associative_teleport_distribution(
    seed_scores: &BTreeMap<Uuid, f32>,
) -> Option<BTreeMap<Uuid, f32>> {
    let seed_total = seed_scores.values().copied().sum::<f32>();
    (seed_total > 0.0).then(|| {
        seed_scores.iter().map(|(node_id, score)| (*node_id, *score / seed_total)).collect()
    })
}

fn associative_adjacency(
    candidate_edges: &[AssociativeCandidateEdge],
) -> BTreeMap<Uuid, Vec<(Uuid, f32)>> {
    let mut adjacency = BTreeMap::<Uuid, Vec<(Uuid, f32)>>::new();
    for edge in candidate_edges {
        adjacency.entry(edge.from_node_id).or_default().push((edge.to_node_id, edge.walk_weight));
        adjacency.entry(edge.to_node_id).or_default().push((edge.from_node_id, edge.walk_weight));
    }
    adjacency
}

fn propagate_associative_ranks(
    ranks: &BTreeMap<Uuid, f32>,
    teleport: &BTreeMap<Uuid, f32>,
    adjacency: &BTreeMap<Uuid, Vec<(Uuid, f32)>>,
) -> BTreeMap<Uuid, f32> {
    let mut next = associative_teleport_base(teleport);
    let dangling_mass = distribute_associative_rank(ranks, adjacency, &mut next);
    distribute_associative_dangling_mass(dangling_mass, teleport, &mut next);
    next
}

fn associative_teleport_base(teleport: &BTreeMap<Uuid, f32>) -> BTreeMap<Uuid, f32> {
    teleport
        .iter()
        .map(|(node_id, score)| (*node_id, score * (1.0 - ASSOCIATIVE_GRAPH_DAMPING)))
        .collect()
}

fn distribute_associative_rank(
    ranks: &BTreeMap<Uuid, f32>,
    adjacency: &BTreeMap<Uuid, Vec<(Uuid, f32)>>,
    next: &mut BTreeMap<Uuid, f32>,
) -> f32 {
    let mut dangling_mass = 0.0;
    for (node_id, rank) in ranks {
        let Some(neighbors) = adjacency.get(node_id) else {
            dangling_mass += *rank;
            continue;
        };
        let total_weight = neighbors.iter().map(|(_, weight)| *weight).sum::<f32>();
        if total_weight <= 0.0 {
            dangling_mass += *rank;
            continue;
        }
        distribute_associative_rank_to_neighbors(*rank, neighbors, total_weight, next);
    }
    dangling_mass
}

fn distribute_associative_rank_to_neighbors(
    rank: f32,
    neighbors: &[(Uuid, f32)],
    total_weight: f32,
    next: &mut BTreeMap<Uuid, f32>,
) {
    for (neighbor_id, weight) in neighbors {
        let propagated = ASSOCIATIVE_GRAPH_DAMPING * rank * (*weight / total_weight);
        *next.entry(*neighbor_id).or_default() += propagated;
    }
}

fn distribute_associative_dangling_mass(
    dangling_mass: f32,
    teleport: &BTreeMap<Uuid, f32>,
    next: &mut BTreeMap<Uuid, f32>,
) {
    if dangling_mass <= 0.0 {
        return;
    }
    for (node_id, score) in teleport {
        *next.entry(*node_id).or_default() += ASSOCIATIVE_GRAPH_DAMPING * dangling_mass * *score;
    }
}

fn graph_edge_text_relevance(
    edge: &RuntimeGraphQueryEdgeRow,
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
