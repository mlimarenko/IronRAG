use super::{KnowledgeChunkRow, RuntimeMatchedChunk, Uuid};

pub(super) const SOURCE_CONTEXT_DOCUMENT_LIMIT: usize = 3;
pub(super) const SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT: usize = SOURCE_CONTEXT_DOCUMENT_LIMIT * 8;
pub(super) const SOURCE_CONTEXT_PROFILE_DOCUMENT_LIMIT: usize = SOURCE_CONTEXT_DOCUMENT_LIMIT * 2;
pub(super) const SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT: usize = 2;
pub(super) const SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD: i32 = 1;
pub(super) const SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD: i32 = 1;
pub(super) const SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD: i32 = 3;
pub(super) const SOURCE_CONTEXT_PROFILE_HEADROOM: usize = 1;
pub(super) const SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT: usize = 2;
pub(super) const SOURCE_CONTEXT_FOCUSED_DECLARATION_LIMIT_PER_DOCUMENT: usize = 1;
pub(super) const SOURCE_CONTEXT_FOCUSED_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT: usize = 64;
pub(super) const SOURCE_CONTEXT_FOCUSED_MATCH_SCORE_BONUS: f32 = 1.0;
pub(super) const SOURCE_CONTEXT_PATH_MATCH_LIMIT_PER_DOCUMENT: usize = 2;
pub(super) const SOURCE_CONTEXT_PATH_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT: usize = 16;
pub(super) const SOURCE_CONTEXT_PATH_MATCH_SCORE_BONUS: f32 = 0.8;
pub(super) const SOURCE_CONTEXT_SETUP_PATH_SCORE_BONUS: f32 = 4.0;
pub(super) const SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD: i32 = 16;
pub(super) const SOURCE_CONTEXT_TABLE_STRUCTURED_FORWARD: i32 = 64;
pub(super) const SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT: usize = 20;
pub(super) const SOURCE_CONTEXT_PROCEDURAL_SETUP_LIMIT_PER_DOCUMENT: usize = 6;
pub(super) const SOURCE_CONTEXT_TABLE_STRUCTURED_LIMIT_PER_DOCUMENT: usize = 32;
pub(super) const SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_SCORE_BONUS: f32 = 0.6;
pub(super) const SOURCE_CONTEXT_CODE_PATTERN_TERM_LIMIT: usize = 10;
pub(super) const SOURCE_CONTEXT_CODE_PATTERN_HIT_LIMIT: usize = 8;
pub(super) const SOURCE_CONTEXT_CODE_PATTERN_SCORE_BONUS: f32 = 3.0;
pub(super) const SOURCE_CONTEXT_TRANSPORT_PATTERN_TERM_LIMIT: usize = 10;
pub(super) const SOURCE_CONTEXT_TRANSPORT_PATTERN_HIT_LIMIT: usize = 16;
pub(super) const SOURCE_CONTEXT_TRANSPORT_PATTERN_SCORE_BONUS: f32 = 2.5;
pub(super) const SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_TERM_LIMIT: usize = 8;
pub(super) const SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_HIT_LIMIT_PER_TERM: usize = 48;
pub(super) const SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_COMPANION_LIMIT: usize = 12;
pub(super) const SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_SCORE_BONUS: f32 = 2.25;
pub(super) const SOURCE_CONTEXT_INVENTORY_PROFILE_TERM_LIMIT: usize = 8;
pub(super) const SOURCE_CONTEXT_INVENTORY_PROFILE_HIT_LIMIT_PER_TERM: usize = 32;
pub(super) const SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT: usize = 6;
pub(super) const SOURCE_CONTEXT_FALLBACK_STRUCTURED_TOP_K_FLOOR: usize = 32;
pub(crate) const SOURCE_SLICE_DEFAULT_COUNT: usize = 12;
pub(crate) const SOURCE_SLICE_MAX_COUNT: usize = 30;
pub(crate) const SOURCE_UNIT_CHUNK_KIND: &str = "source_unit";
pub(super) const HEADING_CHUNK_KIND: &str = "heading";
pub(super) const TABLE_ROW_CHUNK_KIND: &str = "table_row";
pub(super) const CODE_BLOCK_CHUNK_KIND: &str = "code_block";
pub(super) const KEY_VALUE_BLOCK_CHUNK_KIND: &str = "key_value_block";
pub(super) const METADATA_BLOCK_CHUNK_KIND: &str = "metadata_block";
pub(super) const SOURCE_SLICE_CONTEXT_CHARS_PER_UNIT: usize = 1_600;
pub(super) const SOURCE_SLICE_CONTEXT_MAX_CHARS: usize = 64_000;
pub(super) const SOURCE_CONTEXT_SELECTED_PROFILE_BONUS: f32 = 2.0;
pub(super) const SOURCE_CONTEXT_LIBRARY_PROFILE_BONUS: f32 = 1.5;
pub(super) const SOURCE_CONTEXT_STRUCTURED_PROFILE_SCORE_BONUS: f32 = 16.0;
pub(super) const SOURCE_CONTEXT_PROFILE_ANCHOR_SCORE_CAP: f32 = 8.0;
pub(super) const SOURCE_CONTEXT_NEIGHBOR_PENALTY: f32 = 0.01;
pub(super) const SOURCE_CONTEXT_SLICE_PROFILE_BONUS: f32 = 4.0;
pub(super) const SOURCE_CONTEXT_SLICE_BONUS: f32 = 3.0;

pub(super) const SOURCE_CONTEXT_EXCERPT_CHARS: usize = 720;
pub(super) const SOURCE_CONTEXT_GRAPH_EVIDENCE_BONUS: f32 = 0.75;
pub(super) const SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX: i32 = 0;
pub(super) const FOCUSED_DECLARATION_CHUNK_KIND: &str = "focused_declaration";

pub(crate) fn source_anchor_window(anchor: i32, backward: i32, forward: i32) -> (i32, i32) {
    (anchor.saturating_sub(backward.max(0)), anchor.saturating_add(forward.max(0)))
}

#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct StructuredSourceContextDiagnostics {
    pub(crate) eligible_document_count: usize,
    pub(crate) source_profile_count: usize,
    pub(crate) neighbor_count: usize,
    pub(crate) focused_match_count: usize,
    pub(crate) procedural_structured_sibling_count: usize,
    pub(crate) library_profile_count: usize,
    pub(crate) source_slice_count: usize,
}

#[derive(Debug, Clone)]
pub(super) struct SourceContextCandidate {
    pub(super) document_id: Uuid,
    pub(super) revision_id: Uuid,
    pub(super) first_rank: usize,
    pub(super) best_score: f32,
    pub(super) anchors: Vec<SourceContextAnchor>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SourceContextAnchor {
    pub(super) chunk_index: i32,
    pub(super) score: f32,
    pub(super) first_rank: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SourceContextNeighborSpan {
    pub(super) backward: i32,
    pub(super) forward: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StructuredSourceCompanionKind {
    SourceProfile,
    Neighbor,
    FocusedMatch,
    ProceduralStructuredSibling,
    LibrarySourceProfile,
}

#[derive(Debug, Clone)]
pub(super) struct StructuredSourceCompanion {
    pub(super) chunk: RuntimeMatchedChunk,
    pub(super) kind: StructuredSourceCompanionKind,
}

#[derive(Debug, Clone)]
pub(super) struct ScoredSourceContextRow {
    pub(super) row: KnowledgeChunkRow,
    pub(super) score: f32,
    pub(super) additional_declaration: bool,
}

#[derive(Debug, Clone)]
pub(super) struct PreparedSourceContextCandidate {
    pub(super) candidate: SourceContextCandidate,
    pub(super) neighbor_anchors: Vec<SourceContextAnchor>,
    pub(super) focused_rows: Vec<ScoredSourceContextRow>,
    pub(super) path_rows: Vec<ScoredSourceContextRow>,
}
