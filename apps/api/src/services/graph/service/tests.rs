use super::*;
use crate::{
    domains::{knowledge::TypedTechnicalFact, runtime_graph::RuntimeNodeType},
    infra::knowledge_rows::{
        GraphViewData, KnowledgeChunkRow, KnowledgeEntityCandidateRow,
        KnowledgeRelationCandidateRow, KnowledgeRevisionRow,
    },
    services::graph::extract::{GraphEntityCandidate, GraphRelationCandidate},
    shared::extraction::technical_facts::{
        TechnicalFactKind, TechnicalFactQualifier, TechnicalFactValue,
    },
};

fn materialization_fixture()
-> (KnowledgeRevisionRow, repositories::extract_repository::ExtractChunkResultRow) {
    let now = Utc::now();
    let revision = KnowledgeRevisionRow {
        revision_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_number: 1,
        revision_state: "active".to_string(),
        revision_kind: "source".to_string(),
        storage_ref: None,
        source_uri: None,
        document_hint: None,
        mime_type: "text/plain".to_string(),
        checksum: "checksum".to_string(),
        title: None,
        byte_size: 1,
        normalized_text: Some("text".to_string()),
        text_checksum: Some("checksum".to_string()),
        image_checksum: None,
        text_state: "text_readable".to_string(),
        vector_state: "ready".to_string(),
        graph_state: "ready".to_string(),
        text_readable_at: Some(now),
        vector_ready_at: Some(now),
        graph_ready_at: Some(now),
        superseded_by_revision_id: None,
        created_at: now,
    };
    let chunk_result = repositories::extract_repository::ExtractChunkResultRow {
        id: Uuid::now_v7(),
        chunk_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        extract_state: "ready".to_string(),
        provider_call_id: None,
        started_at: now,
        finished_at: Some(now),
        failure_code: None,
    };
    (revision, chunk_result)
}

#[test]
fn merge_projection_data_prefers_incoming_normalized_rows() {
    let node_id = Uuid::now_v7();
    let edge_id = Uuid::now_v7();
    let merged = GraphService::merge_projection_data(
        &GraphViewData {
            nodes: vec![GraphViewNodeWrite {
                node_id,
                identity_key: "entity:a".to_string(),
                label: "A".to_string(),
                node_type: "entity".to_string(),
                support_count: 1,
                summary: None,
                aliases: vec![],
                metadata_json: serde_json::json!({}),
            }],
            edges: vec![],
        },
        &GraphViewData {
            nodes: vec![GraphViewNodeWrite {
                node_id,
                identity_key: "entity:a".to_string(),
                label: "A2".to_string(),
                node_type: "concept".to_string(),
                support_count: 4,
                summary: Some("updated".to_string()),
                aliases: vec!["alias".to_string()],
                metadata_json: serde_json::json!({"k": "v"}),
            }],
            edges: vec![GraphViewEdgeWrite {
                edge_id,
                from_node_id: node_id,
                to_node_id: Uuid::now_v7(),
                relation_type: "links_to".to_string(),
                identity_key: "entity:a--links_to--entity:b".to_string(),
                support_count: 1,
                summary: None,
                weight: None,
                metadata_json: serde_json::json!({}),
            }],
        },
    );

    assert_eq!(merged.nodes.len(), 1);
    assert_eq!(merged.nodes[0].label, "A2");
    assert_eq!(merged.nodes[0].support_count, 4);
    assert!(merged.edges.is_empty(), "dangling edge should be filtered");
}

#[test]
fn relation_fields_are_semantically_empty_rejects_blank_members() {
    assert!(relation_fields_are_semantically_empty("", "supports", "beta"));
    assert!(relation_fields_are_semantically_empty("alpha", "supports", ""));
    assert!(!relation_fields_are_semantically_empty("alpha", "supports", "beta"));
}

#[test]
fn rebuild_outcome_requires_materialized_entities_relations_or_evidence() {
    assert!(!GraphRebuildOutcome::default().has_materialized_graph());
    assert!(
        GraphRebuildOutcome { upserted_entities: 1, ..Default::default() }.has_materialized_graph()
    );
    assert!(
        GraphRebuildOutcome { upserted_relations: 1, ..Default::default() }
            .has_materialized_graph()
    );
    assert!(
        GraphRebuildOutcome { upserted_evidence: 1, ..Default::default() }.has_materialized_graph()
    );
}

#[test]
fn placeholder_entity_parts_require_strict_identity_key() {
    assert!(placeholder_entity_parts_from_key("entity:").is_none());
    assert_eq!(
        placeholder_entity_parts_from_key("entity:acme_print_house"),
        Some((RuntimeNodeType::Entity, "acme_print_house".to_string()))
    );
    assert!(placeholder_entity_parts_from_key("entity:Acme_Print_House").is_none());
}

#[test]
fn relation_candidate_keys_reject_dangling_entity_prefix() {
    assert!(!relation_candidate_keys_are_materializable("entity:acme", "supports", "entity:"));
    assert!(relation_candidate_keys_are_materializable(
        "entity:acme",
        "supports",
        "entity:register"
    ));
}

#[test]
fn reconcile_entity_candidate_row_rejects_mismatched_persisted_key() {
    let mut entity_key_index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    entity_key_index.insert("Acme Print House", RuntimeNodeType::Entity);
    let row = KnowledgeEntityCandidateRow {
        candidate_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_id: Some(Uuid::now_v7()),
        candidate_label: "Acme Print House".to_string(),
        candidate_type: "entity".to_string(),
        candidate_sub_type: None,
        normalization_key: "entity:".to_string(),
        confidence: None,
        extraction_method: "extract_chunk_result".to_string(),
        candidate_state: "active".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    assert!(reconcile_entity_candidate_row(row, &entity_key_index).is_none());
}

#[test]
fn reconcile_relation_candidate_row_rejects_mismatched_endpoint_keys() {
    let entity_key_index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    let row = KnowledgeRelationCandidateRow {
        candidate_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_id: Some(Uuid::now_v7()),
        subject_label: "Acme Print House".to_string(),
        subject_candidate_key: "entity:".to_string(),
        predicate: "mentions".to_string(),
        object_label: "Register".to_string(),
        object_candidate_key: "topic:".to_string(),
        normalized_assertion: "entity:--legacy--topic:".to_string(),
        confidence: None,
        extraction_method: "extract_chunk_result".to_string(),
        candidate_state: "active".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    assert!(reconcile_relation_candidate_row(row, &entity_key_index).is_none());
}

#[test]
fn reconcile_relation_candidate_row_rejects_missing_identity_without_labels() {
    let entity_key_index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    let row = KnowledgeRelationCandidateRow {
        candidate_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_id: Some(Uuid::now_v7()),
        subject_label: String::new(),
        subject_candidate_key: "entity:".to_string(),
        predicate: "supports".to_string(),
        object_label: String::new(),
        object_candidate_key: "entity:acme".to_string(),
        normalized_assertion: "entity:--supports--entity:acme".to_string(),
        confidence: None,
        extraction_method: "extract_chunk_result".to_string(),
        candidate_state: "active".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    assert!(reconcile_relation_candidate_row(row, &entity_key_index).is_none());
}

#[test]
fn reconcile_entity_candidate_row_preserves_matching_type_key() {
    let mut entity_key_index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    entity_key_index.insert("Register", RuntimeNodeType::Concept);
    entity_key_index.insert("Register", RuntimeNodeType::Entity);
    let row = KnowledgeEntityCandidateRow {
        candidate_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_id: Some(Uuid::now_v7()),
        candidate_label: "Register".to_string(),
        candidate_type: "concept".to_string(),
        candidate_sub_type: None,
        normalization_key: "concept:register".to_string(),
        confidence: None,
        extraction_method: "graph_extract".to_string(),
        candidate_state: "active".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let reconciled = reconcile_entity_candidate_row(row, &entity_key_index)
        .expect("entity candidate should reconcile");

    assert_eq!(reconciled.normalization_key, "concept:register");
}

#[test]
fn build_prefixed_entity_key_aliases_collapses_unbranded_product_keys() {
    let revision_id = Uuid::now_v7();
    let branded = ReconciledEntityCandidate {
        normalization_key: "entity:acme_operations_console".to_string(),
        row: KnowledgeEntityCandidateRow {
            candidate_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_id,
            chunk_id: Some(Uuid::now_v7()),
            candidate_label: "Acme Operations Console".to_string(),
            candidate_type: "entity".to_string(),
            candidate_sub_type: None,
            normalization_key: "entity:acme_operations_console".to_string(),
            confidence: None,
            extraction_method: "graph_extract".to_string(),
            candidate_state: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
    };
    let unbranded = ReconciledEntityCandidate {
        normalization_key: "entity:operations_console".to_string(),
        row: KnowledgeEntityCandidateRow {
            candidate_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_id,
            chunk_id: Some(Uuid::now_v7()),
            candidate_label: "Operations Console".to_string(),
            candidate_type: "entity".to_string(),
            candidate_sub_type: None,
            normalization_key: "entity:operations_console".to_string(),
            confidence: None,
            extraction_method: "graph_extract".to_string(),
            candidate_state: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
    };

    let aliases = build_prefixed_entity_key_aliases(&[branded, unbranded]);

    assert_eq!(
        aliases.get("entity:operations_console"),
        Some(&"entity:acme_operations_console".to_string())
    );
}

#[test]
fn apply_entity_key_aliases_to_relation_candidate_rebuilds_assertion() {
    let mut candidate = ReconciledRelationCandidate {
        row: KnowledgeRelationCandidateRow {
            candidate_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_id: Some(Uuid::now_v7()),
            subject_label: "Operations Console".to_string(),
            subject_candidate_key: "entity:operations_console".to_string(),
            predicate: "manages".to_string(),
            object_label: "Register".to_string(),
            object_candidate_key: "entity:register".to_string(),
            normalized_assertion: "entity:operations_console--manages--entity:register".to_string(),
            confidence: None,
            extraction_method: "graph_extract".to_string(),
            candidate_state: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        subject_candidate_key: "entity:operations_console".to_string(),
        predicate: "manages".to_string(),
        object_candidate_key: "entity:register".to_string(),
        normalized_assertion: "entity:operations_console--manages--entity:register".to_string(),
    };

    apply_entity_key_aliases_to_relation_candidate(
        &mut candidate,
        &BTreeMap::from([(
            "entity:operations_console".to_string(),
            "entity:acme_operations_console".to_string(),
        )]),
    );

    assert_eq!(candidate.subject_candidate_key, "entity:acme_operations_console");
    assert_eq!(
        candidate.normalized_assertion,
        "entity:acme_operations_console--manages--entity:register"
    );
}

#[test]
fn build_materialized_extract_candidates_normalizes_unicode_node_rows() {
    let (revision, chunk_result) = materialization_fixture();
    let node_rows = vec![repositories::extract_repository::ExtractNodeCandidateRow {
        id: Uuid::now_v7(),
        chunk_result_id: chunk_result.id,
        identity_key: "entity:".to_string(),
        node_kind: "entity".to_string(),
        display_label: "Acme Print House".to_string(),
        summary: None,
    }];

    let materialized =
        build_materialized_extract_candidates(&revision, &chunk_result, &node_rows, &[]);

    assert_eq!(materialized.entity_candidates.len(), 1);
    assert_eq!(materialized.entity_candidates[0].normalization_key, "entity:acme_print_house");
    assert_eq!(materialized.entity_candidates[0].candidate_label, "Acme Print House");
}

#[test]
fn build_materialized_extract_candidates_derives_relation_labels_from_nodes() {
    let (revision, chunk_result) = materialization_fixture();
    let node_rows = vec![
        repositories::extract_repository::ExtractNodeCandidateRow {
            id: Uuid::now_v7(),
            chunk_result_id: chunk_result.id,
            identity_key: "entity:".to_string(),
            node_kind: "entity".to_string(),
            display_label: "Acme Print House".to_string(),
            summary: None,
        },
        repositories::extract_repository::ExtractNodeCandidateRow {
            id: Uuid::now_v7(),
            chunk_result_id: chunk_result.id,
            identity_key: "concept:register".to_string(),
            node_kind: "concept".to_string(),
            display_label: "Register".to_string(),
            summary: None,
        },
    ];
    let edge_rows = vec![repositories::extract_repository::ExtractEdgeCandidateRow {
        id: Uuid::now_v7(),
        chunk_result_id: chunk_result.id,
        identity_key: "entity:--mentions--concept:register".to_string(),
        edge_kind: "mentions".to_string(),
        from_identity_key: "entity:".to_string(),
        to_identity_key: "concept:register".to_string(),
        summary: None,
    }];

    let materialized =
        build_materialized_extract_candidates(&revision, &chunk_result, &node_rows, &edge_rows);

    assert_eq!(materialized.relation_candidates.len(), 1);
    let relation = &materialized.relation_candidates[0];
    assert_eq!(relation.subject_label, "Acme Print House");
    assert_eq!(relation.object_label, "Register");
    assert_eq!(relation.subject_candidate_key, "entity:acme_print_house");
    assert_eq!(relation.object_candidate_key, "concept:register");
    assert_eq!(
        relation.normalized_assertion,
        "entity:acme_print_house--mentions--concept:register"
    );
}

#[test]
fn entity_identity_normalization_key_preserves_unicode_and_node_type() {
    let entity = GraphEntityCandidate {
        label: "Acme Print House".to_string(),
        node_type: RuntimeNodeType::Entity,
        sub_type: None,
        aliases: vec![],
        summary: None,
    };
    let topic = GraphEntityCandidate {
        label: "Acme Print House".to_string(),
        node_type: RuntimeNodeType::Concept,
        sub_type: None,
        aliases: vec![],
        summary: None,
    };

    assert_eq!(entity_identity_normalization_key(&entity), "entity:acme_print_house");
    assert_eq!(entity_identity_normalization_key(&topic), "concept:acme_print_house");
}

#[test]
fn normalized_relation_assertion_preserves_unicode_entity_keys() {
    let relation = GraphRelationCandidate {
        source_label: "Acme Register".to_string(),
        target_label: "Acme Print House".to_string(),
        relation_type: "part_of".to_string(),
        summary: None,
    };

    assert_eq!(
        normalized_relation_assertion(&relation),
        "entity:acme_register--part_of--entity:acme_print_house"
    );
}

#[test]
fn resolve_entity_evidence_support_prefers_matching_fact_support() {
    let block_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let fact_id = Uuid::now_v7();
    let chunk = KnowledgeChunkRow {
        chunk_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("endpoint_block".to_string()),
        content_text: "GET /api/status".to_string(),
        normalized_text: "GET /api/status".to_string(),
        span_start: None,
        span_end: None,
        token_count: None,
        support_block_ids: vec![block_id],
        section_path: vec!["API".to_string()],
        heading_trail: vec!["Status".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: None,

        window_text: None,

        occurred_at: None,
        occurred_until: None,
    };
    let fact = TypedTechnicalFact {
        fact_id,
        revision_id: chunk.revision_id,
        document_id: chunk.document_id,
        workspace_id: chunk.workspace_id,
        library_id: chunk.library_id,
        fact_kind: TechnicalFactKind::EndpointPath,
        normalized_value: TechnicalFactValue::Text("/api/status".to_string()),
        display_value: "/api/status".to_string(),
        qualifiers: vec![TechnicalFactQualifier {
            key: "method".to_string(),
            value: "GET".to_string(),
        }],
        support_block_ids: vec![block_id],
        support_chunk_ids: vec![chunk_id],
        confidence: Some(0.91),
        extraction_kind: "parser".to_string(),
        conflict_group_id: None,
        created_at: Utc::now(),
    };

    let support =
        resolve_entity_evidence_support("/api/status", "/api/status", Some(&chunk), &[fact]);
    assert_eq!(support.block_id, Some(block_id));
    assert_eq!(support.fact_id, Some(fact_id));
    assert_eq!(support.evidence_kind, "entity_fact_support");
    assert_eq!(support.literal_spans.len(), 1);
}
