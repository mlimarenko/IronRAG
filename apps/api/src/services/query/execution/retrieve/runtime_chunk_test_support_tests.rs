use super::test_prelude::*;

pub(super) fn setup_focus_runtime_chunk(
    document_id: Uuid,
    chunk_index: i32,
    score: f32,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index,
        chunk_kind: Some("paragraph".to_string()),
        document_id,
        document_label: format!("Document {document_id}"),
        excerpt: "setup evidence".to_string(),
        score: Some(score),
        score_kind: RuntimeChunkScoreKind::DocumentIdentity,
        source_text: "setup evidence".to_string(),
    }
}

pub(super) fn latest_version_runtime_chunk(
    document_id: Uuid,
    chunk_index: i32,
    score: f32,
) -> RuntimeMatchedChunk {
    let mut chunk = setup_focus_runtime_chunk(document_id, chunk_index, score);
    chunk.score_kind = RuntimeChunkScoreKind::LatestVersion;
    chunk
}
