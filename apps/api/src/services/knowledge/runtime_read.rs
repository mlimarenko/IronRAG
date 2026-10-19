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
    infra::repositories::{
        self, RuntimeGraphProjectionIdentity, RuntimeGraphQueryEdgeRow, RuntimeGraphQueryNodeRow,
    },
    services::knowledge::error::KnowledgeServiceError,
};

use super::snapshot_identity::published_snapshot_identity;

/// Evict idle graph projections after 3 minutes to reclaim RAM for
/// libraries that are no longer actively queried. On large corpora each
/// cached projection can hold substantial graph state, and under peak
/// concurrent load the backend cgroup can hit its memory ceiling. A
/// tighter idle window trades a slightly higher cache-miss rate for a
/// substantially lower steady-state RSS. A fresh load is triggered on the
/// next cache miss.
const PROJECTION_FRESHNESS_TTL: Duration = Duration::from_mins(3);
const PROJECTION_CACHE_MAX_ENTRIES: usize = 8;
const PROJECTION_LOAD_LOCK_MAX_ENTRIES: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct ActiveRuntimeGraphProjection {
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
type RuntimeGraphProjectionEntries = HashMap<RuntimeGraphProjectionIdentity, CacheEntry>;
type RuntimeGraphProjectionLoadLocks = HashMap<RuntimeGraphProjectionIdentity, Arc<Mutex<()>>>;

#[derive(Debug, Default, Clone)]
pub struct RuntimeGraphProjectionCache {
    entries: Arc<RwLock<RuntimeGraphProjectionEntries>>,
    load_locks: Arc<Mutex<RuntimeGraphProjectionLoadLocks>>,
    #[cfg(feature = "test-support")]
    read_pause: Arc<Mutex<Option<LibraryReadPause>>>,
    #[cfg(feature = "test-support")]
    search_read_pause: Arc<Mutex<Option<LibraryReadPause>>>,
}

#[cfg(feature = "test-support")]
#[derive(Debug)]
struct LibraryReadPause {
    library_id: Uuid,
    reached: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

impl RuntimeGraphProjectionCache {
    /// Parks the next final graph read check for this app and library after its
    /// payload has been loaded. Unlike a process-global probe, another fixture
    /// or an unrelated upload cannot consume this one-shot test gate.
    #[cfg(feature = "test-support")]
    pub async fn pause_next_graph_read(
        &self,
        library_id: Uuid,
    ) -> (tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
        install_library_read_pause(&self.read_pause, library_id).await
    }

    #[cfg(feature = "test-support")]
    async fn pause_selected_graph_read(&self, library_id: Uuid) {
        pause_library_read(&self.read_pause, library_id).await;
    }

    /// Parks the next hydrated HTTP search response for this app/library, without
    /// allowing graph prewarm or another fixture to consume the test gate.
    #[cfg(feature = "test-support")]
    pub async fn pause_next_search_read(
        &self,
        library_id: Uuid,
    ) -> (tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
        install_library_read_pause(&self.search_read_pause, library_id).await
    }

    #[cfg(feature = "test-support")]
    pub(crate) async fn pause_selected_search_read(&self, library_id: Uuid) {
        pause_library_read(&self.search_read_pause, library_id).await;
    }

    async fn get(
        &self,
        selected: RuntimeGraphProjectionIdentity,
    ) -> Option<Arc<ActiveRuntimeGraphProjection>> {
        self.entries
            .read()
            .await
            .get(&selected)
            .filter(|entry| entry.inserted_at.elapsed() < PROJECTION_FRESHNESS_TTL)
            .map(|entry| entry.projection.clone())
    }

    async fn insert(
        &self,
        selected: RuntimeGraphProjectionIdentity,
        projection: Arc<ActiveRuntimeGraphProjection>,
    ) {
        let mut guard = self.entries.write().await;
        // Keep one live projection per library and evict any TTL-expired
        // entries from other libraries to reclaim RAM.
        guard.retain(|identity, entry| {
            identity.library_id != selected.library_id
                && entry.inserted_at.elapsed() < PROJECTION_FRESHNESS_TTL
        });
        while guard.len() >= PROJECTION_CACHE_MAX_ENTRIES {
            let Some(oldest) =
                guard.iter().min_by_key(|(_, entry)| entry.inserted_at).map(|(key, _)| *key)
            else {
                break;
            };
            guard.remove(&oldest);
        }
        guard.insert(selected, CacheEntry { projection, inserted_at: Instant::now() });
        drop(guard);

        let mut load_locks = self.load_locks.lock().await;
        load_locks.retain(|identity, _| {
            identity.library_id != selected.library_id || *identity == selected
        });
    }

    async fn load_lock(&self, selected: RuntimeGraphProjectionIdentity) -> Arc<Mutex<()>> {
        let key = selected;
        let mut guard = self.load_locks.lock().await;
        guard.retain(|_, lock| Arc::strong_count(lock) > 1);
        if let Some(lock) = guard.get(&key) {
            return Arc::clone(lock);
        }
        if guard.len() >= PROJECTION_LOAD_LOCK_MAX_ENTRIES {
            // Every retained lock is currently referenced by an active load.
            // Avoid growing the registry; this caller can still load safely,
            // it merely loses singleflight coalescing until capacity returns.
            return Arc::new(Mutex::new(()));
        }
        Arc::clone(guard.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
    }
}

#[cfg(feature = "test-support")]
async fn install_library_read_pause(
    pending: &Mutex<Option<LibraryReadPause>>,
    library_id: Uuid,
) -> (tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
    let (reached_tx, reached) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    *pending.lock().await =
        Some(LibraryReadPause { library_id, reached: reached_tx, release: release_rx });
    (reached, release)
}

#[cfg(feature = "test-support")]
async fn pause_library_read(pending: &Mutex<Option<LibraryReadPause>>, library_id: Uuid) {
    let pause = {
        let mut guard = pending.lock().await;
        if guard.as_ref().is_some_and(|pause| pause.library_id == library_id) {
            guard.take()
        } else {
            None
        }
    };
    if let Some(pause) = pause {
        let _ = pause.reached.send(());
        let _ = pause.release.await;
    }
}

pub(crate) async fn load_active_runtime_graph_projection(
    state: &AppState,
    library_id: Uuid,
) -> Result<Arc<ActiveRuntimeGraphProjection>, KnowledgeServiceError> {
    let (selected, projection) = load_current_graph_projection(state, library_id).await?;
    if let Some(selected) = selected {
        ensure_selected_graph_current(state, selected).await?;
    }
    Ok(projection)
}

pub(crate) fn selected_graph_projection(
    snapshot: &repositories::RuntimeGraphSnapshotRow,
) -> Result<RuntimeGraphProjectionIdentity, KnowledgeServiceError> {
    published_snapshot_identity(snapshot, snapshot.library_id)?;
    Ok(snapshot.projection_identity())
}

pub(crate) async fn ensure_selected_graph_current(
    state: &AppState,
    selected: RuntimeGraphProjectionIdentity,
) -> Result<(), KnowledgeServiceError> {
    #[cfg(feature = "test-support")]
    state.runtime_graph_projection_cache.pause_selected_graph_read(selected.library_id).await;
    if !repositories::content_fingerprint_repository::graph_projection_is_current(
        &state.persistence.postgres,
        selected,
    )
    .await?
    {
        return Err(KnowledgeServiceError::GraphNotReady {
            message: "selected graph projection or its sources are no longer published".to_string(),
        });
    }
    Ok(())
}

async fn load_current_graph_projection(
    state: &AppState,
    library_id: Uuid,
) -> Result<
    (Option<RuntimeGraphProjectionIdentity>, Arc<ActiveRuntimeGraphProjection>),
    KnowledgeServiceError,
> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load runtime graph snapshot")?;
    let Some(snapshot_row) = snapshot else {
        // A library whose graph was never published — empty, still ingesting,
        // or graph extraction disabled — is a routine state, not an integrity
        // breach. Queries proceed over vector and lexical retrieval with an
        // empty graph, exactly like a published-empty snapshot; the graph
        // workbench keeps its own explicit missing-snapshot surface.
        tracing::info!(
            stage = "graph_projection",
            %library_id,
            "no published graph snapshot; serving an empty projection"
        );
        return Ok((None, empty_graph_projection()));
    };
    let selected = selected_graph_projection(&snapshot_row)?;
    if snapshot_row.graph_status == "empty" {
        return Ok((Some(selected), empty_graph_projection()));
    }

    if let Some(cached) =
        cached_graph_projection(state, selected, "runtime graph projection cache hit").await
    {
        return Ok((Some(selected), cached));
    }

    let load_lock = state.runtime_graph_projection_cache.load_lock(selected).await;
    let _load_guard = load_lock.lock().await;
    if let Some(cached) = cached_graph_projection(
        state,
        selected,
        "runtime graph projection cache hit after coalesced load",
    )
    .await
    {
        return Ok((Some(selected), cached));
    }

    let projection = load_graph_projection_from_postgres(state, selected).await?;
    Ok((Some(selected), projection))
}

fn empty_graph_projection() -> Arc<ActiveRuntimeGraphProjection> {
    Arc::new(ActiveRuntimeGraphProjection { nodes: Vec::new(), edges: Vec::new() })
}

async fn cached_graph_projection(
    state: &AppState,
    selected: RuntimeGraphProjectionIdentity,
    cache_hit_message: &str,
) -> Option<Arc<ActiveRuntimeGraphProjection>> {
    let cached = state.runtime_graph_projection_cache.get(selected).await?;
    let RuntimeGraphProjectionIdentity { library_id, projection_version, topology_generation } =
        selected;
    tracing::debug!(
        stage = "graph_projection_cache",
        %library_id,
        projection_version,
        topology_generation,
        node_count = cached.nodes.len(),
        edge_count = cached.edges.len(),
        "{cache_hit_message}"
    );
    Some(cached)
}

async fn load_graph_projection_from_postgres(
    state: &AppState,
    selected: RuntimeGraphProjectionIdentity,
) -> Result<Arc<ActiveRuntimeGraphProjection>, KnowledgeServiceError> {
    let RuntimeGraphProjectionIdentity { library_id, projection_version, topology_generation } =
        selected;
    let load_started = std::time::Instant::now();
    // Use slim query rows (no `metadata_json`, no `identity_key` on edges) to
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
    state.runtime_graph_projection_cache.insert(selected, Arc::clone(&projection)).await;
    Ok(projection)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn snapshot(
        status: &str,
        version: i64,
        generation: i64,
        nodes: i32,
        edges: i32,
    ) -> repositories::RuntimeGraphSnapshotRow {
        repositories::RuntimeGraphSnapshotRow {
            library_id: Uuid::nil(),
            graph_status: status.to_string(),
            projection_version: version,
            topology_generation: generation,
            node_count: nodes,
            edge_count: edges,
            provenance_coverage_percent: None,
            last_built_at: None,
            last_error_message: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn accepts_only_consistent_published_snapshots() {
        assert!(matches!(
            published_snapshot_identity(&snapshot("ready", 4, 2, 1, 0), Uuid::nil()),
            Ok((4, 2))
        ));
        assert!(matches!(
            published_snapshot_identity(&snapshot("empty", 4, 2, 0, 0), Uuid::nil()),
            Ok((4, 2))
        ));
    }

    #[tokio::test]
    async fn projection_cache_does_not_reuse_a_different_selected_generation()
    -> Result<(), KnowledgeServiceError> {
        let cache = RuntimeGraphProjectionCache::default();
        let first = selected_graph_projection(&snapshot("ready", 4, 2, 1, 0))?;
        let successor = selected_graph_projection(&snapshot("ready", 4, 3, 1, 0))?;
        let projection = empty_graph_projection();
        cache.insert(first, Arc::clone(&projection)).await;
        assert!(cache.get(first).await.is_some_and(|cached| Arc::ptr_eq(&cached, &projection)));
        assert!(cache.get(successor).await.is_none());
        let first_lock = cache.load_lock(first).await;
        let successor_lock = cache.load_lock(successor).await;
        assert!(!Arc::ptr_eq(&first_lock, &successor_lock));
        cache.insert(successor, Arc::clone(&projection)).await;
        assert!(cache.get(first).await.is_none());
        assert!(cache.get(successor).await.is_some());
        Ok(())
    }

    #[test]
    fn rejects_unpublished_or_inconsistent_snapshots() {
        for snapshot in [
            snapshot("building", 4, 2, 1, 0),
            snapshot("ready", 0, 2, 1, 0),
            snapshot("ready", 4, 0, 1, 0),
            snapshot("ready", 4, 2, -1, 0),
            snapshot("empty", 4, 2, 1, 0),
        ] {
            assert!(matches!(
                published_snapshot_identity(&snapshot, Uuid::nil()),
                Err(KnowledgeServiceError::GraphIntegrity { .. })
            ));
        }
    }
}
