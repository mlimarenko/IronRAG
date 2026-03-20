use async_trait::async_trait;
use std::collections::BTreeSet;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct GraphProjectionNodeWrite {
    pub node_id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub support_count: i32,
    pub summary: Option<String>,
    pub aliases: Vec<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct GraphProjectionEdgeWrite {
    pub edge_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub canonical_key: String,
    pub support_count: i32,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct GraphProjectionData {
    pub nodes: Vec<GraphProjectionNodeWrite>,
    pub edges: Vec<GraphProjectionEdgeWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GraphProjectionWriteError {
    #[error("projection contention: {message}")]
    ProjectionContention { message: String },
    #[error("graph persistence integrity: {message}")]
    GraphPersistenceIntegrity { message: String },
    #[error("projection failure: {message}")]
    ProjectionFailure { message: String },
}

impl GraphProjectionWriteError {
    #[must_use]
    pub const fn is_retryable_contention(&self) -> bool {
        matches!(self, Self::ProjectionContention { .. })
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::ProjectionContention { message }
            | Self::GraphPersistenceIntegrity { message }
            | Self::ProjectionFailure { message } => message,
        }
    }
}

#[must_use]
pub fn sanitize_projection_writes(
    nodes: &[GraphProjectionNodeWrite],
    edges: &[GraphProjectionEdgeWrite],
) -> (Vec<GraphProjectionNodeWrite>, Vec<GraphProjectionEdgeWrite>, usize) {
    let mut ordered_nodes = nodes.to_vec();
    ordered_nodes.sort_by_key(|node| node.node_id);

    let available_node_ids = ordered_nodes.iter().map(|node| node.node_id).collect::<BTreeSet<_>>();
    let mut ordered_edges = edges
        .iter()
        .filter(|edge| {
            available_node_ids.contains(&edge.from_node_id)
                && available_node_ids.contains(&edge.to_node_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    ordered_edges.sort_by_key(|edge| (edge.from_node_id, edge.to_node_id, edge.edge_id));

    let skipped_edge_count = edges.len().saturating_sub(ordered_edges.len());
    (ordered_nodes, ordered_edges, skipped_edge_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_edges_that_reference_missing_nodes() {
        let node_id = Uuid::now_v7();
        let missing_node_id = Uuid::now_v7();
        let edge_id = Uuid::now_v7();
        let (nodes, edges, skipped_edge_count) = sanitize_projection_writes(
            &[GraphProjectionNodeWrite {
                node_id,
                canonical_key: "entity:a".to_string(),
                label: "A".to_string(),
                node_type: "entity".to_string(),
                support_count: 1,
                summary: None,
                aliases: vec![],
                metadata_json: serde_json::json!({}),
            }],
            &[GraphProjectionEdgeWrite {
                edge_id,
                from_node_id: node_id,
                to_node_id: missing_node_id,
                relation_type: "links_to".to_string(),
                canonical_key: "entity:a--links_to--entity:b".to_string(),
                support_count: 1,
                summary: None,
                weight: None,
                metadata_json: serde_json::json!({}),
            }],
        );

        assert_eq!(nodes.len(), 1);
        assert!(edges.is_empty());
        assert_eq!(skipped_edge_count, 1);
    }
}

#[async_trait]
pub trait GraphStore: Send + Sync {
    fn backend_name(&self) -> &'static str;
    async fn ping(&self) -> anyhow::Result<()>;
    async fn replace_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
        nodes: &[GraphProjectionNodeWrite],
        edges: &[GraphProjectionEdgeWrite],
    ) -> Result<(), GraphProjectionWriteError>;
    async fn refresh_library_projection_targets(
        &self,
        library_id: Uuid,
        projection_version: i64,
        remove_node_ids: &[Uuid],
        remove_edge_ids: &[Uuid],
        nodes: &[GraphProjectionNodeWrite],
        edges: &[GraphProjectionEdgeWrite],
    ) -> Result<(), GraphProjectionWriteError>;
    async fn load_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
    ) -> anyhow::Result<GraphProjectionData>;
}
