use super::*;

#[test]
fn pg_batcher_keeps_different_postgres_targets_in_separate_buffers() {
    let document_id = Uuid::now_v7();
    let block_id = Uuid::now_v7();
    let mut batcher = PgBatcher::new();

    batcher.push(
        "knowledge_document",
        serde_json::json!({
            "document_id": document_id,
            "title": "Document A",
        }),
    );
    batcher.push(
        "knowledge_structured_block",
        serde_json::json!({
            "block_id": block_id,
            "text": "Section A",
        }),
    );

    let block_id_text = block_id.to_string();
    assert_eq!(batcher.pending.get("knowledge_document").map(Vec::len), Some(1));
    assert_eq!(batcher.pending.get("knowledge_structured_block").map(Vec::len), Some(1));
    assert_eq!(
        batcher
            .pending
            .get("knowledge_structured_block")
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("block_id"))
            .and_then(serde_json::Value::as_str),
        Some(block_id_text.as_str())
    );
}
