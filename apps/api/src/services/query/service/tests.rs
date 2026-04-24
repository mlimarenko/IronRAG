use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::{
    domains::query::{PreparedSegmentReference, QueryTurnKind, QueryVerificationState},
    infra::{
        arangodb::{
            context_store::{KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow},
            document_store::KnowledgeStructuredBlockRow,
            graph_store::KnowledgeEvidenceRow,
        },
        repositories::query_repository,
    },
};

use super::{
    ExternalConversationTurn, MAX_DETAIL_PREPARED_SEGMENT_REFERENCES,
    MAX_DETAIL_TECHNICAL_FACT_REFERENCES, RankedBundleReference,
    context::{derive_fact_rank_refs, selected_fact_ids_for_detail},
    formatting::{
        build_prepared_segment_references, parse_query_verification_state,
        render_answer_source_links,
    },
    session::{
        build_conversation_runtime_context,
        build_conversation_runtime_context_with_external_history,
    },
};

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
        text: "control center".to_string(),
        normalized_text: "control center".to_string(),
        heading_trail: vec!["Acme Control Center - Example".to_string()],
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
        "Что такое Acme Control Center?",
        &HashMap::new(),
    );

    assert_eq!(
        references.first().and_then(|reference| reference.heading_trail.first().cloned()),
        Some("Acme Control Center - Example".to_string())
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
fn render_answer_source_links_deduplicates_hrefs_and_preserves_titles() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let stored_href =
        format!("/v1/content/documents/{document_id}/source?revisionId={revision_id}");
    let references = vec![
        PreparedSegmentReference {
            execution_id: Uuid::now_v7(),
            segment_id: Uuid::now_v7(),
            revision_id,
            block_kind:
                crate::shared::extraction::structured_document::StructuredBlockKind::Paragraph,
            rank: 1,
            score: 0.98,
            heading_trail: vec!["Install".to_string()],
            section_path: vec!["install".to_string()],
            document_id: Some(document_id),
            document_title: Some("runtime-upload-check.pdf".to_string()),
            source_uri: Some("upload://runtime-upload-check.pdf".to_string()),
            source_access: Some(crate::domains::content::ContentSourceAccess {
                kind: crate::domains::content::ContentSourceAccessKind::StoredDocument,
                href: stored_href.clone(),
            }),
        },
        PreparedSegmentReference {
            execution_id: Uuid::now_v7(),
            segment_id: Uuid::now_v7(),
            revision_id,
            block_kind:
                crate::shared::extraction::structured_document::StructuredBlockKind::Paragraph,
            rank: 2,
            score: 0.88,
            heading_trail: vec!["Install".to_string()],
            section_path: vec!["install".to_string()],
            document_id: Some(document_id),
            document_title: Some("runtime-upload-check.pdf".to_string()),
            source_uri: Some("upload://runtime-upload-check.pdf".to_string()),
            source_access: Some(crate::domains::content::ContentSourceAccess {
                kind: crate::domains::content::ContentSourceAccessKind::StoredDocument,
                href: stored_href,
            }),
        },
        PreparedSegmentReference {
            execution_id: Uuid::now_v7(),
            segment_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            block_kind:
                crate::shared::extraction::structured_document::StructuredBlockKind::Paragraph,
            rank: 3,
            score: 0.74,
            heading_trail: vec!["API".to_string()],
            section_path: vec!["api".to_string()],
            document_id: Some(Uuid::now_v7()),
            document_title: Some("Docs".to_string()),
            source_uri: Some("https://example.com/docs".to_string()),
            source_access: Some(crate::domains::content::ContentSourceAccess {
                kind: crate::domains::content::ContentSourceAccessKind::ExternalUrl,
                href: "https://example.com/docs".to_string(),
            }),
        },
    ];

    let rendered =
        render_answer_source_links(&references).expect("answer source links should render");

    assert!(rendered.starts_with("Sources\n"));
    assert_eq!(rendered.lines().filter(|line| line.starts_with("- ")).count(), 2);
    assert!(rendered.contains("runtime-upload-check.pdf"));
    assert!(rendered.contains("https://example.com/docs"));
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
        content_text: "как в the product перемещение сделать скажи".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let assistant_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::Assistant,
        author_principal_id: None,
        content_text: "Могу сразу расписать это пошагово для the product.".to_string(),
        execution_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
    };
    let follow_up_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 3,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "давай".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context = build_conversation_runtime_context(
        &[first_user_turn, assistant_turn, follow_up_turn.clone()],
        follow_up_turn.id,
    );

    assert!(context.effective_query_text.contains("как в the product перемещение сделать скажи"));
    assert!(
        !context
            .effective_query_text
            .contains("Могу сразу расписать это пошагово для the product.")
    );
    assert!(context.effective_query_text.ends_with("давай"));
    assert_eq!(
        context.prompt_history_text.as_deref(),
        Some(
            "User: как в the product перемещение сделать скажи\nAssistant: Могу сразу расписать это пошагово для the product."
        )
    );
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
    assert!(context.coreference_entities.contains(&"targetSecret".to_string()));
    assert!(!context.coreference_entities.contains(&"alphaSecret".to_string()));
    assert!(context.effective_query_text.ends_with("TargetNme how"));
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
        content_text: "как перемещение оформить".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };
    let second_turn = query_repository::QueryTurnRow {
        id: Uuid::now_v7(),
        conversation_id,
        turn_index: 2,
        turn_kind: QueryTurnKind::User,
        author_principal_id: None,
        content_text: "как в the product перемещение сделать скажи".to_string(),
        execution_id: None,
        created_at: Utc::now(),
    };

    let context =
        build_conversation_runtime_context(&[first_turn, second_turn.clone()], second_turn.id);

    assert_eq!(context.effective_query_text, "как в the product перемещение сделать скажи");
    assert_eq!(context.prompt_history_text, None);
}

