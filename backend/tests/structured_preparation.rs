#[path = "support/structured_document_fixtures.rs"]
mod structured_document_fixtures;

use rustrag_backend::{
    services::structured_preparation_service::StructuredPreparationService,
    shared::structured_document::StructuredBlockKind,
};

#[test]
fn structured_preparation_preserves_semantic_blocks_and_chunk_ancestry() {
    let service = StructuredPreparationService::new();
    let prepared = service
        .prepare_revision(structured_document_fixtures::canonical_prepare_command())
        .expect("fixture should prepare successfully");

    assert!(
        prepared.prepared_revision.outline.iter().any(|entry| entry.heading == "REST API"),
        "expected top-level heading in outline"
    );
    assert!(
        prepared.ordered_blocks.windows(2).all(|pair| pair[0].ordinal < pair[1].ordinal),
        "block ordinals must be strictly increasing"
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::Heading)),
        "expected heading block"
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::ListItem)),
        "expected list item block"
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::Table)),
        "expected table block"
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::TableRow)),
        "expected table row block"
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::CodeBlock)),
        "expected code block"
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::EndpointBlock)),
        "expected endpoint block"
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .filter(|block| !matches!(block.block_kind, StructuredBlockKind::Heading))
            .any(|block| !block.heading_trail.is_empty() && !block.section_path.is_empty()),
        "expected non-heading blocks to inherit ancestry"
    );
    assert!(!prepared.chunk_windows.is_empty(), "expected prepared chunk windows");
    assert!(
        prepared.chunk_windows.iter().all(|chunk| !chunk.support_block_ids.is_empty()),
        "prepared chunks must retain supporting block ids"
    );
    assert!(
        prepared
            .chunk_windows
            .iter()
            .any(|chunk| !chunk.heading_trail.is_empty() && !chunk.section_path.is_empty()),
        "expected chunk ancestry derived from headings"
    );
    assert!(
        prepared.chunk_windows.iter().all(|chunk| {
            chunk.literal_digest.as_ref().is_some_and(|digest| !digest.trim().is_empty())
        }),
        "prepared chunks must expose literal digests"
    );
}
