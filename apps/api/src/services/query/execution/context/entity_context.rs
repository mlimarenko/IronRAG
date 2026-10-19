use std::collections::HashSet;

use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    domains::query_ir::{QueryAct, QueryIR},
    infra::repositories::RuntimeGraphQueryNodeRow,
    services::query::text_match::{
        RelatedTokenSelection, build_related_token_candidates, normalized_alnum_tokens,
        select_related_overlap_tokens_from_candidates, token_sequence_exact_or_contains,
    },
    shared::{json_coercion::from_value_or_default, text_tokens::literal_wildcard_prefixes},
};

use super::super::{
    retrieve::excerpt_for,
    types::{QueryGraphIndex, RuntimeMatchedEntity},
};

const ENTITY_MATCH_CONTEXT_LINE_LIMIT: usize = 8;
const ENTITY_SUMMARY_CONTEXT_CHARS: usize = 320;
pub(super) const TARGET_ENTITY_CONTEXT_LINE_LIMIT: usize = 64;
const TARGET_ENTITY_INVENTORY_CONTEXT_LINE_LIMIT: usize = 192;
const TARGET_ENTITY_SUMMARY_CONTEXT_CHARS: usize = 180;

pub(crate) fn target_entity_context_lines(
    query_ir: &QueryIR,
    graph_index: &QueryGraphIndex,
) -> Vec<String> {
    if query_ir.target_entities.is_empty() {
        return Vec::new();
    }

    let line_limit = target_entity_context_line_limit(query_ir);
    let mut seen_nodes = HashSet::<Uuid>::new();
    let mut lines = Vec::new();
    for mention in &query_ir.target_entities {
        if lines.len() >= line_limit {
            break;
        }
        let label = mention.label.trim();
        if label.is_empty() {
            continue;
        }
        let normalized_label = normalized_prefix_match_text(label);
        let wildcard_prefixes = literal_wildcard_prefixes(label, 2);
        if normalized_label.is_empty() && wildcard_prefixes.is_empty() {
            continue;
        }

        let mut matches = graph_index
            .nodes()
            .filter(|node| node.node_type != "document")
            .filter(|node| {
                graph_node_matches_target_label(node, &normalized_label, &wildcard_prefixes)
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then_with(|| right.support_count.cmp(&left.support_count))
                .then_with(|| left.id.cmp(&right.id))
        });
        for node in matches {
            if lines.len() >= line_limit || !seen_nodes.insert(node.id) {
                continue;
            }
            lines.push(graph_node_target_context_line(node));
        }
    }
    lines
}

fn target_entity_context_line_limit(query_ir: &QueryIR) -> usize {
    if matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Meta)
        && query_ir
            .target_entities
            .iter()
            .any(|mention| !literal_wildcard_prefixes(mention.label.trim(), 2).is_empty())
    {
        TARGET_ENTITY_INVENTORY_CONTEXT_LINE_LIMIT
    } else {
        TARGET_ENTITY_CONTEXT_LINE_LIMIT
    }
}

fn graph_node_matches_target_label(
    node: &RuntimeGraphQueryNodeRow,
    normalized_label: &str,
    wildcard_prefixes: &[String],
) -> bool {
    let label = normalized_prefix_match_text(&node.label);
    if !normalized_label.is_empty() && label == normalized_label {
        return true;
    }
    let aliases =
        from_value_or_default::<Vec<String>>("runtime_graph_node.aliases_json", &node.aliases_json);
    if !normalized_label.is_empty()
        && aliases.iter().any(|alias| normalized_prefix_match_text(alias) == normalized_label)
    {
        return true;
    }
    !wildcard_prefixes.is_empty()
        && wildcard_prefixes.iter().any(|prefix| {
            label.starts_with(prefix)
                || aliases
                    .iter()
                    .any(|alias| normalized_prefix_match_text(alias).starts_with(prefix))
        })
}

fn graph_node_target_context_line(node: &RuntimeGraphQueryNodeRow) -> String {
    let tail = format!("{} ({})", node.label, node.node_type);
    if let Some(summary) = node.summary.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        return format!(
            "[graph-node] target_match=explicit evidence: {} | entity_hint: {}",
            excerpt_for(summary, TARGET_ENTITY_SUMMARY_CONTEXT_CHARS),
            tail
        );
    }
    format!("[graph-node] target_match=explicit entity_hint: {tail}")
}

struct EntityMatchTarget {
    label: String,
    wildcard_prefixes: Vec<String>,
    related_tokens: RelatedTokenSelection,
}

pub(super) fn entity_match_context_lines(
    query_ir: &QueryIR,
    entities: &[RuntimeMatchedEntity],
) -> Vec<String> {
    if query_ir.target_entities.is_empty() || entities.is_empty() {
        return Vec::new();
    }
    let target_sets = entity_match_targets(query_ir, entities);
    if target_sets.is_empty() {
        return Vec::new();
    }

    let mut seen_nodes = HashSet::<Uuid>::new();
    entities
        .iter()
        .filter(|entity| seen_nodes.insert(entity.node_id))
        .filter_map(|entity| {
            let kind = entity_match_kind(entity, &target_sets)?;
            Some(format!("[entity-match {kind}] {}", graph_node_context_tail(entity)))
        })
        .take(ENTITY_MATCH_CONTEXT_LINE_LIMIT)
        .collect()
}

fn entity_match_targets(
    query_ir: &QueryIR,
    entities: &[RuntimeMatchedEntity],
) -> Vec<EntityMatchTarget> {
    let target_labels = query_ir
        .target_entities
        .iter()
        .filter_map(|mention| {
            let label = mention.label.trim();
            let is_matchable = !label.is_empty()
                && (!normalized_alnum_tokens(label, 3).is_empty()
                    || !literal_wildcard_prefixes(label, 2).is_empty());
            is_matchable.then(|| label.to_string())
        })
        .collect::<Vec<_>>();
    let related_candidates =
        build_related_token_candidates(entities.iter().map(|entity| entity.label.as_str()), 3);
    target_labels
        .into_iter()
        .map(|label| EntityMatchTarget {
            wildcard_prefixes: literal_wildcard_prefixes(&label, 2),
            related_tokens: select_related_overlap_tokens_from_candidates(
                &label,
                &related_candidates,
                3,
            ),
            label,
        })
        .collect()
}

fn entity_match_kind(
    entity: &RuntimeMatchedEntity,
    targets: &[EntityMatchTarget],
) -> Option<&'static str> {
    let label = entity.label.trim();
    if label.is_empty() {
        return None;
    }
    let label_tokens = normalized_alnum_tokens(label, 3);
    let label_prefix_text = normalized_prefix_match_text(label);
    let mut best_kind = None;
    for target in targets {
        if !target.wildcard_prefixes.is_empty() {
            if target.wildcard_prefixes.iter().any(|prefix| label_prefix_text.starts_with(prefix)) {
                return Some("prefix");
            }
            continue;
        }
        if token_sequence_exact_or_contains(label, &target.label, 3) {
            return Some("exact");
        }
        if !target.related_tokens.is_empty() && target.related_tokens.matches_tokens(&label_tokens)
        {
            best_kind = Some("token-overlap");
        }
    }
    best_kind
}

pub(super) fn graph_node_context_line(entity: &RuntimeMatchedEntity) -> String {
    if let Some(summary) =
        entity.summary.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        return format!(
            "[graph-node] evidence: {} | entity_hint: {}",
            excerpt_for(summary, ENTITY_SUMMARY_CONTEXT_CHARS),
            graph_node_context_tail(entity)
        );
    }
    format!("[graph-node] {}", graph_node_context_tail(entity))
}

fn graph_node_context_tail(entity: &RuntimeMatchedEntity) -> String {
    format!("{} ({})", entity.label, entity.node_type)
}

fn normalized_prefix_match_text(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect::<String>().trim().to_string()
}

pub(crate) fn should_prioritize_retrieved_context_for_query(
    query_ir: &QueryIR,
    retrieved_context: &str,
) -> bool {
    should_prioritize_graph_context_for_query(
        query_ir,
        retrieved_context.contains("[graph-node]") || retrieved_context.contains("[graph-edge"),
        retrieved_context.contains("[graph-evidence"),
    )
}

pub(super) fn should_prioritize_graph_context_for_query(
    query_ir: &QueryIR,
    has_graph_topology_support: bool,
    has_graph_evidence_support: bool,
) -> bool {
    if !(has_graph_topology_support || has_graph_evidence_support) {
        return false;
    }
    if matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Meta)
        && (query_ir.scope == crate::domains::query_ir::QueryScope::LibraryMeta
            || !query_ir.target_entities.is_empty())
    {
        return true;
    }
    !query_ir.target_entities.is_empty()
        && matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::Describe | QueryAct::Compare)
}

pub(super) fn should_prioritize_entity_nodes_before_evidence(
    query_ir: &QueryIR,
    has_entities: bool,
) -> bool {
    has_entities && matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Meta)
}
