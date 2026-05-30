use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, RuntimeGraphQueryEdgeRow, RuntimeGraphQueryNodeRow},
    services::knowledge::error::KnowledgeServiceError,
};

/// Evict idle graph projections after 3 minutes to reclaim RAM for
/// libraries that are no longer actively queried. On large corpora each
/// cached projection can hold substantial graph state, and under peak
/// concurrent load the backend cgroup can hit its memory ceiling. A
/// tighter idle window trades a slightly higher cache-miss rate for a
/// substantially lower steady-state RSS. A fresh load is triggered on the
/// next cache miss.
const PROJECTION_FRESHNESS_TTL: Duration = Duration::from_secs(180);

#[derive(Debug, Clone)]
pub struct ActiveRuntimeGraphProjection {
    pub nodes: Vec<RuntimeGraphQueryNodeRow>,
    pub edges: Vec<RuntimeGraphQueryEdgeRow>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    projection: Arc<ActiveRuntimeGraphProjection>,
    inserted_at: Instant,
}

/// In-memory cache of admitted graph projections. Key is the published
/// topology identity `(library_id, projection_version, topology_generation)`;
/// values are `Arc`-shared so multiple concurrent queries can read the same
/// projection without cloning 100k+ rows. Cache is populated lazily by
/// `load_active_runtime_graph_projection` and evicts older versions for the
/// same library on every insert, and idle entries after
/// `PROJECTION_FRESHNESS_TTL` to bound RSS on multi-library deployments.
type RuntimeGraphProjectionEntries = HashMap<(Uuid, i64, i64), CacheEntry>;
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
            .filter(|entry| entry.inserted_at.elapsed() < PROJECTION_FRESHNESS_TTL)
            .map(|entry| entry.projection.clone())
    }

    async fn insert(
        &self,
        library_id: Uuid,
        projection_version: i64,
        topology_generation: i64,
        projection: Arc<ActiveRuntimeGraphProjection>,
    ) {
        let mut guard = self.entries.write().await;
        // Keep one live projection per library and evict any TTL-expired
        // entries from other libraries to reclaim RAM.
        guard.retain(|(lib, _, _), entry| {
            *lib != library_id && entry.inserted_at.elapsed() < PROJECTION_FRESHNESS_TTL
        });
        guard.insert(
            (library_id, projection_version, topology_generation),
            CacheEntry { projection, inserted_at: Instant::now() },
        );

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
) -> Result<Arc<ActiveRuntimeGraphProjection>, KnowledgeServiceError> {
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
    // Use slim query rows (no `metadata_json`, no `canonical_key` on edges) to
    // reduce per-row heap allocations on large corpora. On a 430 k-edge library
    // each dropped `serde_json::Value` is one fewer heap object; the cumulative
    // savings are proportional to the average `metadata_json` payload size
    // (typically 50–500 B/row depending on sub_type population).
    let edges = repositories::list_admitted_runtime_graph_query_edges_by_library(
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
    let nodes = repositories::list_runtime_graph_query_nodes_by_ids_or_document_type(
        &state.persistence.postgres,
        library_id,
        projection_version,
        &connected_node_ids,
    )
    .await
    .context("failed to load admitted runtime graph nodes")?;
    let elapsed_ms = load_started.elapsed().as_millis();

    // Slim rows bypass `canonicalize_runtime_graph_projection` (which takes fat
    // types and re-allocates). The Postgres query already de-dupes by
    // (library_id, canonical_key, projection_version) at upsert time, so
    // duplicate nodes are not expected here; the dedup pass is a no-op in
    // practice. Edge dedup by (from, relation_type, to) is similarly handled at
    // upsert time. We keep the edges as returned and deduplicate only by id to
    // guard against any unexpected Postgres-level races.
    let mut seen_edge_ids = HashSet::with_capacity(edges.len());
    let edges: Vec<_> = edges.into_iter().filter(|e| seen_edge_ids.insert(e.id)).collect();
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
