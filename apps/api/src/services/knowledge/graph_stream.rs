//! Compact, cacheable NDJSON graph topology stream.
//!
//! The graph viewer needs every node and every edge — no truncation, no
//! top-N. The old JSON endpoint shipped the full `RuntimeKnowledgeEntityRow`
//! / `RuntimeKnowledgeRelationRow` shape which is wasteful: long UUIDs are
//! repeated in every edge, metadata_json fields the client never reads are
//! serialized, and nested object envelopes cost bytes. This module produces
//! the canonical compact wire format used by the graph page.
//!
//! Wire format: NDJSON, one JSON object per line. Sections arrive in this
//! strict order:
//!
//! 1. `meta`     — `{ s: "meta", v, library_id, projection_version, generated_at, node_count, edge_count, document_count }`
//! 2. `id_map`   — `{ s: "id_map", m: { "019d..." : 1, "019d..." : 2, ... } }`
//! 3. `docs`     — `{ s: "docs", d: [ { i, k, t, fn? }, ... ] }` (batched)
//! 4. `nodes`    — `{ s: "nodes", d: [ { i, l, t, ts?, s?, c?, es?, a?, sm? }, ... ] }` (batched)
//! 5. `edges`    — `{ s: "edges", d: [ [from, to, rel, support], ... ] }` (batched)
//! 6. `doc_links`— `{ s: "doc_links", d: [ [doc, target, rel, support], ... ] }` (batched)
//! 7. `end`      — `{ s: "end" }`
//!
//! Field key legend:
//!   i  id (u32 assigned by id_map)
//!   l  label
//!   k  canonical_key / document external_key
//!   t  type (entityType / nodeType)
//!   ts sub_type (only when present)
//!   s  support_count (only when > 1)
//!   c  confidence (only when present)
//!   es entity_state (only when not "active")
//!   a  aliases (only when non-empty)
//!   sm summary (only when present)
//!   fn file_name (only when present, for docs)
//!
//! Defaults omitted from wire: client must treat missing as default (empty
//! aliases, null sub_type, "active" state, support_count = 1, null summary).
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
};

const NODE_BATCH: usize = 512;
const EDGE_BATCH: usize = 1024;
const DOC_LINK_BATCH: usize = 1024;
const DOC_BATCH: usize = 512;
const CACHE_TTL_SECONDS: i64 = 24 * 60 * 60;

/// Builds the Redis cache key for one library + projection_version pair.
fn cache_key(library_id: Uuid, projection_version: i64) -> String {
    format!("graph:{library_id}:v{projection_version}")
}

/// Drops the cached graph topology for one library at one specific
/// `projection_version`. The cache key shape
/// `graph:{library_id}:v{projection_version}` means each version has
/// its own key — reads always look up the current version, so older
/// orphaned keys simply age out via the 24 h TTL. Invalidation is
/// therefore scoped: we delete the exact key we know is about to go
/// stale (a newly published `ready` snapshot for this version), and
/// leave any previous-version keys in place where they continue to
/// serve any in-flight reads keyed on the old snapshot. This replaces
/// an earlier wildcard `graph:{lib}:*` DELETE that wiped historical
/// versions on every snapshot upsert and produced visible cache churn
/// during high-throughput targeted refreshes.
pub async fn invalidate_graph_topology_cache(
    redis: &redis::Client,
    library_id: Uuid,
    projection_version: i64,
) -> anyhow::Result<()> {
    let mut conn = redis
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for graph topology cache invalidation")?;
    let key = cache_key(library_id, projection_version);
    let _: i64 = conn.del(&key).await.context("delete graph topology cache key")?;
    Ok(())
}

/// Per-library prewarm state — now tracks both "in-flight" and
/// "another publish arrived while in-flight, run again after" so a
/// burst of publishes never leaves the cache stale.
///
/// Previous behaviour: if a prewarm was already running for a library,
/// the next publish simply skipped — the in-flight task would be
/// reading the projection state from Postgres at some point in the
/// past, so its eventual SET could be older than the latest publish.
/// On heavy-ingest libraries with `content_mutation` jobs firing
/// `ready` 20–30 times a minute, the cache could trail the live
/// graph by one whole debounce cycle.
///
/// New behaviour: if `pending` is already true when a publish arrives,
/// the incoming prewarm still skips (only one task runs), but the
/// running task notices `pending` on exit and re-runs itself. This
/// coalesces bursts into "one run now + one final run at the end",
/// which converges on the latest state without thrashing.
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
/// to Redis under the current `(library_id, projection_version)` key —
/// **bypassing** the `build_graph_topology_bytes` cache check so a
/// prior cached value cannot short-circuit into returning stale bytes
/// during the projection publish window.
///
/// Call site: projection publish path and API boot. Intentionally
/// does not invalidate first — the SET completes atomically in Redis,
/// so concurrent reads either see the old bytes or the new bytes,
/// never an empty window.
///
/// Debounced via [`PREWARM_IN_FLIGHT`] so a burst of publishes only
/// runs one rebuild at a time per library; the tail is dropped because
/// any in-flight rebuild is already reading the latest projection
/// state from Postgres anyway.
pub async fn prewarm_graph_topology_cache(state: &AppState, library_id: Uuid) {
    {
        let mut state_map = prewarm_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    }

    // Loop so coalesced publishes re-run without re-entry. Exit when
    // no pending arrived during the last run. The lock is only held
    // during the short state transition — never across the actual
    // rebuild.
    loop {
        run_prewarm_once(state, library_id).await;

        let mut state_map = prewarm_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state_map.entry(library_id).or_default();
        if entry.pending {
            entry.pending = false;
            entry.running = true;
            // drop lock + loop re-runs without holding it
            continue;
        }
        entry.running = false;
        return;
    }
}

async fn run_prewarm_once(state: &AppState, library_id: Uuid) {
    let snapshot =
        match repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
        {
            Ok(Some(snapshot))
                if snapshot.graph_status != "empty" && snapshot.projection_version > 0 =>
            {
                snapshot
            }
            Ok(_) => {
                tracing::debug!(
                    %library_id,
                    "graph topology cache prewarm skipped: no ready snapshot",
                );
                return;
            }
            Err(error) => {
                tracing::warn!(
                    %library_id,
                    error = format!("{error:#}"),
                    "graph topology cache prewarm: snapshot lookup failed",
                );
                return;
            }
        };

    let projection_version = snapshot.projection_version;
    let built = match build_compact_topology(state, library_id, projection_version).await {
        Ok(topology) => topology,
        Err(error) => {
            tracing::warn!(
                %library_id,
                projection_version,
                error = format!("{error:#}"),
                "graph topology cache prewarm: build failed",
            );
            return;
        }
    };
    let mut buffer = Vec::<u8>::with_capacity(estimated_capacity(&built));
    render_ndjson_into(&mut buffer, &built);
    let cache_key = cache_key(library_id, projection_version);
    if let Err(error) =
        redis_set_bytes(&state.persistence.redis, &cache_key, &buffer, CACHE_TTL_SECONDS).await
    {
        tracing::warn!(
            %library_id,
            projection_version,
            error = format!("{error:#}"),
            "graph topology cache prewarm: SET failed",
        );
        return;
    }
    tracing::info!(
        %library_id,
        projection_version,
        bytes = buffer.len(),
        "graph topology cache prewarmed",
    );
}

/// Builds the full compact NDJSON graph topology for one library and
/// returns it as a single in-memory buffer. Fast path: Redis cache hit
/// returns the stored bytes verbatim. Slow path: loads from
/// Postgres + Arango, renders, and backfills Redis.
///
/// The handler wraps the returned `Vec<u8>` in a regular
/// `Body::from(bytes)` response. We deliberately do NOT use
/// `Body::from_stream` here: the NDJSON is produced in memory in full
/// anyway (documents + entities must be known before id_map can be
/// emitted), and `tower_http::CompressionLayer` currently mis-frames
/// chunked+gzip responses whose body uses `Body::from_stream`, producing
/// `ERR_INCOMPLETE_CHUNKED_ENCODING` in the browser. A single-buffer
/// body is both simpler and correctly compressed.
pub async fn build_graph_topology_bytes(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Vec<u8>> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("load runtime_graph_snapshot for graph topology")?;

    let Some(snapshot) = snapshot else {
        return Ok(render_empty(library_id, 0));
    };

    if snapshot.graph_status == "empty" || snapshot.projection_version <= 0 {
        return Ok(render_empty(library_id, snapshot.projection_version.max(0)));
    }

    let projection_version = snapshot.projection_version;
    let cache_key = cache_key(library_id, projection_version);

    if let Some(cached) =
        redis_get_bytes(&state.persistence.redis, &cache_key).await.unwrap_or_else(|error| {
            tracing::warn!(
                %library_id,
                projection_version,
                error = format!("{error:#}"),
                "graph topology cache GET failed; rebuilding",
            );
            None
        })
    {
        tracing::debug!(
            %library_id,
            projection_version,
            bytes = cached.len(),
            "graph topology cache hit",
        );
        return Ok(cached);
    }

    let built = build_compact_topology(state, library_id, projection_version).await?;
    let mut buffer = Vec::<u8>::with_capacity(estimated_capacity(&built));
    render_ndjson_into(&mut buffer, &built);

    if let Err(error) =
        redis_set_bytes(&state.persistence.redis, &cache_key, &buffer, CACHE_TTL_SECONDS).await
    {
        tracing::warn!(
            %library_id,
            projection_version,
            error = format!("{error:#}"),
            "graph topology cache SET failed",
        );
    }

    Ok(buffer)
}

fn render_empty(library_id: Uuid, projection_version: i64) -> Vec<u8> {
    let built = CompactTopology {
        library_id,
        projection_version,
        generated_at: Utc::now(),
        id_map: HashMap::new(),
        documents: Vec::new(),
        entities: Vec::new(),
        edges: Vec::new(),
        document_links: Vec::new(),
    };
    let mut buffer = Vec::<u8>::with_capacity(256);
    render_ndjson_into(&mut buffer, &built);
    buffer
}

// ---------------------------------------------------------------------------
// Postgres → compact topology
// ---------------------------------------------------------------------------

struct CompactTopology {
    library_id: Uuid,
    projection_version: i64,
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
    canonical_key: String,
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

async fn build_compact_topology(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
) -> anyhow::Result<CompactTopology> {
    // Canonical topology load in 0.3.3:
    //   1. Load the compact edge list (slim columns) in parallel with
    //      the document-link aggregate — both are library-scoped scans.
    //   2. Derive the admitted node id set from the edge endpoints once,
    //      in memory.
    //   3. Fetch only the node rows we need — `document`-type nodes
    //      (always surfaced as graph siloes) plus the admitted ids.
    //   Steps 1+2 parallelise the two slowest queries, step 3 replaces
    //   a query whose CTE previously re-scanned every edge in the
    //   library (8 s on 80 k edges / 40 k nodes) with a single `id =
    //   any($3) or node_type = 'document'` index probe.
    let edges_started_at = std::time::Instant::now();
    let (edge_rows_compact, document_link_rows) = tokio::try_join!(
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
    )?;
    tracing::debug!(
        %library_id,
        projection_version,
        edge_count = edge_rows_compact.len(),
        document_link_count = document_link_rows.len(),
        elapsed_ms = edges_started_at.elapsed().as_millis() as u64,
        "graph topology: edges + document_links loaded",
    );

    let mut admitted_node_ids: HashSet<Uuid> = HashSet::with_capacity(edge_rows_compact.len() * 2);
    for edge in &edge_rows_compact {
        admitted_node_ids.insert(edge.from_node_id);
        admitted_node_ids.insert(edge.to_node_id);
    }
    let admitted_node_ids_vec: Vec<Uuid> = admitted_node_ids.iter().copied().collect();

    let nodes_started_at = std::time::Instant::now();
    let node_rows = repositories::list_runtime_graph_nodes_by_ids_or_document_type(
        &state.persistence.postgres,
        library_id,
        projection_version,
        &admitted_node_ids_vec,
    )
    .await
    .context("load admitted runtime_graph_node rows for topology stream")?;
    tracing::debug!(
        %library_id,
        projection_version,
        node_count = node_rows.len(),
        admitted_ids = admitted_node_ids_vec.len(),
        elapsed_ms = nodes_started_at.elapsed().as_millis() as u64,
        "graph topology: nodes loaded via admitted ids",
    );

    // `document` nodes carry their original content_document.id inside
    // metadata_json.document_id — that is the UUID the frontend uses to
    // navigate to the documents page, so we fetch the Arango title/file
    // payloads keyed by that id.
    let document_node_ids: HashSet<Uuid> =
        node_rows.iter().filter(|row| row.node_type == "document").map(|row| row.id).collect();

    let document_uuid_by_runtime_node: HashMap<Uuid, Uuid> = node_rows
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
        .collect();

    let document_ids: Vec<Uuid> = document_uuid_by_runtime_node
        .values()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let documents = state
        .arango_document_store
        .list_documents_by_ids(&document_ids)
        .await
        .context("load arango knowledge_document rows for topology stream")?;
    let document_by_uuid: HashMap<
        Uuid,
        &crate::infra::arangodb::document_store::KnowledgeDocumentRow,
    > = documents.iter().map(|doc| (doc.document_id, doc)).collect();

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
    let mut compact_edges: Vec<CompactEdge> = Vec::with_capacity(edge_rows_compact.len());
    for row in &edge_rows_compact {
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

    let mut compact_document_links: Vec<CompactDocumentLink> =
        Vec::with_capacity(document_link_rows.len());
    for row in &document_link_rows {
        let document_uuid = match document_by_uuid.get(&row.document_id) {
            Some(_) => row.document_id,
            None => continue,
        };
        let Some(&document_num) = id_map.get(&document_uuid) else { continue };
        let Some(&target_num) = id_map.get(&row.target_node_id) else { continue };
        compact_document_links.push(CompactDocumentLink {
            document: document_num,
            target: target_num,
            relation_type: row.relation_type.clone(),
            support_count: row.support_count,
        });
    }

    // Fill in any document rows that were not yet enriched by Arango (the
    // projection holds the runtime_graph_node for them, but the Arango
    // knowledge_document row may be missing after a failed import — we
    // still want the graph to render the document node). We already
    // allocated ids for every Arango-backed document above; now cover the
    // ones present only as runtime_graph_node `document` rows so they
    // also get a numeric slot the edges and doc_links can reference.
    for row in &node_rows {
        if row.node_type != "document" {
            continue;
        }
        let document_uuid = match document_uuid_by_runtime_node.get(&row.id) {
            Some(uuid) => *uuid,
            None => continue,
        };
        if id_map.contains_key(&document_uuid) {
            continue;
        }
        let num = allocate(document_uuid, &mut id_map, &mut next_num);
        compact_documents.push(CompactDocument {
            num,
            external_key: row.canonical_key.clone(),
            title: Some(row.label.clone()),
            file_name: None,
        });
    }

    Ok(CompactTopology {
        library_id,
        projection_version,
        generated_at: Utc::now(),
        id_map,
        documents: compact_documents,
        entities: compact_entities,
        edges: compact_edges,
        document_links: compact_document_links,
    })
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
        canonical_key: row.canonical_key.clone(),
        entity_type: row.node_type.clone(),
        entity_sub_type,
        summary: row.summary.clone(),
        aliases,
        support_count: row.support_count,
        confidence,
        entity_state,
    }
}

// ---------------------------------------------------------------------------
// Compact topology → NDJSON bytes
// ---------------------------------------------------------------------------

fn estimated_capacity(topology: &CompactTopology) -> usize {
    // Rough upper-bound: 128 bytes per entity, 32 per edge, 64 per doc.
    // Good enough to amortize allocations without over-allocating.
    256 + topology.documents.len() * 160
        + topology.entities.len() * 160
        + topology.edges.len() * 64
        + topology.document_links.len() * 72
        + topology.id_map.len() * 48
}

fn render_ndjson_into(buffer: &mut Vec<u8>, topology: &CompactTopology) {
    push_line(
        buffer,
        &json!({
            "s": "meta",
            "v": 1,
            "library_id": topology.library_id,
            "projection_version": topology.projection_version,
            "generated_at": topology.generated_at.to_rfc3339(),
            "node_count": topology.entities.len() + topology.documents.len(),
            "edge_count": topology.edges.len() + topology.document_links.len(),
            "document_count": topology.documents.len(),
        }),
    );

    // id_map as a single object. 25k UUID→u32 pairs is ~1.2 MB uncompressed,
    // compresses well under gzip/zstd. Sent as one frame so the client can
    // build its reverse map before materializing nodes/edges.
    let mut id_map_object = serde_json::Map::with_capacity(topology.id_map.len());
    for (uuid, num) in &topology.id_map {
        id_map_object.insert(uuid.to_string(), Value::from(*num));
    }
    push_line(buffer, &json!({ "s": "id_map", "m": Value::Object(id_map_object) }));

    for chunk in topology.documents.chunks(DOC_BATCH) {
        let data: Vec<Value> = chunk.iter().map(doc_to_value).collect();
        push_line(buffer, &json!({ "s": "docs", "d": data }));
    }

    for chunk in topology.entities.chunks(NODE_BATCH) {
        let data: Vec<Value> = chunk.iter().map(entity_to_value).collect();
        push_line(buffer, &json!({ "s": "nodes", "d": data }));
    }

    for chunk in topology.edges.chunks(EDGE_BATCH) {
        let data: Vec<Value> = chunk
            .iter()
            .map(|edge| json!([edge.from, edge.to, edge.relation_type, edge.support_count]))
            .collect();
        push_line(buffer, &json!({ "s": "edges", "d": data }));
    }

    for chunk in topology.document_links.chunks(DOC_LINK_BATCH) {
        let data: Vec<Value> = chunk
            .iter()
            .map(|link| json!([link.document, link.target, link.relation_type, link.support_count]))
            .collect();
        push_line(buffer, &json!({ "s": "doc_links", "d": data }));
    }

    push_line(buffer, &json!({ "s": "end" }));
}

fn doc_to_value(doc: &CompactDocument) -> Value {
    // Note: the UUID is NOT repeated here — the id_map frame already
    // binds `doc.num` to the real UUID, so the client reverses the
    // mapping once per payload rather than carrying 36 redundant bytes
    // on every row.
    let mut obj = serde_json::Map::with_capacity(5);
    obj.insert("i".into(), Value::from(doc.num));
    if !doc.external_key.is_empty() {
        obj.insert("k".into(), Value::from(doc.external_key.clone()));
    }
    if let Some(title) = doc.title.as_ref().filter(|value| !value.is_empty()) {
        obj.insert("t".into(), Value::from(title.clone()));
    }
    if let Some(file_name) = doc.file_name.as_ref().filter(|value| !value.is_empty()) {
        obj.insert("fn".into(), Value::from(file_name.clone()));
    }
    Value::Object(obj)
}

fn entity_to_value(entity: &CompactEntity) -> Value {
    let mut obj = serde_json::Map::with_capacity(10);
    obj.insert("i".into(), Value::from(entity.num));
    obj.insert("l".into(), Value::from(entity.label.clone()));
    obj.insert("k".into(), Value::from(entity.canonical_key.clone()));
    obj.insert("t".into(), Value::from(entity.entity_type.clone()));
    if let Some(sub_type) = entity.entity_sub_type.as_ref().filter(|value| !value.is_empty()) {
        obj.insert("ts".into(), Value::from(sub_type.clone()));
    }
    if entity.support_count > 1 {
        obj.insert("s".into(), Value::from(entity.support_count));
    }
    if let Some(confidence) = entity.confidence {
        obj.insert("c".into(), Value::from(confidence));
    }
    if entity.entity_state != "active" {
        obj.insert("es".into(), Value::from(entity.entity_state.clone()));
    }
    if !entity.aliases.is_empty() {
        obj.insert("a".into(), Value::from(entity.aliases.clone()));
    }
    if let Some(summary) = entity.summary.as_ref().filter(|value| !value.is_empty()) {
        obj.insert("sm".into(), Value::from(summary.clone()));
    }
    Value::Object(obj)
}

fn push_line(buffer: &mut Vec<u8>, value: &Value) {
    // Using serde_json::to_writer keeps one allocation-free path into the
    // already reserved Vec<u8>.
    if serde_json::to_writer(&mut *buffer, value).is_ok() {
        buffer.push(b'\n');
    }
}

// ---------------------------------------------------------------------------
// Redis helpers
// ---------------------------------------------------------------------------

/// redis-rs 1.2 defaults the per-command response timeout to 500 ms,
/// which is fine for ~100-byte IR cache reads but blows up on the
/// graph topology payload (17+ MB on a reference-sized library).
/// The prod symptom was `redis SET EX graph topology cache: timed out:
/// timed out` right after every prewarm — the cache appeared populated
/// in the log then disappeared because the SET had already failed at
/// the client. 10 s comfortably covers a 20 MB SET over the bridged
/// Docker network; any slower and something else is wrong.
const TOPOLOGY_CACHE_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn topology_connection_config() -> redis::AsyncConnectionConfig {
    redis::AsyncConnectionConfig::new().set_response_timeout(Some(TOPOLOGY_CACHE_RESPONSE_TIMEOUT))
}

async fn redis_get_bytes(client: &redis::Client, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let mut conn = client
        .get_multiplexed_async_connection_with_config(&topology_connection_config())
        .await
        .context("connect to redis for graph topology cache read")?;
    let value: Option<Vec<u8>> = conn.get(key).await.context("redis GET graph topology cache")?;
    Ok(value)
}

async fn redis_set_bytes(
    client: &redis::Client,
    key: &str,
    value: &[u8],
    ttl_seconds: i64,
) -> anyhow::Result<()> {
    let mut conn = client
        .get_multiplexed_async_connection_with_config(&topology_connection_config())
        .await
        .context("connect to redis for graph topology cache write")?;
    let _: () = conn
        .set_ex(key, value, ttl_seconds.max(1) as u64)
        .await
        .context("redis SET EX graph topology cache")?;
    Ok(())
}
