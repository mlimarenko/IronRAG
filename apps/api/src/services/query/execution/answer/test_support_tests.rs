use super::{QueryTargetKind, RuntimeMatchedChunk, Uuid};

pub(super) fn source_slice_ir(count: u16) -> crate::domains::query_ir::QueryIR {
    crate::domains::query_ir::QueryIR {
        act: crate::domains::query_ir::QueryAct::Enumerate,
        scope: crate::domains::query_ir::QueryScope::SingleDocument,
        language: crate::domains::query_ir::QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Record],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(crate::domains::query_ir::SourceSliceSpec {
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
            count: Some(count),
            filter: crate::domains::query_ir::SourceSliceFilter::None,
        }),
        retrieval_query: None,
        confidence: 0.9,
    }
}

pub(super) fn latest_source_slice_ir(count: u16) -> crate::domains::query_ir::QueryIR {
    let mut ir = source_slice_ir(count);
    ir.act = crate::domains::query_ir::QueryAct::Describe;
    ir.scope = crate::domains::query_ir::QueryScope::LibraryMeta;
    ir.target_types = vec![QueryTargetKind::Release];
    if let Some(slice) = ir.source_slice.as_mut() {
        slice.filter = crate::domains::query_ir::SourceSliceFilter::ReleaseMarker;
    }
    ir
}

pub(super) fn low_confidence_concept_ir() -> crate::domains::query_ir::QueryIR {
    crate::domains::query_ir::QueryIR {
        act: crate::domains::query_ir::QueryAct::Describe,
        scope: crate::domains::query_ir::QueryScope::LibraryMeta,
        language: crate::domains::query_ir::QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Concept],
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

pub(super) fn exact_version_ir(version: &str) -> crate::domains::query_ir::QueryIR {
    crate::domains::query_ir::QueryIR {
        act: crate::domains::query_ir::QueryAct::Describe,
        scope: crate::domains::query_ir::QueryScope::SingleDocument,
        language: crate::domains::query_ir::QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Version],
        target_entities: Vec::new(),
        literal_constraints: vec![crate::domains::query_ir::LiteralSpan {
            text: version.to_string(),
            kind: crate::domains::query_ir::LiteralKind::Version,
        }],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

pub(super) fn configure_how_focus_ir(focus: &str) -> crate::domains::query_ir::QueryIR {
    crate::domains::query_ir::QueryIR {
        act: crate::domains::query_ir::QueryAct::ConfigureHow,
        scope: crate::domains::query_ir::QueryScope::SingleDocument,
        language: crate::domains::query_ir::QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Procedure],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(crate::domains::query_ir::DocumentHint { hint: focus.to_string() }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.95,
    }
}

pub(super) fn configure_update_focus_ir(focus: &str) -> crate::domains::query_ir::QueryIR {
    let mut query_ir = configure_how_focus_ir(focus);
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: focus.to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    query_ir
}
pub(super) fn source_unit(index: i32, text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: index,
        chunk_kind: Some(super::super::SOURCE_UNIT_CHUNK_KIND.to_string()),
        document_label: "records.jsonl".to_string(),
        excerpt: text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(3.0),
        source_text: text.to_string(),
    }
}

pub(super) fn evidence_chunk(index: i32, kind: Option<&str>, text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: index,
        chunk_kind: kind.map(str::to_string),
        document_label: "records.jsonl".to_string(),
        excerpt: text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(3.0),
        source_text: text.to_string(),
    }
}

pub(super) fn release_identity_chunk(
    document_id: Uuid,
    revision_id: Uuid,
    index: i32,
    score: f32,
    label: &str,
    text: &str,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index: index,
        chunk_kind: Some("document_identity".to_string()),
        document_label: label.to_string(),
        excerpt: text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::DocumentIdentity,
        score: Some(score),
        source_text: text.to_string(),
    }
}

pub(super) fn release_relevance_chunk(
    document_id: Uuid,
    revision_id: Uuid,
    index: i32,
    score: f32,
    label: &str,
    text: &str,
) -> RuntimeMatchedChunk {
    let mut chunk = release_identity_chunk(document_id, revision_id, index, score, label, text);
    chunk.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::Relevance;
    chunk.chunk_kind = Some("paragraph".to_string());
    chunk
}

pub(super) fn source_context_chunk(
    document_id: Uuid,
    revision_id: Uuid,
    index: i32,
    text: &str,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index: index,
        chunk_kind: Some("table_row".to_string()),
        document_label: "Subject Alpha setup".to_string(),
        excerpt: text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(100.0 - index as f32),
        source_text: text.to_string(),
    }
}

pub(super) fn configure_how_ir() -> crate::domains::query_ir::QueryIR {
    crate::domains::query_ir::QueryIR {
        act: crate::domains::query_ir::QueryAct::ConfigureHow,
        scope: crate::domains::query_ir::QueryScope::SingleDocument,
        language: crate::domains::query_ir::QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Parameter],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(crate::domains::query_ir::DocumentHint {
            hint: "Subject Alpha setup".to_string(),
        }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}
