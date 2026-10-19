use std::collections::{BTreeMap, HashMap};

use uuid::Uuid;

use crate::{
    domains::runtime_graph::RuntimeNodeType,
    infra::repositories::{RuntimeGraphDocumentLinkRow, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    services::graph::identity::{normalize_graph_identity_component, runtime_node_type_from_key},
};

#[derive(Debug, Clone)]
pub(crate) struct NormalizedRuntimeGraphNodes {
    pub nodes: Vec<RuntimeGraphNodeRow>,
    pub node_id_remap: HashMap<Uuid, Uuid>,
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedRuntimeGraphProjection {
    pub nodes: Vec<RuntimeGraphNodeRow>,
    pub edges: Vec<RuntimeGraphEdgeRow>,
    pub node_id_remap: HashMap<Uuid, Uuid>,
}

#[must_use]
pub(crate) fn normalize_runtime_graph_nodes(
    nodes: Vec<RuntimeGraphNodeRow>,
) -> NormalizedRuntimeGraphNodes {
    let mut selected_by_identity: BTreeMap<String, RuntimeGraphNodeRow> = BTreeMap::new();
    let mut remap = HashMap::new();

    for node in nodes {
        let identity = node_identity_key(&node);
        match selected_by_identity.remove(&identity) {
            Some(existing) => {
                let (selected, duplicate) = if compare_node_quality(&node, &existing).is_lt() {
                    (node, existing)
                } else {
                    (existing, node)
                };
                remap.insert(duplicate.id, selected.id);
                selected_by_identity.insert(identity, selected);
            }
            None => {
                selected_by_identity.insert(identity, node);
            }
        }
    }

    let nodes = selected_by_identity.into_values().collect::<Vec<_>>();
    NormalizedRuntimeGraphNodes { nodes, node_id_remap: remap }
}

#[must_use]
pub(crate) fn normalize_runtime_graph_projection(
    nodes: Vec<RuntimeGraphNodeRow>,
    edges: Vec<RuntimeGraphEdgeRow>,
) -> NormalizedRuntimeGraphProjection {
    let normalized_nodes = normalize_runtime_graph_nodes(nodes);
    let edges = normalize_runtime_graph_edges(edges, &normalized_nodes.node_id_remap);
    NormalizedRuntimeGraphProjection {
        nodes: normalized_nodes.nodes,
        edges,
        node_id_remap: normalized_nodes.node_id_remap,
    }
}

#[must_use]
pub(crate) fn normalize_runtime_graph_edges(
    edges: Vec<RuntimeGraphEdgeRow>,
    node_id_remap: &HashMap<Uuid, Uuid>,
) -> Vec<RuntimeGraphEdgeRow> {
    let mut selected_by_signature: BTreeMap<(Uuid, String, Uuid), RuntimeGraphEdgeRow> =
        BTreeMap::new();
    for mut edge in edges {
        edge.from_node_id = remap_node_id(edge.from_node_id, node_id_remap);
        edge.to_node_id = remap_node_id(edge.to_node_id, node_id_remap);
        if edge.from_node_id == edge.to_node_id || edge.relation_type.trim().is_empty() {
            continue;
        }

        let signature = (edge.from_node_id, edge.relation_type.trim().to_string(), edge.to_node_id);
        match selected_by_signature.remove(&signature) {
            Some(existing) => {
                let selected =
                    if compare_edge_quality(&edge, &existing).is_lt() { edge } else { existing };
                selected_by_signature.insert(signature, selected);
            }
            None => {
                selected_by_signature.insert(signature, edge);
            }
        }
    }
    selected_by_signature.into_values().collect()
}

#[must_use]
pub(crate) fn normalize_runtime_graph_document_links(
    links: Vec<RuntimeGraphDocumentLinkRow>,
    nodes: &[RuntimeGraphNodeRow],
    node_id_remap: &HashMap<Uuid, Uuid>,
) -> Vec<RuntimeGraphDocumentLinkRow> {
    let node_type_by_id =
        nodes.iter().map(|node| (node.id, node.node_type.clone())).collect::<HashMap<_, _>>();
    let mut links_by_signature =
        BTreeMap::<(Uuid, Uuid, String), RuntimeGraphDocumentLinkRow>::new();

    for mut link in links {
        link.target_node_id = remap_node_id(link.target_node_id, node_id_remap);
        if let Some(node_type) = node_type_by_id.get(&link.target_node_id) {
            link.target_node_type.clone_from(node_type);
        }
        let signature =
            (link.document_id, link.target_node_id, link.relation_type.trim().to_string());
        match links_by_signature.get_mut(&signature) {
            Some(existing) => {
                existing.support_count = existing.support_count.saturating_add(link.support_count);
            }
            None => {
                links_by_signature.insert(signature, link);
            }
        }
    }

    links_by_signature.into_values().collect()
}

#[must_use]
pub(crate) fn remap_node_id(node_id: Uuid, node_id_remap: &HashMap<Uuid, Uuid>) -> Uuid {
    node_id_remap.get(&node_id).copied().unwrap_or(node_id)
}

fn node_identity_key(node: &RuntimeGraphNodeRow) -> String {
    if node.node_type == "document" {
        return format!("document:{}", node.id);
    }
    normalize_graph_identity_component(&node.label)
}

fn compare_node_quality(
    left: &RuntimeGraphNodeRow,
    right: &RuntimeGraphNodeRow,
) -> std::cmp::Ordering {
    right
        .support_count
        .cmp(&left.support_count)
        .then_with(|| node_type_rank(&right.node_type).cmp(&node_type_rank(&left.node_type)))
        .then_with(|| left.label.cmp(&right.label))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

fn compare_edge_quality(
    left: &RuntimeGraphEdgeRow,
    right: &RuntimeGraphEdgeRow,
) -> std::cmp::Ordering {
    right
        .support_count
        .cmp(&left.support_count)
        .then_with(|| left.relation_type.cmp(&right.relation_type))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

fn node_type_rank(node_type: &str) -> u8 {
    match runtime_node_type_from_key(&format!("{node_type}:x")) {
        RuntimeNodeType::Document => 0,
        RuntimeNodeType::Concept => 1,
        RuntimeNodeType::Entity => 2,
        RuntimeNodeType::Person
        | RuntimeNodeType::Organization
        | RuntimeNodeType::Location
        | RuntimeNodeType::Event
        | RuntimeNodeType::Artifact
        | RuntimeNodeType::Natural
        | RuntimeNodeType::Process
        | RuntimeNodeType::Attribute => 3,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_duplicate_non_document_labels_and_remaps_edges() {
        let library_id = Uuid::now_v7();
        let source = node(library_id, "entity:source", "Source", "entity", 5);
        let weak_target = node(library_id, "entity:target", "Shared Target", "entity", 2);
        let strong_target = node(library_id, "artifact:target", "Shared Target", "artifact", 4);
        let edge_a = edge(library_id, source.id, weak_target.id, "uses", 2);
        let edge_b = edge(library_id, source.id, strong_target.id, "uses", 7);

        let projection = normalize_runtime_graph_projection(
            vec![source.clone(), weak_target.clone(), strong_target.clone()],
            vec![edge_a, edge_b.clone()],
        );

        assert_eq!(projection.nodes.len(), 2);
        assert!(projection.nodes.iter().any(|node| node.id == strong_target.id));
        assert_eq!(projection.node_id_remap.get(&weak_target.id), Some(&strong_target.id));
        assert_eq!(projection.edges.len(), 1);
        assert_eq!(projection.edges[0].from_node_id, source.id);
        assert_eq!(projection.edges[0].to_node_id, strong_target.id);
        assert_eq!(projection.edges[0].id, edge_b.id);
    }

    #[test]
    fn keeps_document_nodes_separate_when_titles_match_entities() {
        let library_id = Uuid::now_v7();
        let document = node(library_id, "document:a", "Shared", "document", 1);
        let entity = node(library_id, "entity:shared", "Shared", "entity", 1);

        let projection = normalize_runtime_graph_nodes(vec![document, entity]);

        assert_eq!(projection.nodes.len(), 2);
        assert!(projection.node_id_remap.is_empty());
    }

    #[test]
    fn document_links_follow_normalized_node_remap() {
        let library_id = Uuid::now_v7();
        let weak = node(library_id, "entity:target", "Target", "entity", 1);
        let strong = node(library_id, "artifact:target", "Target", "artifact", 3);
        let projection = normalize_runtime_graph_nodes(vec![weak.clone(), strong.clone()]);
        let document_id = Uuid::now_v7();
        let links = normalize_runtime_graph_document_links(
            vec![
                document_link(document_id, weak.id, "entity", 1),
                document_link(document_id, strong.id, "artifact", 2),
            ],
            &projection.nodes,
            &projection.node_id_remap,
        );

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_node_id, strong.id);
        assert_eq!(links[0].target_node_type, "artifact");
        assert_eq!(links[0].support_count, 3);
    }

    fn node(
        library_id: Uuid,
        identity_key: &str,
        label: &str,
        node_type: &str,
        support_count: i32,
    ) -> RuntimeGraphNodeRow {
        RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id,
            identity_key: identity_key.to_string(),
            label: label.to_string(),
            node_type: node_type.to_string(),
            aliases_json: json!([]),
            summary: None,
            metadata_json: json!({}),
            support_count,
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn edge(
        library_id: Uuid,
        from_node_id: Uuid,
        to_node_id: Uuid,
        relation_type: &str,
        support_count: i32,
    ) -> RuntimeGraphEdgeRow {
        RuntimeGraphEdgeRow {
            id: Uuid::now_v7(),
            library_id,
            from_node_id,
            to_node_id,
            relation_type: relation_type.to_string(),
            identity_key: format!("{from_node_id}--{relation_type}--{to_node_id}"),
            summary: None,
            weight: None,
            support_count,
            metadata_json: json!({}),
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn document_link(
        document_id: Uuid,
        target_node_id: Uuid,
        target_node_type: &str,
        support_count: i64,
    ) -> RuntimeGraphDocumentLinkRow {
        RuntimeGraphDocumentLinkRow {
            document_id,
            target_node_id,
            target_node_type: target_node_type.to_string(),
            relation_type: "supports".to_string(),
            support_count,
        }
    }
}
