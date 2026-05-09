use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(super) struct PendingChunkInsert {
    pub(super) chunk_index: i32,
    pub(super) start_offset: i32,
    pub(super) end_offset: i32,
    pub(super) token_count: Option<i32>,
    pub(super) chunk_kind: Option<String>,
    pub(super) content_text: String,
    pub(super) normalized_text: String,
    pub(super) text_checksum: String,
    pub(super) support_block_ids: Vec<Uuid>,
    pub(super) section_path: Vec<String>,
    pub(super) heading_trail: Vec<String>,
    pub(super) literal_digest: Option<String>,
    pub(super) quality_score: Option<f32>,
    pub(super) window_text: Option<String>,
    /// Earliest record timestamp aggregated into this chunk (JSONL ingest
    /// only; None for non-temporal sources). Computed via the canonical
    /// `record_jsonl::extract_chunk_temporal_bounds` helper at construction
    /// time so a single source feeds Postgres + Arango writers downstream.
    pub(super) occurred_at: Option<DateTime<Utc>>,
    /// Latest record timestamp aggregated into this chunk. Equals
    /// `occurred_at` for single-record chunks; None when `occurred_at` is
    /// None.
    pub(super) occurred_until: Option<DateTime<Utc>>,
}

pub(super) fn locate_chunk_offsets(
    text: &str,
    chunk_text: &str,
    next_search_char: usize,
) -> (usize, usize) {
    let start_byte = char_offset_to_byte_index(text, next_search_char);
    if let Some(relative_start) = text[start_byte..].find(chunk_text) {
        let chunk_start_byte = start_byte + relative_start;
        let chunk_end_byte = chunk_start_byte + chunk_text.len();
        let chunk_start = text[..chunk_start_byte].chars().count();
        let chunk_end = text[..chunk_end_byte].chars().count();
        return (chunk_start, chunk_end);
    }

    let chunk_start = next_search_char;
    let chunk_end = chunk_start.saturating_add(chunk_text.chars().count());
    (chunk_start, chunk_end)
}

fn char_offset_to_byte_index(text: &str, char_offset: usize) -> usize {
    text.char_indices().nth(char_offset).map_or(text.len(), |(byte_index, _)| byte_index)
}
