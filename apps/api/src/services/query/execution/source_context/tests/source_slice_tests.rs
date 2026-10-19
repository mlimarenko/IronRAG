use super::*;

#[test]
fn map_source_unit_block_preserves_record_ordinal_and_text() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
    let block = source_unit_block(
        document_id,
        revision_id,
        42,
        "[unit_id=u-42 occurred_at=2026-04-29T12:22:51Z] final record",
    );
    let block_id = block.block_id;
    let support_chunk_id = Uuid::now_v7();
    let mut support_chunk =
        chunk_row(document_id, revision_id, 42, "paragraph", "supporting record payload");
    support_chunk.chunk_id = support_chunk_id;

    let mapped =
        map_source_unit_block(block, Some(&support_chunk), 3.0, &document_index, &[]).unwrap();

    assert_eq!(mapped.chunk_id, support_chunk_id);
    assert_ne!(mapped.chunk_id, block_id);
    assert_eq!(mapped.chunk_index, 42);
    assert_eq!(mapped.chunk_kind.as_deref(), Some(SOURCE_UNIT_CHUNK_KIND));
    assert!(is_source_unit_runtime_chunk(&mapped));
    assert!(mapped.source_text.contains("final record"));
}

#[test]
fn map_source_unit_block_extends_text_with_support_payload() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
    let block = source_unit_block(
        document_id,
        revision_id,
        7,
        "[unit_id=u-7 occurred_at=2026-04-29T12:22:51Z] ![preview](asset.png)",
    );
    let support_chunk = chunk_row(
        document_id,
        revision_id,
        7,
        "paragraph",
        "Record title\n- Added neutral evidence line\n- Added second neutral evidence line",
    );

    let mapped = map_source_unit_block(block, Some(&support_chunk), 3.0, &document_index, &[])
        .expect("mapped source unit");

    assert!(mapped.source_text.starts_with("[unit_id=u-7"));
    assert!(mapped.source_text.contains("![preview](asset.png)"));
    assert!(mapped.source_text.contains("Record title"));
    assert!(mapped.source_text.contains("Added neutral evidence line"));
    assert_eq!(mapped.chunk_id, support_chunk.chunk_id);
}

#[test]
fn ordered_source_slice_candidate_stays_aligned_with_top_ranked_document() {
    let prose_document_id = Uuid::now_v7();
    let prose_revision_id = Uuid::now_v7();
    let first_record_document_id = Uuid::now_v7();
    let second_record_document_id = Uuid::now_v7();
    let ranked_chunks = vec![
        runtime_chunk(prose_document_id, prose_revision_id, 0),
        runtime_chunk(first_record_document_id, Uuid::now_v7(), 0),
        runtime_chunk(second_record_document_id, Uuid::now_v7(), 0),
    ];
    let candidates = collect_source_context_candidates(&ranked_chunks);

    let selected = top_ranked_source_context_candidate(&ranked_chunks, &candidates)
        .expect("top-ranked candidate");

    assert_eq!(selected.document_id, prose_document_id);
    assert_ne!(selected.document_id, first_record_document_id);
    assert_ne!(selected.document_id, second_record_document_id);
}
