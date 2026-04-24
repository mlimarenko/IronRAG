use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
};

#[derive(Debug, Clone)]
pub struct ActiveRuntimeGraphProjection {
    pub nodes: Vec<RuntimeGraphNodeRow>,
    pub edges: Vec<RuntimeGraphEdgeRow>,
}

/// In-memory cache of admitted graph projections. Key is
/// `(library_id, projection_version)`; values are `Arc`-shared so
/// multiple concurrent queries can read the same projection without
/// cloning 100k+ rows. Cache is populated lazily by
/// `load_active_runtime_graph_projection` and evicts older versions
/// for the same library on every miss, which keeps the working set
/// bounded by `active libraries × 1 current version`.
type RuntimeGraphProjectionEntries = HashMap<(Uuid, i64), Arc<ActiveRuntimeGraphProjection>>;

#[derive(Debug, Default, Clone)]
pub struct RuntimeGraphProjectionCache {
    entries: Arc<RwLock<RuntimeGraphProjectionEntries>>,
}

impl RuntimeGraphProjectionCache {
    async fn get(
        &self,
        library_id: Uuid,
        projection_version: i64,
    ) -> Option<Arc<ActiveRuntimeGraphProjection>> {
        self.entries.read().await.get(&(library_id, projection_version)).cloned()
    }

    async fn insert(
        &self,
        library_id: Uuid,
        projection_version: i64,
        projection: Arc<ActiveRuntimeGraphProjection>,
    ) {
        let mut guard = self.entries.write().await;
        // Evict any older versions for the same library — the
        // projection version increments monotonically on every
        // rebuild, so we can drop entries with a lower version
        // safely (no concurrent caller should be reading them).
        guard.retain(|(lib, version), _| *lib != library_id || *version == projection_version);
        guard.insert((library_id, projection_version), projection);
    }
}

pub async fn load_active_runtime_graph_projection(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Arc<ActiveRuntimeGraphProjection>> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load runtime graph snapshot")?;
    let Some(snapshot_row) = snapshot else {
        return Ok(Arc::new(ActiveRuntimeGraphProjection { nodes: Vec::new(), edges: Vec::new() }));
    };

    let projection_version = snapshot_row.projection_version.max(1);
    if snapshot_row.graph_status == "empty"
        || (snapshot_row.node_count <= 0 && snapshot_row.edge_count <= 0)
    {
        return Ok(Arc::new(ActiveRuntimeGraphProjection { nodes: Vec::new(), edges: Vec::new() }));
    }

    if let Some(cached) =
        state.runtime_graph_projection_cache.get(library_id, projection_version).await
    {
        tracing::debug!(
            stage = "graph_projection_cache",
            %library_id,
            projection_version,
            node_count = cached.nodes.len(),
            edge_count = cached.edges.len(),
            "runtime graph projection cache hit"
        );
        return Ok(cached);
    }

    let load_started = std::time::Instant::now();
    // Nodes + edges are fully independent Postgres reads; running them
    // sequentially (the original pattern) cost about sum(per-query ms)
    // ≈ 11 s on the reference library cache miss. `try_join!` pins the
    // load at max(nodes_ms, edges_ms) ≈ 6–7 s. Caller caches the
    // `Arc<ActiveRuntimeGraphProjection>` keyed by projection_version,
    // so the savings compound across every turn that hits the same
    // version (~every turn between projection publishes on prod).
    let (nodes_result, edges_result) = tokio::join!(
        repositories::list_admitted_runtime_graph_nodes_by_library(
            &state.persistence.postgres,
            library_id,
            projection_version,
        ),
        repositories::list_admitted_runtime_graph_edges_by_library(
            &state.persistence.postgres,
            library_id,
            projection_version,
        ),
    );
    let nodes = nodes_result.context("failed to load admitted runtime graph nodes")?;
    let edges = edges_result.context("failed to load admitted runtime graph edges")?;
    let elapsed_ms = load_started.elapsed().as_millis();

    let projection = Arc::new(ActiveRuntimeGraphProjection { nodes, edges });
    tracing::info!(
        stage = "graph_projection_cache",
        %library_id,
        projection_version,
        node_count = projection.nodes.len(),
        edge_count = projection.edges.len(),
        elapsed_ms,
        "runtime graph projection loaded from Postgres (cache miss)"
    );
    state
        .runtime_graph_projection_cache
        .insert(library_id, projection_version, Arc::clone(&projection))
        .await;
    Ok(projection)
}
