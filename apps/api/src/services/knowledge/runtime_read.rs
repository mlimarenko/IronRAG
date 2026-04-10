use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        arangodb::graph_store::{GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite},
        repositories::{self, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    },
    shared::json_coercion::from_value_or_default,
};

#[derive(Debug, Clone)]
pub struct ActiveRuntimeGraphProjection {
    pub nodes: Vec<RuntimeGraphNodeRow>,
    pub edges: Vec<RuntimeGraphEdgeRow>,
}

pub async fn load_active_runtime_graph_projection(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<ActiveRuntimeGraphProjection> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load runtime graph snapshot")?;
    let Some(snapshot_row) = snapshot else {
        return Ok(ActiveRuntimeGraphProjection { nodes: Vec::new(), edges: Vec::new() });
    };

    let projection_version = snapshot_row.projection_version.max(1);
    if snapshot_row.graph_status == "empty"
        || (snapshot_row.node_count <= 0 && snapshot_row.edge_count <= 0)
    {
        return Ok(ActiveRuntimeGraphProjection { nodes: Vec::new(), edges: Vec::new() });
    }

    let nodes = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load admitted runtime graph nodes")?;
    let edges = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load admitted runtime graph edges")?;

    Ok(ActiveRuntimeGraphProjection { nodes, edges })
}

#[must_use]
pub fn graph_view_data_from_runtime_projection(
    projection: &ActiveRuntimeGraphProjection,
) -> GraphViewData {
    let nodes = projection
        .nodes
        .iter()
        .map(|node| GraphViewNodeWrite {
            node_id: node.id,
            canonical_key: node.canonical_key.clone(),
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            support_count: node.support_count,
            summary: node.summary.clone(),
            aliases: from_value_or_default("runtime_graph_node.aliases_json", &node.aliases_json),
            metadata_json: node.metadata_json.clone(),
        })
        .collect();
    let edges = projection
        .edges
        .iter()
        .map(|edge| GraphViewEdgeWrite {
            edge_id: edge.id,
            from_node_id: edge.from_node_id,
            to_node_id: edge.to_node_id,
            relation_type: edge.relation_type.clone(),
            canonical_key: edge.canonical_key.clone(),
            support_count: edge.support_count,
            summary: edge.summary.clone(),
            weight: edge.weight,
            metadata_json: edge.metadata_json.clone(),
        })
        .collect();
    GraphViewData { nodes, edges }
}
