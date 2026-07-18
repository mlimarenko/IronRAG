use super::*;
use crate::domains::query_ir::{
    EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryIR, QueryLanguage,
    QueryScope,
};

fn chunk_row(chunk_index: i32, text: &str) -> KnowledgeChunkRow {
    let chunk_id = Uuid::now_v7();
    KnowledgeChunkRow {
        chunk_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index,
        chunk_kind: if text.contains("[source_profile ") {
            Some("source_profile".to_string())
        } else {
            Some("paragraph".to_string())
        },
        content_text: text.to_string(),
        normalized_text: text.to_string(),
        span_start: None,
        span_end: None,
        token_count: None,
        support_block_ids: Vec::new(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),
        window_text: None,
        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
    }
}

fn exact_literal_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::ConfigKey],
        target_entities: Vec::new(),
        literal_constraints: vec![LiteralSpan {
            text: "route_map".to_string(),
            kind: LiteralKind::Identifier,
        }],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

fn low_confidence_short_token_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.25,
    }
}

#[test]
fn untyped_low_confidence_graph_evidence_stays_complete() {
    let lines = vec![
        "[graph-evidence target=\"QX\"] QX alphaFlag = true".to_string(),
        "[graph-evidence target=\"ZZ\"] ZZ betaFlag = true".to_string(),
    ];

    let rendered = render_graph_evidence_context_lines_for_focus(
        "QX settings",
        &lines,
        None,
        &low_confidence_short_token_ir(),
    );

    assert!(rendered.contains("QX alphaFlag"));
    assert!(rendered.contains("ZZ betaFlag"));
}

#[test]
fn contextual_low_confidence_setup_context_keeps_related_parameter_chunks() {
    let setup_document_id = Uuid::now_v7();
    let parameter_document_id = Uuid::now_v7();
    let setup_revision_id = Uuid::now_v7();
    let parameter_revision_id = Uuid::now_v7();
    let question = "scope: Provider Alpha setup was selected earlier\nliteral anchors: `https://alpha.local/api`\nquestion: provider_alpha_setup.md Provider Alpha module configuration all parameters url";
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: setup_revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id: setup_document_id,
            document_label: "provider_alpha_setup.md".to_string(),
            excerpt: "Install alpha-module and edit /opt/alpha/alpha.conf.".to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(0.96),
            source_text:
                "Install alpha-module. Edit /opt/alpha/alpha.conf in [Main]. url = https://alpha.local/api."
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: parameter_revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id: parameter_document_id,
            document_label: "provider_alpha_change_notes.md".to_string(),
            excerpt: "Provider Alpha parameter table.".to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(0.95),
            source_text:
                "| alphaPrintSlip | boolean | true false | Print the slip | | alphaFillDetails | boolean | true false | Fill detail fields |"
                    .to_string(),
        },
    ];
    let context = build_canonical_answer_context(
        question,
        &low_confidence_short_token_ir(),
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &chunks,
        &[],
    );

    assert!(context.contains("/opt/alpha/alpha.conf"), "{context}");
    assert!(context.contains("alphaPrintSlip"), "{context}");
    assert!(context.contains("alphaFillDetails"), "{context}");
}

#[test]
fn source_coverage_is_enabled_for_exact_literal_queries() {
    assert!(should_request_source_coverage_chunks("route_map", &exact_literal_query_ir()));
    assert!(should_request_source_coverage_chunks(
        "configure package",
        &QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![
                crate::domains::query_ir::QueryTargetKind::Package,
                crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
                crate::domains::query_ir::QueryTargetKind::ConfigKey,
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    ));
    assert!(should_request_source_coverage_chunks(
        "how to update Sample Target",
        &QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![
                crate::domains::query_ir::QueryTargetKind::Procedure,
                crate::domains::query_ir::QueryTargetKind::Concept
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: Some("how to update Sample Target".to_string()),
            confidence: 0.9,
        }
    ));
    assert!(!should_request_source_coverage_chunks(
        "compare these documents",
        &QueryIR {
            act: QueryAct::Compare,
            scope: QueryScope::MultiDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    ));
}

#[test]
fn source_coverage_is_enabled_for_bounded_inventory_queries() {
    assert!(should_request_source_coverage_chunks(
        "what values are exposed?",
        &QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![
                crate::domains::query_ir::QueryTargetKind::Attribute,
                crate::domains::query_ir::QueryTargetKind::Record
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    ));
    assert!(should_request_source_coverage_chunks(
        "what entries are defined?",
        &QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: vec![EntityMention {
                label: "Subject Alpha".to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    ));
    assert!(should_request_source_coverage_chunks(
        "which exact value is defined?",
        &QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: vec![LiteralSpan {
                text: "alpha_key".to_string(),
                kind: LiteralKind::Identifier,
            }],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.25,
        }
    ));
    assert!(!query_ir_requests_inventory_source_coverage(&QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.25,
    }));
    assert!(!query_ir_requests_inventory_source_coverage(&QueryIR {
        act: QueryAct::Meta,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Facet],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }));
    assert!(!query_ir_requests_inventory_source_coverage(&QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.25,
    }));
}

#[test]
fn source_coverage_selection_keeps_profile_edges_and_middle() {
    let rows = (0..10)
        .map(|index| {
            if index == 5 {
                chunk_row(index, "[source_profile source_format=record_jsonl unit_count=42]")
            } else {
                chunk_row(index, &format!("chunk {index}"))
            }
        })
        .collect::<Vec<_>>();

    let selected = select_source_coverage_chunk_rows(rows, 8, &[]);
    let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

    assert!(selected_indexes.contains(&0));
    assert!(selected_indexes.contains(&1));
    assert!(selected_indexes.contains(&4));
    assert!(selected_indexes.contains(&5));
    assert!(selected_indexes.contains(&8));
    assert!(selected_indexes.contains(&9));
    assert!(selected.iter().any(is_source_profile_chunk));
}

/// Regression guard for the long-document configuration retrieval
/// gap. A 27-chunk source document at limit=12 used to produce a
/// gap from index 14 to index 25 because the stride fill stopped
/// once it accumulated 12 indices counting the forced head/middle/
/// tail anchors. On real data this skipped exactly the chunk
/// holding the canonical INI block, so the model truthfully
/// reported the context as incomplete.
///
/// With greedy max-min coverage the selector must hit at least
/// one index in every quartile of the document, so a long-doc
/// config query covers the full index range.
#[test]
fn select_source_coverage_chunk_rows_covers_long_document_without_quartile_gap() {
    let total = 27_usize;
    let limit = 12;
    let rows = (0..total)
        .map(|index| chunk_row(i32::try_from(index).expect("index in i32 range"), "body"))
        .collect::<Vec<_>>();

    let selected = select_source_coverage_chunk_rows(rows, limit, &[]);
    let mut selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();
    selected_indexes.sort();

    assert_eq!(selected_indexes.len(), limit);

    let quartile = total / 4;
    let in_q1 = selected_indexes.iter().any(|index| (*index as usize) < quartile);
    let in_q2 = selected_indexes
        .iter()
        .any(|index| (*index as usize) >= quartile && (*index as usize) < 2 * quartile);
    let in_q3 = selected_indexes
        .iter()
        .any(|index| (*index as usize) >= 2 * quartile && (*index as usize) < 3 * quartile);
    let in_q4 = selected_indexes.iter().any(|index| (*index as usize) >= 3 * quartile);

    assert!(in_q1, "first quartile must be represented: {selected_indexes:?}");
    assert!(in_q2, "second quartile must be represented: {selected_indexes:?}");
    assert!(
        in_q3,
        "third quartile must be represented (regression of stride-fill gap that skipped this range): {selected_indexes:?}"
    );
    assert!(in_q4, "fourth quartile must be represented: {selected_indexes:?}");

    // Maximum gap between consecutive selected indices must stay
    // bounded — on a 27-chunk document at limit=12 no run of
    // unselected indices should exceed `total / (limit - 1) + 2`
    // which is roughly the spacing of an even partition.
    let max_gap = selected_indexes.windows(2).map(|pair| pair[1] - pair[0]).max().unwrap_or(0);
    let upper_bound = (total / (limit - 1) + 2) as i32;
    assert!(
        max_gap <= upper_bound,
        "max gap {max_gap} must stay within {upper_bound} for total={total} limit={limit}: {selected_indexes:?}"
    );
}

#[test]
fn source_coverage_selection_prioritizes_focused_keyword_rows() {
    let rows = (0..18)
        .map(|index| {
            if index == 12 {
                chunk_row(index, "resource threshold RATE_LIMIT_REQUESTS exact anchor")
            } else {
                chunk_row(index, &format!("chunk {index}"))
            }
        })
        .collect::<Vec<_>>();
    let plan_keywords = vec!["RATE_LIMIT_REQUESTS".to_string()];

    let selected = select_source_coverage_chunk_rows(rows, 8, &plan_keywords);
    let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

    assert!(
        selected_indexes.contains(&12),
        "focused exact keyword row must be retained: {selected_indexes:?}"
    );
}

#[test]
fn source_coverage_selection_prefers_late_structural_anchor_rows() {
    let rows = (0..24)
        .map(|index| match index {
            2 => chunk_row(index, "generic cloudwatch alarms and threshold overview"),
            19 => chunk_row(
                index,
                "resource aws_cloudwatch_metric_alarm ecs_cpu_high metric_name CPUUtilization threshold 85",
            ),
            _ => chunk_row(index, &format!("chunk {index}")),
        })
        .collect::<Vec<_>>();
    let plan_keywords =
        vec!["cloudwatch".to_string(), "cpu".to_string(), "CPUUtilization".to_string()];

    let selected = select_source_coverage_chunk_rows(rows, 8, &plan_keywords);
    let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

    assert!(
        selected_indexes.contains(&19),
        "late exact structural row must be retained: {selected_indexes:?}"
    );
}
