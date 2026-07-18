use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::{
    domains::{
        agent_runtime::RuntimeLifecycleState,
        query::{QueryTurnKind, QueryVerificationState},
    },
    infra::{
        knowledge_rows::{
            KnowledgeBundleChunkReferenceRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleRelationReferenceRow, KnowledgeChunkRow,
            KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow, KnowledgeEvidenceRow,
            KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow,
        },
        repositories::query_repository,
    },
    services::query::execution::{
        QueryChunkReferenceSnapshot, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    },
};

use super::{
    ExternalConversationTurn, MAX_DETAIL_GRAPH_EDGE_REFERENCES, MAX_DETAIL_GRAPH_NODE_REFERENCES,
    MAX_DETAIL_PREPARED_SEGMENT_REFERENCES, MAX_DETAIL_TECHNICAL_FACT_REFERENCES,
    RankedBundleReference,
    context::{
        derive_fact_rank_refs, seed_chunk_refs_from_answer_context,
        seed_entity_refs_from_answer_graph, seed_relation_endpoint_refs_from_answer_graph,
        seed_relation_refs_from_answer_graph, select_diversified_fact_ids,
        selected_fact_ids_for_detail,
    },
    formatting::{
        build_prepared_segment_references, map_chunk_references, map_entity_references,
        map_execution_runtime_summary, map_relation_references, parse_query_verification_state,
    },
    session::{
        build_conversation_runtime_context,
        build_conversation_runtime_context_with_external_history,
        build_prior_grounded_answer_context_messages, derive_conversation_title,
        normalize_explicit_conversation_title, select_prior_grounded_answer_replay_executions,
    },
};

#[test]
fn explicit_conversation_title_is_trimmed_and_collapsed() {
    let title = normalize_explicit_conversation_title("  Durable\n  session   title  ")
        .expect("bounded non-empty title");

    assert_eq!(title, "Durable session title");
}

#[test]
fn explicit_conversation_title_rejects_empty_input() {
    let error = normalize_explicit_conversation_title(" \n\t ").expect_err("empty title");

    assert!(matches!(error, crate::interfaces::http::router_support::ApiError::BadRequest(_)));
}

#[test]
fn explicit_conversation_title_rejects_more_than_the_contract_limit() {
    let over_limit = "x".repeat(super::QUERY_CONVERSATION_TITLE_LIMIT + 1);
    let error = normalize_explicit_conversation_title(&over_limit).expect_err("oversized title");

    assert!(matches!(error, crate::interfaces::http::router_support::ApiError::BadRequest(_)));
}

#[test]
fn derived_conversation_title_includes_ellipsis_within_the_contract_limit() {
    let source = "x".repeat(super::QUERY_CONVERSATION_TITLE_LIMIT + 5);
    let title = derive_conversation_title(&source).expect("non-empty derived title");

    assert_eq!(title.chars().count(), super::QUERY_CONVERSATION_TITLE_LIMIT);
    assert!(title.ends_with('…'));
}

fn technical_fact_row(
    fact_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    canonical_value: &str,
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
        canonical_value_text: canonical_value.to_string(),
        canonical_value_exact: canonical_value.to_string(),
        canonical_value_json: json!(canonical_value),
        display_value: canonical_value.to_string(),
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
    let canonical_value = "https://sample.test";
    let facts = vec![
        technical_fact_row(base_id, document_id, revision_id, canonical_value, json!([])),
        technical_fact_row(
            other_document_id_fact,
            other_document_id,
            other_revision_id,
            canonical_value,
            json!([]),
        ),
        technical_fact_row(
            other_revision_id_fact,
            document_id,
            other_revision_id,
            canonical_value,
            json!([]),
        ),
        technical_fact_row(
            qualified_id,
            document_id,
            revision_id,
            canonical_value,
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
fn seed_chunk_refs_from_answer_context_uses_answer_chunks_as_canonical_source() {
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
fn selected_fact_ids_for_detail_stays_bounded_to_canonical_limit() {
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
fn build_prepared_segment_references_prioritizes_query_matching_headings_and_limits_revision_fanout()
 {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let telegram_revision_id = Uuid::now_v7();
    let control_revision_id = Uuid::now_v7();
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
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    };
    let mut block_rank_refs = HashMap::new();
    let mut blocks = Vec::new();
    for ordinal in 0..12_i32 {
        let block_id = Uuid::now_v7();
        blocks.push(KnowledgeStructuredBlockRow {
            block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: telegram_revision_id,
            ordinal,
            block_kind: if ordinal == 0 { "heading".to_string() } else { "list_item".to_string() },
            text: "telegram".to_string(),
            normalized_text: "telegram".to_string(),
            heading_trail: vec!["Acme Telegram Bot - Example".to_string()],
            section_path: vec!["acme-telegram-bot-example".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });
        block_rank_refs.insert(
            block_id,
            RankedBundleReference {
                rank: 1,
                score: 100.0 - f64::from(ordinal),
                reasons: BTreeSet::from(["test".to_string()]),
            },
        );
    }
    let control_heading_id = Uuid::now_v7();
    blocks.push(KnowledgeStructuredBlockRow {
        block_id: control_heading_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: control_revision_id,
        ordinal: 0,
        block_kind: "heading".to_string(),
        text: "sample console".to_string(),
        normalized_text: "sample console".to_string(),
        heading_trail: vec!["Acme Sample Console - Example".to_string()],
        section_path: vec!["acme-control-center-example".to_string()],
        page_number: None,
        span_start: None,
        span_end: None,
        parent_block_id: None,
        table_coordinates_json: None,
        code_language: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    });
    block_rank_refs.insert(
        control_heading_id,
        RankedBundleReference {
            rank: 2,
            score: 90.0,
            reasons: BTreeSet::from(["test".to_string()]),
        },
    );

    let references = build_prepared_segment_references(
        Some(&bundle),
        &blocks,
        &block_rank_refs,
        "What is Acme Sample Console?",
        None,
        &HashMap::new(),
    );

    assert_eq!(
        references.first().and_then(|reference| reference.heading_trail.first().cloned()),
        Some("Acme Sample Console - Example".to_string())
    );
    assert!(
        references.iter().all(|reference| reference.revision_id == control_revision_id),
        "focused query should retain only the best matching revision when focus is explicit"
    );
    assert!(references.len() <= MAX_DETAIL_PREPARED_SEGMENT_REFERENCES);
    assert!(
        references.iter().filter(|reference| reference.revision_id == telegram_revision_id).count()
            <= super::MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION
    );
}

#[test]
fn build_prepared_segment_references_prefers_rare_query_terms_over_generic_rank() {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let generic_revision_id = Uuid::now_v7();
    let focused_revision_id = Uuid::now_v7();
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
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    };

    let generic_block_id = Uuid::now_v7();
    let focused_block_id = Uuid::now_v7();
    let blocks = vec![
        KnowledgeStructuredBlockRow {
            block_id: generic_block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: generic_revision_id,
            ordinal: 0,
            block_kind: "heading".to_string(),
            text: "Workspace templates and general setup".to_string(),
            normalized_text: "workspace templates and general setup".to_string(),
            heading_trail: vec!["Workspace templates".to_string()],
            section_path: vec!["workspace_templates".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        KnowledgeStructuredBlockRow {
            block_id: focused_block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: focused_revision_id,
            ordinal: 0,
            block_kind: "heading".to_string(),
            text: "Workspace plan options: Free, Standard, Advanced".to_string(),
            normalized_text: "workspace plan options free standard advanced".to_string(),
            heading_trail: vec!["Pricing policy: plan options".to_string()],
            section_path: vec!["pricing_policy_plan_options".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
    ];
    let mut block_rank_refs = HashMap::new();
    block_rank_refs.insert(
        generic_block_id,
        RankedBundleReference {
            rank: 1,
            score: 2_000_000.0,
            reasons: BTreeSet::from(["source_coverage".to_string()]),
        },
    );
    block_rank_refs.insert(
        focused_block_id,
        RankedBundleReference {
            rank: 9,
            score: 10.0,
            reasons: BTreeSet::from(["content_anchor".to_string()]),
        },
    );

    let references = build_prepared_segment_references(
        Some(&bundle),
        &blocks,
        &block_rank_refs,
        "workspace plans",
        None,
        &HashMap::new(),
    );

    assert_eq!(
        references.first().and_then(|reference| reference.heading_trail.first().cloned()),
        Some("Pricing policy: plan options".to_string()),
        "rare query-term overlap should beat a higher-ranked generic companion segment"
    );
}

#[test]
fn build_prepared_segment_references_prefers_answer_supported_blocks_over_generic_titles() {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let generic_revision_id = Uuid::now_v7();
    let focused_revision_id = Uuid::now_v7();
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
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    };

    let generic_block_id = Uuid::now_v7();
    let focused_block_id = Uuid::now_v7();
    let blocks = vec![
        KnowledgeStructuredBlockRow {
            block_id: generic_block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: generic_revision_id,
            ordinal: 0,
            block_kind: "heading".to_string(),
            text: "Workspace plans overview".to_string(),
            normalized_text: "workspace plans overview".to_string(),
            heading_trail: vec!["Workspace plans overview".to_string()],
            section_path: vec!["workspace_plans_overview".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        KnowledgeStructuredBlockRow {
            block_id: focused_block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: focused_revision_id,
            ordinal: 0,
            block_kind: "paragraph".to_string(),
            text: "Available options: Free, Standard, Advanced.".to_string(),
            normalized_text: "available options free standard advanced".to_string(),
            heading_trail: vec!["Pricing policy".to_string()],
            section_path: vec!["pricing_policy".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
    ];
    let mut block_rank_refs = HashMap::new();
    block_rank_refs.insert(
        generic_block_id,
        RankedBundleReference {
            rank: 1,
            score: 2_000_000.0,
            reasons: BTreeSet::from(["source_coverage".to_string()]),
        },
    );
    block_rank_refs.insert(
        focused_block_id,
        RankedBundleReference {
            rank: 20,
            score: 1.0,
            reasons: BTreeSet::from(["content_anchor".to_string()]),
        },
    );

    let references = build_prepared_segment_references(
        Some(&bundle),
        &blocks,
        &block_rank_refs,
        "workspace plans",
        Some("Available options: Free, Standard, Advanced."),
        &HashMap::new(),
    );

    assert_eq!(
        references.first().and_then(|reference| reference.heading_trail.first().cloned()),
        Some("Pricing policy".to_string()),
        "answer-supported source blocks should outrank generic high-rank title matches"
    );
}

#[test]
fn parse_query_verification_state_maps_canonical_values() {
    assert_eq!(parse_query_verification_state("verified"), QueryVerificationState::Verified);
    assert_eq!(
        parse_query_verification_state("insufficient_evidence"),
        QueryVerificationState::InsufficientEvidence
    );
    assert_eq!(parse_query_verification_state("unknown"), QueryVerificationState::NotRun);
}

#[test]
fn build_conversation_runtime_context_exports_bounded_typed_grounded_answer_tool_history() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "which connector variants exist".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let system_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::System,
        author_principal_id: None,
        content_text: "internal lifecycle note".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let tool_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::Tool,
        author_principal_id: None,
        content_text: "tool observation".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 4,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Connector Alpha uses `alphaSecret`.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 5,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "and limitations?".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, system_turn, tool_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    assert_eq!(context.grounded_answer_tool_history.len(), 2);
    assert!(matches!(context.grounded_answer_tool_history[0].turn_kind, QueryTurnKind::User));
    assert!(matches!(context.grounded_answer_tool_history[1].turn_kind, QueryTurnKind::Assistant));
    assert!(context.grounded_answer_tool_history.iter().all(|turn| {
        !turn.content_text.contains("System note")
            && !turn.content_text.contains("Tool observation")
            && !turn.content_text.contains("internal lifecycle note")
            && !turn.content_text.contains("tool observation")
    }));
}

#[test]
fn build_conversation_runtime_context_keeps_short_disambiguator_for_tool_follow_up() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "how do I configure account connectors".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_choice_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Available variants: Provider Alpha, Provider Beta.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let disambiguator_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "Provider Alpha".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_detail_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 4,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Provider Alpha uses `/opt/provider-alpha/connector.conf` with `alphaUrl`."
            .to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let second_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 5,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "show how to configure it".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let second_assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 6,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Set `alphaSecret` and `alphaTimeout`.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 7,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "ready config please".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[
            first_user_turn,
            assistant_choice_turn,
            disambiguator_turn,
            assistant_detail_turn,
            second_user_turn,
            second_assistant_turn,
            follow_up_turn.clone(),
        ],
        follow_up_turn.id,
    );

    let history_text = context
        .grounded_answer_tool_history
        .iter()
        .map(|turn| turn.content_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(history_text.contains("Provider Alpha"));
    assert!(history_text.contains("/opt/provider-alpha/connector.conf"));
    assert!(history_text.contains("alphaSecret"));
    assert_eq!(context.grounded_answer_tool_history.len(), 6);
}

#[test]
fn build_conversation_runtime_context_preserves_dense_assistant_literals_in_tool_history() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "which package-like modules exist".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: format!(
            "`pkg-alpha` `pkg-beta` `pkg-gamma` `pkg-delta` {} `pkg-epsilon` `pkg-zeta` `pkg-eta` `pkg-theta`",
            "long filler ".repeat(260)
        ),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "describe each item".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    let assistant_history = context
        .grounded_answer_tool_history
        .iter()
        .find(|turn| {
            matches!(turn.turn_kind, QueryTurnKind::Assistant)
                && turn.content_text.starts_with("ir.memory.literals.v1: ")
        })
        .expect("assistant history should be exported");
    assert!(assistant_history.content_text.starts_with("ir.memory.literals.v1: "));
    assert!(assistant_history.content_text.contains("`pkg-alpha`"));
    assert!(assistant_history.content_text.contains("`pkg-theta`"));
    assert!(
        assistant_history.content_text.chars().count() < 900,
        "dense assistant history should preserve exact literals without replaying most prior prose"
    );

    let prompt_history_message = context
        .prompt_history_messages
        .iter()
        .find(|message| {
            message.content.as_deref().is_some_and(|content| {
                content.contains("ir.context.compact-literal-memory.v1:")
                    && content.contains("`pkg-alpha`")
            })
        })
        .expect("compact literal memory should reach prompt history");
    assert_eq!(prompt_history_message.role, "system");
    let prompt_history_text = prompt_history_message.content.as_deref().unwrap_or_default();
    assert!(prompt_history_text.contains("ir.memory.literals.v1: "));
    assert!(prompt_history_text.starts_with("ir.context.compact-literal-memory.v1:"));
}

#[test]
fn build_conversation_runtime_context_preserves_long_path_literals_in_tool_history() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "how do I configure Provider Alpha".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: format!(
            "Use `/opt/provider-alpha/configuration/files/provider-alpha-primary.conf` and set `alphaSecret` `alphaUrl` `alphaTimeout` `alphaMode` `alphaToken` `alphaCurrency` `alphaRetries`. {}",
            "long filler ".repeat(260)
        ),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "show the config".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    let assistant_history = context
        .grounded_answer_tool_history
        .iter()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .expect("assistant history should be exported");
    assert!(
        assistant_history
            .content_text
            .contains("`/opt/provider-alpha/configuration/files/provider-alpha-primary.conf`")
    );
}

#[test]
fn build_conversation_runtime_context_separates_external_history_from_current_text() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "now enumerate the integration variants and missing limits".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::User,
        content_text: "what does the library say about payment setup".to_string(),
    }];

    let context = build_conversation_runtime_context_with_external_history(
        std::slice::from_ref(&current_turn),
        current_turn.id,
        &external_prior_turns,
    );

    assert_eq!(context.query_compiler_history.len(), 1);
    assert_eq!(context.query_compiler_history[0].turn_kind, QueryTurnKind::User);
    assert_eq!(
        context.query_compiler_history[0].content_text,
        "what does the library say about payment setup"
    );
    assert_eq!(
        context.current_question_text,
        "now enumerate the integration variants and missing limits"
    );
    assert!(context.has_prior_conversation);
}

#[test]
fn build_conversation_runtime_context_keeps_compacted_literals_case_insensitive() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "describe the settings".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text:
            "`AlphaKey` `alphakey` `BetaKey` `GammaKey` `DeltaKey` `EpsilonKey` `ZetaKey` `EtaKey` `ThetaKey`"
                .to_string(),
    }];

    let context = build_conversation_runtime_context_with_external_history(
        std::slice::from_ref(&current_turn),
        current_turn.id,
        &external_prior_turns,
    );

    let assistant_history = context
        .grounded_answer_tool_history
        .iter()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .expect("assistant tool history");
    let literal_line = assistant_history
        .content_text
        .lines()
        .find(|line| line.starts_with("ir.memory.literals.v1:"))
        .expect("compacted literal summary");
    assert!(literal_line.contains("`AlphaKey`"));
    assert!(
        !literal_line.contains("`alphakey`"),
        "compact literal summary keeps the first spelling only"
    );
}

#[test]
fn prior_grounded_answer_context_messages_preserve_chunk_evidence() {
    let library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let higher_rank_chunk_id = Uuid::now_v7();
    let reference = test_chunk_reference(chunk_id, 2, 0.92);
    let higher_rank_reference = test_chunk_reference(higher_rank_chunk_id, 1, 0.88);
    let chunk = test_chunk_row(
        library_id,
        chunk_id,
        "Connector Alpha configuration uses [Alpha]\nalphaSecret = ${ALPHA_SECRET}\nalphaMode = strict",
        &["Setup", "Configuration"],
    );
    let higher_rank_chunk = test_chunk_row(
        library_id,
        higher_rank_chunk_id,
        "Connector Alpha package is alpha-connector.",
        &["Setup", "Package"],
    );

    let replay = build_prior_grounded_answer_context_messages(
        library_id,
        execution_id,
        "How is Connector Alpha configured?",
        &[reference, higher_rank_reference],
        &[chunk, higher_rank_chunk],
        4_000,
    )
    .expect("prior grounded answer replay");

    assert_eq!(replay.messages.len(), 1);
    assert_eq!(replay.chunk_ids, vec![higher_rank_chunk_id, chunk_id]);
    let message = &replay.messages[0];
    assert_eq!(message.role, "system");
    assert!(message.tool_calls.is_empty());
    assert!(message.tool_call_id.is_none());
    assert!(message.name.is_none());
    let content = message.content.as_deref().expect("context content");
    assert!(content.contains("Earlier grounded answer evidence"));
    assert!(content.contains(&chunk_id.to_string()));
    assert!(content.contains("section: Setup > Configuration"));
    assert!(content.contains("alphaSecret"));
    let package_position = content.find("alpha-connector").expect("package chunk");
    let config_position = content.find("alphaSecret").expect("config chunk");
    assert!(package_position < config_position);
}

#[test]
fn prior_grounded_answer_context_messages_filter_foreign_library_chunks() {
    let library_id = Uuid::now_v7();
    let foreign_library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let kept_chunk_id = Uuid::now_v7();
    let foreign_chunk_id = Uuid::now_v7();
    let references = vec![
        test_chunk_reference(foreign_chunk_id, 1, 0.99),
        test_chunk_reference(kept_chunk_id, 2, 0.88),
    ];
    let chunks = vec![
        test_chunk_row(
            foreign_library_id,
            foreign_chunk_id,
            "Foreign connector setting must not cross library boundaries.",
            &["Foreign"],
        ),
        test_chunk_row(
            library_id,
            kept_chunk_id,
            "Connector Beta keeps betaWindow in [Beta].",
            &["Setup", "Beta"],
        ),
    ];

    let replay = build_prior_grounded_answer_context_messages(
        library_id,
        execution_id,
        "How is Connector Beta configured?",
        &references,
        &chunks,
        4_000,
    )
    .expect("filtered prior grounded answer replay");

    assert_eq!(replay.chunk_ids, vec![kept_chunk_id]);
    let content = replay.messages[0].content.as_deref().expect("context content");
    assert!(content.contains("betaWindow"));
    assert!(!content.contains("Foreign connector"));
    assert!(!content.contains(&foreign_chunk_id.to_string()));
}

#[test]
fn prior_grounded_answer_replay_selection_preserves_newest_first_order() {
    let library_id = Uuid::now_v7();
    let foreign_library_id = Uuid::now_v7();
    let latest_id = Uuid::now_v7();
    let older_id = Uuid::now_v7();
    let failed_id = Uuid::now_v7();
    let foreign_id = Uuid::now_v7();
    let now = Utc::now();
    let executions = vec![
        test_query_execution_row(
            library_id,
            latest_id,
            RuntimeLifecycleState::Completed,
            None,
            "latest precise setup question",
            now,
        ),
        test_query_execution_row(
            library_id,
            failed_id,
            RuntimeLifecycleState::Failed,
            Some("tool_error"),
            "failed setup question",
            now,
        ),
        test_query_execution_row(
            foreign_library_id,
            foreign_id,
            RuntimeLifecycleState::Completed,
            None,
            "foreign library question",
            now,
        ),
        test_query_execution_row(
            library_id,
            older_id,
            RuntimeLifecycleState::Completed,
            None,
            "older broad setup question",
            now,
        ),
    ];

    let selected = select_prior_grounded_answer_replay_executions(executions, library_id, 2);

    let selected_ids = selected.iter().map(|execution| execution.id).collect::<Vec<_>>();
    assert_eq!(selected_ids, vec![latest_id, older_id]);
}

fn test_chunk_reference(chunk_id: Uuid, rank: i32, score: f64) -> KnowledgeBundleChunkReferenceRow {
    KnowledgeBundleChunkReferenceRow {
        bundle_id: Uuid::now_v7(),
        chunk_id,
        rank,
        score,
        inclusion_reason: Some("synthetic".to_string()),
        created_at: Utc::now(),
    }
}

fn test_query_execution_row(
    library_id: Uuid,
    execution_id: Uuid,
    runtime_lifecycle_state: RuntimeLifecycleState,
    failure_code: Option<&str>,
    query_text: &str,
    started_at: chrono::DateTime<Utc>,
) -> query_repository::QueryExecutionRow {
    query_repository::QueryExecutionRow {
        id: execution_id,
        workspace_id: Uuid::now_v7(),
        library_id,
        conversation_id: Uuid::now_v7(),
        context_bundle_id: Uuid::now_v7(),
        request_turn_id: Some(Uuid::now_v7()),
        response_turn_id: Some(Uuid::now_v7()),
        binding_id: Some(Uuid::now_v7()),
        runtime_execution_id: Uuid::now_v7(),
        runtime_lifecycle_state,
        runtime_active_stage: None,
        turn_budget: 5,
        turn_count: 1,
        parallel_action_limit: 3,
        query_text: query_text.to_string(),
        failure_code: failure_code.map(str::to_string),
        failure_summary_redacted: None,
        started_at,
        completed_at: Some(started_at),
    }
}

#[test]
fn runtime_summary_view_does_not_expose_persisted_diagnostic_or_query_text() {
    let library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let now = Utc::now();
    let private_query = "view-sentinel-private-query";
    let private_diagnostic = "view-sentinel-secret-diagnostic";
    let mut row = test_query_execution_row(
        library_id,
        execution_id,
        RuntimeLifecycleState::Failed,
        Some("query_provider_failed"),
        private_query,
        now,
    );
    row.failure_summary_redacted = Some(format!("{private_diagnostic}: {private_query}"));

    let summary = map_execution_runtime_summary(&row, &[]);
    let exposed_json = serde_json::to_string(&summary).expect("serialize runtime summary view");

    assert_eq!(summary.failure_summary_redacted.as_deref(), Some("query_provider_failed"));
    assert!(!exposed_json.contains(private_diagnostic));
    assert!(!exposed_json.contains(private_query));
}

fn test_chunk_row(
    library_id: Uuid,
    chunk_id: Uuid,
    content_text: &str,
    heading_trail: &[&str],
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id,
        workspace_id: Uuid::now_v7(),
        library_id,
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("text".to_string()),
        content_text: content_text.to_string(),
        normalized_text: content_text.to_string(),
        span_start: None,
        span_end: None,
        token_count: None,
        support_block_ids: Vec::new(),
        section_path: heading_trail.iter().map(|value| (*value).to_string()).collect(),
        heading_trail: heading_trail.iter().map(|value| (*value).to_string()).collect(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: None,
        window_text: None,
        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
    }
}
