use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Context;
use tokio::sync::{Mutex, RwLock};
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

/// In-memory cache of admitted graph projections. Key is the published
/// topology identity `(library_id, projection_version, topology_generation)`;
/// values are `Arc`-shared so
/// multiple concurrent queries can read the same projection without
/// cloning 100k+ rows. Cache is populated lazily by
/// `load_active_runtime_graph_projection` and evicts older versions
/// for the same library on every miss, which keeps the working set
/// bounded by `active libraries × 1 current version`.
type RuntimeGraphProjectionEntries = HashMap<(Uuid, i64, i64), Arc<ActiveRuntimeGraphProjection>>;
type RuntimeGraphProjectionLoadLocks = HashMap<(Uuid, i64, i64), Arc<Mutex<()>>>;

#[derive(Debug, Default, Clone)]
pub struct RuntimeGraphProjectionCache {
    entries: Arc<RwLock<RuntimeGraphProjectionEntries>>,
    load_locks: Arc<Mutex<RuntimeGraphProjectionLoadLocks>>,
}

impl RuntimeGraphProjectionCache {
    async fn get(
        &self,
        library_id: Uuid,
        projection_version: i64,
        topology_generation: i64,
    ) -> Option<Arc<ActiveRuntimeGraphProjection>> {
        self.entries
            .read()
            .await
            .get(&(library_id, projection_version, topology_generation))
            .cloned()
    }

    async fn insert(
        &self,
        library_id: Uuid,
        projection_version: i64,
        topology_generation: i64,
        projection: Arc<ActiveRuntimeGraphProjection>,
    ) {
        let mut guard = self.entries.write().await;
        // Keep one live projection per library. The published topology
        // identity includes generation because targeted refreshes mutate
        // the active projection without changing projection_version.
        guard.retain(|(lib, _, _), _| *lib != library_id);
        guard.insert((library_id, projection_version, topology_generation), projection);

        let mut load_locks = self.load_locks.lock().await;
        load_locks.retain(|(lib, version, generation), _| {
            *lib != library_id
                || (*version == projection_version && *generation == topology_generation)
        });
    }

    async fn load_lock(
        &self,
        library_id: Uuid,
        projection_version: i64,
        topology_generation: i64,
    ) -> Arc<Mutex<()>> {
        let key = (library_id, projection_version, topology_generation);
        let mut guard = self.load_locks.lock().await;
        Arc::clone(guard.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
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
    let topology_generation = snapshot_row.topology_generation.max(0);
    if snapshot_row.graph_status == "empty"
        || (snapshot_row.node_count <= 0 && snapshot_row.edge_count <= 0)
    {
        return Ok(Arc::new(ActiveRuntimeGraphProjection { nodes: Vec::new(), edges: Vec::new() }));
    }

    if let Some(cached) = state
        .runtime_graph_projection_cache
        .get(library_id, projection_version, topology_generation)
        .await
    {
        tracing::debug!(
            stage = "graph_projection_cache",
            %library_id,
            projection_version,
            topology_generation,
            node_count = cached.nodes.len(),
            edge_count = cached.edges.len(),
            "runtime graph projection cache hit"
        );
        return Ok(cached);
    }

    let load_lock = state
        .runtime_graph_projection_cache
        .load_lock(library_id, projection_version, topology_generation)
        .await;
    let _load_guard = load_lock.lock().await;
    if let Some(cached) = state
        .runtime_graph_projection_cache
        .get(library_id, projection_version, topology_generation)
        .await
    {
        tracing::debug!(
            stage = "graph_projection_cache",
            %library_id,
            projection_version,
            topology_generation,
            node_count = cached.nodes.len(),
            edge_count = cached.edges.len(),
            "runtime graph projection cache hit after coalesced load"
        );
        return Ok(cached);
    }

    let load_started = std::time::Instant::now();
    let edges = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load admitted runtime graph edges")?;
    let mut connected_node_ids = HashSet::with_capacity(edges.len().saturating_mul(2));
    for edge in &edges {
        connected_node_ids.insert(edge.from_node_id);
        connected_node_ids.insert(edge.to_node_id);
    }
    let connected_node_ids: Vec<Uuid> = connected_node_ids.into_iter().collect();
    let nodes = repositories::list_runtime_graph_nodes_by_ids_or_document_type(
        &state.persistence.postgres,
        library_id,
        projection_version,
        &connected_node_ids,
    )
    .await
    .context("failed to load admitted runtime graph nodes")?;
    let elapsed_ms = load_started.elapsed().as_millis();

    let projection = Arc::new(ActiveRuntimeGraphProjection { nodes, edges });
    tracing::info!(
        stage = "graph_projection_cache",
        %library_id,
        projection_version,
        topology_generation,
        node_count = projection.nodes.len(),
        edge_count = projection.edges.len(),
        elapsed_ms,
        "runtime graph projection loaded from Postgres (cache miss)"
    );
    state
        .runtime_graph_projection_cache
        .insert(library_id, projection_version, topology_generation, Arc::clone(&projection))
        .await;
    Ok(projection)
}
