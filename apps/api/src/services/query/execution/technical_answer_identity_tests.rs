use super::*;
use crate::{
    domains::query_ir::{
        EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryScope,
    },
    services::query::execution::RuntimeChunkScoreKind,
};

fn chunk(document_id: Uuid, label: &str, source_text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_label: label.to_string(),
        excerpt: source_text.to_string(),
        score_kind: RuntimeChunkScoreKind::SourceContext,
        score: Some(1.0),
        source_text: source_text.to_string(),
    }
}

#[test]
fn assignment_source_fit_rejects_focus_found_only_in_another_document() {
    let assignment_document_id = Uuid::now_v7();
    let focused_document_id = Uuid::now_v7();
    let chunks = vec![
        chunk(
            assignment_document_id,
            "framework_setup.md",
            "entrypoint = package.main\nmode = production",
        ),
        chunk(
            focused_document_id,
            "service_manifest.yaml",
            "DATABASE_URL: protocol://database:5432/sample",
        ),
    ];
    let mut query_ir =
        QueryIR::test_with_act_scope(QueryAct::RetrieveValue, QueryScope::SingleDocument);
    query_ir.target_entities =
        vec![EntityMention { label: "database connection".to_string(), role: EntityRole::Subject }];
    query_ir.literal_constraints =
        vec![LiteralSpan { kind: LiteralKind::Identifier, text: "DATABASE_URL".to_string() }];
    query_ir.retrieval_query = Some("database connection DATABASE_URL 5432".to_string());

    assert!(!transport_assignment_source_addresses_question_focus(
        &query_ir,
        assignment_document_id,
        &chunks,
    ));
}

#[test]
fn assignment_source_fit_accepts_focus_from_a_sibling_chunk_in_the_same_document() {
    let candidate_document_id = Uuid::now_v7();
    let chunks = vec![
        chunk(
            candidate_document_id,
            "service_settings.cfg",
            "endpoint = protocol://sample.invalid:9443\ntimeout = 30",
        ),
        chunk(
            candidate_document_id,
            "service_settings.cfg",
            "The InventoryGateway connection is configured above.",
        ),
    ];
    let mut query_ir =
        QueryIR::test_with_act_scope(QueryAct::RetrieveValue, QueryScope::SingleDocument);
    query_ir.target_entities =
        vec![EntityMention { label: "InventoryGateway".to_string(), role: EntityRole::Subject }];

    assert!(transport_assignment_source_addresses_question_focus(
        &query_ir,
        candidate_document_id,
        &chunks,
    ));
}
