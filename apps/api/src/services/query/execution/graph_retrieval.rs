use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        provider_profiles::EffectiveProviderProfile,
        query_ir::{QueryAct, QueryIR, QueryScope},
    },
    services::query::{
        planner::RuntimeQueryPlan,
        text_match::{
            RelatedTokenSelection, near_token_overlap_count, normalized_alnum_tokens,
            select_related_overlap_tokens, token_sequence_exact_or_contains,
        },
    },
};

use super::{
    QueryGraphIndex, RetrievalBundle, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    resolve_runtime_vector_search_context, score_value,
};

pub(crate) async fn retrieve_entity_hits(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<Vec<RuntimeMatchedEntity>> {
    let vector_hits = if question_embedding.is_empty() {
        Vec::new()
    } else if let Some(context) =
        resolve_runtime_vector_search_context(state, library_id, provider_profile).await?
    {
        state
            .arango_search_store
            .search_entity_vectors_by_similarity(
                library_id,
                &context.model_catalog_id.to_string(),
                question_embedding,
                limit.max(1),
                None,
            )
            .await
            .context("failed to search canonical entity vectors for runtime query")?
            .into_iter()
            .filter_map(|hit| {
                graph_index.node(hit.entity_id).map(|node| RuntimeMatchedEntity {
                    node_id: node.id,
                    label: node.label.clone(),
                    node_type: node.node_type.clone(),
                    score: Some(hit.score as f32),
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let lexical_hits = query_relevant_entity_hits(plan, query_ir, graph_index, limit);
    Ok(merge_entity_retrieval_lanes(vector_hits, lexical_hits, limit))
}

pub(crate) async fn retrieve_relationship_hits(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
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
        query_ir,
        entity_seed_limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let topology_hits = related_edges_for_entities(
        &entity_hits,
        graph_index,
        plan,
        query_ir,
        entity_seed_limit.saturating_mul(2),
    );
    let lexical_hits = lexical_relationship_hits(plan, graph_index);
    Ok(merge_relationships(topology_hits, lexical_hits, limit))
}

pub(crate) async fn retrieve_local_bundle(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    limit: usize,
    question_embedding: &[f32],
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<RetrievalBundle> {
    let entity_hits = retrieve_entity_hits(
        state,
        library_id,
        provider_profile,
        plan,
        query_ir,
        limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let relationships =
        related_edges_for_entities(&entity_hits, graph_index, plan, query_ir, limit);
    Ok(RetrievalBundle { entities: entity_hits, relationships, chunks: Vec::new() })
}

pub(crate) async fn retrieve_global_bundle(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
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
        limit,
        question_embedding,
        graph_index,
    )
    .await?;
    let entities = entities_from_relationships(&relationships, graph_index, limit);
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

fn lexical_entity_hits(
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    graph_index: &QueryGraphIndex,
) -> Vec<RuntimeMatchedEntity> {
    let search_keywords = graph_relevance_keywords(plan, query_ir);
    let target_types = graph_target_types(query_ir);
    let target_entity_profiles = graph_target_entity_profiles(query_ir, graph_index);
    let mut hits = graph_index
        .nodes()
        .filter(|node| node.node_type != "document")
        .filter_map(|node| {
            graph_node_relevance(node, &search_keywords, &target_types, &target_entity_profiles)
        })
        .map(|relevance| RuntimeMatchedEntity {
            node_id: relevance.node.id,
            label: relevance.node.label.clone(),
            node_type: relevance.node.node_type.clone(),
            score: Some(relevance.score),
        })
        .collect::<Vec<_>>();
    hits.sort_by(score_desc_entities);
    hits
}

pub(crate) fn query_relevant_entity_hits(
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    graph_index: &QueryGraphIndex,
    limit: usize,
) -> Vec<RuntimeMatchedEntity> {
    let mut hits = lexical_entity_hits(plan, query_ir, graph_index);
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
        for mention in &ir.target_entities {
            push(&mention.label);
        }
    }
    keywords
}

struct GraphNodeRelevance<'a> {
    node: &'a crate::infra::repositories::RuntimeGraphNodeRow,
    score: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphTargetEntityProfile {
    profile_key: String,
    target_label: String,
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
    ir.target_entities
        .iter()
        .filter_map(|mention| {
            let label = mention.label.trim();
            if label.is_empty() {
                return None;
            }
            let target_tokens = normalized_alnum_tokens(label, 3);
            if target_tokens.is_empty() {
                return None;
            }
            let profile_key = target_tokens.iter().cloned().collect::<Vec<_>>().join("\u{0}");
            if !seen.insert(profile_key.clone()) {
                return None;
            }
            let related_tokens = select_related_overlap_tokens(
                label,
                graph_index
                    .nodes()
                    .filter(|node| node.node_type != "document")
                    .map(|node| node.label.as_str()),
                3,
            );
            Some(GraphTargetEntityProfile {
                profile_key,
                target_label: label.to_string(),
                related_tokens,
            })
        })
        .collect()
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
    fields
        .iter()
        .filter_map(|field| {
            let field_text = field.text.trim();
            if field_text.is_empty() {
                return None;
            }
            if token_sequence_exact_or_contains(field_text, &profile.target_label, 3) {
                return Some(graph_target_entity_exact_field_score(field.kind));
            }
            let field_tokens = normalized_alnum_tokens(field_text, 3);
            if !profile.related_tokens.is_empty()
                && profile.related_tokens.matches_tokens(&field_tokens)
            {
                return Some(graph_target_entity_related_field_score(field.kind));
            }
            None
        })
        .max()
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

fn graph_node_relevance<'a>(
    node: &'a crate::infra::repositories::RuntimeGraphNodeRow,
    keywords: &[String],
    target_types: &BTreeSet<String>,
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

    let exact_match = keywords.iter().map(|keyword| keyword.to_lowercase()).any(|keyword| {
        label.contains(&keyword)
            || summary.contains(&keyword)
            || node_type.contains(&keyword)
            || aliases.iter().any(|alias| alias.contains(&keyword))
    });
    let keyword_tokens = keywords.iter().cloned().collect::<BTreeSet<_>>();
    let summary_tokens = normalized_alnum_tokens(&summary, 3);
    let node_type_tokens = normalized_alnum_tokens(&node_type, 3);
    let alias_tokens =
        aliases.iter().flat_map(|alias| normalized_alnum_tokens(alias, 3)).collect::<BTreeSet<_>>();
    let token_overlap = near_token_overlap_count(&keyword_tokens, &label_tokens)
        + near_token_overlap_count(&keyword_tokens, &summary_tokens)
        + near_token_overlap_count(&keyword_tokens, &node_type_tokens)
        + near_token_overlap_count(&keyword_tokens, &alias_tokens);

    if exact_match || token_overlap > 0 {
        let score = 0.22 + (token_overlap.min(8) as f32 * 0.02);
        return Some(GraphNodeRelevance { node, score });
    }

    if target_types.contains(&node_type) {
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
    plan: &RuntimeQueryPlan,
    graph_index: &QueryGraphIndex,
) -> Vec<RuntimeMatchedRelationship> {
    let mut hits = graph_index
        .edges()
        .filter(|edge| {
            plan.keywords
                .iter()
                .any(|keyword| edge.relation_type.to_ascii_lowercase().contains(keyword))
        })
        .filter_map(|edge| map_edge_hit(edge.id, Some(0.2), graph_index))
        .collect::<Vec<_>>();
    hits.sort_by(score_desc_relationships);
    hits
}

pub(crate) fn related_edges_for_entities(
    entities: &[RuntimeMatchedEntity],
    graph_index: &QueryGraphIndex,
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    top_k: usize,
) -> Vec<RuntimeMatchedRelationship> {
    let entity_ids = entities.iter().map(|entity| entity.node_id).collect::<BTreeSet<_>>();
    let entity_scores = entities
        .iter()
        .map(|entity| (entity.node_id, score_value(entity.score)))
        .collect::<HashMap<_, _>>();
    let search_keywords = graph_relevance_keywords(plan, query_ir);
    let mut relationships = graph_index
        .edges()
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
            let relevance =
                relevance + graph_edge_text_relevance(edge, graph_index, &search_keywords);
            map_edge_hit(edge.id, Some(relevance), graph_index)
        })
        .collect::<Vec<_>>();
    relationships.sort_by(score_desc_relationships);
    relationships.truncate(top_k);
    relationships
}

fn graph_edge_text_relevance(
    edge: &crate::infra::repositories::RuntimeGraphEdgeRow,
    graph_index: &QueryGraphIndex,
    keywords: &[String],
) -> f32 {
    if keywords.is_empty() {
        return 0.0;
    }
    let Some(from_node) = graph_index.node(edge.from_node_id) else {
        return 0.0;
    };
    let Some(to_node) = graph_index.node(edge.to_node_id) else {
        return 0.0;
    };
    let keyword_tokens = keywords.iter().cloned().collect::<BTreeSet<_>>();
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
    let overlap = near_token_overlap_count(&keyword_tokens, &edge_tokens);
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
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        lexical_entity_hits, merge_entity_retrieval_lanes, related_edges_for_entities, score_value,
    };
    use crate::{
        domains::query_ir::{
            DocumentHint, EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope,
        },
        infra::repositories::{RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
        services::{
            knowledge::runtime_read::ActiveRuntimeGraphProjection,
            query::{
                execution::{QueryGraphIndex, RuntimeMatchedEntity},
                planner::{RuntimeQueryPlan, build_query_plan},
            },
        },
    };

    fn graph_index_with_nodes(nodes: Vec<RuntimeGraphNodeRow>) -> QueryGraphIndex {
        let positions =
            nodes.iter().enumerate().map(|(position, node)| (node.id, position)).collect();
        QueryGraphIndex::new(
            std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges: Vec::new() }),
            positions,
            Default::default(),
        )
    }

    fn graph_index_with_projection(
        nodes: Vec<RuntimeGraphNodeRow>,
        edges: Vec<RuntimeGraphEdgeRow>,
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

    fn node(label: &str, node_type: &str, summary: Option<&str>) -> RuntimeGraphNodeRow {
        RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            canonical_key: format!("{node_type}:{}", label.to_lowercase()),
            label: label.to_string(),
            node_type: node_type.to_string(),
            aliases_json: json!([]),
            summary: summary.map(str::to_string),
            metadata_json: json!({}),
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
    ) -> RuntimeGraphEdgeRow {
        RuntimeGraphEdgeRow {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            from_node_id,
            to_node_id,
            relation_type: relation_type.to_string(),
            canonical_key: format!("{from_node_id}:{relation_type}:{to_node_id}"),
            summary: summary.map(str::to_string),
            weight: None,
            support_count: 1,
            metadata_json: json!({}),
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
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
            confidence: 0.9,
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
            confidence: 0.9,
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

        let hits = lexical_entity_hits(&plan, Some(&ir), &graph_index);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "[26.04.2026 22:36]");
        assert_eq!(hits[0].node_type, "event");
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

        let hits = lexical_entity_hits(&plan, None, &graph_index);

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

        let hits = lexical_entity_hits(&plan, Some(&ir), &graph_index);

        assert_eq!(hits[0].label, "/srv/reports/archive");
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

        let hits = lexical_entity_hits(&plan, Some(&ir), &graph_index);

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

        let hits = lexical_entity_hits(&plan, Some(&ir), &graph_index);

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

        let duplicate_hits = lexical_entity_hits(&plan, Some(&duplicate_ir), &graph_index);
        let single_hits = lexical_entity_hits(&plan, Some(&single_ir), &graph_index);

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

        let hits = lexical_entity_hits(&plan, Some(&ir), &graph_index);

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
        let graph_index = graph_index_with_nodes(vec![node("chat.txt", "document", None)]);

        assert!(
            lexical_entity_hits(&plan, Some(&inventory_ir(&["document"])), &graph_index).is_empty()
        );
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
                score: Some(9_000.0),
            },
            RuntimeMatchedEntity {
                node_id: vector_two.id,
                label: vector_two.label,
                node_type: vector_two.node_type,
                score: Some(8_000.0),
            },
        ];
        let lexical_hits = vec![RuntimeMatchedEntity {
            node_id: needle.id,
            label: needle.label,
            node_type: needle.node_type,
            score: Some(0.24),
        }];

        let merged = merge_entity_retrieval_lanes(vector_hits, lexical_hits, 2);

        assert!(merged.iter().any(|entity| entity.node_id == needle.id));
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
    fn related_edges_rank_edge_text_relevance_before_stable_ties() {
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
            score: Some(0.3),
        }];

        let hits = related_edges_for_entities(&entities, &graph_index, &plan, None, 2);

        assert_eq!(hits[0].to_label, "needle artifact");
        assert!(score_value(hits[0].score) > score_value(hits[1].score));
    }
}
