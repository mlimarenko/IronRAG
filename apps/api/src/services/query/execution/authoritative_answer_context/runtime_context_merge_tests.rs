use super::source_coverage::{
    query_ir_requests_setup_source_coverage, source_coverage_document_limit,
};
use super::*;

#[test]
fn authoritative_evidence_scope_requires_exact_current_readable_prepared_revision() {
    let sql = CURRENT_READABLE_PREPARED_SCOPES_SQL;

    for predicate in [
        "d.document_id = sr.document_id",
        "d.workspace_id = sr.workspace_id",
        "d.library_id = sr.library_id",
        "d.readable_revision_id = sr.revision_id",
        "d.document_state = 'active'",
        "d.deleted_at is null",
        "sr.revision_id = any($1::uuid[])",
        "sr.preparation_state = 'prepared'",
    ] {
        assert!(sql.contains(predicate), "missing evidence scope predicate: {predicate}");
    }
    assert!(!sql.contains("preparation_state = 'preparing'"));
}

#[test]
fn broad_procedure_requests_five_source_documents() {
    let query_ir = QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: crate::domains::query_ir::QueryLanguage::Auto,
        target_types: vec![
            QueryTargetKind::ConfigurationFile,
            QueryTargetKind::ConfigKey,
            QueryTargetKind::Procedure,
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
        confidence: 0.95,
    };

    assert!(query_ir_requests_setup_source_coverage(&query_ir));
    assert_eq!(source_coverage_document_limit(&query_ir), 5);
}

#[test]
fn merge_runtime_context_chunks_keeps_focused_document_refs() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let runtime_chunk = RuntimeMatchedChunk {
        chunk_id,
        document_id,
        revision_id,
        chunk_index: 0,
        chunk_kind: Some("code_block".to_string()),
        document_label: "focused.yaml".to_string(),
        excerpt: "focused evidence".to_string(),
        score_kind: RuntimeChunkScoreKind::FocusedDocument,
        score: Some(10.0),
        source_text: "focused evidence".to_string(),
    };

    let mut chunks = Vec::new();
    merge_runtime_context_chunks(&mut chunks, &[runtime_chunk]);

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk_id, chunk_id);
    assert_eq!(chunks[0].score_kind, RuntimeChunkScoreKind::FocusedDocument);
}
