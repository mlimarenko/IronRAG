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
    /// only; None for non-temporal sources). Computed through the shared
    /// `record_jsonl::extract_chunk_temporal_bounds` helper at construction
    /// time so a single source feeds downstream persistence writers.
    pub(super) occurred_at: Option<DateTime<Utc>>,
    /// Latest record timestamp aggregated into this chunk. Equals
    /// `occurred_at` for single-record chunks; None when `occurred_at` is
    /// None.
    pub(super) occurred_until: Option<DateTime<Utc>>,
}
