use super::*;

fn make_chunk(revision_id: Uuid, chunk_index: i32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id,
        chunk_index,
        chunk_kind: Some("paragraph".to_string()),
        document_label: "doc".to_string(),
        excerpt: "excerpt".to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "text".to_string(),
    }
}

/// Case 1: document has 1 text chunk + 5 image stubs → text chunk ends up
/// before all image-source chunks in the output.
#[test]
fn text_chunk_before_image_stubs_when_mixed() {
    let text_revision = Uuid::now_v7();
    let image_revision = Uuid::now_v7();
    let mut image_revision_ids = HashSet::new();
    image_revision_ids.insert(image_revision);

    let mut chunks = vec![
        make_chunk(image_revision, 0),
        make_chunk(image_revision, 1),
        make_chunk(text_revision, 2),
        make_chunk(image_revision, 3),
        make_chunk(image_revision, 4),
        make_chunk(image_revision, 5),
    ];

    deprioritize_image_source_chunks(&mut chunks, &image_revision_ids);

    // Text chunk must appear before all image chunks
    let text_pos = chunks.iter().position(|c| c.revision_id == text_revision).unwrap();
    let all_image_after = chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| c.revision_id == image_revision)
        .all(|(pos, _)| pos > text_pos);
    assert!(all_image_after, "all image chunks should follow the text chunk");
    // All 6 chunks must still be present
    assert_eq!(chunks.len(), 6);
}

/// Case 2: document has ONLY image stubs → order is unchanged (all survive).
#[test]
fn image_only_context_is_not_reordered() {
    let image_revision = Uuid::now_v7();
    let mut image_revision_ids = HashSet::new();
    image_revision_ids.insert(image_revision);

    let original_chunk_indices: Vec<i32> = (0..5).collect();
    let mut chunks: Vec<RuntimeMatchedChunk> =
        original_chunk_indices.iter().map(|&i| make_chunk(image_revision, i)).collect();

    deprioritize_image_source_chunks(&mut chunks, &image_revision_ids);

    let result_indices: Vec<i32> = chunks.iter().map(|c| c.chunk_index).collect();
    assert_eq!(result_indices, original_chunk_indices, "image-only list must not be reordered");
    assert_eq!(chunks.len(), 5);
}

/// Case 3: two documents — one with text+image stubs, one with only image
/// stubs.  The second document's image chunks should be preserved
/// (deprioritized relative to the first doc's text chunk, not dropped).
#[test]
fn image_only_doc_preserved_alongside_mixed_doc() {
    let text_revision = Uuid::now_v7();
    let mixed_image_revision = Uuid::now_v7();
    let pure_image_revision = Uuid::now_v7();
    let mut image_revision_ids = HashSet::new();
    image_revision_ids.insert(mixed_image_revision);
    image_revision_ids.insert(pure_image_revision);

    let mut chunks = vec![
        make_chunk(mixed_image_revision, 0), // image stub from mixed doc
        make_chunk(text_revision, 1),        // text chunk from mixed doc
        make_chunk(pure_image_revision, 2),  // image from image-only doc
        make_chunk(pure_image_revision, 3),  // image from image-only doc
    ];

    deprioritize_image_source_chunks(&mut chunks, &image_revision_ids);

    // Text chunk must come before all image chunks
    let text_pos = chunks.iter().position(|c| c.revision_id == text_revision).unwrap();
    let image_positions: Vec<usize> = chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| image_revision_ids.contains(&c.revision_id))
        .map(|(i, _)| i)
        .collect();
    assert!(
        image_positions.iter().all(|&p| p > text_pos),
        "all image chunks should follow the text chunk"
    );
    // Pure-image doc's chunks are still present (not dropped)
    let pure_image_count = chunks.iter().filter(|c| c.revision_id == pure_image_revision).count();
    assert_eq!(pure_image_count, 2, "image-only doc chunks must not be dropped");
    assert_eq!(chunks.len(), 4);
}
