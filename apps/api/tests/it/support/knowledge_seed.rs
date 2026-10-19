//! Seeding a knowledge chunk, shared by the suites that need one.
//!
//! Two suites were writing the same twenty-line `CreateKnowledgeChunkCommand`
//! with different fixtures around it. The command is the product's contract for
//! what a ready chunk looks like, so it belongs in one place: when a required
//! field is added, one call site has to change, not every suite that ever
//! seeded a chunk.

use anyhow::{Context, Result};
use uuid::Uuid;

use ironrag_backend::{
    app::state::AppState, infra::repositories::content_chunk_repository::NewContentChunkProjection,
    services::knowledge::service::CreateKnowledgeChunkCommand,
};

/// Identifies the chunk `write_ready_chunk` seeds inside one revision.
pub(crate) struct ReadyChunkSeed<'a> {
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) document_id: Uuid,
    pub(crate) revision_id: Uuid,
    pub(crate) chunk_index: i32,
    pub(crate) text: &'a str,
}

/// A ready paragraph in both text and vector generations, before repository publication.
pub(crate) fn ready_paragraph_projection(
    seed: ReadyChunkSeed<'_>,
    end_offset: i32,
    token_count: i32,
    text_checksum: &str,
    generation: i64,
) -> NewContentChunkProjection {
    NewContentChunkProjection {
        workspace_id: seed.workspace_id,
        library_id: seed.library_id,
        document_id: seed.document_id,
        revision_id: seed.revision_id,
        chunk_index: seed.chunk_index,
        start_offset: 0,
        end_offset,
        token_count: Some(token_count),
        chunk_kind: Some("paragraph".to_string()),
        content_text: seed.text.to_string(),
        normalized_text: seed.text.to_string(),
        text_checksum: text_checksum.to_string(),
        support_block_ids: Vec::new(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(generation),
        vector_generation: Some(generation),
        quality_score: Some(1.0),
        window_text: None,
        occurred_at: None,
        occurred_until: None,
    }
}

/// Writes one ready chunk with `text` as both its content and normalized form.
pub(crate) async fn write_ready_chunk(state: &AppState, seed: ReadyChunkSeed<'_>) -> Result<Uuid> {
    let ReadyChunkSeed { workspace_id, library_id, document_id, revision_id, chunk_index, text } =
        seed;
    let chunk_id = Uuid::now_v7();
    state
        .application_services
        .knowledge
        .write_chunk(
            state,
            CreateKnowledgeChunkCommand {
                chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_index,
                chunk_kind: None,
                content_text: text.to_string(),
                normalized_text: text.to_string(),
                span_start: None,
                span_end: None,
                token_count: Some(
                    i32::try_from(text.split_whitespace().count()).unwrap_or(i32::MAX),
                ),
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
            },
        )
        .await
        .context("failed to write knowledge chunk")?;
    Ok(chunk_id)
}
