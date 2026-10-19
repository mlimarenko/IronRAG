use super::test_prelude::*;

fn release_inventory_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Release],
        target_entities: vec![EntityMention {
            label: "Alpha Suite".to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: vec![LiteralSpan {
            text: "5".to_string(),
            kind: LiteralKind::NumericCode,
        }],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec {
            direction: SourceSliceDirection::Tail,
            count: Some(5),
            filter: SourceSliceFilter::ReleaseMarker,
        }),
        retrieval_query: Some("latest 5 Alpha Suite releases".to_string()),
        confidence: 0.96,
    }
}

#[test]
fn compiled_ordinary_query_does_not_enable_answer_history() {
    let query_ir = release_inventory_query_ir();

    assert!(!compiled_query_uses_answer_history(
        query_ir.retrieval_query.as_deref().expect("retrieval query"),
        &query_ir,
    ));
}

#[test]
fn compiled_follow_up_query_enables_answer_history() {
    let mut query_ir = release_inventory_query_ir();
    query_ir.act = QueryAct::FollowUp;

    assert!(compiled_query_uses_answer_history("refine it", &query_ir));
}

#[test]
fn compiler_resolved_standalone_query_enables_answer_history_after_refs_are_cleared() {
    let mut query_ir = release_inventory_query_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.conversation_refs.clear();
    query_ir.retrieval_query = Some("Subject Alpha refinement".to_string());

    assert!(compiled_query_uses_answer_history("refinement", &query_ir));
}
