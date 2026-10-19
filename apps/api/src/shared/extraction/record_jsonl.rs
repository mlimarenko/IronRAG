use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::shared::extraction::{
    ExtractionLineHint, ExtractionLineSignal, ExtractionOutput, ExtractionSourceMetadata,
    ExtractionStructureHints,
    structured_document::{StructuredBlockData, StructuredBlockKind},
};

mod chunking;
mod config_comments;
mod parsing;
mod projection;
mod rendering;

use config_comments::{
    ConfigSyntax, anchor_comment_runs, extract_comment_runs, extract_document_header_text,
};

pub use chunking::{focused_record_unit_excerpt, split_large_record_units};
pub use parsing::{
    StructuredRecordHint, extract_record_jsonl, normalize_structured_records,
    structured_record_hint,
};
pub use projection::{extract_chunk_temporal_bounds, project_record_unit_values_for_graph};

/// Authoritative source-format token for the record-stream path.
///
/// Every structured-record input (JSONL/NDJSON, plain JSON object/array, YAML
/// document/stream) renders into the same field-aware text and is persisted
/// under this single token. Downstream record-stream-aware logic
/// (graph-extraction policy, reprocess source reconstruction) keys off this
/// value, so it is a single source of truth — do NOT introduce per-wire-format
/// variants.
pub const RECORD_JSONL_SOURCE_FORMAT: &str = "record_jsonl";
const RECORD_JSONL_FIELD_VALUE_LIMIT: usize = 240;
/// Free-text (prose) leaves render with a far higher cap than single-token
/// fields so message/body text is not truncated to a config-value width.
const RECORD_JSONL_FREE_TEXT_VALUE_LIMIT: usize = 8_000;
const RECORD_JSONL_ARRAY_ITEM_LIMIT: usize = 8;
const RECORD_JSONL_OBJECT_DEPTH_LIMIT: usize = 10;

#[cfg(test)]
mod tests;
