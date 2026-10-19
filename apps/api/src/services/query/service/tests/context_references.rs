use super::*;

fn technical_fact_row(
    fact_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    normalized_value: &str,
    qualifiers: serde_json::Value,
) -> KnowledgeTechnicalFactRow {
    let now = Utc::now();
    KnowledgeTechnicalFactRow {
        fact_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        fact_kind: "url".to_string(),
        normalized_value_text: normalized_value.to_string(),
        normalized_value_exact: normalized_value.to_string(),
        normalized_value_json: json!(normalized_value),
        display_value: normalized_value.to_string(),
        qualifiers_json: qualifiers,
        support_block_ids: Vec::new(),
        support_chunk_ids: Vec::new(),
        confidence: Some(0.9),
        extraction_kind: "synthetic_test".to_string(),
        conflict_group_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn ranked_fact_reference(rank: i32, score: f64) -> RankedBundleReference {
    RankedBundleReference { rank, score, reasons: BTreeSet::from(["synthetic_test".to_string()]) }
}

#[test]
fn diversified_fact_selection_collapses_occurrences_and_backfills_distinct_facts() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let preferred_id = Uuid::now_v7();
    let duplicate_id = Uuid::now_v7();
    let second_value_id = Uuid::now_v7();
    let third_value_id = Uuid::now_v7();
    let refs = HashMap::from([
        (preferred_id, ranked_fact_reference(1, 100.0)),
        (duplicate_id, ranked_fact_reference(2, 99.0)),
        (second_value_id, ranked_fact_reference(3, 98.0)),
        (third_value_id, ranked_fact_reference(4, 97.0)),
    ]);
    let facts = vec![
        technical_fact_row(
            preferred_id,
            document_id,
            revision_id,
            "https://sample.test",
            json!([]),
        ),
        technical_fact_row(
            duplicate_id,
            document_id,
            revision_id,
            "https://sample.test",
            json!([]),
        ),
        technical_fact_row(
            second_value_id,
            document_id,
            revision_id,
            "https://sample.test/status",
            json!([]),
        ),
        technical_fact_row(
            third_value_id,
            document_id,
            revision_id,
            "https://sample.test/health",
            json!([]),
        ),
    ];

    let selected = select_diversified_fact_ids(&refs, &facts, 3);

    assert_eq!(selected, vec![preferred_id, second_value_id, third_value_id]);
}

#[test]
fn diversified_fact_selection_retains_distinct_provenance_and_qualifiers() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let other_revision_id = Uuid::now_v7();
    let base_id = Uuid::now_v7();
    let other_document_id_fact = Uuid::now_v7();
    let other_revision_id_fact = Uuid::now_v7();
    let qualified_id = Uuid::now_v7();
    let refs = HashMap::from([
        (base_id, ranked_fact_reference(1, 100.0)),
        (other_document_id_fact, ranked_fact_reference(2, 99.0)),
        (other_revision_id_fact, ranked_fact_reference(3, 98.0)),
        (qualified_id, ranked_fact_reference(4, 97.0)),
    ]);
    let normalized_value = "https://sample.test";
    let facts = vec![
        technical_fact_row(base_id, document_id, revision_id, normalized_value, json!([])),
        technical_fact_row(
            other_document_id_fact,
            other_document_id,
            other_revision_id,
            normalized_value,
            json!([]),
        ),
        technical_fact_row(
            other_revision_id_fact,
            document_id,
            other_revision_id,
            normalized_value,
            json!([]),
        ),
        technical_fact_row(
            qualified_id,
            document_id,
            revision_id,
            normalized_value,
            json!([{ "key": "role", "value": "callback" }]),
        ),
    ];

    let selected = select_diversified_fact_ids(&refs, &facts, 4);

    assert_eq!(
        selected,
        vec![base_id, other_document_id_fact, other_revision_id_fact, qualified_id]
    );
}

#[test]
fn diversified_fact_selection_keeps_missing_rows_fail_safe() {
    let missing_id = Uuid::now_v7();
    let known_id = Uuid::now_v7();
    let duplicate_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let refs = HashMap::from([
        (missing_id, ranked_fact_reference(1, 100.0)),
        (known_id, ranked_fact_reference(2, 99.0)),
        (duplicate_id, ranked_fact_reference(3, 98.0)),
    ]);
    let facts = vec![
        technical_fact_row(known_id, document_id, revision_id, "https://sample.test", json!([])),
        technical_fact_row(
            duplicate_id,
            document_id,
            revision_id,
            "https://sample.test",
            json!([]),
        ),
    ];

    let selected = select_diversified_fact_ids(&refs, &facts, 3);

    assert_eq!(selected, vec![missing_id, known_id]);
}

#[test]
fn missing_bundle_execution_id_does_not_panic_or_emit_dangling_references() {
    let bundle_id = Uuid::now_v7();
    let bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: None,
            bundle_state: "ready".to_string(),
            bundle_strategy: "hybrid".to_string(),
            requested_mode: "mix".to_string(),
            resolved_mode: "mix".to_string(),
            selected_fact_ids: Vec::new(),
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: json!({}),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: vec![KnowledgeBundleChunkReferenceRow {
            bundle_id,
            chunk_id: Uuid::now_v7(),
            rank: 1,
            score: 1.0,
            inclusion_reason: Some("synthetic".to_string()),
            created_at: Utc::now(),
        }],
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    };

    assert!(map_chunk_references(&bundle).is_empty());
}

#[test]
fn seed_chunk_refs_from_answer_context_uses_answer_chunks_as_authoritative_source() {
    let first_chunk_id = Uuid::now_v7();
    let second_chunk_id = Uuid::now_v7();
    let refs = vec![
        QueryChunkReferenceSnapshot { chunk_id: first_chunk_id, rank: 2, score: 0.45 },
        QueryChunkReferenceSnapshot { chunk_id: second_chunk_id, rank: 1, score: 0.90 },
    ];

    let seeded = seed_chunk_refs_from_answer_context(&refs);

    assert_eq!(seeded.len(), 2);
    let first = seeded.get(&first_chunk_id).expect("first answer chunk");
    assert_eq!(first.rank, 2);
    assert_eq!(first.score, 0.45);
    assert!(first.reasons.contains("answer_context"));

    let second = seeded.get(&second_chunk_id).expect("second answer chunk");
    assert_eq!(second.rank, 1);
    assert_eq!(second.score, 0.90);
    assert!(second.reasons.contains("answer_context"));
}

#[test]
fn seed_entity_refs_from_answer_graph_uses_selected_graph_context() {
    let node_id = Uuid::now_v7();
    let refs = vec![RuntimeMatchedEntity {
        node_id,
        label: "Alpha Gateway".to_string(),
        node_type: "component".to_string(),
        summary: None,
        score: Some(0.82),
    }];
    let mut seeded = HashMap::new();

    seed_entity_refs_from_answer_graph(&refs, &mut seeded);

    let reference = seeded.get(&node_id).expect("selected graph entity");
    assert_eq!(reference.rank, 1);
    assert!((reference.score - 0.82).abs() < 0.000_001);
    assert!(reference.reasons.contains("answer_graph_context"));
}

#[test]
fn seed_relation_refs_from_answer_graph_uses_selected_graph_context() {
    let edge_id = Uuid::now_v7();
    let refs = vec![RuntimeMatchedRelationship {
        edge_id,
        relation_type: "depends_on".to_string(),
        from_node_id: Uuid::now_v7(),
        from_label: "Alpha Service".to_string(),
        to_node_id: Uuid::now_v7(),
        to_label: "Beta Store".to_string(),
        summary: Some("Alpha Service reads configuration from Beta Store.".to_string()),
        support_count: 2,
        score: Some(0.76),
    }];
    let mut seeded = HashMap::new();

    seed_relation_refs_from_answer_graph(&refs, &mut seeded);

    let reference = seeded.get(&edge_id).expect("selected graph relation");
    assert_eq!(reference.rank, 1);
    assert!((reference.score - 0.76).abs() < 0.000_001);
    assert!(reference.reasons.contains("answer_graph_context"));
}

#[test]
fn seed_relation_endpoint_refs_from_answer_graph_prioritizes_relation_nodes() {
    let from_node_id = Uuid::now_v7();
    let to_node_id = Uuid::now_v7();
    let relation_id = Uuid::now_v7();
    let relation_refs = vec![RuntimeMatchedRelationship {
        edge_id: relation_id,
        relation_type: "routes_to".to_string(),
        from_node_id,
        from_label: "Anchor Node".to_string(),
        to_node_id,
        to_label: "Target Node".to_string(),
        summary: Some("Anchor routes to target through a synthetic link.".to_string()),
        support_count: 3,
        score: Some(0.92),
    }];
    let mut entity_refs = HashMap::new();

    seed_relation_endpoint_refs_from_answer_graph(&relation_refs, &mut entity_refs);

    let from_reference = entity_refs.get(&from_node_id).expect("from-node endpoint");
    let to_reference = entity_refs.get(&to_node_id).expect("to-node endpoint");

    assert_eq!(from_reference.rank, 1);
    assert_eq!(to_reference.rank, 1);
    assert!((from_reference.score - 0.92).abs() < 0.000_001);
    assert!((to_reference.score - 0.92).abs() < 0.000_001);
    assert!(from_reference.reasons.contains("answer_relation_endpoint"));
    assert!(to_reference.reasons.contains("answer_relation_endpoint"));
}

#[test]
fn derive_fact_rank_refs_merges_evidence_and_selected_fact_ids() {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let fact_id = Uuid::now_v7();
    let evidence_id = Uuid::now_v7();
    let bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: "hybrid".to_string(),
            requested_mode: "mix".to_string(),
            resolved_mode: "mix".to_string(),
            selected_fact_ids: vec![fact_id],
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: json!({}),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: vec![
            crate::infra::knowledge_rows::KnowledgeBundleEvidenceReferenceRow {
                bundle_id,
                evidence_id,
                rank: 2,
                score: 42.0,
                inclusion_reason: Some("relation_evidence".to_string()),
                created_at: Utc::now(),
            },
        ],
    };
    let evidence_rows = vec![KnowledgeEvidenceRow {
        evidence_id,
        workspace_id: bundle.bundle.workspace_id,
        library_id: bundle.bundle.library_id,
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_id: None,
        block_id: Some(Uuid::now_v7()),
        fact_id: Some(fact_id),
        span_start: None,
        span_end: None,
        quote_text: "GET /api/status".to_string(),
        literal_spans_json: json!([]),
        evidence_kind: "relation_fact_support".to_string(),
        extraction_method: "graph_extract".to_string(),
        confidence: Some(0.9),
        evidence_state: "active".to_string(),
        freshness_generation: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }];

    let fact_refs = derive_fact_rank_refs(&bundle, &evidence_rows);
    let reference = fact_refs.get(&fact_id).expect("fact reference");
    assert_eq!(reference.rank, 1);
    assert!(reference.score >= 42.0);
}

#[test]
fn selected_fact_ids_for_detail_stays_bounded_to_configured_limit() {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let selected_fact_id = Uuid::now_v7();
    let bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: "hybrid".to_string(),
            requested_mode: "mix".to_string(),
            resolved_mode: "mix".to_string(),
            selected_fact_ids: vec![selected_fact_id],
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: json!({}),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    };
    let fact_rank_refs = (0..40)
        .map(|index| {
            (
                Uuid::now_v7(),
                RankedBundleReference {
                    rank: index + 1,
                    score: 100.0 - f64::from(index),
                    reasons: BTreeSet::from(["test".to_string()]),
                },
            )
        })
        .collect::<HashMap<_, _>>();

    let fact_ids = selected_fact_ids_for_detail(&bundle, &fact_rank_refs);
    assert_eq!(fact_ids.len(), MAX_DETAIL_TECHNICAL_FACT_REFERENCES);
    assert_eq!(fact_ids.first().copied(), Some(selected_fact_id));
}

#[test]
fn graph_references_for_detail_stay_bounded_and_ranked() {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let total = MAX_DETAIL_GRAPH_NODE_REFERENCES + 8;
    let bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: "hybrid".to_string(),
            requested_mode: "mix".to_string(),
            resolved_mode: "mix".to_string(),
            selected_fact_ids: Vec::new(),
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: json!({}),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: Vec::new(),
        entity_references: (0..total)
            .map(|index| {
                let rank = i32::try_from(total - index).expect("synthetic rank fits i32");
                let entity_id = Uuid::now_v7();
                KnowledgeBundleEntityReferenceRow {
                    bundle_id,
                    entity_id,
                    rank,
                    score: f64::from(rank),
                    inclusion_reason: Some("synthetic".to_string()),
                    created_at: Utc::now(),
                }
            })
            .collect(),
        relation_references: (0..(MAX_DETAIL_GRAPH_EDGE_REFERENCES + 8))
            .map(|index| {
                let total = MAX_DETAIL_GRAPH_EDGE_REFERENCES + 8;
                let rank = i32::try_from(total - index).expect("synthetic rank fits i32");
                let relation_id = Uuid::now_v7();
                KnowledgeBundleRelationReferenceRow {
                    bundle_id,
                    relation_id,
                    rank,
                    score: f64::from(rank),
                    inclusion_reason: Some("synthetic".to_string()),
                    created_at: Utc::now(),
                }
            })
            .collect(),
        evidence_references: Vec::new(),
    };

    let entity_references = map_entity_references(&bundle);
    let relation_references = map_relation_references(&bundle);

    assert_eq!(entity_references.len(), MAX_DETAIL_GRAPH_NODE_REFERENCES);
    assert_eq!(relation_references.len(), MAX_DETAIL_GRAPH_EDGE_REFERENCES);
    assert_eq!(entity_references.first().map(|reference| reference.rank), Some(1));
    assert_eq!(relation_references.first().map(|reference| reference.rank), Some(1));
    assert!(entity_references.iter().all(|reference| {
        usize::try_from(reference.rank).is_ok_and(|rank| rank <= MAX_DETAIL_GRAPH_NODE_REFERENCES)
    }));
    assert!(relation_references.iter().all(|reference| {
        usize::try_from(reference.rank).is_ok_and(|rank| rank <= MAX_DETAIL_GRAPH_EDGE_REFERENCES)
    }));
}

#[test]
fn parse_query_verification_state_maps_supported_values() {
    assert_eq!(parse_query_verification_state("verified"), QueryVerificationState::Verified);
    assert_eq!(
        parse_query_verification_state("insufficient_evidence"),
        QueryVerificationState::InsufficientEvidence
    );
    assert_eq!(parse_query_verification_state("unknown"), QueryVerificationState::NotRun);
}
