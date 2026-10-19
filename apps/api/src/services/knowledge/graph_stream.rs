//! Compact, cacheable NDJSON graph topology stream.
//!
//! The graph viewer needs every node and every edge — no truncation, no
//! top-N. The old JSON endpoint shipped the full `RuntimeKnowledgeEntityRow`
//! / `RuntimeKnowledgeRelationRow` shape which is wasteful: long UUIDs are
//! repeated in every edge, `metadata_json` fields the client never reads are
//! serialized, and nested object envelopes cost bytes. This module produces
//! the compact wire format used by the graph page.
//!
//! Wire format: NDJSON, one JSON object per line. Sections arrive in this
//! strict order:
//!
//! 1. `meta`     — `{ s: "meta", v, library_id, projection_version, topology_generation, generated_at, node_count, edge_count, document_count }`
//! 2. `id_map`   — `{ s: "id_map", m: { "019d..." : 1, "019d..." : 2, ... } }`
//! 3. `docs`     — `{ s: "docs", d: [ { i, k, t, fn? }, ... ] }` (batched)
//! 4. `nodes`    — `{ s: "nodes", d: [ { i, l, t, ts?, s?, c?, es?, a?, sm? }, ... ] }` (batched)
//! 5. `edges`    — `{ s: "edges", d: [ [from, to, rel, support], ... ] }` (batched)
//! 6. `doc_links`— `{ s: "doc_links", d: [ [doc, target, rel, support], ... ] }` (batched)
//! 7. `end`      — `{ s: "end" }`
//!
//! Field key legend:
//!   i  id (u32 assigned by `id_map`)
//!   l  label
//!   k  `identity_key` / document `external_key`
//!   t  type (entityType / nodeType)
//!   ts `sub_type` (only when present)
//!   s  `support_count` (only when > 1)
//!   c  confidence (only when present)
//!   es `entity_state` (only when not "active")
//!   a  aliases (only when non-empty)
//!   sm summary (only when present)
//!   fn `file_name` (only when present, for docs)
//!
//! Defaults omitted from wire: client must treat missing as default (empty
//! aliases, null `sub_type`, "active" state, `support_count` = 1, null summary).
//! `metadata_json`, `workspace_id`, `library_id`, `contradiction_state`,
//! `freshness_generation`, `created_at`, `updated_at`, `normalized_assertion`
//! are NOT serialized — the frontend never reads them from this endpoint.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use chrono::{DateTime, Utc};
use redis::AsyncCommands;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, RuntimeGraphNodeRow},
    services::knowledge::error::KnowledgeServiceError,
};

mod cache;
mod render;
#[cfg(test)]
mod tests;

use super::runtime_read::{ensure_selected_graph_current, selected_graph_projection};
#[cfg(test)]
use super::snapshot_identity::published_snapshot_identity;
use cache::{CacheWrite, redis_get_bytes, redis_set_bytes};
use render::render_compact_topology;

const NODE_BATCH: usize = 512;
const EDGE_BATCH: usize = 1024;
const DOC_LINK_BATCH: usize = 1024;
const DOC_BATCH: usize = 512;
const CACHE_TTL_SECONDS: i64 = 24 * 60 * 60;
const MAX_CACHE_VALUE_BYTES: usize = 64 * 1024 * 1024;
const INITIAL_PROJECTION_VERSION: i64 = 1;
const UNPUBLISHED_TOPOLOGY_GENERATION: i64 = 0;

/// Builds the Redis cache key for one published graph topology generation.
fn cache_key(library_id: Uuid, projection_version: i64, topology_generation: i64) -> String {
    format!("graph:{library_id}:v{projection_version}:g{topology_generation}")
}

/// Per-library prewarm state — tracks both "in-flight" and
/// "another publish arrived while in-flight, run again after" so a
/// burst of publishes never leaves the cache stale. When `pending` is
/// set on exit, the task re-runs itself, coalescing bursts into
/// "one run now + one final run at the end" without thrashing.
static PREWARM_STATE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<Uuid, PrewarmState>>,
> = std::sync::OnceLock::new();

#[derive(Debug, Default, Clone, Copy)]
struct PrewarmState {
    /// A prewarm task is currently running for this library.
    running: bool,
    /// Another publish arrived while a task was running; when the
    /// current task finishes, it should start one more to pick up
    /// the latest state.
    pending: bool,
}

fn prewarm_state() -> &'static std::sync::Mutex<std::collections::HashMap<Uuid, PrewarmState>> {
    PREWARM_STATE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Forces a fresh build of the NDJSON topology and writes it straight
/// to Redis under the current topology generation key —
/// **bypassing** the `build_graph_topology_bytes` cache check so a
/// prior cached value cannot short-circuit into returning stale bytes
/// during the projection publish window.
///
/// Call site: projection publish path and API boot. Intentionally
/// does not invalidate first — the SET completes atomically in Redis,
/// so concurrent reads either see the old bytes or the new bytes,
/// never an empty window.
///
/// Debounced via `PREWARM_STATE` so a burst of publishes only
/// runs one rebuild at a time per library; the tail is dropped because
/// any in-flight rebuild is already reading the latest projection
/// state from Postgres anyway.
pub async fn prewarm_graph_topology_cache(state: &AppState, library_id: Uuid) {
    {
        let mut state_map =
            prewarm_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state_map.entry(library_id).or_default();
        if entry.running {
            // A task is already running: mark pending so the running task
            // re-runs itself at the end and picks up our publish.
            entry.pending = true;
            tracing::debug!(
                %library_id,
                "graph topology cache prewarm coalesced: existing task will re-run for latest state",
            );
            return;
        }
        entry.running = true;
        entry.pending = false;
        drop(state_map);
    }

    // Loop so coalesced publishes re-run without re-entry. Exit when
    // no pending arrived during the last run. The lock is only held
    // during the short state transition — never across the actual
    // rebuild.
    loop {
        run_prewarm_once(state, library_id).await;

        let mut state_map =
            prewarm_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state_map.entry(library_id).or_default();
        let should_rerun = if entry.pending {
            entry.pending = false;
            entry.running = true;
            true
        } else {
            // No task is active and no tail run is pending. Remove the slot
            // entirely so one-off libraries cannot grow this registry forever.
            state_map.remove(&library_id);
            false
        };
        drop(state_map);
        if should_rerun {
            continue;
        }
        return;
    }
}

async fn run_prewarm_once(state: &AppState, library_id: Uuid) {
    let Some(snapshot) = ready_prewarm_snapshot(state, library_id).await else {
        return;
    };
    let projection_version = snapshot.projection_version;
    let topology_generation = snapshot.topology_generation;
    let Some(buffer) =
        build_prewarm_bytes(state, library_id, projection_version, topology_generation).await
    else {
        return;
    };
    store_prewarm_bytes(state, library_id, projection_version, topology_generation, &buffer).await;
}

fn log_prewarm_snapshot_skip(library_id: Uuid) {
    tracing::debug!(
        %library_id,
        "graph topology cache prewarm skipped: no ready snapshot",
    );
}

fn log_prewarm_snapshot_lookup_failed(library_id: Uuid, error: &impl std::fmt::Display) {
    tracing::warn!(
        %library_id,
        error = format!("{error:#}"),
        "graph topology cache prewarm: snapshot lookup failed",
    );
}

/// Loads the snapshot a prewarm may build from. A missing or non-ready
/// snapshot is a routine skip, not an error.
async fn ready_prewarm_snapshot(
    state: &AppState,
    library_id: Uuid,
) -> Option<repositories::RuntimeGraphSnapshotRow> {
    match repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id).await {
        Ok(Some(snapshot)) if snapshot.graph_status == "ready" => Some(snapshot),
        Ok(_) => {
            log_prewarm_snapshot_skip(library_id);
            None
        }
        Err(error) => {
            log_prewarm_snapshot_lookup_failed(library_id, &error);
            None
        }
    }
}

async fn build_prewarm_bytes(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
) -> Option<Vec<u8>> {
    match build_compact_topology(state, library_id, projection_version, topology_generation).await {
        Ok(topology) => Some(render_compact_topology(&topology)),
        Err(error) => {
            tracing::warn!(
                %library_id,
                projection_version,
                error = format!("{error:#}"),
                "graph topology cache prewarm: build failed",
            );
            None
        }
    }
}

fn log_prewarm_written(
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
    bytes: usize,
) {
    tracing::info!(
        %library_id,
        projection_version,
        topology_generation,
        bytes,
        "graph topology cache prewarmed",
    );
}

fn log_prewarm_skipped_too_large(
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
    bytes: usize,
) {
    tracing::warn!(
        %library_id,
        projection_version,
        topology_generation,
        bytes,
        max_bytes = MAX_CACHE_VALUE_BYTES,
        "graph topology cache prewarm skipped: payload exceeds cache value budget",
    );
}

fn log_prewarm_set_failed(
    library_id: Uuid,
    projection_version: i64,
    error: &impl std::fmt::Display,
) {
    tracing::warn!(
        %library_id,
        projection_version,
        error = format!("{error:#}"),
        "graph topology cache prewarm: SET failed",
    );
}

async fn store_prewarm_bytes(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
    buffer: &[u8],
) {
    let cache_key = cache_key(library_id, projection_version, topology_generation);
    match redis_set_bytes(&state.persistence.redis, &cache_key, buffer, CACHE_TTL_SECONDS).await {
        Ok(CacheWrite::Written) => {
            log_prewarm_written(library_id, projection_version, topology_generation, buffer.len());
        }
        Ok(CacheWrite::SkippedTooLarge) => {
            log_prewarm_skipped_too_large(
                library_id,
                projection_version,
                topology_generation,
                buffer.len(),
            );
        }
        Err(error) => {
            log_prewarm_set_failed(library_id, projection_version, &error);
        }
    }
}

/// Builds the full compact NDJSON graph topology for one library and
/// returns it as a single in-memory buffer. Fast path: Redis cache hit
/// returns the stored bytes verbatim. Slow path: loads from PostgreSQL-backed
/// stores, renders, and backfills Redis.
///
/// The handler wraps the returned `Vec<u8>` in a regular
/// `Body::from(bytes)` response. We deliberately do NOT use
/// `Body::from_stream` here: the NDJSON is produced in memory in full
/// anyway (documents + entities must be known before `id_map` can be
/// emitted), and `tower_http::CompressionLayer` currently mis-frames
/// chunked+gzip responses whose body uses `Body::from_stream`, producing
/// `ERR_INCOMPLETE_CHUNKED_ENCODING` in the browser. A single-buffer
/// body is both simpler and correctly compressed.
pub async fn build_graph_topology_bytes(
    state: &AppState,
    library_id: Uuid,
) -> Result<Vec<u8>, KnowledgeServiceError> {
    let (selected, bytes) = load_graph_topology_bytes(state, library_id).await?;
    if let Some(selected) = selected {
        ensure_selected_graph_current(state, selected).await?;
    }
    Ok(bytes)
}

async fn load_graph_topology_bytes(
    state: &AppState,
    library_id: Uuid,
) -> Result<(Option<repositories::RuntimeGraphProjectionIdentity>, Vec<u8>), KnowledgeServiceError>
{
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("load runtime_graph_snapshot for graph topology")?;

    let Some(snapshot) = snapshot else {
        return Ok((
            None,
            render_empty(library_id, INITIAL_PROJECTION_VERSION, UNPUBLISHED_TOPOLOGY_GENERATION),
        ));
    };

    let selected = selected_graph_projection(&snapshot)?;
    let projection_version = selected.projection_version;
    let topology_generation = selected.topology_generation;
    if snapshot.graph_status == "empty" {
        return Ok((
            Some(selected),
            render_empty(library_id, projection_version, topology_generation),
        ));
    }
    let cache_key = cache_key(library_id, projection_version, topology_generation);

    if let Some(cached) = cached_topology_bytes(
        state,
        library_id,
        &cache_key,
        projection_version,
        topology_generation,
    )
    .await
    {
        return Ok((Some(selected), cached));
    }

    let built =
        build_compact_topology(state, library_id, projection_version, topology_generation).await?;
    let buffer = render_compact_topology(&built);
    backfill_topology_cache(
        state,
        library_id,
        &cache_key,
        projection_version,
        topology_generation,
        &buffer,
    )
    .await;

    Ok((Some(selected), buffer))
}

/// Reads back a cached topology buffer, warning and rebuilding on a Redis
/// failure so a cache outage never fails the endpoint.
async fn cached_topology_bytes(
    state: &AppState,
    library_id: Uuid,
    cache_key: &str,
    projection_version: i64,
    topology_generation: i64,
) -> Option<Vec<u8>> {
    let cached =
        redis_get_bytes(&state.persistence.redis, cache_key).await.unwrap_or_else(|error| {
            tracing::warn!(
                %library_id,
                projection_version,
                topology_generation,
                error = format!("{error:#}"),
                "graph topology cache GET failed; rebuilding",
            );
            None
        })?;
    tracing::debug!(
        %library_id,
        projection_version,
        topology_generation,
        bytes = cached.len(),
        "graph topology cache hit",
    );
    Some(cached)
}

fn log_backfill_skipped_too_large(
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
    bytes: usize,
) {
    tracing::warn!(
        %library_id,
        projection_version,
        topology_generation,
        bytes,
        max_bytes = MAX_CACHE_VALUE_BYTES,
        "graph topology cache backfill skipped: payload exceeds cache value budget",
    );
}

fn log_backfill_set_failed(
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
    error: &impl std::fmt::Display,
) {
    tracing::warn!(
        %library_id,
        projection_version,
        topology_generation,
        error = format!("{error:#}"),
        "graph topology cache SET failed",
    );
}

async fn backfill_topology_cache(
    state: &AppState,
    library_id: Uuid,
    cache_key: &str,
    projection_version: i64,
    topology_generation: i64,
    buffer: &[u8],
) {
    match redis_set_bytes(&state.persistence.redis, cache_key, buffer, CACHE_TTL_SECONDS).await {
        Ok(CacheWrite::Written) => {}
        Ok(CacheWrite::SkippedTooLarge) => {
            log_backfill_skipped_too_large(
                library_id,
                projection_version,
                topology_generation,
                buffer.len(),
            );
        }
        Err(error) => {
            log_backfill_set_failed(library_id, projection_version, topology_generation, &error);
        }
    }
}

fn render_empty(library_id: Uuid, projection_version: i64, topology_generation: i64) -> Vec<u8> {
    let built = CompactTopology {
        library_id,
        projection_version,
        topology_generation,
        generated_at: Utc::now(),
        id_map: HashMap::new(),
        documents: Vec::new(),
        entities: Vec::new(),
        edges: Vec::new(),
        document_links: Vec::new(),
    };
    render_compact_topology(&built)
}

// ---------------------------------------------------------------------------
// Postgres → compact topology
// ---------------------------------------------------------------------------

struct CompactTopology {
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
    generated_at: DateTime<Utc>,
    /// UUID → dense u32 id used in every downstream section.
    id_map: HashMap<Uuid, u32>,
    documents: Vec<CompactDocument>,
    entities: Vec<CompactEntity>,
    edges: Vec<CompactEdge>,
    document_links: Vec<CompactDocumentLink>,
}

struct CompactDocument {
    num: u32,
    external_key: String,
    title: Option<String>,
    file_name: Option<String>,
}

struct CompactEntity {
    num: u32,
    label: String,
    identity_key: String,
    entity_type: String,
    entity_sub_type: Option<String>,
    summary: Option<String>,
    aliases: Vec<String>,
    support_count: i32,
    confidence: Option<f64>,
    entity_state: String,
}

struct CompactEdge {
    from: u32,
    to: u32,
    relation_type: String,
    support_count: i32,
}

struct CompactDocumentLink {
    document: u32,
    target: u32,
    relation_type: String,
    support_count: i64,
}

fn log_topology_edges_loaded(
    library_id: Uuid,
    projection_version: i64,
    edge_count: usize,
    document_link_count: usize,
    elapsed_ms: u64,
) {
    tracing::debug!(
        %library_id,
        projection_version,
        edge_count,
        document_link_count,
        elapsed_ms,
        "graph topology: edges + document_links loaded",
    );
}

fn log_topology_nodes_loaded(
    library_id: Uuid,
    projection_version: i64,
    node_count: usize,
    admitted_ids: usize,
    elapsed_ms: u64,
) {
    tracing::debug!(
        %library_id,
        projection_version,
        node_count,
        admitted_ids,
        elapsed_ms,
        "graph topology: nodes loaded via admitted ids",
    );
}

async fn build_compact_topology(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
) -> anyhow::Result<CompactTopology> {
    // Primary topology load in 0.3.3:
    //   1. Load the compact edge list (slim columns) in parallel with
    //      the document-link aggregate — both are library-scoped scans.
    //   2. Derive the admitted node id set from the edge endpoints once,
    //      in memory.
    //   3. Fetch only the node rows we need — `document`-type nodes
    //      (always surfaced as graph siloes) plus the admitted ids.
    //   Steps 1+2 parallelise the two slowest queries, step 3 uses
    //   a single `id = any($3) or node_type = 'document'` index probe
    //   instead of a full-library CTE edge scan.
    let edges_started_at = std::time::Instant::now();
    let (edge_rows_compact, document_link_rows) =
        load_topology_edges(state, library_id, projection_version).await?;
    log_topology_edges_loaded(
        library_id,
        projection_version,
        edge_rows_compact.len(),
        document_link_rows.len(),
        edges_started_at.elapsed().as_millis() as u64,
    );

    let admitted_node_ids_vec = admitted_edge_node_ids(&edge_rows_compact);

    let nodes_started_at = std::time::Instant::now();
    let node_rows = repositories::list_runtime_graph_nodes_by_ids_or_document_type(
        &state.persistence.postgres,
        library_id,
        projection_version,
        &admitted_node_ids_vec,
    )
    .await
    .context("load admitted runtime_graph_node rows for topology stream")?;
    log_topology_nodes_loaded(
        library_id,
        projection_version,
        node_rows.len(),
        admitted_node_ids_vec.len(),
        nodes_started_at.elapsed().as_millis() as u64,
    );
    // `document` nodes carry their original content_document.id inside
    // metadata_json.document_id — that is the UUID the frontend uses to
    // navigate to the documents page, so we fetch the knowledge-plane
    // title/file payloads keyed by that id.
    let document_node_ids: HashSet<Uuid> =
        node_rows.iter().filter(|row| row.node_type == "document").map(|row| row.id).collect();
    let document_uuid_by_runtime_node = runtime_document_uuid_map(&node_rows);

    let document_ids: Vec<Uuid> = document_uuid_by_runtime_node
        .values()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let documents = state
        .document_store
        .list_documents_by_ids(&document_ids)
        .await
        .context("load knowledge_document rows for topology stream")?;
    let document_by_uuid: HashMap<Uuid, &crate::infra::knowledge_rows::KnowledgeDocumentRow> =
        documents.iter().map(|doc| (doc.document_id, doc)).collect();

    let mut id_map: HashMap<Uuid, u32> = HashMap::with_capacity(node_rows.len() + documents.len());
    let mut next_num: u32 = 1;
    let allocate = |uuid: Uuid, id_map: &mut HashMap<Uuid, u32>, next_num: &mut u32| -> u32 {
        *id_map.entry(uuid).or_insert_with(|| {
            let num = *next_num;
            *next_num += 1;
            num
        })
    };

    // Allocate contiguous ids: documents first, then entities. Edges and
    // doc_links reference the numeric slot.
    let mut compact_documents: Vec<CompactDocument> = Vec::with_capacity(documents.len());
    for doc in &documents {
        let num = allocate(doc.document_id, &mut id_map, &mut next_num);
        compact_documents.push(CompactDocument {
            num,
            external_key: doc.external_key.clone(),
            title: doc.title.clone(),
            file_name: doc.file_name.clone(),
        });
    }

    let mut compact_entities: Vec<CompactEntity> = Vec::with_capacity(node_rows.len());
    for row in &node_rows {
        if row.node_type == "document" {
            continue;
        }
        let num = allocate(row.id, &mut id_map, &mut next_num);
        compact_entities.push(map_entity(row, num));
    }

    // Edges between entity nodes only — document supports are represented
    // via the doc_links section, matching existing frontend semantics.
    let compact_edges = build_compact_edges(&edge_rows_compact, &document_node_ids, &id_map);

    let compact_document_links =
        build_compact_document_links(&document_link_rows, &document_by_uuid, &id_map);

    // Fill in any document rows that were not yet enriched by the knowledge
    // store (the projection holds the runtime_graph_node for them, but the
    // knowledge_document row may be missing after a failed import — we still
    // want the graph to render the document node). We already allocated ids for
    // every knowledge-backed document above; now cover the ones present only as
    // runtime_graph_node `document` rows so they also get a numeric slot the
    // edges and doc_links can reference.
    append_runtime_only_documents(
        &node_rows,
        &document_uuid_by_runtime_node,
        &mut id_map,
        &mut next_num,
        &mut compact_documents,
    );

    Ok(CompactTopology {
        library_id,
        projection_version,
        topology_generation,
        generated_at: Utc::now(),
        id_map,
        documents: compact_documents,
        entities: compact_entities,
        edges: compact_edges,
        document_links: compact_document_links,
    })
}

async fn load_topology_edges(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
) -> anyhow::Result<(
    Vec<repositories::RuntimeGraphEdgeCompactRow>,
    Vec<repositories::RuntimeGraphDocumentLinkRow>,
)> {
    tokio::try_join!(
        async {
            repositories::list_admitted_runtime_graph_edges_compact_by_library(
                &state.persistence.postgres,
                library_id,
                projection_version,
            )
            .await
            .context("load admitted runtime_graph_edge rows for topology stream")
        },
        async {
            repositories::list_runtime_graph_document_links_by_library(
                &state.persistence.postgres,
                library_id,
                projection_version,
            )
            .await
            .context("load runtime_graph_document_link rows for topology stream")
        },
    )
}

fn admitted_edge_node_ids(edges: &[repositories::RuntimeGraphEdgeCompactRow]) -> Vec<Uuid> {
    let mut admitted_node_ids: HashSet<Uuid> = HashSet::with_capacity(edges.len() * 2);
    for edge in edges {
        admitted_node_ids.insert(edge.from_node_id);
        admitted_node_ids.insert(edge.to_node_id);
    }
    admitted_node_ids.into_iter().collect()
}

fn runtime_document_uuid_map(node_rows: &[RuntimeGraphNodeRow]) -> HashMap<Uuid, Uuid> {
    node_rows
        .iter()
        .filter(|row| row.node_type == "document")
        .filter_map(|row| {
            let document_uuid = row
                .metadata_json
                .get("document_id")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())?;
            Some((row.id, document_uuid))
        })
        .collect()
}

fn build_compact_edges(
    edge_rows_compact: &[repositories::RuntimeGraphEdgeCompactRow],
    document_node_ids: &HashSet<Uuid>,
    id_map: &HashMap<Uuid, u32>,
) -> Vec<CompactEdge> {
    let mut compact_edges: Vec<CompactEdge> = Vec::with_capacity(edge_rows_compact.len());
    for row in edge_rows_compact {
        if document_node_ids.contains(&row.from_node_id)
            || document_node_ids.contains(&row.to_node_id)
        {
            continue;
        }
        let (Some(&from), Some(&to)) = (id_map.get(&row.from_node_id), id_map.get(&row.to_node_id))
        else {
            continue;
        };
        compact_edges.push(CompactEdge {
            from,
            to,
            relation_type: row.relation_type.clone(),
            support_count: row.support_count,
        });
    }
    compact_edges
}

fn build_compact_document_links(
    document_link_rows: &[repositories::RuntimeGraphDocumentLinkRow],
    document_by_uuid: &HashMap<Uuid, &crate::infra::knowledge_rows::KnowledgeDocumentRow>,
    id_map: &HashMap<Uuid, u32>,
) -> Vec<CompactDocumentLink> {
    let mut compact_document_links: Vec<CompactDocumentLink> =
        Vec::with_capacity(document_link_rows.len());
    for row in document_link_rows {
        let document_uuid = match document_by_uuid.get(&row.document_id) {
            Some(_) => row.document_id,
            None => continue,
        };
        let Some(&document_num) = id_map.get(&document_uuid) else {
            continue;
        };
        let Some(&target_num) = id_map.get(&row.target_node_id) else {
            continue;
        };
        compact_document_links.push(CompactDocumentLink {
            document: document_num,
            target: target_num,
            relation_type: row.relation_type.clone(),
            support_count: row.support_count,
        });
    }
    compact_document_links
}

fn append_runtime_only_documents(
    node_rows: &[RuntimeGraphNodeRow],
    document_uuid_by_runtime_node: &HashMap<Uuid, Uuid>,
    id_map: &mut HashMap<Uuid, u32>,
    next_num: &mut u32,
    documents: &mut Vec<CompactDocument>,
) {
    for row in node_rows.iter().filter(|row| row.node_type == "document") {
        let Some(&document_uuid) = document_uuid_by_runtime_node.get(&row.id) else {
            continue;
        };
        if id_map.contains_key(&document_uuid) {
            continue;
        }
        let num = *next_num;
        *next_num += 1;
        id_map.insert(document_uuid, num);
        documents.push(CompactDocument {
            num,
            external_key: row.identity_key.clone(),
            title: Some(row.label.clone()),
            file_name: None,
        });
    }
}

fn map_entity(row: &RuntimeGraphNodeRow, num: u32) -> CompactEntity {
    let aliases = row
        .aliases_json
        .as_array()
        .map(|values| {
            values.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let entity_sub_type =
        row.metadata_json.get("sub_type").and_then(Value::as_str).map(ToOwned::to_owned);

    let confidence = row.metadata_json.get("confidence").and_then(Value::as_f64);

    // `entity_state` / `relation_state` live inside metadata_json; the
    // projection admits them as "active" unless downstream lifecycle
    // marks them otherwise. The frontend uses this to annotate inactive
    // nodes; defaulting to "active" keeps the wire frame small.
    let entity_state = row
        .metadata_json
        .get("entity_state")
        .and_then(Value::as_str)
        .or_else(|| row.metadata_json.get("relation_state").and_then(Value::as_str))
        .unwrap_or("active")
        .to_string();

    CompactEntity {
        num,
        label: row.label.clone(),
        identity_key: row.identity_key.clone(),
        entity_type: row.node_type.clone(),
        entity_sub_type,
        summary: row.summary.clone(),
        aliases,
        support_count: row.support_count,
        confidence,
        entity_state,
    }
}
