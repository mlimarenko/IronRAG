use std::collections::HashSet;

use sha2::{Digest, Sha256};

use self::quality::{compute_chunk_quality_score, compute_simhash, mark_near_duplicates};
use crate::shared::extraction::structured_document::{
    StructuredBlockData, StructuredBlockKind, StructuredChunkWindow,
};
use crate::shared::extraction::table_summary::is_table_summary_text;

mod code_boundaries;
mod code_splitting;
mod quality;
#[cfg(test)]
mod tests;

pub use code_splitting::split_large_code_blocks;

#[derive(Debug, Clone, Copy)]
pub struct StructuredChunkingProfile {
    pub max_chars: usize,
    pub overlap_chars: usize,
}

impl Default for StructuredChunkingProfile {
    fn default() -> Self {
        Self { max_chars: 2_800, overlap_chars: 280 }
    }
}

#[must_use]
pub fn build_structured_chunk_windows(
    blocks: &[StructuredBlockData],
    profile: StructuredChunkingProfile,
) -> Vec<StructuredChunkWindow> {
    let filtered_blocks = filter_chunkable_blocks(blocks);
    let mut chunks = StructuredChunkWindowBuilder::new(&filtered_blocks, profile).build();

    mark_near_duplicates(&mut chunks);
    compute_window_text_pass(&mut chunks);

    chunks
}

fn filter_chunkable_blocks(blocks: &[StructuredBlockData]) -> Vec<StructuredBlockData> {
    let table_parent_ids_with_rows = blocks
        .iter()
        .filter(|block| block.block_kind == StructuredBlockKind::TableRow)
        .filter_map(|block| block.parent_block_id)
        .collect::<HashSet<_>>();

    blocks
        .iter()
        .filter(|block| !block.is_boilerplate)
        .filter(|block| {
            block.block_kind != StructuredBlockKind::Table
                || !table_parent_ids_with_rows.contains(&block.block_id)
        })
        .cloned()
        .collect()
}

struct StructuredChunkWindowBuilder<'block> {
    blocks: &'block [StructuredBlockData],
    profile: StructuredChunkingProfile,
    chunks: Vec<StructuredChunkWindow>,
    window_start: usize,
    current_char_count: usize,
}

impl<'block> StructuredChunkWindowBuilder<'block> {
    fn new(blocks: &'block [StructuredBlockData], profile: StructuredChunkingProfile) -> Self {
        Self { blocks, profile, chunks: Vec::new(), window_start: 0, current_char_count: 0 }
    }

    fn build(mut self) -> Vec<StructuredChunkWindow> {
        for (index, block) in self.blocks.iter().enumerate() {
            self.push_block(index, block);
        }
        self.flush_trailing_window();
        self.chunks
    }

    fn push_block(&mut self, index: usize, block: &StructuredBlockData) {
        if is_standalone_chunk_block(block) {
            self.push_standalone_block(index, block);
            return;
        }

        let block_len = chunk_block_len(block);
        let projected = projected_window_len(self.current_char_count, block_len);
        if self.window_start < index && projected > self.profile.max_chars {
            self.flush_overflowing_window(index);
            return;
        }
        if self.should_start_heading_window(index, block) {
            self.flush_window(self.window_start, index);
            self.window_start = index;
            self.current_char_count = block_len;
            return;
        }

        self.current_char_count = if self.window_start == index { block_len } else { projected };
    }

    fn push_standalone_block(&mut self, index: usize, block: &StructuredBlockData) {
        self.flush_window(self.window_start, index);
        push_structured_chunk_window(&mut self.chunks, std::slice::from_ref(block));
        self.window_start = index.saturating_add(1);
        self.current_char_count = 0;
    }

    fn flush_overflowing_window(&mut self, index: usize) {
        self.flush_window(self.window_start, index);
        self.window_start = compute_overlap_start(
            self.blocks,
            self.window_start,
            index,
            self.profile.overlap_chars,
        );
        self.current_char_count = count_window_chars(&self.blocks[self.window_start..=index]);
    }

    fn should_start_heading_window(&self, index: usize, block: &StructuredBlockData) -> bool {
        block.block_kind == StructuredBlockKind::Heading
            && self.window_start < index
            && self.current_char_count >= self.profile.max_chars / 3
    }

    fn flush_trailing_window(&mut self) {
        self.flush_window(self.window_start, self.blocks.len());
    }

    fn flush_window(&mut self, start: usize, end: usize) {
        if start < end {
            push_structured_chunk_window(&mut self.chunks, &self.blocks[start..end]);
        }
    }
}

fn is_standalone_chunk_block(block: &StructuredBlockData) -> bool {
    matches!(
        block.block_kind,
        StructuredBlockKind::TableRow
            | StructuredBlockKind::SourceProfile
            | StructuredBlockKind::SourceUnit
    ) || (block.block_kind == StructuredBlockKind::MetadataBlock
        && is_table_summary_text(&block.normalized_text))
}

fn projected_window_len(current_char_count: usize, block_len: usize) -> usize {
    if current_char_count == 0 {
        block_len
    } else {
        current_char_count.saturating_add(2).saturating_add(block_len)
    }
}

fn count_window_chars(blocks: &[StructuredBlockData]) -> usize {
    blocks.iter().map(chunk_block_len).fold(0, projected_window_len)
}

/// Compute the overlap start index: walk backward from `flush_end` toward `window_start`,
/// accumulating block lengths until the overlap budget is exceeded.
fn compute_overlap_start(
    blocks: &[StructuredBlockData],
    window_start: usize,
    flush_end: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 || flush_end == 0 {
        return flush_end;
    }

    let mut overlap_used = 0_usize;
    let mut overlap_start = flush_end;

    for i in (window_start..flush_end).rev() {
        let block_len = chunk_block_len(&blocks[i]);
        let projected = if overlap_used == 0 {
            block_len
        } else {
            overlap_used.saturating_add(2).saturating_add(block_len)
        };
        if projected > overlap_chars {
            break;
        }
        overlap_used = projected;
        overlap_start = i;
    }

    overlap_start
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
    let literal_digest =
        format!("sha256:{}", hex::encode(Sha256::digest(normalized_text.as_bytes())));
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

    let quality_score = compute_chunk_quality_score(blocks);
    let simhash_fingerprint = Some(compute_simhash(&normalized_text));

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
        quality_score,
        simhash_fingerprint,
        is_near_duplicate: false,
        window_text: None,
    });
}

// ---------------------------------------------------------------------------
// Sentence-window retrieval
// ---------------------------------------------------------------------------

/// Maximum token budget for `window_text` (approx. whitespace-split words).
const WINDOW_TEXT_MAX_TOKENS: usize = 1_500;

/// Number of sentence-radius neighbours to include on each side of a chunk.
const WINDOW_SENTENCE_RADIUS: usize = 2;

/// Post-pass: fills `window_text` on every chunk in `chunks` by extracting
/// ±`WINDOW_SENTENCE_RADIUS` sentences from the neighbouring chunks' content.
/// The result is capped at `WINDOW_TEXT_MAX_TOKENS` tokens.
pub(crate) fn compute_window_text_pass(chunks: &mut [StructuredChunkWindow]) {
    let sentences_per_chunk =
        chunks.iter().map(|chunk| split_into_sentences(&chunk.content_text)).collect::<Vec<_>>();

    for (index, chunk) in chunks.iter_mut().enumerate() {
        let before = collect_preceding_sentences(&sentences_per_chunk, index);
        let after = collect_following_sentences(&sentences_per_chunk, index);
        chunk.window_text = build_window_text(&chunk.content_text, &before, &after);
    }
}

fn collect_preceding_sentences(sentences_per_chunk: &[Vec<String>], index: usize) -> Vec<&str> {
    let mut before = Vec::new();
    let mut remaining = WINDOW_SENTENCE_RADIUS;
    let mut chunk_index = index;

    loop {
        if chunk_index == 0 {
            break;
        }
        chunk_index -= 1;
        let sentences = &sentences_per_chunk[chunk_index];
        let take = remaining.min(sentences.len());
        before.extend(sentences[sentences.len() - take..].iter().map(String::as_str));
        remaining = remaining.saturating_sub(take);
        if remaining == 0 {
            break;
        }
    }
    before.reverse();
    before
}

fn collect_following_sentences(sentences_per_chunk: &[Vec<String>], index: usize) -> Vec<&str> {
    let mut after = Vec::new();
    let mut remaining = WINDOW_SENTENCE_RADIUS;
    let mut chunk_index = index;

    loop {
        chunk_index += 1;
        let Some(sentences) = sentences_per_chunk.get(chunk_index) else {
            break;
        };
        let take = remaining.min(sentences.len());
        after.extend(sentences[..take].iter().map(String::as_str));
        remaining = remaining.saturating_sub(take);
        if remaining == 0 {
            break;
        }
    }
    after
}

fn build_window_text(core: &str, before: &[&str], after: &[&str]) -> Option<String> {
    let prefix =
        if before.is_empty() { String::new() } else { format!("{}\n\n", before.join(" ")) };
    let suffix = if after.is_empty() { String::new() } else { format!("\n\n{}", after.join(" ")) };
    let trimmed = trim_to_tokens(&format!("{prefix}{core}{suffix}"), WINDOW_TEXT_MAX_TOKENS);
    (trimmed.len() > core.len()).then_some(trimmed)
}

/// Naively splits text into sentences at `.`, `!`, `?` boundaries.
fn split_into_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
            }
            current.clear();
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }
    sentences
}

/// Truncates `text` to at most `max_tokens` whitespace-split words.
fn trim_to_tokens(text: &str, max_tokens: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= max_tokens {
        return text.to_string();
    }
    words[..max_tokens].join(" ")
}

fn dominant_chunk_kind(blocks: &[StructuredBlockData]) -> StructuredBlockKind {
    blocks
        .iter()
        .find_map(|block| match block.block_kind {
            StructuredBlockKind::EndpointBlock
            | StructuredBlockKind::CodeBlock
            | StructuredBlockKind::Table
            | StructuredBlockKind::TableRow
            | StructuredBlockKind::SourceProfile
            | StructuredBlockKind::SourceUnit => Some(block.block_kind),
            _ => None,
        })
        .unwrap_or_else(|| blocks[0].block_kind)
}

fn chunk_block_len(block: &StructuredBlockData) -> usize {
    char_count(match block.block_kind {
        StructuredBlockKind::TableRow => block.normalized_text.trim(),
        _ => block.normalized_text.trim(),
    })
}
