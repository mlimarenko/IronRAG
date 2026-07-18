use std::collections::{HashMap, HashSet};

use crate::{
    domains::runtime_graph::RuntimeGraphArtifactFilterReason, infra::knowledge_rows::GraphViewData,
};

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
    pub const fn new(filter_empty_relations: bool, filter_degenerate_self_loops: bool) -> Self {
        Self { filter_empty_relations, filter_degenerate_self_loops }
    }

    #[must_use]
    pub fn normalized_relation_type(&self, relation_type: &str) -> String {
        crate::services::graph::identity::normalize_relation_type(relation_type)
    }

    #[must_use]
    pub fn filter_projection(&self, projection: &GraphViewData) -> GraphViewData {
        let retained_node_ids = projection
            .nodes
            .iter()
            .filter(|node| self.allows_node(&node.node_type, &node.label))
            .map(|node| node.node_id)
            .collect::<HashSet<_>>();
        let node_key_index = projection
            .nodes
            .iter()
            .filter(|node| retained_node_ids.contains(&node.node_id))
            .map(|node| (node.node_id, node.canonical_key.clone()))
            .collect::<HashMap<_, _>>();
        let edges = projection
            .edges
            .iter()
            .filter(|edge| {
                if !retained_node_ids.contains(&edge.from_node_id)
                    || !retained_node_ids.contains(&edge.to_node_id)
                {
                    return false;
                }
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
                retained_node_ids.contains(&node.node_id)
                    && (node.node_type == "document" || connected_node_ids.contains(&node.node_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        GraphViewData { nodes, edges }
    }

    #[must_use]
    pub fn allows_node(&self, node_type: &str, label: &str) -> bool {
        node_type == "document"
            || !crate::services::graph::identity::is_structural_literal_label(label)
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
        let normalized_relation_type =
            crate::services::graph::identity::normalize_relation_type(relation_type);

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::knowledge_rows::{GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite};
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
    fn accepts_arbitrary_structurally_valid_relation_types() {
        let guard = GraphQualityGuardService::new(true, true);

        assert_eq!(guard.filter_reason("foo", "bar", "opaque_predicate_7"), None);
    }

    #[test]
    fn rejects_structurally_invalid_relation_types() {
        let guard = GraphQualityGuardService::new(true, true);

        assert_eq!(
            guard.filter_reason("foo", "bar", "Invalid Predicate"),
            Some(RuntimeGraphArtifactFilterReason::EmptyRelation)
        );
    }

    #[test]
    fn rejects_canonical_self_loops_without_typed_relation_metadata() {
        let guard = GraphQualityGuardService::new(true, true);

        assert_eq!(
            guard.filter_reason("foo", "foo", "aliases"),
            Some(RuntimeGraphArtifactFilterReason::DegenerateSelfLoop)
        );
    }

    #[test]
    fn rejects_structurally_valid_self_loops_when_empty_filter_is_disabled() {
        let guard = GraphQualityGuardService::new(false, true);

        assert_eq!(
            guard.filter_reason("foo", "foo", "same_as"),
            Some(RuntimeGraphArtifactFilterReason::DegenerateSelfLoop)
        );
    }

    #[test]
    fn filters_bad_edges_and_orphan_nodes_from_projection() {
        let guard = GraphQualityGuardService::new(true, true);
        let document_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let literal_id = Uuid::now_v7();
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
                    node_id: literal_id,
                    canonical_key: "attribute:false".to_string(),
                    label: "false".to_string(),
                    node_type: "attribute".to_string(),
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
                    from_node_id: document_id,
                    to_node_id: literal_id,
                    relation_type: "mentions".to_string(),
                    canonical_key: "document:1--mentions--attribute:false".to_string(),
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
        assert!(filtered.nodes.iter().all(|node| node.node_id != literal_id));
    }

    #[test]
    fn keeps_non_literal_attribute_nodes() {
        let guard = GraphQualityGuardService::new(true, true);

        assert!(!guard.allows_node("attribute", "false"));
        assert!(guard.allows_node("attribute", "False"));
        assert!(guard.allows_node("attribute", "42"));
        assert!(guard.allows_node("attribute", "3.12.4"));
        assert!(guard.allows_node("attribute", "Alpha false mode"));
        assert!(guard.allows_node("document", "false"));
    }
}
