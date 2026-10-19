use crate::domains::query_ir::{
    QueryAct, QueryIR, QueryLanguage, QueryScope, SourceSliceDirection, SourceSliceFilter,
    SourceSliceSpec,
};
use uuid::Uuid;

use super::super::source_context::SOURCE_UNIT_CHUNK_KIND;
use super::*;

fn runtime_chunk(kind: Option<&str>, score: f32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 1,
        chunk_kind: kind.map(str::to_string),
        document_label: "records.jsonl".to_string(),
        excerpt: "record".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: "record".to_string(),
    }
}

#[test]
fn chunk_reference_snapshots_include_ordered_source_units() {
    let profile = runtime_chunk(Some("source_profile"), 4.0);
    let source_unit = runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0);
    let ordinary = runtime_chunk(Some("text"), 2.0);

    let snapshots = build_query_chunk_reference_snapshots(&[
        profile.clone(),
        source_unit.clone(),
        ordinary.clone(),
    ]);

    assert_eq!(snapshots.len(), 3);
    assert_eq!(snapshots[0].chunk_id, profile.chunk_id);
    assert_eq!(snapshots[0].rank, 1);
    assert_eq!(snapshots[1].chunk_id, source_unit.chunk_id);
    assert_eq!(snapshots[1].rank, 2);
    assert_eq!(snapshots[2].chunk_id, ordinary.chunk_id);
    assert_eq!(snapshots[2].rank, 3);
}

#[test]
fn ordered_source_units_preserve_source_order() {
    let later =
        RuntimeMatchedChunk { chunk_index: 7, ..runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0) };
    let earlier =
        RuntimeMatchedChunk { chunk_index: 3, ..runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0) };
    let ordinary = runtime_chunk(Some("text"), 2.0);

    let units = collect_ordered_source_units(&[later.clone(), ordinary, earlier.clone()]);

    assert_eq!(units.len(), 2);
    assert_eq!(units[0].chunk_id, earlier.chunk_id);
    assert_eq!(units[1].chunk_id, later.chunk_id);
}

#[test]
fn query_ir_target_types_expand_technical_literal_selection() {
    let question = "Which commands and settings configure scanning through RareProtocol?";
    let query_ir = QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::Protocol,
            crate::domains::query_ir::QueryTargetKind::Path,
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
        confidence: 0.82,
    };
    let target_document_id = Uuid::now_v7();
    let target_chunk_id = Uuid::now_v7();
    let mut chunks = (0..14)
        .map(|index| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: index,
            chunk_kind: None,
            document_label: format!("noisy-{index}.md"),
            excerpt: "General operations memo without command literals.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0 - (index as f32 * 0.01)),
            source_text: "General operations memo without command literals.".to_string(),
        })
        .collect::<Vec<_>>();
    chunks.push(RuntimeMatchedChunk {
        chunk_id: target_chunk_id,
        document_id: target_document_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_label: "rare-protocol-scan-folder.md".to_string(),
        excerpt: "RareProtocol setup: create /srv/scans and set scan_share = writable.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.42),
        source_text: "RareProtocol setup: create /srv/scans and set scan_share = writable."
            .to_string(),
    });
    let focus_keywords = technical_literal_focus_keywords(question, Some(&query_ir));

    let default_selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir: &query_ir,
        chunks: &chunks,
        technical_literal_intent: TechnicalLiteralIntent::default(),
        top_k: 8,
        literal_focus_keywords: &focus_keywords,
        preferred_document_ids: &[],
        pagination_requested: false,
    });
    assert!(
        !default_selected.iter().any(|chunk| chunk.chunk_id == target_chunk_id),
        "default selection is capped before the later needle chunk"
    );

    let technical_intent = effective_technical_literal_intent(
        question,
        Some(&query_ir),
        TechnicalLiteralIntent::default(),
    );
    assert!(technical_intent.any());

    let expanded_selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir: &query_ir,
        chunks: &chunks,
        technical_literal_intent: technical_intent,
        top_k: 8,
        literal_focus_keywords: &focus_keywords,
        preferred_document_ids: &[],
        pagination_requested: false,
    });

    assert!(
        expanded_selected.iter().any(|chunk| chunk.chunk_id == target_chunk_id),
        "QueryIR technical target types must keep later exact-literal evidence candidates"
    );
}

#[test]
fn effective_technical_literal_intent_unions_planner_and_query_ir_signals() {
    let query_ir = QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::ConfigKey,
            crate::domains::query_ir::QueryTargetKind::Path,
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
        confidence: 0.8,
    };

    let intent = effective_technical_literal_intent(
        "Which settings should the client use?",
        Some(&query_ir),
        TechnicalLiteralIntent { wants_urls: true, ..TechnicalLiteralIntent::default() },
    );

    assert!(intent.wants_urls);
    assert!(intent.wants_paths);
    assert!(intent.wants_methods);
    assert!(intent.wants_parameters);
}

#[test]
fn latest_version_queries_do_not_include_graph_context() {
    let query_ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::LibraryMeta,
        language: QueryLanguage::Auto,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::Release,
            crate::domains::query_ir::QueryTargetKind::Version,
        ],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec {
            direction: SourceSliceDirection::Tail,
            count: Some(10),
            filter: SourceSliceFilter::ReleaseMarker,
        }),
        retrieval_query: None,
        confidence: 0.9,
    };

    assert!(!include_graph_context_for_query(&query_ir));
}
