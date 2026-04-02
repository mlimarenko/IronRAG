use std::collections::{HashMap, HashSet};

use crate::{
    domains::runtime_graph::RuntimeGraphArtifactFilterReason,
    infra::arangodb::graph_store::GraphViewData, services::graph_identity,
};

const EXPLICIT_SELF_LOOP_RELATION_TYPES: &[&str] =
    &["alias_of", "aliases", "equivalent_to", "same_as", "self_reference", "self_refers_to"];

#[derive(Debug, Clone)]
pub struct GraphQualityGuardService {
    filter_empty_relations: bool,
    filter_degenerate_self_loops: bool,
}

impl Default for GraphQualityGuardService {
    fn default() -> Self {
        Self::new(true, true)
    }
}

impl GraphQualityGuardService {
    #[must_use]
    pub fn new(filter_empty_relations: bool, filter_degenerate_self_loops: bool) -> Self {
        Self { filter_empty_relations, filter_degenerate_self_loops }
    }

    #[must_use]
    pub fn normalized_relation_type(&self, relation_type: &str) -> String {
        graph_identity::normalize_relation_type(relation_type)
    }

    #[must_use]
    pub fn filter_projection(&self, projection: &GraphViewData) -> GraphViewData {
        let node_key_index = projection
            .nodes
            .iter()
            .map(|node| (node.node_id, node.canonical_key.clone()))
            .collect::<HashMap<_, _>>();
        let edges = projection
            .edges
            .iter()
            .filter(|edge| {
                let from_node_key =
                    node_key_index.get(&edge.from_node_id).map(String::as_str).unwrap_or_default();
                let to_node_key =
                    node_key_index.get(&edge.to_node_id).map(String::as_str).unwrap_or_default();
                self.allows_relation(from_node_key, to_node_key, &edge.relation_type)
            })
            .cloned()
            .collect::<Vec<_>>();
        let connected_node_ids = edges
            .iter()
            .flat_map(|edge| [edge.from_node_id, edge.to_node_id])
            .collect::<HashSet<_>>();
        let nodes = projection
            .nodes
            .iter()
            .filter(|node| {
                node.node_type == "document" || connected_node_ids.contains(&node.node_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        GraphViewData { nodes, edges }
    }

    #[must_use]
    pub fn filter_reason(
        &self,
        from_node_key: &str,
        to_node_key: &str,
        relation_type: &str,
    ) -> Option<RuntimeGraphArtifactFilterReason> {
        let trimmed = relation_type.trim();
        if self.filter_empty_relations && trimmed.is_empty() {
            return Some(RuntimeGraphArtifactFilterReason::EmptyRelation);
        }
        let raw_slug = graph_identity::normalize_graph_identity_component(trimmed);
        let normalized_relation_type = graph_identity::normalize_relation_type(relation_type);

        if self.filter_degenerate_self_loops
            && !from_node_key.trim().is_empty()
            && from_node_key == to_node_key
            && self.is_explicit_self_loop_slug_or_normalized(
                raw_slug.as_str(),
                normalized_relation_type.as_str(),
            )
        {
            return None;
        }

        if graph_identity::is_noise_relation_type(&raw_slug) {
            return Some(RuntimeGraphArtifactFilterReason::LowValueArtifact);
        }
        if self.filter_empty_relations && normalized_relation_type.is_empty() {
            return Some(RuntimeGraphArtifactFilterReason::EmptyRelation);
        }
        if self.filter_degenerate_self_loops
            && !from_node_key.trim().is_empty()
            && from_node_key == to_node_key
        {
            return Some(RuntimeGraphArtifactFilterReason::DegenerateSelfLoop);
        }
        None
    }

    #[must_use]
    pub fn allows_relation(
        &self,
        from_node_key: &str,
        to_node_key: &str,
        relation_type: &str,
    ) -> bool {
        self.filter_reason(from_node_key, to_node_key, relation_type).is_none()
    }

    #[must_use]
    fn is_explicit_self_loop_slug_or_normalized(&self, raw_slug: &str, normalized: &str) -> bool {
        EXPLICIT_SELF_LOOP_RELATION_TYPES.iter().any(|&known| known == raw_slug)
            || (!normalized.is_empty()
                && (normalized.starts_with("self_") || normalized.ends_with("_self")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::arangodb::graph_store::{
        GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite,
    };
    use uuid::Uuid;

    #[test]
    fn rejects_empty_relation_types_when_enabled() {
        let guard = GraphQualityGuardService::new(true, true);

        assert_eq!(
            guard.filter_reason("foo", "bar", "   "),
            Some(RuntimeGraphArtifactFilterReason::EmptyRelation)
        );
    }

    #[test]
    fn rejects_degenerate_self_loops_when_enabled() {
        let guard = GraphQualityGuardService::new(true, true);

        assert_eq!(
            guard.filter_reason("foo", "foo", "mentions"),
            Some(RuntimeGraphArtifactFilterReason::DegenerateSelfLoop)
        );
    }

    #[test]
    fn rejects_low_value_relation_types() {
        let guard = GraphQualityGuardService::new(true, true);

        assert_eq!(
            guard.filter_reason("foo", "bar", "related_to"),
            Some(RuntimeGraphArtifactFilterReason::LowValueArtifact)
        );
    }

    #[test]
    fn allows_explicit_self_loop_relation_types() {
        let guard = GraphQualityGuardService::new(true, true);

        assert_eq!(guard.filter_reason("foo", "foo", "same_as"), None);
    }

    #[test]
    fn filters_bad_edges_and_orphan_nodes_from_projection() {
        let guard = GraphQualityGuardService::new(true, true);
        let document_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let orphan_id = Uuid::now_v7();
        let projection = GraphViewData {
            nodes: vec![
                GraphViewNodeWrite {
                    node_id: document_id,
                    canonical_key: "document:1".to_string(),
                    label: "Doc".to_string(),
                    node_type: "document".to_string(),
                    support_count: 1,
                    summary: None,
                    aliases: Vec::new(),
                    metadata_json: serde_json::json!({}),
                },
                GraphViewNodeWrite {
                    node_id: entity_id,
                    canonical_key: "entity:alpha".to_string(),
                    label: "Alpha".to_string(),
                    node_type: "entity".to_string(),
                    support_count: 1,
                    summary: None,
                    aliases: Vec::new(),
                    metadata_json: serde_json::json!({}),
                },
                GraphViewNodeWrite {
                    node_id: orphan_id,
                    canonical_key: "entity:orphan".to_string(),
                    label: "Orphan".to_string(),
                    node_type: "entity".to_string(),
                    support_count: 1,
                    summary: None,
                    aliases: Vec::new(),
                    metadata_json: serde_json::json!({}),
                },
            ],
            edges: vec![
                GraphViewEdgeWrite {
                    edge_id: Uuid::now_v7(),
                    from_node_id: document_id,
                    to_node_id: entity_id,
                    relation_type: "mentions".to_string(),
                    canonical_key: "document:1--mentions--entity:alpha".to_string(),
                    support_count: 1,
                    summary: None,
                    weight: None,
                    metadata_json: serde_json::json!({}),
                },
                GraphViewEdgeWrite {
                    edge_id: Uuid::now_v7(),
                    from_node_id: orphan_id,
                    to_node_id: orphan_id,
                    relation_type: "mentions".to_string(),
                    canonical_key: "entity:orphan--mentions--entity:orphan".to_string(),
                    support_count: 1,
                    summary: None,
                    weight: None,
                    metadata_json: serde_json::json!({}),
                },
            ],
        };

        let filtered = guard.filter_projection(&projection);

        assert_eq!(filtered.edges.len(), 1);
        assert_eq!(filtered.nodes.len(), 2);
        assert!(filtered.nodes.iter().all(|node| node.node_id != orphan_id));
    }
}
