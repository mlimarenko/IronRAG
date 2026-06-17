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
        arangodb::{
            context_store::{
                KnowledgeBundleChunkReferenceRow, KnowledgeBundleEntityReferenceRow,
                KnowledgeBundleRelationReferenceRow, KnowledgeContextBundleReferenceSetRow,
                KnowledgeContextBundleRow,
            },
            document_store::{KnowledgeChunkRow, KnowledgeStructuredBlockRow},
            graph_store::KnowledgeEvidenceRow,
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
        seed_relation_refs_from_answer_graph, selected_fact_ids_for_detail,
    },
    formatting::{
        build_prepared_segment_references, map_entity_references, map_relation_references,
        parse_query_verification_state,
    },
    session::{
        build_conversation_runtime_context,
        build_conversation_runtime_context_with_external_history,
        build_prior_grounded_answer_context_messages,
        select_prior_grounded_answer_replay_executions,
        should_replay_prior_grounded_answer_context,
    },
};

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
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
            crate::infra::arangodb::context_store::KnowledgeBundleEvidenceReferenceRow {
                key: format!("{bundle_id}:{evidence_id}"),
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
        key: evidence_id.to_string(),
        arango_id: None,
        arango_rev: None,
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
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
                    key: format!("{bundle_id}:{entity_id}"),
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
                    key: format!("{bundle_id}:{relation_id}"),
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
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
            key: block_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
        key: control_heading_id.to_string(),
        arango_id: None,
        arango_rev: None,
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
fn parse_query_verification_state_maps_canonical_values() {
    assert_eq!(parse_query_verification_state("verified"), QueryVerificationState::Verified);
    assert_eq!(
        parse_query_verification_state("insufficient_evidence"),
        QueryVerificationState::InsufficientEvidence
    );
    assert_eq!(parse_query_verification_state("unknown"), QueryVerificationState::NotRun);
}

#[test]
fn build_conversation_runtime_context_rewrites_short_follow_up_from_history() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "tell me how to move items in the product".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Sure, here are the product steps.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "continue".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    assert!(context.effective_query_text.contains("tell me how to move items in the product"));
    assert!(context.contextual_follow_up);
    assert!(!context.effective_query_text.contains("Sure, here are the product steps."));
    assert!(context.effective_query_text.starts_with("scope: "));
    assert!(context.effective_query_text.contains("\nquestion: continue"));
    assert!(context.effective_query_text.ends_with("continue"));
    assert_eq!(
        context.prompt_history_text.as_deref(),
        Some(
            "User: tell me how to move items in the product\nAssistant: Sure, here are the product steps."
        )
    );
    assert_eq!(
        context.query_planning_history_text.as_deref(),
        Some("User: tell me how to move items in the product")
    );
    assert_eq!(context.prompt_history_messages.len(), 2);
    assert_eq!(context.prompt_history_messages[0].role, "user");
    assert_eq!(context.prompt_history_messages[1].role, "assistant");
}

#[test]
fn build_conversation_runtime_context_prefers_matching_history_snippet_for_short_follow_up() {
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
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "\
Connector Alpha uses the [Alpha] section with `alphaSecret`.
Connector TargetName uses the [TargetName] section with `targetSecret` and merchantId."
            .to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "TargetNme how".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    assert!(context.effective_query_text.contains("which connector variants exist"));
    assert!(context.effective_query_text.contains("Connector TargetName"));
    assert!(context.effective_query_text.contains("targetSecret"));
    assert!(!context.effective_query_text.contains("Connector Alpha"));
    assert!(context.effective_query_text.starts_with("scope: "));
    assert!(context.effective_query_text.contains("\nquestion: TargetNme how"));
    assert!(context.coreference_entities.contains(&"TargetName".to_string()));
    assert!(!context.coreference_entities.contains(&"targetSecret".to_string()));
    assert!(!context.coreference_entities.contains(&"alphaSecret".to_string()));
    assert!(context.effective_query_text.ends_with("TargetNme how"));
    assert_eq!(
        context.query_planning_history_text.as_deref(),
        Some("User: which connector variants exist")
    );
    assert_eq!(context.prompt_history_messages.len(), 2);
}

#[test]
fn build_conversation_runtime_context_keeps_standalone_question_without_rewrite() {
    let conversation_id = Uuid::now_v7();
    let first_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "how to fill in a transfer".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let second_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "tell me how to move items in the product".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context =
        build_conversation_runtime_context(&[first_turn, second_turn.clone()], second_turn.id);

    assert_eq!(context.effective_query_text, "tell me how to move items in the product");
    assert!(!context.contextual_follow_up);
    assert_eq!(context.prompt_history_text.as_deref(), Some("User: how to fill in a transfer"));
    assert_eq!(context.query_planning_history_text, None);
    assert_eq!(context.prompt_history_messages.len(), 1);
    assert_eq!(context.prompt_history_messages[0].role, "user");
    assert!(
        !should_replay_prior_grounded_answer_context(&context),
        "standalone questions should not receive prior grounded-answer chunk replay"
    );
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
fn build_conversation_runtime_context_scopes_history_overlapping_follow_up() {
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
        content_text: "To configure Provider Alpha settings, use `alphaPackage`, `/opt/alpha.conf`, `alphaTimeout`, and `alphaMode`.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "explain how to configure all settings".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    assert!(context.effective_query_text.starts_with("scope: "));
    assert!(context.contextual_follow_up);
    assert!(context.effective_query_text.contains("ir.memory.anchors.v1:"));
    assert!(context.effective_query_text.contains("`alphaPackage`"));
    assert!(context.effective_query_text.contains("`/opt/alpha.conf`"));
    assert!(context.effective_query_text.ends_with("explain how to configure all settings"));
    assert!(context.coreference_entities.contains(&"Provider".to_string()));
    assert!(context.coreference_entities.contains(&"Alpha".to_string()));
    assert!(should_replay_prior_grounded_answer_context(&context));
}

#[test]
fn build_conversation_runtime_context_scopes_medium_length_assistant_follow_up() {
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
        content_text: "Provider Alpha uses `alphaPackage`, `/opt/alpha.conf`, `alphaTimeout`, and `alphaMode`.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "show me every setting now".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    assert!(context.contextual_follow_up);
    assert!(context.effective_query_text.starts_with("scope: "));
    assert!(context.effective_query_text.contains("Provider Alpha"));
    assert!(context.effective_query_text.contains("ir.memory.anchors.v1:"));
    assert!(context.effective_query_text.contains("`alphaPackage`"));
    assert!(context.effective_query_text.ends_with("show me every setting now"));
    assert!(should_replay_prior_grounded_answer_context(&context));
}

#[test]
fn build_conversation_runtime_context_avoids_polluted_latest_assistant_literals() {
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
    let choice_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Available variants: Provider Alpha, Provider Beta.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let subject_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "Provider Alpha".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let good_answer_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 4,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Provider Alpha uses `alphaPackage`, `/opt/alpha.conf`, and `alphaTimeout`."
            .to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let config_follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 5,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "show every setting now".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let polluted_answer_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 6,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Provider Alpha remains the selected variant.\n`betaPackage`\n`/opt/beta.conf`\n`betaPort`"
            .to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let procedure_follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 7,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "show complete steps".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[
            first_user_turn,
            choice_turn,
            subject_turn,
            good_answer_turn,
            config_follow_up_turn,
            polluted_answer_turn,
            procedure_follow_up_turn.clone(),
        ],
        procedure_follow_up_turn.id,
    );

    assert!(context.contextual_follow_up);
    assert!(context.effective_query_text.contains("ir.memory.anchors.v1:"));
    assert!(context.effective_query_text.contains("`alphaPackage`"));
    assert!(context.effective_query_text.contains("`/opt/alpha.conf`"));
    assert!(!context.effective_query_text.contains("`betaPackage`"));
    assert!(!context.effective_query_text.contains("`/opt/beta.conf`"));
    assert!(context.coreference_entities.contains(&"Provider".to_string()));
    assert!(context.coreference_entities.contains(&"Alpha".to_string()));
    assert!(!context.coreference_entities.contains(&"Beta".to_string()));
}

#[test]
fn build_conversation_runtime_context_keeps_new_topic_question_out_of_prior_config_answer() {
    let conversation_id = Uuid::now_v7();
    let troubleshooting_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "what should we do when Provider Alpha terminal loses payment confirmation"
            .to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "which connector variants exist".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Available variants: Provider Alpha, Provider Beta.".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "Provider Alpha".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Provider Alpha setup uses `alphaPackage`, `/opt/alpha.conf`, `alphaTimeout`, and `alphaMode`."
                .to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "show every setting now".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text:
                "Use `alphaPackage`, `/opt/alpha.conf`, `[Main]`, `alphaTimeout`, and `alphaMode`."
                    .to_string(),
        },
    ];

    let context = build_conversation_runtime_context_with_external_history(
        &[troubleshooting_turn.clone()],
        troubleshooting_turn.id,
        &external_prior_turns,
    );

    assert!(context.contextual_follow_up);
    assert!(context.effective_query_text.starts_with("scope: "));
    assert!(context.effective_query_text.contains("topic: Provider Alpha"));
    assert!(context.effective_query_text.contains("Provider Alpha terminal"));
    assert!(!context.effective_query_text.contains("ir.memory.anchors.v1:"));
    assert!(!context.effective_query_text.contains("`alphaPackage`"));
    assert!(!context.effective_query_text.contains("`/opt/alpha.conf`"));
    assert!(!context.effective_query_text.contains("`alphaTimeout`"));
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
fn build_conversation_runtime_context_drops_compact_memory_for_standalone_question() {
    let conversation_id = Uuid::now_v7();
    let first_user_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "which record statuses are supported?".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text:
            "ir.memory.literals.v1: `status.pending`, `status.active`, `status.archived`\n\
             The record status values are `status.pending`, `status.active`, and `status.archived`."
                .to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "What callback events are supported, and how are callback payloads signed?"
            .to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, current_turn.clone()],
        current_turn.id,
    );

    assert!(!context.contextual_follow_up);
    assert!(
        context
            .prompt_history_text
            .as_deref()
            .is_none_or(|text| !text.contains("ir.memory.literals.v1: ")),
        "standalone answer history text must not inherit compact literal memory"
    );
    assert!(
        context.prompt_history_messages.iter().all(|message| !message
            .content
            .as_deref()
            .unwrap_or_default()
            .contains("ir.memory.literals.v1: ")),
        "standalone questions must not inherit compact literal memory as prompt evidence"
    );
    assert!(
        context
            .grounded_answer_tool_history
            .iter()
            .all(|turn| !turn.content_text.contains("ir.memory.literals.v1: ")),
        "standalone grounded_answer calls must not inherit compact literal memory"
    );
}

#[test]
fn build_conversation_runtime_context_pins_old_assistant_literals_for_follow_up() {
    let conversation_id = Uuid::now_v7();
    let mut turns = Vec::new();
    turns.push(query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "list package identifiers".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    });
    turns.push(query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Found `pkg-alpha`, `pkg-beta`, `/opt/pkg-alpha.conf`, and `alphaTimeout`."
            .to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    });
    for pair_index in 0..7 {
        turns.push(query_repository::QueryTurnRow {
            id: Uuid::now_v7(),
            conversation_id,
            turn_index: 3 + pair_index * 2,
            turn_kind: QueryTurnKind::User,
            author_principal_id: None,
            content_text: format!("unrelated checkpoint {pair_index}"),
            execution_id: None,
            created_at: Utc::now(),
        });
        turns.push(query_repository::QueryTurnRow {
            id: Uuid::now_v7(),
            conversation_id,
            turn_index: 4 + pair_index * 2,
            turn_kind: QueryTurnKind::Assistant,
            author_principal_id: None,
            content_text: format!("checkpoint acknowledged {pair_index}"),
            execution_id: Some(Uuid::now_v7()),
            created_at: Utc::now(),
        });
    }
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 17,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "continue".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    turns.push(follow_up_turn.clone());

    let context = build_conversation_runtime_context(&turns, follow_up_turn.id);

    assert!(context.effective_query_text.contains("ir.memory.anchors.v1:"));
    assert!(context.effective_query_text.contains("`pkg-alpha`"));
    assert!(context.effective_query_text.contains("`alphaTimeout`"));

    let prompt_anchor_message = context
        .prompt_history_messages
        .iter()
        .find(|message| {
            message.role == "system"
                && message.content.as_deref().is_some_and(|content| {
                    content.contains("ir.context.pinned-literal-anchors.v1:")
                })
        })
        .expect("pinned anchors should reach prompt history");
    let prompt_anchor_text = prompt_anchor_message.content.as_deref().unwrap_or_default();
    assert!(prompt_anchor_text.contains("`pkg-beta`"));
    assert!(prompt_anchor_text.contains("`/opt/pkg-alpha.conf`"));

    let tool_anchor_turn = context
        .grounded_answer_tool_history
        .first()
        .expect("pinned anchors should reach tool history");
    assert!(matches!(tool_anchor_turn.turn_kind, QueryTurnKind::Assistant));
    assert!(tool_anchor_turn.content_text.starts_with("ir.memory.anchors.v1:"));
    assert!(tool_anchor_turn.content_text.contains("`pkg-alpha`"));
    assert!(tool_anchor_turn.content_text.contains("`alphaTimeout`"));
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
fn build_conversation_runtime_context_scopes_external_history_turn_even_when_current_text_is_long()
{
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
        &[current_turn.clone()],
        current_turn.id,
        &external_prior_turns,
    );

    assert_eq!(
        context.query_planning_history_text.as_deref(),
        Some("User: what does the library say about payment setup")
    );
    assert_eq!(
        context.effective_query_text,
        "scope: what does the library say about payment setup\nquestion: now enumerate the integration variants and missing limits"
    );
}

#[test]
fn build_conversation_runtime_context_ignores_question_marker_for_history_focus() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "Q100. summarize ports".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Q100 appears only in a page footer marker.".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Network ports include transport endpoints for Alpha Suite.".to_string(),
        },
    ];

    let context = build_conversation_runtime_context_with_external_history(
        &[current_turn.clone()],
        current_turn.id,
        &external_prior_turns,
    );

    assert!(context.effective_query_text.contains("Network ports include transport endpoints"));
    assert!(!context.effective_query_text.contains("page footer marker"));
    assert!(context.effective_query_text.ends_with("question: Q100. summarize ports"));
}

#[test]
fn build_conversation_runtime_context_preserves_prior_answer_literals_for_external_follow_up() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "describe each item".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "which package-like modules exist".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "`pkg-alpha`\n`pkg-beta`\n`pkg-gamma`\n`pkg-delta`\n`pkg-epsilon`\n`pkg-zeta`\n`pkg-eta`\n`pkg-theta`\n`pkg-iota`\n`pkg-kappa`\n`pkg-lambda`\n`pkg-mu`".to_string(),
        },
    ];

    let context = build_conversation_runtime_context_with_external_history(
        &[current_turn.clone()],
        current_turn.id,
        &external_prior_turns,
    );

    assert!(context.effective_query_text.starts_with("scope: "));
    assert!(context.effective_query_text.contains("which package-like modules exist"));
    assert!(!context.effective_query_text.contains("entities:"));
    assert!(context.effective_query_text.contains("ir.memory.anchors.v1:"));
    assert!(context.effective_query_text.contains("`pkg-alpha`"));
    assert!(context.effective_query_text.contains("`pkg-mu`"));
    assert!(context.effective_query_text.ends_with("describe each item"));
    assert!(!context.coreference_entities.contains(&"pkg-alpha".to_string()));
    assert!(!context.coreference_entities.contains(&"pkg-mu".to_string()));
    let tool_history = context
        .grounded_answer_tool_history
        .iter()
        .map(|turn| turn.content_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(tool_history.contains("`pkg-alpha`"));
    assert!(tool_history.contains("`pkg-mu`"));
}

#[test]
fn build_conversation_runtime_context_converts_compacted_literals_to_retrieval_anchors() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "explain all settings".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "how do I configure Sample Subject".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "ir.memory.literals.v1: `sample-module`, `/opt/sample/sample.conf`, `enableSample`, `/var/log/sample.log`\nSample Subject settings use a module package, a module configuration file, and parameter defaults.".to_string(),
        },
    ];

    let context = build_conversation_runtime_context_with_external_history(
        &[current_turn.clone()],
        current_turn.id,
        &external_prior_turns,
    );

    assert!(context.effective_query_text.starts_with("scope: "));
    assert!(context.effective_query_text.contains("Sample Subject settings use"));
    assert!(!context.effective_query_text.contains("ir.memory.literals.v1:"));
    assert!(context.effective_query_text.contains("ir.memory.anchors.v1:"));
    assert!(context.effective_query_text.contains("`/var/log/sample.log`"));
    assert!(context.effective_query_text.contains("`enableSample`"));
    assert!(context.effective_query_text.ends_with("explain all settings"));
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
        &[current_turn.clone()],
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
fn build_conversation_runtime_context_scopes_four_token_external_follow_up() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "show full ready config".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "how do I configure Provider Alpha".to_string(),
        },
        ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Provider Alpha uses `alpha-provider-module`, `/opt/alpha/alpha.conf`, and `enableAlpha`.".to_string(),
        },
    ];

    let context = build_conversation_runtime_context_with_external_history(
        &[current_turn.clone()],
        current_turn.id,
        &external_prior_turns,
    );

    assert!(context.effective_query_text.starts_with("scope: "));
    assert!(context.effective_query_text.contains("how do I configure Provider Alpha"));
    assert!(context.effective_query_text.contains("entities: Provider, Alpha"));
    assert!(context.effective_query_text.contains("ir.memory.anchors.v1:"));
    assert!(context.effective_query_text.contains("`alpha-provider-module`"));
    assert!(context.effective_query_text.contains("`/opt/alpha/alpha.conf`"));
    assert!(context.effective_query_text.ends_with("show full ready config"));
    let tool_history = context
        .grounded_answer_tool_history
        .iter()
        .map(|turn| turn.content_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(tool_history.contains("`alpha-provider-module`"));
    assert!(tool_history.contains("`/opt/alpha/alpha.conf`"));
}

#[test]
fn build_conversation_runtime_context_drops_plain_backtick_words_from_literal_anchors() {
    let conversation_id = Uuid::now_v7();
    let current_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "explain settings".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let external_prior_turns = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "Use `setup` and `engine`; configure `alphaModule` in `/etc/alpha.conf`."
            .to_string(),
    }];

    let context = build_conversation_runtime_context_with_external_history(
        &[current_turn.clone()],
        current_turn.id,
        &external_prior_turns,
    );

    let anchor_line = context
        .effective_query_text
        .lines()
        .find(|line| line.contains("ir.memory.anchors.v1:"))
        .expect("literal anchor scope");
    assert!(anchor_line.contains("`alphaModule`"));
    assert!(anchor_line.contains("`/etc/alpha.conf`"));
    assert!(!anchor_line.contains("`setup`"));
    assert!(!anchor_line.contains("`engine`"));
}

#[test]
fn build_conversation_runtime_context_standalone_question_after_assistant_answer_drops_coreference()
{
    let conversation_id = Uuid::now_v7();
    let first_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 1,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "how do I configure connector alpha".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Connector Alpha uses `alphaSecret` in section [Alpha].".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let standalone_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "what is the dashboard session timeout setting".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_turn, assistant_turn, standalone_turn.clone()],
        standalone_turn.id,
    );

    assert_eq!(context.effective_query_text, "what is the dashboard session timeout setting");
    assert!(!context.contextual_follow_up);
    assert_eq!(
        context.prompt_history_text.as_deref(),
        Some(
            "User: how do I configure connector alpha\nAssistant: Connector Alpha uses `alphaSecret` in section [Alpha]."
        )
    );
    assert_eq!(context.query_planning_history_text, None);
    assert_eq!(context.prompt_history_messages.len(), 2);
    assert!(context.coreference_entities.is_empty());
    assert!(
        !context.effective_query_text.contains("alphaSecret"),
        "standalone query should not be rewritten with prior entities"
    );
    assert!(
        !context.effective_query_text.contains("ir.memory.anchors.v1:"),
        "standalone query should not inherit prior literal anchors"
    );
    assert!(
        !should_replay_prior_grounded_answer_context(&context),
        "standalone questions after assistant answers should not replay prior grounded chunks"
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
        key: format!("chunk-ref-{chunk_id}"),
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

fn test_chunk_row(
    library_id: Uuid,
    chunk_id: Uuid,
    content_text: &str,
    heading_trail: &[&str],
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        key: format!("chunk-{chunk_id}"),
        arango_id: None,
        arango_rev: None,
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
