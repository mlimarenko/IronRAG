use sha2::{Digest, Sha256};

use crate::shared::structured_document::{
    StructuredBlockData, StructuredBlockKind, StructuredChunkWindow,
};

#[derive(Debug, Clone, Copy)]
pub struct StructuredChunkingProfile {
    pub max_chars: usize,
}

impl Default for StructuredChunkingProfile {
    fn default() -> Self {
        Self { max_chars: 1_600 }
    }
}

#[must_use]
pub fn build_structured_chunk_windows(
    blocks: &[StructuredBlockData],
    profile: StructuredChunkingProfile,
) -> Vec<StructuredChunkWindow> {
    let mut chunks = Vec::<StructuredChunkWindow>::new();
    let mut window_start = 0_usize;
    let mut current_char_count = 0_usize;

    for (index, block) in blocks.iter().enumerate() {
        let block_len = chunk_block_len(block);
        let projected = if current_char_count == 0 {
            block_len
        } else {
            current_char_count.saturating_add(2).saturating_add(block_len)
        };

        if window_start < index && projected > profile.max_chars {
            push_structured_chunk_window(&mut chunks, &blocks[window_start..index]);
            window_start = index;
            current_char_count = block_len;
            continue;
        }

        if window_start == index {
            current_char_count = block_len;
        } else {
            current_char_count = projected;
        }
    }

    if window_start < blocks.len() {
        push_structured_chunk_window(&mut chunks, &blocks[window_start..]);
    }

    chunks
}

fn char_count(input: &str) -> usize {
    input.chars().count()
}

fn push_structured_chunk_window(
    out: &mut Vec<StructuredChunkWindow>,
    blocks: &[StructuredBlockData],
) {
    if blocks.is_empty() {
        return;
    }

    let content_text =
        blocks.iter().map(|block| block.text.trim()).collect::<Vec<_>>().join("\n\n");
    let normalized_text =
        blocks.iter().map(|block| block.normalized_text.trim()).collect::<Vec<_>>().join("\n\n");
    let literal_digest = format!("sha256:{:x}", Sha256::digest(normalized_text.as_bytes()));
    let support_block_ids = blocks.iter().map(|block| block.block_id).collect::<Vec<_>>();
    let heading_trail = blocks
        .iter()
        .rev()
        .find(|block| !block.heading_trail.is_empty())
        .map(|block| block.heading_trail.clone())
        .unwrap_or_default();
    let section_path = blocks
        .iter()
        .rev()
        .find(|block| !block.section_path.is_empty())
        .map(|block| block.section_path.clone())
        .unwrap_or_default();
    let token_count = i32::try_from(normalized_text.split_whitespace().count()).ok();

    out.push(StructuredChunkWindow {
        chunk_index: i32::try_from(out.len()).unwrap_or(i32::MAX),
        chunk_kind: dominant_chunk_kind(blocks),
        support_block_ids,
        content_text,
        normalized_text,
        heading_trail,
        section_path,
        token_count,
        literal_digest: Some(literal_digest),
    });
}

fn dominant_chunk_kind(blocks: &[StructuredBlockData]) -> StructuredBlockKind {
    blocks
        .iter()
        .find_map(|block| match block.block_kind {
            StructuredBlockKind::EndpointBlock
            | StructuredBlockKind::CodeBlock
            | StructuredBlockKind::Table
            | StructuredBlockKind::TableRow => Some(block.block_kind),
            _ => None,
        })
        .unwrap_or_else(|| blocks[0].block_kind)
}

fn chunk_block_len(block: &StructuredBlockData) -> usize {
    char_count(block.normalized_text.trim())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{StructuredChunkingProfile, build_structured_chunk_windows};
    use crate::shared::structured_document::{StructuredBlockData, StructuredBlockKind};

    #[test]
    fn builds_structured_chunk_windows_from_semantic_blocks() {
        let blocks = vec![
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::Heading,
                text: "API".to_string(),
                normalized_text: "API".to_string(),
                heading_trail: vec!["API".to_string()],
                section_path: vec!["api".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: None,
                table_coordinates: None,
                code_language: None,
            },
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 1,
                block_kind: StructuredBlockKind::EndpointBlock,
                text: "GET /v1/accounts".to_string(),
                normalized_text: "GET /v1/accounts".to_string(),
                heading_trail: vec!["API".to_string()],
                section_path: vec!["api".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: None,
                table_coordinates: None,
                code_language: None,
            },
        ];

        let chunks =
            build_structured_chunk_windows(&blocks, StructuredChunkingProfile { max_chars: 80 });

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_kind, StructuredBlockKind::EndpointBlock);
        assert_eq!(chunks[0].support_block_ids.len(), 2);
        assert_eq!(chunks[0].heading_trail, vec!["API".to_string()]);
        assert!(
            chunks[0].literal_digest.as_deref().is_some_and(|value| value.starts_with("sha256:"))
        );
    }
}
