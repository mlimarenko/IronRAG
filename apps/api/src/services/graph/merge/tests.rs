use super::persistence::graph_edge_integrity_skip_reason;
use super::*;
use crate::domains::runtime_graph::RuntimeNodeType;
use crate::services::graph::extract::GraphEntityCandidate;

#[test]
fn normalizes_aliases_and_deduplicates_them() {
    let aliases = normalize_graph_aliases("OpenAI", &["OpenAI".into(), " Open AI ".into()]);
    assert_eq!(aliases, vec!["Open AI".to_string(), "OpenAI".to_string()]);
}

#[test]
fn normalizes_relation_type_to_snake_case() {
    assert_eq!(
        crate::services::graph::identity::normalize_relation_type("mentions_in"),
        "mentions_in"
    );
}

#[test]
fn builds_graph_edge_identity_key_from_nodes_and_relation() {
    assert_eq!(
        crate::services::graph::identity::graph_edge_identity_key(
            "document:1",
            "mentions_in",
            "entity:openai"
        ),
        "document:1--mentions_in--entity:openai"
    );
}

#[test]
fn changed_node_ids_include_edge_endpoints() {
    let source_id = uuid::Uuid::now_v7();
    let target_id = uuid::Uuid::now_v7();
    let outcome = GraphMergeOutcome {
        nodes: vec![],
        edges: vec![RuntimeGraphEdgeRow {
            id: uuid::Uuid::now_v7(),
            library_id: uuid::Uuid::now_v7(),
            from_node_id: source_id,
            to_node_id: target_id,
            relation_type: "depends_on".to_string(),
            identity_key: "entity:a--depends_on--entity:b".to_string(),
            summary: None,
            weight: None,
            support_count: 1,
            metadata_json: serde_json::json!({}),
            projection_version: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }],
    };

    let changed = outcome.changed_node_ids();

    assert!(changed.contains(&source_id));
    assert!(changed.contains(&target_id));
}

#[test]
fn normalizes_labels_to_graph_safe_slug() {
    assert_eq!(
        crate::services::graph::identity::normalize_graph_identity_component(
            " Knowledge   Graph 2.0 "
        ),
        "knowledge_graph_2_0"
    );
}

#[test]
fn normalizes_non_ascii_labels_to_graph_safe_slug() {
    assert_eq!(
        crate::services::graph::identity::normalize_graph_identity_component(
            " Acme Imprenta Düsseldorf "
        ),
        "acme_imprenta_düsseldorf"
    );
}

#[test]
fn normalizes_mixed_script_labels_to_graph_safe_slug() {
    assert_eq!(
        crate::services::graph::identity::normalize_graph_identity_component(
            " Acme: Receipt V2 / QR "
        ),
        "acme_receipt_v2_qr"
    );
}

#[test]
fn rejects_invalid_non_ascii_relation_types() {
    assert!(
        crate::services::graph::identity::normalize_relation_type(" είναι μέρος του ").is_empty()
    );
}

#[test]
fn build_entity_key_index_prefers_entity_over_topic_for_same_label() {
    let candidates = GraphExtractionCandidateSet {
        entities: vec![
            GraphEntityCandidate {
                label: "Register".to_string(),
                node_type: RuntimeNodeType::Concept,
                sub_type: None,
                aliases: vec![],
                summary: None,
            },
            GraphEntityCandidate {
                label: "Register".to_string(),
                node_type: RuntimeNodeType::Entity,
                sub_type: None,
                aliases: vec![],
                summary: None,
            },
        ],
        relations: vec![],
    };

    let index = build_entity_key_index(&candidates);

    assert_eq!(index.node_identity_key_for_label("Register"), "entity:register");
}

#[test]
fn merge_graph_quality_metadata_tracks_summary_fragments() {
    let metadata = merge_graph_quality_metadata(None, None, Some("Budget approval moved to Q2."));

    assert_eq!(metadata["summary_fragments"], serde_json::json!(["Budget approval moved to Q2."]));
}

#[test]
fn filtered_relation_preserves_endpoint_evidence() {
    let library_id = uuid::Uuid::now_v7();
    let source_node = test_graph_node(library_id, "entity:source", "Source");
    let target_node = test_graph_node(library_id, "entity:target", "Target");
    let document_node = test_graph_node(library_id, "document:source", "Document");
    let mut accumulator = MergeAccumulator::new(document_node);

    accumulator.record_relation_result(RelationMergeResult::Filtered {
        source_node: Box::new(source_node.clone()),
        target_node: Box::new(target_node.clone()),
    });

    assert_eq!(
        accumulator.nodes.iter().skip(1).map(|node| node.id).collect::<Vec<_>>(),
        vec![source_node.id, target_node.id]
    );
    assert_eq!(accumulator.evidence_count, 3);
    assert_eq!(accumulator.filtered_artifact_count, 1);
    assert_eq!(
        accumulator
            .evidence_targets
            .iter()
            .filter(|target| target.evidence_context_key.starts_with("relation_"))
            .map(|target| target.target_id)
            .collect::<Vec<_>>(),
        vec![source_node.id, target_node.id]
    );
}

fn test_graph_node(library_id: uuid::Uuid, identity_key: &str, label: &str) -> RuntimeGraphNodeRow {
    RuntimeGraphNodeRow {
        id: uuid::Uuid::now_v7(),
        library_id,
        identity_key: identity_key.to_string(),
        label: label.to_string(),
        node_type: "entity".to_string(),
        aliases_json: serde_json::json!([]),
        summary: None,
        metadata_json: serde_json::json!({}),
        support_count: 1,
        projection_version: 1,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn flags_projection_version_mismatch_as_graph_integrity_skip() {
    let scope = GraphMergeScope::new(uuid::Uuid::now_v7(), 4);
    let from_node = RuntimeGraphNodeRow {
        id: uuid::Uuid::now_v7(),
        library_id: scope.library_id,
        identity_key: "entity:a".to_string(),
        label: "A".to_string(),
        node_type: "entity".to_string(),
        aliases_json: serde_json::json!([]),
        summary: None,
        metadata_json: serde_json::json!({}),
        support_count: 1,
        projection_version: 3,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let to_node = RuntimeGraphNodeRow {
        id: uuid::Uuid::now_v7(),
        library_id: scope.library_id,
        identity_key: "entity:b".to_string(),
        label: "B".to_string(),
        node_type: "entity".to_string(),
        aliases_json: serde_json::json!([]),
        summary: None,
        metadata_json: serde_json::json!({}),
        support_count: 1,
        projection_version: 4,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    assert_eq!(
        graph_edge_integrity_skip_reason(&scope, &from_node, &to_node),
        Some("projection_version_mismatch")
    );
}
