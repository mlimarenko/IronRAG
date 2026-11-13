//! Complete compact topology materialization for durable background preparation.
//! Output rendering and private artifact streaming live in the child modules.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use super::preparation::GraphTopologyIdentity;
#[cfg(test)]
use super::snapshot_identity::published_snapshot_identity;
use crate::{
    app::state::AppState,
    infra::repositories::{self, RuntimeGraphTopologyNodeRow},
};

pub(crate) mod artifact;
mod render;
#[cfg(test)]
mod tests;

const NODE_BATCH: usize = 512;
const EDGE_BATCH: usize = 1024;
const DOC_LINK_BATCH: usize = 1024;
const DOC_BATCH: usize = 512;

async fn load_compact_topology(
    state: &AppState,
    identity: &GraphTopologyIdentity,
) -> anyhow::Result<CompactTopology> {
    if identity.topology_generation == 0 {
        return Ok(empty_topology(identity.library_id, identity.projection_version, 0));
    }
    build_compact_topology(
        state,
        identity.library_id,
        identity.projection_version,
        identity.topology_generation,
    )
    .await
}

fn empty_topology(
    library_id: Uuid,
    projection_version: i64,
    topology_generation: i64,
) -> CompactTopology {
    CompactTopology {
        library_id,
        projection_version,
        topology_generation,
        generated_at: Utc::now(),
        id_map: HashMap::new(),
        documents: Vec::new(),
        entities: Vec::new(),
        edges: Vec::new(),
        document_links: Vec::new(),
    }
}
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
    let node_rows = repositories::list_runtime_graph_topology_nodes_by_ids_or_document_type(
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

fn runtime_document_uuid_map(node_rows: &[RuntimeGraphTopologyNodeRow]) -> HashMap<Uuid, Uuid> {
    node_rows
        .iter()
        .filter(|row| row.node_type == "document")
        .filter_map(|row| {
            let document_uuid = row
                .document_id_json
                .as_ref()
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
    node_rows: &[RuntimeGraphTopologyNodeRow],
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

fn map_entity(row: &RuntimeGraphTopologyNodeRow, num: u32) -> CompactEntity {
    let aliases = row
        .aliases_json
        .as_array()
        .map(|values| {
            values.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let entity_sub_type = row.sub_type_json.as_ref().and_then(Value::as_str).map(ToOwned::to_owned);

    let confidence = row.confidence_json.as_ref().and_then(Value::as_f64);

    // `entity_state` / `relation_state` live inside metadata_json; the
    // projection admits them as "active" unless downstream lifecycle
    // marks them otherwise. The frontend uses this to annotate inactive
    // nodes; defaulting to "active" keeps the wire frame small.
    let entity_state = row
        .entity_state_json
        .as_ref()
        .and_then(Value::as_str)
        .or_else(|| row.relation_state_json.as_ref().and_then(Value::as_str))
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
