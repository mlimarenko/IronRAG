use super::test_prelude::*;

pub(super) fn document_row(file_name: &str, title: &str) -> KnowledgeDocumentRow {
    document_row_with_source(file_name, title, None, None)
}

pub(super) fn document_row_with_source(
    file_name: &str,
    title: &str,
    source_uri: Option<&str>,
    document_hint: Option<&str>,
) -> KnowledgeDocumentRow {
    let document_id = Uuid::now_v7();
    KnowledgeDocumentRow {
        document_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: file_name.to_string(),
        file_name: Some(file_name.to_string()),
        title: Some(title.to_string()),
        source_uri: source_uri.map(str::to_string),
        document_hint: document_hint.map(str::to_string),
        document_state: "active".to_string(),
        active_revision_id: Some(Uuid::now_v7()),
        readable_revision_id: Some(Uuid::now_v7()),
        latest_revision_no: Some(1),
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        deleted_at: None,
    }
}

pub(super) fn revision_row(
    document_id: Uuid,
    revision_id: Uuid,
    revision_number: i64,
    text_state: &str,
) -> KnowledgeRevisionRow {
    let now = Utc::now();
    KnowledgeRevisionRow {
        revision_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_number,
        revision_state: "active".to_string(),
        revision_kind: "snapshot".to_string(),
        storage_ref: None,
        source_uri: None,
        document_hint: None,
        mime_type: "text/plain".to_string(),
        checksum: revision_id.to_string(),
        title: Some("Alpha Connector".to_string()),
        byte_size: 128,
        normalized_text: None,
        text_checksum: None,
        image_checksum: None,
        text_state: text_state.to_string(),
        vector_state: "ready".to_string(),
        graph_state: "ready".to_string(),
        text_readable_at: Some(now),
        vector_ready_at: Some(now),
        graph_ready_at: Some(now),
        superseded_by_revision_id: None,
        created_at: now,
    }
}

pub(super) fn setup_query_ir(focus: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::En,
        target_types: vec![QueryTargetKind::Package, QueryTargetKind::ConfigurationFile],
        target_entities: vec![EntityMention {
            label: "module settings".to_string(),
            role: EntityRole::Object,
        }],
        literal_constraints: Vec::<LiteralSpan>::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(crate::domains::query_ir::DocumentHint { hint: focus.to_string() }),
        conversation_refs: Vec::<UnresolvedRef>::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

pub(super) fn low_confidence_untyped_query_ir() -> QueryIR {
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.act = QueryAct::Describe;
    query_ir.confidence = 0.25;
    query_ir.target_types.clear();
    query_ir.target_entities.clear();
    query_ir.document_focus = None;
    query_ir
}

pub(super) fn chunk_row(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    text: &str,
) -> KnowledgeChunkRow {
    let chunk_id = Uuid::now_v7();
    KnowledgeChunkRow {
        chunk_id,
        workspace_id,
        library_id,
        document_id,
        revision_id,
        chunk_index,
        chunk_kind: Some("code_block".to_string()),
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
        quality_score: None,
        window_text: None,
        occurred_at: None,
        occurred_until: None,
    }
}

pub(super) fn answer_source_chunk(
    chunk_index: i32,
    chunk_kind: &str,
    content_text: &str,
    normalized_text: &str,
) -> KnowledgeChunkRow {
    let mut chunk = chunk_row(
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        chunk_index,
        normalized_text,
    );
    chunk.chunk_kind = Some(chunk_kind.to_string());
    chunk.content_text = content_text.to_string();
    chunk.quality_score = Some(1.0);
    chunk
}
