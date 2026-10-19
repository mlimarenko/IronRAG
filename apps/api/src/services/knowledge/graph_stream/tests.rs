use super::render::{doc_to_value, entity_to_value, push_batched_section, render_compact_topology};
use super::*;

fn snapshot(
    graph_status: &str,
    projection_version: i64,
    topology_generation: i64,
    node_count: i32,
    edge_count: i32,
) -> repositories::RuntimeGraphSnapshotRow {
    repositories::RuntimeGraphSnapshotRow {
        library_id: Uuid::nil(),
        graph_status: graph_status.to_string(),
        projection_version,
        topology_generation,
        node_count,
        edge_count,
        provenance_coverage_percent: None,
        last_built_at: None,
        last_error_message: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn published_snapshot_identity_rejects_invalid_states_and_counts() {
    let library_id = Uuid::nil();
    for invalid in [
        snapshot("building", 1, 1, 0, 0),
        snapshot("ready", 0, 1, 0, 0),
        snapshot("ready", 1, 0, 0, 0),
        snapshot("empty", 1, 1, 1, 0),
        snapshot("empty", 1, 1, 0, 1),
        snapshot("ready", 1, 1, -1, 0),
        snapshot("ready", 1, 1, 0, -1),
    ] {
        assert!(published_snapshot_identity(&invalid, library_id).is_err());
    }
    assert_eq!(
        published_snapshot_identity(&snapshot("ready", 7, 11, 2, 1), library_id)
            .expect("published snapshot identity"),
        (7, 11)
    );
}

#[test]
fn cache_key_includes_topology_generation() {
    let library_id = Uuid::parse_str("019dcb4d-49f4-7cb1-a19b-f9ee0a10c509").expect("valid uuid");

    assert_eq!(cache_key(library_id, 7, 42), "graph:019dcb4d-49f4-7cb1-a19b-f9ee0a10c509:v7:g42");
}

#[test]
fn render_empty_emits_expected_unpublished_topology() {
    let library_id = Uuid::nil();
    let lines: Vec<Value> = String::from_utf8(render_empty(
        library_id,
        INITIAL_PROJECTION_VERSION,
        UNPUBLISHED_TOPOLOGY_GENERATION,
    ))
    .expect("NDJSON is UTF-8")
    .lines()
    .map(|line| serde_json::from_str(line).expect("valid JSON frame"))
    .collect();

    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[0],
        json!({
            "s": "meta",
            "v": 1,
            "library_id": library_id,
            "projection_version": 1,
            "topology_generation": 0,
            "generated_at": lines[0]["generated_at"],
            "node_count": 0,
            "edge_count": 0,
            "document_count": 0,
        })
    );
    assert_eq!(lines[1], json!({ "s": "id_map", "m": {} }));
    assert_eq!(lines[2], json!({ "s": "end" }));
}

#[test]
fn render_compact_topology_preserves_the_compact_ndjson_contract() {
    let library_id = Uuid::nil();
    let document_id = Uuid::from_u128(1);
    let entity_id = Uuid::from_u128(2);
    let topology = CompactTopology {
        library_id,
        projection_version: 3,
        topology_generation: 5,
        generated_at: DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
            .expect("valid timestamp")
            .with_timezone(&Utc),
        id_map: HashMap::from([(document_id, 1), (entity_id, 2)]),
        documents: vec![CompactDocument {
            num: 1,
            external_key: "source-1".into(),
            title: Some("Document title".into()),
            file_name: Some("document.txt".into()),
        }],
        entities: vec![CompactEntity {
            num: 2,
            label: "Entity label".into(),
            identity_key: "entity-key".into(),
            entity_type: "entity".into(),
            entity_sub_type: Some("kind".into()),
            summary: Some("Summary".into()),
            aliases: vec!["Alias".into()],
            support_count: 2,
            confidence: Some(0.75),
            entity_state: "inactive".into(),
        }],
        edges: vec![CompactEdge {
            from: 2,
            to: 2,
            relation_type: "relates_to".into(),
            support_count: 1,
        }],
        document_links: vec![CompactDocumentLink {
            document: 1,
            target: 2,
            relation_type: "supports".into(),
            support_count: 4,
        }],
    };

    let lines: Vec<Value> = String::from_utf8(render_compact_topology(&topology))
        .expect("NDJSON is UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSON frame"))
        .collect();

    assert_eq!(lines.len(), 7);
    assert_eq!(lines[0]["s"], "meta");
    assert_eq!(lines[0]["node_count"], 2);
    assert_eq!(lines[1]["m"][document_id.to_string()], 1);
    assert_eq!(
        lines[2],
        json!({ "s": "docs", "d": [{ "i": 1, "k": "source-1", "t": "Document title", "fn": "document.txt" }] })
    );
    assert_eq!(
        lines[3],
        json!({ "s": "nodes", "d": [{ "i": 2, "l": "Entity label", "k": "entity-key", "t": "entity", "ts": "kind", "s": 2, "c": 0.75, "es": "inactive", "a": ["Alias"], "sm": "Summary" }] })
    );
    assert_eq!(lines[4], json!({ "s": "edges", "d": [[2, 2, "relates_to", 1]] }));
    assert_eq!(lines[5], json!({ "s": "doc_links", "d": [[1, 2, "supports", 4]] }));
    assert_eq!(lines[6], json!({ "s": "end" }));
}

#[test]
fn map_entity_extracts_valid_compact_fields() {
    let row = runtime_node(
        Uuid::from_u128(2),
        "entity",
        json!(["Alias", 1, null]),
        json!({ "sub_type": "kind", "confidence": 0.75, "relation_state": "inactive" }),
    );

    let entity = map_entity(&row, 8);

    assert_eq!(entity.num, 8);
    assert_eq!(entity.aliases, vec!["Alias"]);
    assert_eq!(entity.entity_sub_type.as_deref(), Some("kind"));
    assert_eq!(entity.confidence, Some(0.75));
    assert_eq!(entity.entity_state, "inactive");
}

#[test]
fn runtime_only_documents_receive_ids_without_replacing_known_documents() {
    let known_document_id = Uuid::from_u128(1);
    let runtime_document_id = Uuid::from_u128(2);
    let new_document_id = Uuid::from_u128(3);
    let invalid_document_id = Uuid::from_u128(4);
    let mut id_map = HashMap::from([(known_document_id, 1)]);
    let mut next_num = 2;
    let mut documents = vec![CompactDocument {
        num: 1,
        external_key: "known".into(),
        title: Some("Known document".into()),
        file_name: None,
    }];
    let node_rows = vec![
        runtime_node(
            runtime_document_id,
            "document",
            Value::Null,
            json!({ "document_id": known_document_id }),
        ),
        runtime_node(
            new_document_id,
            "document",
            Value::Null,
            json!({ "document_id": new_document_id }),
        ),
        runtime_node(invalid_document_id, "document", Value::Null, json!({})),
    ];
    let document_uuid_by_runtime_node = HashMap::from([
        (runtime_document_id, known_document_id),
        (new_document_id, new_document_id),
    ]);

    append_runtime_only_documents(
        &node_rows,
        &document_uuid_by_runtime_node,
        &mut id_map,
        &mut next_num,
        &mut documents,
    );

    assert_eq!(id_map.get(&known_document_id), Some(&1));
    assert_eq!(id_map.get(&new_document_id), Some(&2));
    assert_eq!(next_num, 3);
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[1].external_key, "key-3");
    assert_eq!(documents[1].title.as_deref(), Some("Label 3"));
}

#[test]
fn admitted_edge_nodes_include_both_endpoints_once() {
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let third = Uuid::from_u128(3);
    let edges = [
        edge(first, second, "relates_to", 1),
        edge(first, third, "relates_to", 1),
        edge(second, first, "relates_to", 1),
    ];

    let admitted = admitted_edge_node_ids(&edges);

    assert_eq!(admitted.len(), 3);
    assert_eq!(admitted.into_iter().collect::<HashSet<_>>(), HashSet::from([first, second, third]));
    assert!(admitted_edge_node_ids(&[]).is_empty());
}

#[test]
fn runtime_document_map_accepts_only_document_nodes_with_valid_uuid_metadata() {
    let document_id = Uuid::from_u128(100);
    let rows = [
        runtime_node(
            Uuid::from_u128(1),
            "document",
            Value::Null,
            json!({ "document_id": document_id }),
        ),
        runtime_node(
            Uuid::from_u128(2),
            "entity",
            Value::Null,
            json!({ "document_id": document_id }),
        ),
        runtime_node(
            Uuid::from_u128(3),
            "document",
            Value::Null,
            json!({ "document_id": "invalid" }),
        ),
        runtime_node(Uuid::from_u128(4), "document", Value::Null, json!({ "document_id": 42 })),
        runtime_node(Uuid::from_u128(5), "document", Value::Null, json!({})),
    ];

    assert_eq!(
        runtime_document_uuid_map(&rows),
        HashMap::from([(Uuid::from_u128(1), document_id)])
    );
}

#[test]
fn compact_edges_exclude_documents_and_unmapped_endpoints_without_reordering() {
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let document = Uuid::from_u128(3);
    let unmapped = Uuid::from_u128(4);
    let rows = [
        edge(second, first, "first", 7),
        edge(document, first, "document-source", 1),
        edge(first, document, "document-target", 1),
        edge(unmapped, first, "missing-source", 1),
        edge(first, unmapped, "missing-target", 1),
        edge(first, second, "last", 3),
    ];
    let id_map = HashMap::from([(first, 101), (second, 202), (document, 303)]);

    let compact = build_compact_edges(&rows, &HashSet::from([document]), &id_map);

    assert_eq!(
        compact
            .iter()
            .map(|row| (row.from, row.to, row.relation_type.as_str(), row.support_count))
            .collect::<Vec<_>>(),
        vec![(202, 101, "first", 7), (101, 202, "last", 3)],
    );
}

fn edge(
    from_node_id: Uuid,
    to_node_id: Uuid,
    relation_type: &str,
    support_count: i32,
) -> repositories::RuntimeGraphEdgeCompactRow {
    repositories::RuntimeGraphEdgeCompactRow {
        from_node_id,
        to_node_id,
        relation_type: relation_type.to_owned(),
        support_count,
    }
}

fn runtime_node(
    id: Uuid,
    node_type: &str,
    aliases_json: Value,
    metadata_json: Value,
) -> RuntimeGraphNodeRow {
    RuntimeGraphNodeRow {
        id,
        library_id: Uuid::nil(),
        identity_key: format!("key-{}", id.as_u128()),
        label: format!("Label {}", id.as_u128()),
        node_type: node_type.into(),
        aliases_json,
        summary: Some("Summary".into()),
        metadata_json,
        support_count: 1,
        projection_version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn compact_values_omit_defaults_and_invalid_aliases() {
    let entity = CompactEntity {
        num: 2,
        label: "Entity".into(),
        identity_key: "entity".into(),
        entity_type: "entity".into(),
        entity_sub_type: None,
        summary: Some(String::new()),
        aliases: Vec::new(),
        support_count: 1,
        confidence: None,
        entity_state: "active".into(),
    };
    let document = CompactDocument {
        num: 1,
        external_key: String::new(),
        title: Some(String::new()),
        file_name: Some(String::new()),
    };

    assert_eq!(doc_to_value(&document), json!({ "i": 1 }));
    assert_eq!(
        entity_to_value(&entity),
        json!({ "i": 2, "l": "Entity", "k": "entity", "t": "entity" })
    );
}

#[test]
fn batched_sections_keep_all_values_in_order() {
    let mut buffer = Vec::new();

    push_batched_section(&mut buffer, "items", &[1, 2, 3], 2, |value| json!(value));

    let lines: Vec<Value> = String::from_utf8(buffer)
        .expect("NDJSON is UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSON frame"))
        .collect();
    assert_eq!(
        lines,
        vec![json!({ "s": "items", "d": [1, 2] }), json!({ "s": "items", "d": [3] }),]
    );
}
