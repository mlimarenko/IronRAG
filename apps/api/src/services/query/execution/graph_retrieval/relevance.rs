use std::collections::BTreeSet;

use crate::{
    domains::query_ir::{QueryAct, QueryIR, QueryScope, QueryTargetKind},
    services::query::{
        planner::RuntimeQueryPlan,
        text_match::{near_token_overlap_count, normalized_alnum_tokens},
    },
};

use super::super::{
    QueryGraphIndex, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    tuning::EXACT_LABEL_TERM_SENSE_DOMINANCE_RATIO,
};
use super::{
    retrieval::{map_edge_hit, score_desc_entities, score_desc_relationships},
    target_profiles::{
        GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind,
        GraphTargetEntityProfile, graph_target_entity_coverage_score,
    },
};

#[derive(Debug, Clone)]
pub(in super::super) struct GraphQueryRelevanceProfile {
    keywords: Vec<String>,
    pub(in super::super) keyword_tokens: BTreeSet<String>,
    relationship_keywords: Vec<String>,
    target_types: BTreeSet<QueryTargetKind>,
    inventory_support_fallback: bool,
}

pub(super) fn graph_relevance_profile(
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
pub(super) fn lexical_entity_hits(
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
    let relevance_hits = graph_index
        .nodes()
        .filter(|node| include_documents || node.node_type != "document")
        .filter_map(|node| graph_node_relevance(node, relevance_profile, target_entity_profiles))
        .collect::<Vec<_>>();

    // When any active profile uses exact-label-term matching (short identifier
    // mentions), apply a sense-dominance filter before seeding neighborhood
    // expansion. All nodes that matched via an exact_label_terms profile are
    // inspected; only those whose support_count is within
    // EXACT_LABEL_TERM_SENSE_DOMINANCE_RATIO of the strongest match are kept.
    // This prevents high-homograph short identifiers from expanding into the
    // wrong sense neighborhood.
    let has_exact_label_profile =
        target_entity_profiles.iter().any(|p| !p.exact_label_terms.is_empty());

    let relevance_hits = if has_exact_label_profile {
        apply_exact_label_sense_dominance_filter(relevance_hits, target_entity_profiles)
    } else {
        relevance_hits
    };

    let mut hits = relevance_hits
        .into_iter()
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

/// Applies the sense-dominance filter to hits collected during exact-label-term
/// matching. Nodes that matched via an exact_label_terms profile are separated
/// from those that matched by other profile paths. Among the exact-matched nodes,
/// only those with `support_count >= max_support / RATIO` survive. Nodes that
/// did not match via exact_label_terms are passed through unchanged.
fn apply_exact_label_sense_dominance_filter<'a>(
    hits: Vec<GraphNodeRelevance<'a>>,
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> Vec<GraphNodeRelevance<'a>> {
    // Partition into exact-matched vs other. "Exact-matched" means the node's
    // label or alias equals one of the exact_label_terms in any active profile.
    let exact_profiles: Vec<&GraphTargetEntityProfile> =
        target_entity_profiles.iter().filter(|p| !p.exact_label_terms.is_empty()).collect();

    let (mut exact_hits, other_hits): (Vec<_>, Vec<_>) = hits.into_iter().partition(|relevance| {
        let label_lower = relevance.node.label.trim().to_lowercase();
        let aliases = crate::shared::json_coercion::from_value_or_default::<Vec<String>>(
            "runtime_graph_node.aliases_json",
            &relevance.node.aliases_json,
        );
        exact_profiles.iter().any(|p| {
            p.exact_label_terms.iter().any(|term| {
                let term_lower = term.to_lowercase();
                label_lower == term_lower
                    || aliases.iter().any(|a| a.trim().to_lowercase() == term_lower)
            })
        })
    });

    if exact_hits.len() <= 1 {
        // Nothing to filter: zero or one exact hit means no ambiguity.
        exact_hits.extend(other_hits);
        return exact_hits;
    }

    let max_support = exact_hits.iter().map(|r| r.node.support_count).max().unwrap_or(0);

    let threshold = (max_support as f32 / EXACT_LABEL_TERM_SENSE_DOMINANCE_RATIO).floor() as i32;

    let mut result: Vec<GraphNodeRelevance<'_>> =
        exact_hits.into_iter().filter(|r| r.node.support_count >= threshold).collect();
    result.extend(other_hits);
    result
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

pub(super) fn query_relevant_entity_hits_with_relevance(
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

fn graph_target_types(query_ir: Option<&QueryIR>) -> BTreeSet<QueryTargetKind> {
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
    ir.target_types.iter().copied().collect()
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

    if QueryTargetKind::from_wire(&node.node_type)
        .is_some_and(|target| relevance_profile.target_types.contains(&target))
    {
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

pub(super) fn lexical_relationship_hits(
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
