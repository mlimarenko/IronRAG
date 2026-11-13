use super::*;
use crate::services::knowledge::preparation::GraphTopologyIdentity;
use std::io::{self, Write};

// ---------------------------------------------------------------------------
// Compact topology → NDJSON bytes
// ---------------------------------------------------------------------------

pub(super) fn render_ndjson_into(
    buffer: &mut impl Write,
    topology: &CompactTopology,
    identity: &GraphTopologyIdentity,
    preparation_id: Uuid,
) -> io::Result<()> {
    push_line(
        buffer,
        &json!({
            "s": "meta",
            "v": 1,
            "library_id": topology.library_id,
            "preparation_id": preparation_id,
            "source_truth_version": identity.source_truth_version,
            "projection_version": topology.projection_version,
            "topology_generation": topology.topology_generation,
            "generated_at": topology.generated_at.to_rfc3339(),
            "node_count": topology.entities.len() + topology.documents.len(),
            "edge_count": topology.edges.len() + topology.document_links.len(),
            "document_count": topology.documents.len(),
        }),
    )?;

    let mut id_map_object = serde_json::Map::with_capacity(1024);
    for (uuid, num) in &topology.id_map {
        id_map_object.insert(uuid.to_string(), Value::from(*num));
        if id_map_object.len() == 1024 {
            push_line(buffer, &json!({ "s": "id_map", "m": std::mem::take(&mut id_map_object) }))?;
        }
    }
    if !id_map_object.is_empty() || topology.id_map.is_empty() {
        push_line(buffer, &json!({ "s": "id_map", "m": Value::Object(id_map_object) }))?;
    }

    push_batched_section(buffer, "docs", &topology.documents, DOC_BATCH, doc_to_value)?;
    push_batched_section(buffer, "nodes", &topology.entities, NODE_BATCH, entity_to_value)?;
    push_batched_section(buffer, "edges", &topology.edges, EDGE_BATCH, |edge| {
        json!([edge.from, edge.to, edge.relation_type, edge.support_count])
    })?;
    push_batched_section(buffer, "doc_links", &topology.document_links, DOC_LINK_BATCH, |link| {
        json!([link.document, link.target, link.relation_type, link.support_count])
    })?;

    push_line(
        buffer,
        &json!({ "s": "end", "preparation_id": preparation_id,
        "node_count": topology.entities.len() + topology.documents.len(),
        "edge_count": topology.edges.len() + topology.document_links.len(),
        "document_count": topology.documents.len() }),
    )
}

pub(super) fn push_batched_section<T>(
    buffer: &mut impl Write,
    section: &str,
    values: &[T],
    batch_size: usize,
    to_value: impl Fn(&T) -> Value,
) -> io::Result<()> {
    for chunk in values.chunks(batch_size) {
        let data: Vec<Value> = chunk.iter().map(&to_value).collect();
        push_line(buffer, &json!({ "s": section, "d": data }))?;
    }
    Ok(())
}

pub(super) fn doc_to_value(doc: &CompactDocument) -> Value {
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

pub(super) fn entity_to_value(entity: &CompactEntity) -> Value {
    let mut obj = serde_json::Map::with_capacity(10);
    obj.insert("i".into(), Value::from(entity.num));
    obj.insert("l".into(), Value::from(entity.label.clone()));
    obj.insert("k".into(), Value::from(entity.identity_key.clone()));
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

fn push_line(buffer: &mut impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *buffer, value)?;
    buffer.write_all(b"\n")
}
