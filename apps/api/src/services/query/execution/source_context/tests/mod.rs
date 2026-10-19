use chrono::Utc;

use crate::domains::query_ir::{LiteralKind, LiteralSpan, QueryAct, QueryLanguage, QueryScope};

use super::*;

mod budgeting_tests;
mod candidates_tests;
mod loading_tests;
mod rendering_tests;
mod selection_tests;
mod source_slice_tests;
#[path = "source_context_structural_tests.rs"]
mod structural_tests;

fn runtime_chunk(document_id: Uuid, revision_id: Uuid, index: i32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index: index,
        chunk_kind: Some("metadata_block".to_string()),
        document_label: "event-stream.jsonl".to_string(),
        excerpt: format!("unit {index}"),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.5 - index as f32 * 0.01),
        source_text: format!("[unit_id=u-{index}] unit {index}"),
    }
}

fn companion_chunk(
    document_id: Uuid,
    revision_id: Uuid,
    index: i32,
    kind: &str,
    score: f32,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index: index,
        chunk_kind: Some(kind.to_string()),
        document_label: "event-stream.jsonl".to_string(),
        excerpt: format!("{kind} {index}"),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: format!("{kind} {index}"),
    }
}

fn chunk_row(
    document_id: Uuid,
    revision_id: Uuid,
    index: i32,
    kind: &str,
    text: &str,
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index: index,
        chunk_kind: Some(kind.to_string()),
        content_text: text.to_string(),
        normalized_text: text.to_string(),
        span_start: Some(0),
        span_end: Some(text.len() as i32),
        token_count: Some(1),
        support_block_ids: Vec::new(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: None,
        window_text: None,
        occurred_at: None,
        occurred_until: None,
    }
}

fn document_row(document_id: Uuid, revision_id: Uuid) -> KnowledgeDocumentRow {
    KnowledgeDocumentRow {
        document_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: "event-stream.jsonl".to_string(),
        file_name: Some("event-stream.jsonl".to_string()),
        title: Some("event-stream.jsonl".to_string()),
        source_uri: None,
        document_hint: None,
        document_state: "active".to_string(),
        active_revision_id: Some(revision_id),
        readable_revision_id: Some(revision_id),
        latest_revision_no: Some(1),
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        deleted_at: None,
    }
}

fn source_unit_block(
    document_id: Uuid,
    revision_id: Uuid,
    ordinal: i32,
    text: &str,
) -> KnowledgeStructuredBlockRow {
    KnowledgeStructuredBlockRow {
        block_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        ordinal,
        block_kind: SOURCE_UNIT_CHUNK_KIND.to_string(),
        text: text.to_string(),
        normalized_text: text.to_string(),
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        page_number: None,
        span_start: Some(0),
        span_end: Some(text.len() as i32),
        parent_block_id: None,
        table_coordinates_json: None,
        code_language: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn source_slice_ir(direction: SourceSliceDirection, count: Option<u16>) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Record],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec { direction, count, filter: SourceSliceFilter::None }),
        retrieval_query: None,
        confidence: 0.8,
    }
}

fn latest_release_source_slice_ir(count: Option<u16>) -> QueryIR {
    let mut ir = source_slice_ir(SourceSliceDirection::Tail, count);
    ir.act = QueryAct::Describe;
    ir.scope = QueryScope::LibraryMeta;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Release];
    if let Some(slice) = ir.source_slice.as_mut() {
        slice.filter = SourceSliceFilter::ReleaseMarker;
    }
    ir
}

fn source_context_ir(act: QueryAct, scope: QueryScope) -> QueryIR {
    QueryIR::test_with_act_scope(act, scope)
}
