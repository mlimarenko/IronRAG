use super::*;

#[test]
fn source_slice_top_k_expands_and_clamps_context_budget() {
    let requested = source_slice_ir(SourceSliceDirection::Tail, Some(20));
    let defaulted = source_slice_ir(SourceSliceDirection::Head, None);
    let too_large = source_slice_ir(SourceSliceDirection::All, Some(500));

    assert_eq!(source_slice_context_top_k(&requested, 8), 21);
    assert_eq!(source_slice_context_top_k(&defaulted, 8), 13);
    assert_eq!(source_slice_context_top_k(&too_large, 8), 31);
    assert_eq!(structured_source_context_top_k(&requested, 8), 21);
}

#[test]
fn latest_release_tail_slice_carries_typed_marker_filter() {
    let query_ir = latest_release_source_slice_ir(Some(10));
    let slice = query_ir.source_slice.as_ref().unwrap();

    assert_eq!(slice.filter, SourceSliceFilter::ReleaseMarker);
}

#[test]
fn ordinary_tail_slice_keeps_unfiltered_tail_units() {
    let query_ir = source_slice_ir(SourceSliceDirection::Tail, Some(2));
    let slice = query_ir.source_slice.as_ref().unwrap();

    assert_eq!(slice.filter, SourceSliceFilter::None);
}

#[test]
fn procedural_source_context_expands_default_top_k() {
    let query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);

    assert_eq!(
        structured_source_context_top_k(&query_ir, 5),
        procedural_source_context_chunk_floor()
    );
    assert_eq!(
        structured_source_context_top_k(
            &query_ir,
            procedural_source_context_chunk_floor().saturating_add(1)
        ),
        procedural_source_context_chunk_floor().saturating_add(1)
    );
}

#[test]
fn descriptive_source_context_keeps_default_top_k() {
    let query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);

    assert_eq!(structured_source_context_top_k(&query_ir, 5), 5);
}

#[test]
fn fallback_structured_context_expands_top_k_when_structured_chunks_exist() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    query_ir.confidence = 0.25;
    let mut structured = runtime_chunk(document_id, revision_id, 12);
    structured.chunk_kind = Some("key_value_block".to_string());
    structured.source_text = "alphaKey = true".to_string();
    let mut plain = runtime_chunk(document_id, revision_id, 13);
    plain.chunk_kind = None;
    plain.source_text = "general overview".to_string();

    assert_eq!(
        structured_source_context_top_k_for_chunks(&query_ir, 24, &[structured]),
        fallback_structured_source_context_chunk_floor()
    );
    assert_eq!(
        structured_source_context_top_k_for_chunks(&query_ir, 24, &[plain]),
        fallback_structured_source_context_chunk_floor()
    );
}

#[test]
fn structured_source_unit_inventory_expands_top_k_for_describe_questions() {
    let query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let plain = runtime_chunk(document_id, revision_id, 12);

    assert!(requests_structured_source_unit_inventory_context(&query_ir));
    assert_eq!(
        structured_source_context_top_k_for_chunks(&query_ir, 24, &[plain]),
        fallback_structured_source_context_chunk_floor()
    );
    assert_eq!(
        source_context_neighbor_span_for_request(
            Some(&query_ir),
            requests_structured_source_unit_inventory_context(&query_ir)
        ),
        SourceContextNeighborSpan {
            backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
            forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
        }
    );
}
