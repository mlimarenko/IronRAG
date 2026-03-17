use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        graph_store::{GraphProjectionEdgeWrite, GraphProjectionNodeWrite},
        repositories::{self, RuntimeGraphSnapshotRow},
    },
};

#[derive(Debug, Clone)]
pub struct GraphProjectionScope {
    pub library_id: Uuid,
    pub projection_version: i64,
}

#[derive(Debug, Clone)]
pub struct GraphProjectionOutcome {
    pub projection_version: i64,
    pub node_count: usize,
    pub edge_count: usize,
    pub graph_status: String,
}

impl GraphProjectionScope {
    #[must_use]
    pub const fn new(library_id: Uuid, projection_version: i64) -> Self {
        Self { library_id, projection_version }
    }
}

#[must_use]
pub fn active_projection_version(snapshot: Option<&RuntimeGraphSnapshotRow>) -> i64 {
    snapshot.map(|row| row.projection_version).filter(|value| *value > 0).unwrap_or(1)
}

#[must_use]
pub fn next_projection_version(snapshot: Option<&RuntimeGraphSnapshotRow>) -> i64 {
    snapshot.map(|_| active_projection_version(snapshot) + 1).unwrap_or(1)
}

pub async fn resolve_projection_scope(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<GraphProjectionScope> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while resolving projection scope")?;
    Ok(GraphProjectionScope::new(library_id, active_projection_version(snapshot.as_ref())))
}

pub async fn ensure_empty_graph_snapshot(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
) -> anyhow::Result<GraphProjectionOutcome> {
    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        library_id,
        "empty",
        projection_version,
        0,
        0,
        Some(0.0),
        None,
    )
    .await
    .context("failed to persist empty graph snapshot")?;

    Ok(GraphProjectionOutcome {
        projection_version,
        node_count: 0,
        edge_count: 0,
        graph_status: "empty".to_string(),
    })
}

pub async fn mark_graph_snapshot_stale(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    node_count: usize,
    edge_count: usize,
    reason: Option<&str>,
) -> anyhow::Result<GraphProjectionOutcome> {
    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        library_id,
        "stale",
        projection_version,
        i32::try_from(node_count).unwrap_or(i32::MAX),
        i32::try_from(edge_count).unwrap_or(i32::MAX),
        Some(if node_count == 0 && edge_count == 0 { 0.0 } else { 100.0 }),
        reason,
    )
    .await
    .context("failed to mark graph snapshot as stale")?;

    Ok(GraphProjectionOutcome {
        projection_version,
        node_count,
        edge_count,
        graph_status: "stale".to_string(),
    })
}

pub async fn project_canonical_graph(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> anyhow::Result<GraphProjectionOutcome> {
    synchronize_projection_support_counts(state, scope).await?;
    let nodes = repositories::list_admitted_runtime_graph_nodes_by_projection(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load canonical graph nodes for projection")?;
    let edges = repositories::list_admitted_runtime_graph_edges_by_projection(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load canonical graph edges for projection")?;

    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        scope.library_id,
        "building",
        scope.projection_version,
        i32::try_from(nodes.len()).unwrap_or(i32::MAX),
        i32::try_from(edges.len()).unwrap_or(i32::MAX),
        Some(provenance_coverage_percent(&nodes, &edges)),
        None,
    )
    .await
    .context("failed to mark graph snapshot as building")?;

    if nodes.is_empty() && edges.is_empty() {
        return ensure_empty_graph_snapshot(state, scope.library_id, scope.projection_version)
            .await;
    }

    let node_writes = nodes
        .iter()
        .map(|node| GraphProjectionNodeWrite {
            node_id: node.id,
            canonical_key: node.canonical_key.clone(),
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            support_count: node.support_count,
            summary: node.summary.clone(),
            aliases: serde_json::from_value(node.aliases_json.clone()).unwrap_or_default(),
            metadata_json: node.metadata_json.clone(),
        })
        .collect::<Vec<_>>();
    let edge_writes = edges
        .iter()
        .map(|edge| GraphProjectionEdgeWrite {
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
        .collect::<Vec<_>>();

    if let Err(error) = state
        .graph_store
        .replace_library_projection(
            scope.library_id,
            scope.projection_version,
            &node_writes,
            &edge_writes,
        )
        .await
    {
        repositories::upsert_runtime_graph_snapshot(
            &state.persistence.postgres,
            scope.library_id,
            "failed",
            scope.projection_version,
            i32::try_from(nodes.len()).unwrap_or(i32::MAX),
            i32::try_from(edges.len()).unwrap_or(i32::MAX),
            Some(provenance_coverage_percent(&nodes, &edges)),
            Some(&error.to_string()),
        )
        .await
        .context("failed to mark graph snapshot as failed after Neo4j projection error")?;
        return Err(error).context("failed to project canonical graph into Neo4j");
    }

    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        scope.library_id,
        "ready",
        scope.projection_version,
        i32::try_from(nodes.len()).unwrap_or(i32::MAX),
        i32::try_from(edges.len()).unwrap_or(i32::MAX),
        Some(provenance_coverage_percent(&nodes, &edges)),
        None,
    )
    .await
    .context("failed to mark graph snapshot as ready")?;

    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count: node_writes.len(),
        edge_count: edge_writes.len(),
        graph_status: "ready".to_string(),
    })
}

pub async fn rebuild_projection_from_canonical(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
) -> anyhow::Result<GraphProjectionOutcome> {
    project_canonical_graph(state, &GraphProjectionScope::new(library_id, projection_version)).await
}

async fn synchronize_projection_support_counts(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> anyhow::Result<()> {
    repositories::recalculate_runtime_graph_support_counts(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to recalculate canonical graph support counts before projection")?;
    repositories::delete_runtime_graph_edges_without_support(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to prune zero-support graph edges before projection")?;
    repositories::delete_runtime_graph_nodes_without_support(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to prune zero-support graph nodes before projection")?;

    Ok(())
}

fn provenance_coverage_percent(
    nodes: &[repositories::RuntimeGraphNodeRow],
    edges: &[repositories::RuntimeGraphEdgeRow],
) -> f64 {
    if nodes.is_empty() && edges.is_empty() { 0.0 } else { 100.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_projection_version_to_one_when_snapshot_is_absent() {
        assert_eq!(active_projection_version(None), 1);
    }

    #[test]
    fn keeps_existing_projection_version_when_snapshot_exists() {
        let snapshot = RuntimeGraphSnapshotRow {
            project_id: Uuid::nil(),
            graph_status: "ready".to_string(),
            projection_version: 7,
            node_count: 3,
            edge_count: 2,
            provenance_coverage_percent: Some(100.0),
            last_built_at: None,
            last_error_message: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(active_projection_version(Some(&snapshot)), 7);
    }

    #[test]
    fn falls_back_to_one_when_snapshot_version_is_zero() {
        let snapshot = RuntimeGraphSnapshotRow {
            project_id: Uuid::nil(),
            graph_status: "building".to_string(),
            projection_version: 0,
            node_count: 0,
            edge_count: 0,
            provenance_coverage_percent: Some(0.0),
            last_built_at: None,
            last_error_message: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(active_projection_version(Some(&snapshot)), 1);
    }

    #[test]
    fn increments_projection_version_for_rebuilds() {
        let snapshot = RuntimeGraphSnapshotRow {
            project_id: Uuid::nil(),
            graph_status: "ready".to_string(),
            projection_version: 3,
            node_count: 2,
            edge_count: 1,
            provenance_coverage_percent: Some(100.0),
            last_built_at: None,
            last_error_message: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(next_projection_version(Some(&snapshot)), 4);
        assert_eq!(next_projection_version(None), 1);
    }
}
