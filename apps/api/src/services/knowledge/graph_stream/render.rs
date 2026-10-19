use super::*;

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

pub(super) fn render_compact_topology(topology: &CompactTopology) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(estimated_capacity(topology));
    render_ndjson_into(&mut buffer, topology);
    buffer
}

fn render_ndjson_into(buffer: &mut Vec<u8>, topology: &CompactTopology) {
    push_line(
        buffer,
        &json!({
            "s": "meta",
            "v": 1,
            "library_id": topology.library_id,
            "projection_version": topology.projection_version,
            "topology_generation": topology.topology_generation,
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

    push_batched_section(buffer, "docs", &topology.documents, DOC_BATCH, doc_to_value);
    push_batched_section(buffer, "nodes", &topology.entities, NODE_BATCH, entity_to_value);
    push_batched_section(buffer, "edges", &topology.edges, EDGE_BATCH, |edge| {
        json!([edge.from, edge.to, edge.relation_type, edge.support_count])
    });
    push_batched_section(buffer, "doc_links", &topology.document_links, DOC_LINK_BATCH, |link| {
        json!([link.document, link.target, link.relation_type, link.support_count])
    });

    push_line(buffer, &json!({ "s": "end" }));
}

pub(super) fn push_batched_section<T>(
    buffer: &mut Vec<u8>,
    section: &str,
    values: &[T],
    batch_size: usize,
    to_value: impl Fn(&T) -> Value,
) {
    for chunk in values.chunks(batch_size) {
        let data: Vec<Value> = chunk.iter().map(&to_value).collect();
        push_line(buffer, &json!({ "s": section, "d": data }));
    }
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

fn push_line(buffer: &mut Vec<u8>, value: &Value) {
    // Using serde_json::to_writer keeps one allocation-free path into the
    // already reserved Vec<u8>.
    if serde_json::to_writer(&mut *buffer, value).is_ok() {
        buffer.push(b'\n');
    }
}
