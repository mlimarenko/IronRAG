use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        graph_store::{GraphProjectionData, GraphProjectionEdgeWrite, GraphProjectionNodeWrite},
        repositories::{self, graph_repository},
    },
    services::{
        graph_merge::{self, GraphMergeOutcome, GraphMergeScope},
        graph_projection::{self, GraphProjectionOutcome, GraphProjectionScope},
        graph_summary::{GraphSummaryRefreshRequest, GraphSummaryService},
    },
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphSupportCountRefreshOutcome {
    pub scanned_nodes: usize,
    pub scanned_edges: usize,
    pub updated_nodes: usize,
    pub updated_edges: usize,
}

#[derive(Clone, Default)]
pub struct GraphService;

impl GraphService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn merge_projection_data(
        current: &GraphProjectionData,
        incoming: &GraphProjectionData,
    ) -> GraphProjectionData {
        let mut nodes = BTreeMap::<String, GraphProjectionNodeWrite>::new();
        for node in current.nodes.iter().chain(incoming.nodes.iter()) {
            nodes.insert(node.canonical_key.clone(), node.clone());
        }

        let mut edges = BTreeMap::<String, GraphProjectionEdgeWrite>::new();
        for edge in current.edges.iter().chain(incoming.edges.iter()) {
            edges.insert(edge.canonical_key.clone(), edge.clone());
        }

        let merged = GraphProjectionData {
            nodes: nodes.into_values().collect(),
            edges: edges.into_values().collect(),
        };
        let (nodes, edges, _) =
            crate::infra::graph_store::sanitize_projection_writes(&merged.nodes, &merged.edges);
        GraphProjectionData { nodes, edges }
    }

    #[must_use]
    pub fn select_active_projection<'a>(
        &self,
        rows: &'a [graph_repository::GraphProjectionRow],
    ) -> Option<&'a graph_repository::GraphProjectionRow> {
        rows.iter()
            .filter(|row| row.projection_state == "active")
            .max_by_key(|row| row.started_at)
            .or_else(|| rows.iter().max_by_key(|row| row.started_at))
    }

    #[must_use]
    pub fn select_active_projection_id(
        &self,
        rows: &[graph_repository::GraphProjectionRow],
    ) -> Option<Uuid> {
        self.select_active_projection(rows).map(|row| row.id)
    }

    pub async fn merge_chunk_graph_candidates(
        &self,
        pool: &sqlx::PgPool,
        graph_quality_guard: &crate::services::graph_quality_guard::GraphQualityGuardService,
        scope: &GraphMergeScope,
        document: &repositories::DocumentRow,
        chunk: &repositories::ChunkRow,
        candidates: &crate::services::graph_extract::GraphExtractionCandidateSet,
        extraction_recovery: Option<&crate::domains::graph_quality::ExtractionRecoverySummary>,
    ) -> Result<GraphMergeOutcome> {
        graph_merge::merge_chunk_graph_candidates(
            pool,
            graph_quality_guard,
            scope,
            document,
            chunk,
            candidates,
            extraction_recovery,
        )
        .await
    }

    pub async fn refresh_support_counts(
        &self,
        state: &AppState,
        projection_id: Uuid,
        targeted_node_ids: &[Uuid],
        targeted_edge_ids: &[Uuid],
    ) -> Result<GraphSupportCountRefreshOutcome> {
        let mut updated_nodes = 0usize;
        let mut updated_edges = 0usize;

        let mut nodes = graph_repository::list_graph_nodes_by_projection(
            &state.persistence.postgres,
            projection_id,
        )
        .await
        .context("failed to load graph nodes for support-count refresh")?;
        if !targeted_node_ids.is_empty() {
            let targeted = targeted_node_ids.iter().copied().collect::<BTreeSet<_>>();
            nodes.retain(|node| targeted.contains(&node.id));
        }

        for node in &nodes {
            let support_count = i32::try_from(
                graph_repository::list_graph_node_evidence_by_node(
                    &state.persistence.postgres,
                    node.id,
                )
                .await
                .with_context(|| format!("failed to load evidence for graph node {}", node.id))?
                .len(),
            )
            .unwrap_or(i32::MAX);
            if support_count != node.support_count {
                graph_repository::update_graph_node(
                    &state.persistence.postgres,
                    node.id,
                    &node.node_kind,
                    &node.display_label,
                    node.summary.as_deref(),
                    support_count,
                )
                .await
                .with_context(|| format!("failed to refresh support count for node {}", node.id))?;
                updated_nodes += 1;
            }
        }

        let mut edges = graph_repository::list_graph_edges_by_projection(
            &state.persistence.postgres,
            projection_id,
        )
        .await
        .context("failed to load graph edges for support-count refresh")?;
        if !targeted_edge_ids.is_empty() {
            let targeted = targeted_edge_ids.iter().copied().collect::<BTreeSet<_>>();
            edges.retain(|edge| targeted.contains(&edge.id));
        }

        for edge in &edges {
            let support_count = i32::try_from(
                graph_repository::list_graph_edge_evidence_by_edge(
                    &state.persistence.postgres,
                    edge.id,
                )
                .await
                .with_context(|| format!("failed to load evidence for graph edge {}", edge.id))?
                .len(),
            )
            .unwrap_or(i32::MAX);
            if support_count != edge.support_count {
                graph_repository::update_graph_edge(
                    &state.persistence.postgres,
                    edge.id,
                    &edge.edge_kind,
                    edge.from_node_id,
                    edge.to_node_id,
                    edge.summary.as_deref(),
                    support_count,
                )
                .await
                .with_context(|| format!("failed to refresh support count for edge {}", edge.id))?;
                updated_edges += 1;
            }
        }

        Ok(GraphSupportCountRefreshOutcome {
            scanned_nodes: nodes.len(),
            scanned_edges: edges.len(),
            updated_nodes,
            updated_edges,
        })
    }

    pub async fn refresh_summaries(
        &self,
        state: &AppState,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
    ) -> Result<u64> {
        GraphSummaryService::default().refresh_summaries(state, library_id, refresh).await
    }

    pub async fn invalidate_summaries(
        &self,
        state: &AppState,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
    ) -> Result<u64> {
        GraphSummaryService::default().invalidate_summaries(state, library_id, refresh).await
    }

    pub async fn project_canonical_graph(
        &self,
        state: &AppState,
        scope: &GraphProjectionScope,
    ) -> Result<GraphProjectionOutcome> {
        graph_projection::project_canonical_graph(state, scope).await
    }

    pub async fn rebuild_library_graph(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<GraphProjectionOutcome> {
        crate::services::graph_rebuild::rebuild_library_graph(state, library_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::graph_store::GraphProjectionData;
    use chrono::{Duration, Utc};

    #[test]
    fn merge_projection_data_prefers_incoming_canonical_rows() {
        let node_id = Uuid::now_v7();
        let edge_id = Uuid::now_v7();
        let merged = GraphService::merge_projection_data(
            &GraphProjectionData {
                nodes: vec![GraphProjectionNodeWrite {
                    node_id,
                    canonical_key: "entity:a".to_string(),
                    label: "A".to_string(),
                    node_type: "entity".to_string(),
                    support_count: 1,
                    summary: None,
                    aliases: vec![],
                    metadata_json: serde_json::json!({}),
                }],
                edges: vec![],
            },
            &GraphProjectionData {
                nodes: vec![GraphProjectionNodeWrite {
                    node_id,
                    canonical_key: "entity:a".to_string(),
                    label: "A2".to_string(),
                    node_type: "topic".to_string(),
                    support_count: 4,
                    summary: Some("updated".to_string()),
                    aliases: vec!["alias".to_string()],
                    metadata_json: serde_json::json!({"k": "v"}),
                }],
                edges: vec![GraphProjectionEdgeWrite {
                    edge_id,
                    from_node_id: node_id,
                    to_node_id: Uuid::now_v7(),
                    relation_type: "links_to".to_string(),
                    canonical_key: "entity:a--links_to--entity:b".to_string(),
                    support_count: 1,
                    summary: None,
                    weight: None,
                    metadata_json: serde_json::json!({}),
                }],
            },
        );

        assert_eq!(merged.nodes.len(), 1);
        assert_eq!(merged.nodes[0].label, "A2");
        assert_eq!(merged.nodes[0].support_count, 4);
        assert!(merged.edges.is_empty(), "dangling edge should be filtered");
    }

    #[test]
    fn select_active_projection_prefers_active_latest_projection() {
        let first = graph_repository::GraphProjectionRow {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            source_attempt_id: None,
            projection_state: "active".to_string(),
            started_at: Utc::now() - Duration::minutes(10),
            completed_at: None,
            superseded_at: None,
        };
        let second =
            graph_repository::GraphProjectionRow { started_at: Utc::now(), ..first.clone() };
        let service = GraphService::new();

        assert_eq!(
            service.select_active_projection_id(&[first.clone(), second.clone()]),
            Some(second.id)
        );
        assert_eq!(
            service.select_active_projection_id(&[
                first,
                graph_repository::GraphProjectionRow {
                    projection_state: "building".to_string(),
                    ..second.clone()
                }
            ]),
            Some(second.id)
        );
    }
}
