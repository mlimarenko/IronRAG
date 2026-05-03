use std::collections::HashMap;

use anyhow::Result;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
};

/// Stateless community detection service using label propagation.
pub struct CommunityDetectionService;

impl CommunityDetectionService {
    /// Run community detection on a library's graph.
    ///
    /// Uses label propagation: each node starts in its own community, then
    /// iteratively adopts the most common community among its weighted
    /// neighbours. Convergence usually happens within a handful of iterations.
    pub async fn detect_communities(&self, state: &AppState, library_id: Uuid) -> Result<()> {
        let pool = &state.persistence.postgres;

        let snapshot = repositories::get_runtime_graph_snapshot(pool, library_id).await?;
        let projection_version = match snapshot {
            Some(s) => s.projection_version,
            None => return Ok(()),
        };

        let nodes =
            repositories::list_runtime_graph_nodes_by_library(pool, library_id, projection_version)
                .await?;
        let edges =
            repositories::list_runtime_graph_edges_by_library(pool, library_id, projection_version)
                .await?;

        if nodes.is_empty() {
            return Ok(());
        }

        let mut community = run_label_propagation(&nodes, &edges);

        // Renumber communities to contiguous 0..N
        let mut comm_map: HashMap<usize, usize> = HashMap::new();
        let mut next_id = 0usize;
        for comm in community.values() {
            comm_map.entry(*comm).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                id
            });
        }
        for comm in community.values_mut() {
            *comm = comm_map[comm];
        }

        // Compute community sizes
        let mut sizes: HashMap<usize, usize> = HashMap::new();
        for &comm in community.values() {
            *sizes.entry(comm).or_default() += 1;
        }

        persist_communities(pool, library_id, projection_version, &community, &sizes).await?;

        let largest = sizes.values().max().copied().unwrap_or(0);

        info!(
            library_id = %library_id,
            communities = next_id,
            largest = largest,
            "community detection complete"
        );

        Ok(())
    }
}

/// Run community detection after ingestion, mirroring the entity resolution
/// pattern. Skips libraries with fewer than 10 nodes.
pub async fn detect_after_ingestion(state: &AppState, library_id: Uuid) -> Result<()> {
    let pool = &state.persistence.postgres;
    let snapshot = repositories::get_runtime_graph_snapshot(pool, library_id).await?;
    let node_count = snapshot.as_ref().map_or(0, |s| s.node_count);
    if node_count < 10 {
        return Ok(());
    }
    CommunityDetectionService.detect_communities(state, library_id).await
}

// ---------------------------------------------------------------------------
// Label propagation algorithm
// ---------------------------------------------------------------------------

fn run_label_propagation(
    nodes: &[RuntimeGraphNodeRow],
    edges: &[RuntimeGraphEdgeRow],
) -> HashMap<Uuid, usize> {
    // Build adjacency list: node_id -> [(neighbour_id, weight)]
    let mut adj: HashMap<Uuid, Vec<(Uuid, i32)>> = HashMap::new();
    for edge in edges {
        adj.entry(edge.from_node_id).or_default().push((edge.to_node_id, edge.support_count));
        adj.entry(edge.to_node_id).or_default().push((edge.from_node_id, edge.support_count));
    }

    // Initialize: each node in its own community
    let mut community: HashMap<Uuid, usize> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        community.insert(node.id, i);
    }

    // Iterate
    let max_iterations: u64 = 20;
    let mut node_ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();

    for iteration in 0..max_iterations {
        let mut changed = false;

        // Deterministic shuffle based on node id bytes + iteration
        node_ids.sort_by_key(|id| {
            let bytes = id.as_bytes();
            u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])
            .wrapping_add(iteration)
        });

        for &node_id in &node_ids {
            let neighbours = match adj.get(&node_id) {
                Some(n) if !n.is_empty() => n,
                _ => continue,
            };

            // Count weighted votes for each community among neighbours
            let mut votes: HashMap<usize, i32> = HashMap::new();
            for &(neighbour_id, weight) in neighbours {
                if let Some(&comm) = community.get(&neighbour_id) {
                    *votes.entry(comm).or_default() += weight;
                }
            }

            // Pick community with most weighted votes
            if let Some((&best_comm, _)) = votes.iter().max_by_key(|&(_, &v)| v) {
                let current = community[&node_id];
                if best_comm != current {
                    community.insert(node_id, best_comm);
                    changed = true;
                }
            }
        }

        if !changed {
            break;
        }
    }

    community
}

// ---------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------

async fn persist_communities(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    community: &HashMap<Uuid, usize>,
    sizes: &HashMap<usize, usize>,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM runtime_graph_community
         WHERE library_id = $1 AND projection_version = $2",
    )
    .bind(library_id)
    .bind(projection_version)
    .execute(pool)
    .await?;

    let mut members_by_community: HashMap<usize, Vec<Uuid>> = HashMap::new();
    for (&node_id, &community_id) in community {
        members_by_community.entry(community_id).or_default().push(node_id);
    }

    for (&comm_id, &node_count) in sizes {
        let mut member_node_ids = members_by_community.remove(&comm_id).unwrap_or_default();
        member_node_ids.sort();
        if member_node_ids.is_empty() || node_count == 0 {
            continue;
        }

        sqlx::query(
            "INSERT INTO runtime_graph_community
                (library_id, projection_version, member_node_ids)
             VALUES ($1, $2, $3)",
        )
        .bind(library_id)
        .bind(projection_version)
        .bind(&member_node_ids)
        .execute(pool)
        .await?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Community summary generation
// ---------------------------------------------------------------------------

/// Generate deterministic summaries for detected communities using top entities
/// and the relationships between them. Returns the number of communities updated.
pub async fn generate_community_summaries(state: &AppState, library_id: Uuid) -> Result<usize> {
    let pool = &state.persistence.postgres;

    let communities = sqlx::query_as::<_, (Uuid, Vec<Uuid>, i32)>(
        "SELECT id, member_node_ids, cardinality(member_node_ids)::integer AS node_count
         FROM runtime_graph_community
         WHERE library_id = $1
         ORDER BY cardinality(member_node_ids) DESC",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await?;

    if communities.is_empty() {
        return Ok(0);
    }

    let snapshot = repositories::get_runtime_graph_snapshot(pool, library_id).await?;
    let projection_version = match snapshot {
        Some(s) => s.projection_version,
        None => return Ok(0),
    };

    let nodes =
        repositories::list_runtime_graph_nodes_by_library(pool, library_id, projection_version)
            .await?;
    let edges =
        repositories::list_runtime_graph_edges_by_library(pool, library_id, projection_version)
            .await?;

    let node_by_id: HashMap<Uuid, &RuntimeGraphNodeRow> =
        nodes.iter().map(|node| (node.id, node)).collect();

    let mut updated = 0usize;
    for (community_id, member_node_ids, node_count) in &communities {
        let member_set: std::collections::BTreeSet<Uuid> =
            member_node_ids.iter().copied().collect();
        let intra_edges: Vec<_> = edges
            .iter()
            .filter(|e| member_set.contains(&e.from_node_id) && member_set.contains(&e.to_node_id))
            .take(5)
            .collect();

        let mut top_nodes = member_node_ids
            .iter()
            .filter_map(|node_id| node_by_id.get(node_id).copied())
            .collect::<Vec<_>>();
        top_nodes.sort_by_key(|node| std::cmp::Reverse(node.support_count));
        let top_display =
            top_nodes.iter().take(3).map(|node| node.label.as_str()).collect::<Vec<_>>();
        let entity_list = top_display.join(", ");

        let mut summary =
            format!("Community of {node_count} entities centered around {entity_list}.",);

        if !intra_edges.is_empty() {
            let rel_descriptions: Vec<String> = intra_edges
                .iter()
                .map(|e| {
                    let from = node_by_id
                        .get(&e.from_node_id)
                        .map(|node| node.label.as_str())
                        .unwrap_or("?");
                    let to = node_by_id
                        .get(&e.to_node_id)
                        .map(|node| node.label.as_str())
                        .unwrap_or("?");
                    format!("{from} {} {to}", e.relation_type)
                })
                .collect();
            summary.push_str(&format!(" Key relationships: {}.", rel_descriptions.join(", ")));
        }

        sqlx::query(
            "UPDATE runtime_graph_community
             SET summary_text = $1
             WHERE id = $2 AND library_id = $3",
        )
        .bind(&summary)
        .bind(community_id)
        .bind(library_id)
        .execute(pool)
        .await?;

        updated += 1;
    }

    info!(
        library_id = %library_id,
        updated = updated,
        "community summaries generated"
    );

    Ok(updated)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_node(id: Uuid, label: &str, support_count: i32) -> RuntimeGraphNodeRow {
        RuntimeGraphNodeRow {
            id,
            library_id: Uuid::nil(),
            canonical_key: label.to_lowercase(),
            label: label.to_string(),
            node_type: "concept".to_string(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count,
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_edge(from: Uuid, to: Uuid, support_count: i32) -> RuntimeGraphEdgeRow {
        RuntimeGraphEdgeRow {
            id: Uuid::new_v4(),
            library_id: Uuid::nil(),
            from_node_id: from,
            to_node_id: to,
            relation_type: "related_to".to_string(),
            canonical_key: format!("{from}|{to}"),
            summary: None,
            weight: Some(1.0),
            support_count,
            metadata_json: serde_json::json!({}),
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn triangle_forms_single_community() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();

        let nodes =
            vec![make_node(a, "Node A", 3), make_node(b, "Node B", 2), make_node(c, "Node C", 1)];
        let edges = vec![make_edge(a, b, 5), make_edge(b, c, 5), make_edge(a, c, 5)];

        let community = run_label_propagation(&nodes, &edges);

        // All three nodes should be in the same community
        assert_eq!(community[&a], community[&b]);
        assert_eq!(community[&b], community[&c]);
    }

    #[test]
    fn disconnected_components_get_different_communities() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let d = Uuid::new_v4();

        let nodes = vec![
            make_node(a, "Node A", 1),
            make_node(b, "Node B", 1),
            make_node(c, "Node C", 1),
            make_node(d, "Node D", 1),
        ];
        // Two disconnected edges: a-b and c-d
        let edges = vec![make_edge(a, b, 1), make_edge(c, d, 1)];

        let community = run_label_propagation(&nodes, &edges);

        // a and b in one community, c and d in another
        assert_eq!(community[&a], community[&b]);
        assert_eq!(community[&c], community[&d]);
        assert_ne!(community[&a], community[&c]);
    }

    #[test]
    fn isolated_nodes_stay_in_own_community() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        let nodes = vec![make_node(a, "Node A", 1), make_node(b, "Node B", 1)];
        let edges: Vec<RuntimeGraphEdgeRow> = vec![];

        let community = run_label_propagation(&nodes, &edges);

        assert_ne!(community[&a], community[&b]);
    }

    #[test]
    fn empty_graph_returns_empty() {
        let community = run_label_propagation(&[], &[]);
        assert!(community.is_empty());
    }
}
