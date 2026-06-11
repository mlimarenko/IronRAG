use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::shared::extraction::{
    ExtractionLineHint, ExtractionLineSignal, ExtractionOutput, ExtractionSourceMetadata,
    ExtractionStructureHints,
};

/// Canonical source-format token for the record-stream path.
///
/// Every structured-record input (JSONL/NDJSON, plain JSON object/array, YAML
/// document/stream) renders into the same field-aware text and is persisted
/// under this single token. Downstream record-stream-aware logic
/// (graph-extraction policy, reprocess source reconstruction) keys off this
/// value, so it is a single source of truth — do NOT introduce per-wire-format
/// variants.
pub const RECORD_JSONL_SOURCE_FORMAT: &str = "record_jsonl";
const RECORD_JSONL_FIELD_LIMIT: usize = 32;
const RECORD_JSONL_FIELD_VALUE_LIMIT: usize = 240;
/// Free-text (prose) leaves render with a far higher cap than single-token
/// fields so message/body text is not truncated to a config-value width.
const RECORD_JSONL_FREE_TEXT_VALUE_LIMIT: usize = 8_000;
const RECORD_JSONL_ARRAY_ITEM_LIMIT: usize = 8;
const RECORD_JSONL_OBJECT_DEPTH_LIMIT: usize = 4;

/// Structural hint that narrows which structured-record parsers may attempt the
/// payload. Detection stays structural (attempt-parse decides record vs. text);
/// the hint only disambiguates among the gated wire formats so arbitrary
/// text-like input (markdown/prose, which `serde_yaml` would happily read as a
/// mapping) never gets misrouted into the record renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredRecordHint {
    /// Newline-delimited JSON objects (`.jsonl` / `.ndjson` / ndjson mimes).
    Jsonl,
    /// A single JSON document: object → 1 record, array of objects → N records.
    Json,
    /// A YAML document or `---` multi-document stream / sequence of mappings.
    Yaml,
    /// A TOML document: the top-level table → 1 record.
    Toml,
}

/// Resolves the structured-record hint from the filename extension and declared
/// mime, or `None` when neither names a structured-record wire format. Used by
/// the extraction router to gate the record path; arbitrary text-like input
/// (no json/yaml hint) is never offered to the record normalizer.
#[must_use]
pub fn structured_record_hint(
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Option<StructuredRecordHint> {
    let extension = file_name
        .and_then(|value| std::path::Path::new(value).extension())
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    let mime_essence = mime_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase);

    match extension.as_deref() {
        Some("jsonl" | "ndjson") => return Some(StructuredRecordHint::Jsonl),
        Some("json") => return Some(StructuredRecordHint::Json),
        Some("yaml" | "yml") => return Some(StructuredRecordHint::Yaml),
        Some("toml") => return Some(StructuredRecordHint::Toml),
        _ => {}
    }
    match mime_essence.as_deref() {
        Some(
            "application/jsonl"
            | "application/ndjson"
            | "application/x-jsonlines"
            | "application/x-ndjson",
        ) => Some(StructuredRecordHint::Jsonl),
        Some("application/json") => Some(StructuredRecordHint::Json),
        Some("application/yaml" | "application/x-yaml" | "text/yaml" | "text/x-yaml") => {
            Some(StructuredRecordHint::Yaml)
        }
        Some("application/toml" | "text/toml") => Some(StructuredRecordHint::Toml),
        _ => None,
    }
}

/// Normalizes a structured-record payload into the common record stream
/// (`Vec<Map<String, Value>>`) shared by every wire format, then renders it
/// through the single canonical record renderer.
///
/// Detection is structural within the gated `hint`: the bytes must actually
/// parse into a record-shaped value (an object, or a sequence/array/stream of
/// objects). Non-record-shaped payloads (a scalar, an array of scalars, a
/// deeply irregular blob, non-string YAML keys, an empty document) return
/// `None` so the caller degrades gracefully to the existing text path. This
/// path is intentionally lenient — it never surfaces a parse failure as an
/// extraction error.
#[must_use]
pub fn normalize_structured_records(
    file_bytes: &[u8],
    hint: StructuredRecordHint,
) -> Option<ExtractionOutput> {
    let raw_text = std::str::from_utf8(file_bytes).ok()?;
    let records = match hint {
        StructuredRecordHint::Jsonl => parse_jsonl_records(raw_text),
        StructuredRecordHint::Json => {
            parse_json_records(raw_text).or_else(|| parse_jsonl_records(raw_text))
        }
        StructuredRecordHint::Yaml => parse_yaml_records(raw_text),
        StructuredRecordHint::Toml => parse_toml_records(raw_text),
    }?;
    if records.is_empty() {
        return None;
    }
    render_record_stream(&records).ok()
}

fn parse_jsonl_records(raw_text: &str) -> Option<Vec<Map<String, Value>>> {
    let mut records = Vec::<Map<String, Value>>::new();
    for line in raw_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed).ok()?;
        records.push(value.as_object()?.clone());
    }
    (!records.is_empty()).then_some(records)
}

fn parse_json_records(raw_text: &str) -> Option<Vec<Map<String, Value>>> {
    let value: Value = serde_json::from_str(raw_text.trim()).ok()?;
    records_from_value(value)
}

fn parse_yaml_records(raw_text: &str) -> Option<Vec<Map<String, Value>>> {
    let mut records = Vec::<Map<String, Value>>::new();
    let mut any_document = false;
    for document in serde_yaml::Deserializer::from_str(raw_text) {
        any_document = true;
        let yaml_value = serde_yaml::Value::deserialize(document).ok()?;
        // YAML supports non-string keys; serialize via serde_json::Value, which
        // rejects those, so only string-keyed mappings flow through.
        let json_value = serde_json::to_value(&yaml_value).ok()?;
        if matches!(json_value, Value::Null) {
            continue;
        }
        records.extend(records_from_value(json_value)?);
    }
    if !any_document {
        return None;
    }
    (!records.is_empty()).then_some(records)
}

fn parse_toml_records(raw_text: &str) -> Option<Vec<Map<String, Value>>> {
    // A TOML document is always a top-level table → exactly one record. TOML
    // datetime values do not map onto serde_json::Value, so such documents
    // (and any other non-mappable shape) deserialize to an error and degrade
    // to the text path rather than panicking. Array-of-tables is rendered as
    // nested array fields of the single record; per-table record explosion is
    // a follow-up.
    let value: Value = toml_edit::de::from_str(raw_text).ok()?;
    records_from_value(value)
}

/// Maps a single decoded structured value into zero or more record objects.
/// An object becomes one record; an array becomes one record per object
/// element. Any other shape (scalar, array of scalars/mixed) yields `None`,
/// signalling a non-record payload that must degrade to the text path.
fn records_from_value(value: Value) -> Option<Vec<Map<String, Value>>> {
    match value {
        Value::Object(object) => Some(vec![object]),
        Value::Array(items) => {
            if items.is_empty() {
                return None;
            }
            let mut records = Vec::<Map<String, Value>>::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Object(object) => records.push(object),
                    _ => return None,
                }
            }
            Some(records)
        }
        _ => None,
    }
}

/// Strict JSONL/NDJSON front-end: parses each non-blank line as a JSON object
/// and renders the common record stream. Keeps fail-loud semantics for the
/// declared-JSONL upload path (invalid lines and empty payloads are errors).
pub fn extract_record_jsonl(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let raw_text = std::str::from_utf8(file_bytes)
        .map_err(|_| anyhow!("invalid utf-8 record jsonl payload"))?;
    let mut records = Vec::<Map<String, Value>>::new();
    for (line_index, line) in raw_text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|error| anyhow!("invalid record jsonl line {}: {error}", line_index + 1))?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("record jsonl line {} must be a JSON object", line_index + 1))?;
        records.push(object.clone());
    }
    if records.is_empty() {
        return Err(anyhow!("record jsonl payload contains no records"));
    }
    render_record_stream(&records)
}

/// Canonical renderer over the common record stream. Every wire format funnels
/// here so JSON/YAML/JSONL all get identical field-aware rendering, the
/// cross-record schema profile, and temporal bounds. No second renderer.
fn render_record_stream(records: &[Map<String, Value>]) -> Result<ExtractionOutput> {
    let mut rendered_records = Vec::<RenderedRecord>::with_capacity(records.len());
    let mut profile = RecordSourceProfile::default();

    for (record_index, object) in records.iter().enumerate() {
        let rendered = render_record_unit(object, record_index)?;
        profile.observe(object);
        rendered_records.push(RenderedRecord { source_ordinal: record_index, text: rendered });
    }

    if rendered_records.is_empty() {
        return Err(anyhow!("record stream payload contains no records"));
    }

    let mut rendered_lines = Vec::<String>::with_capacity(rendered_records.len() + 1);
    let mut hints = Vec::<ExtractionLineHint>::new();
    let mut offset = 0_i32;

    push_rendered_line(
        &mut rendered_lines,
        &mut hints,
        &mut offset,
        profile.render_header(),
        Vec::new(),
        vec![ExtractionLineSignal::SourceProfile, ExtractionLineSignal::MetadataCandidate],
    );
    for record in rendered_records {
        push_rendered_line(
            &mut rendered_lines,
            &mut hints,
            &mut offset,
            record.text,
            vec![i32::try_from(record.source_ordinal).unwrap_or(i32::MAX)],
            vec![ExtractionLineSignal::SourceUnit],
        );
    }

    Ok(ExtractionOutput {
        extraction_kind: RECORD_JSONL_SOURCE_FORMAT.to_string(),
        content_text: rendered_lines.join("\n"),
        page_count: None,
        warnings: Vec::new(),
        source_metadata: ExtractionSourceMetadata {
            source_format: RECORD_JSONL_SOURCE_FORMAT.to_string(),
            page_count: None,
            line_count: i32::try_from(hints.len()).unwrap_or(i32::MAX),
        },
        structure_hints: ExtractionStructureHints { lines: hints },
        source_map: serde_json::json!({
            "adapter": RECORD_JSONL_SOURCE_FORMAT,
            "recordCount": profile.record_count,
            "sourceProfile": profile.to_json(),
        }),
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images: Vec::new(),
    })
}

#[derive(Debug)]
struct RenderedRecord {
    source_ordinal: usize,
    text: String,
}

/// Shape-based, key-agnostic profile across a heterogeneous record stream.
///
/// Records may come from any system with arbitrary, irregular, deeply-nested
/// schemas. The profile never assumes a key name: it counts records, tracks
/// how often each top-level key path appears (so heterogeneous schemas are
/// observable), counts records carrying free text, and aggregates the temporal
/// span from value-shaped timestamps detected anywhere in each record.
#[derive(Debug, Default)]
struct RecordSourceProfile {
    record_count: usize,
    text_record_count: usize,
    key_coverage: BTreeMap<String, usize>,
    distinct_keys: BTreeSet<String>,
    time_start: Option<DateTime<Utc>>,
    time_end: Option<DateTime<Utc>>,
}

impl RecordSourceProfile {
    fn observe(&mut self, record: &Map<String, Value>) {
        self.record_count += 1;
        if record_has_free_text(record) {
            self.text_record_count += 1;
        }
        for key in record.keys() {
            let sanitized = sanitize_field_path_segment(key);
            self.distinct_keys.insert(sanitized.clone());
            *self.key_coverage.entry(sanitized).or_default() += 1;
        }
        if let Some(occurred_at) = detect_record_timestamp(record) {
            self.time_start = Some(self.time_start.map_or(occurred_at, |c| c.min(occurred_at)));
            self.time_end = Some(self.time_end.map_or(occurred_at, |c| c.max(occurred_at)));
        }
    }

    fn render_header(&self) -> String {
        let mut parts = vec![
            format!("source_format={RECORD_JSONL_SOURCE_FORMAT}"),
            "sequence_kind=record_stream".to_string(),
            format!("unit_count={}", self.record_count),
            format!("text_unit_count={}", self.text_record_count),
            format!("distinct_key_count={}", self.distinct_keys.len()),
        ];
        if !self.key_coverage.is_empty() {
            parts.push(format!("top_keys={}", render_count_map(&self.key_coverage, 12)));
        }
        if let Some(time_start) = self.time_start {
            parts.push(format!("time_start={}", time_start.to_rfc3339()));
        }
        if let Some(time_end) = self.time_end {
            parts.push(format!("time_end={}", time_end.to_rfc3339()));
        }
        format!("[source_profile {}]", parts.join(" "))
    }

    fn to_json(&self) -> Value {
        serde_json::json!({
            "sourceFormat": RECORD_JSONL_SOURCE_FORMAT,
            "unitCount": self.record_count,
            "textUnitCount": self.text_record_count,
            "distinctKeyCount": self.distinct_keys.len(),
            "keyCoverage": self.key_coverage,
            "timeStart": self.time_start.map(|value| value.to_rfc3339()),
            "timeEnd": self.time_end.map(|value| value.to_rfc3339()),
        })
    }
}

fn push_rendered_line(
    rendered_lines: &mut Vec<String>,
    hints: &mut Vec<ExtractionLineHint>,
    offset: &mut i32,
    text: String,
    source_ordinals: Vec<i32>,
    signals: Vec<ExtractionLineSignal>,
) {
    if !rendered_lines.is_empty() {
        *offset = offset.saturating_add(1);
    }
    let start_offset = *offset;
    *offset = offset.saturating_add(i32::try_from(text.chars().count()).unwrap_or(i32::MAX));
    hints.push(ExtractionLineHint {
        ordinal: i32::try_from(hints.len()).unwrap_or(i32::MAX),
        source_ordinals,
        page_number: None,
        text: text.clone(),
        start_offset: Some(start_offset),
        end_offset: Some(*offset),
        signals,
    });
    rendered_lines.push(text);
}

fn render_count_map(counts: &BTreeMap<String, usize>, limit: usize) -> String {
    counts
        .iter()
        .take(limit)
        .map(|(key, count)| format!("{key}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Renders one arbitrary record into a single searchable line, fully generic
/// over its keys. No key name is special-cased: the line carries the record
/// ordinal, an `occurred_at=` token IF a value-shaped timestamp is found
/// anywhere in the record (so the canonical temporal extractor can aggregate
/// it), and every leaf value flattened to `dotted.path=value` so any deep
/// identifier/path/name stays searchable text. Free-text leaves render with a
/// generous cap so message bodies are not truncated to a config-value width.
fn render_record_unit(record: &Map<String, Value>, record_index: usize) -> Result<String> {
    let fields = render_generic_fields(record);
    if fields.is_empty() {
        return Err(anyhow!(
            "record {} must contain at least one non-null scalar value",
            record_index + 1
        ));
    }

    let mut header_parts = vec![format!("unit_ordinal={record_index}")];
    if let Some(occurred_at) = detect_record_timestamp(record) {
        header_parts.push(format!("occurred_at={}", occurred_at.to_rfc3339()));
    }

    let mut rendered = format!("[{}]", header_parts.join(" "));
    rendered.push(' ');
    rendered.push_str("fields: ");
    rendered.push_str(&fields.join("; "));
    Ok(rendered)
}

/// Detects a record-level timestamp by VALUE SHAPE, not by key name: the
/// earliest value-shaped timestamp found anywhere in the record (any depth,
/// under any key) is used as the record's canonical `occurred_at`. Returns
/// `None` when no value parses as a timestamp.
fn detect_record_timestamp(record: &Map<String, Value>) -> Option<DateTime<Utc>> {
    let mut earliest: Option<DateTime<Utc>> = None;
    collect_value_timestamps(&Value::Object(record.clone()), 0, &mut |ts| {
        earliest = Some(earliest.map_or(ts, |current| current.min(ts)));
    });
    earliest
}

fn collect_value_timestamps(value: &Value, depth: usize, visit: &mut impl FnMut(DateTime<Utc>)) {
    if depth > RECORD_JSONL_OBJECT_DEPTH_LIMIT {
        return;
    }
    match value {
        Value::String(text) => {
            if let Some(ts) = parse_value_timestamp_string(text) {
                visit(ts);
            }
        }
        Value::Number(number) => {
            if let Some(ts) = parse_value_timestamp_epoch(number) {
                visit(ts);
            }
        }
        Value::Array(items) => {
            for item in items.iter().take(RECORD_JSONL_ARRAY_ITEM_LIMIT) {
                collect_value_timestamps(item, depth + 1, visit);
            }
        }
        Value::Object(object) => {
            for nested in object.values() {
                collect_value_timestamps(nested, depth + 1, visit);
            }
        }
        Value::Bool(_) | Value::Null => {}
    }
}

/// Parses an RFC3339 / ISO-8601 instant from a string value. Key-agnostic: the
/// string itself must look like a timestamp; the key it lives under is
/// irrelevant.
fn parse_value_timestamp_string(value: &str) -> Option<DateTime<Utc>> {
    let trimmed = value.trim();
    if trimmed.len() < 10 {
        return None;
    }
    DateTime::parse_from_rfc3339(trimmed).ok().map(|parsed| parsed.with_timezone(&Utc))
}

/// Parses a plausible epoch timestamp from a numeric value, bounded hard to
/// avoid mis-stamping ids/counts/amounts. Only 10-digit (seconds) and 13-digit
/// (milliseconds) integers within a sane calendar range are accepted.
fn parse_value_timestamp_epoch(number: &serde_json::Number) -> Option<DateTime<Utc>> {
    let raw = number.as_i64()?;
    if raw <= 0 {
        return None;
    }
    // 1e9 ≈ 2001-09, 1e10 (10 digits) upper-bounds seconds at ~2286; 13 digits
    // are milliseconds. Anything outside these magnitudes is treated as a plain
    // number, not a time.
    let seconds = match raw {
        1_000_000_000..=9_999_999_999 => raw,
        1_000_000_000_000..=9_999_999_999_999 => raw / 1_000,
        _ => return None,
    };
    DateTime::from_timestamp(seconds, 0)
}

/// Flattens an arbitrary record into `dotted.path=value` leaf fields. Generic
/// over keys and depth; every leaf string/number/bool becomes searchable text.
/// Bounded by the shared depth/array/field caps so huge or pathological
/// structures degrade gracefully without unbounded output.
fn render_generic_fields(record: &Map<String, Value>) -> Vec<String> {
    let mut fields = Vec::<String>::new();
    let mut path = Vec::<String>::new();
    for (key, value) in record {
        path.push(sanitize_field_path_segment(key));
        collect_leaf_fields(value, &mut path, &mut fields, 0);
        path.pop();
        if fields.len() >= RECORD_JSONL_FIELD_LIMIT {
            break;
        }
    }
    fields
}

fn collect_leaf_fields(
    value: &Value,
    path: &mut Vec<String>,
    fields: &mut Vec<String>,
    depth: usize,
) {
    if fields.len() >= RECORD_JSONL_FIELD_LIMIT || path.is_empty() {
        return;
    }
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => {
            if let Some(rendered) =
                value_to_string(value).and_then(|scalar| render_leaf_value(&scalar))
            {
                fields.push(format!("{}={rendered}", path.join(".")));
            }
        }
        Value::Array(items) => {
            let scalar_values = items
                .iter()
                .take(RECORD_JSONL_ARRAY_ITEM_LIMIT)
                .filter_map(value_to_string)
                .filter_map(|scalar| render_leaf_value(&scalar))
                .collect::<Vec<_>>();
            if !scalar_values.is_empty() {
                fields.push(format!("{}={}", path.join("."), scalar_values.join(", ")));
                return;
            }
            if depth >= RECORD_JSONL_OBJECT_DEPTH_LIMIT {
                return;
            }
            for (index, item) in items.iter().take(RECORD_JSONL_ARRAY_ITEM_LIMIT).enumerate() {
                path.push(index.to_string());
                collect_leaf_fields(item, path, fields, depth + 1);
                path.pop();
                if fields.len() >= RECORD_JSONL_FIELD_LIMIT {
                    break;
                }
            }
        }
        Value::Object(object) => {
            if depth >= RECORD_JSONL_OBJECT_DEPTH_LIMIT {
                return;
            }
            for (key, nested) in object {
                path.push(sanitize_field_path_segment(key));
                collect_leaf_fields(nested, path, fields, depth + 1);
                path.pop();
                if fields.len() >= RECORD_JSONL_FIELD_LIMIT {
                    break;
                }
            }
        }
        Value::Null => {}
    }
}

/// Reports whether a record carries human-readable free text — a multi-word
/// string leaf anywhere in the record. Generic: it looks at value shape (a
/// string with whitespace), never at the key name.
fn record_has_free_text(record: &Map<String, Value>) -> bool {
    fn any_free_text(value: &Value, depth: usize) -> bool {
        if depth > RECORD_JSONL_OBJECT_DEPTH_LIMIT {
            return false;
        }
        match value {
            Value::String(text) => value_is_free_text(text),
            Value::Array(items) => items
                .iter()
                .take(RECORD_JSONL_ARRAY_ITEM_LIMIT)
                .any(|i| any_free_text(i, depth + 1)),
            Value::Object(object) => object.values().any(|v| any_free_text(v, depth + 1)),
            _ => false,
        }
    }
    any_free_text(&Value::Object(record.clone()), 0)
}

/// A leaf string is "free text" when it contains internal whitespace, i.e. it
/// reads as prose/message body rather than a single token/identifier.
fn value_is_free_text(value: &str) -> bool {
    value.split_whitespace().count() > 1
}

/// Renders one leaf scalar to searchable text. Free-text leaves (prose) get a
/// generous cap so message bodies survive; single-token leaves use the tighter
/// field-value cap. The cap is chosen by VALUE SHAPE, not by key name.
fn render_leaf_value(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let cap = if value_is_free_text(value) {
        RECORD_JSONL_FREE_TEXT_VALUE_LIMIT
    } else {
        RECORD_JSONL_FIELD_VALUE_LIMIT
    };
    let mut rendered = normalized.chars().take(cap).collect::<String>().replace(';', ",");
    if normalized.chars().count() > cap {
        rendered.push_str("...");
    }
    Some(rendered)
}

fn sanitize_field_path_segment(value: &str) -> String {
    let rendered = value
        .chars()
        .map(|ch| if ch.is_alphanumeric() || ch == '_' || ch == '-' { ch } else { '_' })
        .collect::<String>();
    if rendered.is_empty() { "_".to_string() } else { rendered }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Scans rendered chunk text for `occurred_at=ISO` substrings emitted by
/// `render_record_unit` and returns the (min, max) inclusive temporal bounds of
/// the records the chunk aggregates. The `occurred_at=` token is derived from
/// value-shaped timestamp detection (any key, any depth), not an input key
/// name, so it is fully format- and schema-agnostic.
///
/// Returns `None` for chunks that never appeared in a `record_jsonl`
/// extraction — they have no canonical temporal interpretation. Single-record
/// chunks return `Some((ts, ts))`. Chunks combining records across a month
/// boundary return the wider span so retrieval-side overlap filters
/// (`occurred_at < @t_end AND occurred_until >= @t_start`) work correctly.
///
/// This is the canonical temporal extractor: shared by the ingest write path
/// (so new chunks are temporally indexed) and the runtime backfill binary
/// (so older chunks become indexed without re-ingest). No second
/// implementation may parse this header — keep this fn as the only authority.
pub fn extract_chunk_temporal_bounds(text: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    const HEADER_KEY: &str = "occurred_at=";
    let mut min_ts: Option<DateTime<Utc>> = None;
    let mut max_ts: Option<DateTime<Utc>> = None;

    for fragment in text.split(HEADER_KEY).skip(1) {
        let end = fragment
            .find(|c: char| c.is_whitespace() || c == ']' || c == '|')
            .unwrap_or(fragment.len());
        let candidate = &fragment[..end];
        if let Ok(parsed) = DateTime::parse_from_rfc3339(candidate) {
            let utc = parsed.with_timezone(&Utc);
            min_ts = Some(min_ts.map_or(utc, |existing| existing.min(utc)));
            max_ts = Some(max_ts.map_or(utc, |existing| existing.max(utc)));
        }
    }

    match (min_ts, max_ts) {
        (Some(min), Some(max)) => Some((min, max)),
        _ => None,
    }
}

/// Projects a rendered `source_unit` line into a values-only, scaffolding-free
/// text suitable for graph extraction. Search and embeddings keep the full
/// field-aware rendered text; only the graph LLM is fed this projection, so the
/// knowledge graph anchors on real record VALUES (names, identifiers, paths,
/// places) instead of the renderer's machine scaffolding.
///
/// Dropped, because they are bookkeeping rather than knowledge: the
/// `[unit_ordinal=N occurred_at=ISO]` header, the dotted-path field KEYS (the
/// `key.path=` left-hand side of each leaf — already structural metadata, not
/// content), and any timestamp-shaped VALUE (already captured as the chunk's
/// canonical `occurred_at`/`occurred_until` temporal bounds, so it must not also
/// become an entity node).
///
/// Fully format- and schema-agnostic: it keys off the structural shape
/// guaranteed by [`render_record_unit`] — the literal `fields: ` boundary, the
/// `; ` field separator (values never contain a raw `;`; [`render_leaf_value`]
/// rewrites it to `,`), and the first `=` per field (path segments are
/// sanitized to alnum/`_`/`-`, so a key never contains `=`, and a value's own
/// `=`, e.g. in a URL, survives). No field name is ever special-cased.
///
/// Returns `None` when the projection has no non-timestamp value left (e.g. a
/// record whose only leaves were timestamps), so the caller skips an empty graph
/// chunk rather than feeding bare scaffolding.
#[must_use]
pub fn project_record_unit_values_for_graph(text: &str) -> Option<String> {
    const FIELDS_MARKER: &str = "fields: ";
    let mut value_lines = Vec::<String>::new();

    for line in text.lines() {
        let Some(body) = line.split_once(FIELDS_MARKER).map(|(_, body)| body) else {
            continue;
        };
        let mut values = Vec::<String>::new();
        for field in body.split("; ") {
            // Take the value after the first `=`; keys never contain `=`.
            let Some((_, value)) = field.split_once('=') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            // Drop timestamp-shaped values; they are the record's temporal
            // metadata, not entities.
            if parse_value_timestamp_string(value).is_some() {
                continue;
            }
            values.push(value.to_string());
        }
        if !values.is_empty() {
            value_lines.push(values.join("; "));
        }
    }

    (!value_lines.is_empty()).then(|| value_lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_generic_record_jsonl_units() {
        // Arbitrary heterogeneous records — no assumed key vocabulary. The
        // profile is shape-based; every leaf value (including ones nested under
        // arbitrary keys) is searchable, and a value-shaped timestamp under any
        // key stamps the record.
        let payload = br#"{"id":"msg-1","recordedAt":"2026-04-28T09:00:00Z","speaker":{"role":"user","label":"User One"},"text":"How do I configure search?"}
{"ref":"msg-2","speaker":{"role":"assistant"},"body":"Use the retrieval settings."}"#;

        let output = extract_record_jsonl(payload).expect("jsonl extraction");

        assert_eq!(output.extraction_kind, RECORD_JSONL_SOURCE_FORMAT);
        assert_eq!(output.source_metadata.source_format, RECORD_JSONL_SOURCE_FORMAT);
        assert_eq!(output.structure_hints.lines.len(), 3);
        assert!(output.content_text.starts_with("[source_profile source_format=record_jsonl"));
        assert!(output.content_text.contains("unit_count=2"));
        // Heterogeneous schema is observable via shape-based key coverage.
        assert!(output.content_text.contains("distinct_key_count="));
        assert!(output.content_text.contains("top_keys="));
        // Every leaf value stays searchable text, regardless of key name.
        assert!(output.content_text.contains("id=msg-1"));
        assert!(output.content_text.contains("ref=msg-2"));
        assert!(output.content_text.contains("speaker.role=user"));
        assert!(output.content_text.contains("speaker.role=assistant"));
        // Value-shaped timestamp under an arbitrary key name stamps the record.
        assert!(output.content_text.contains("occurred_at=2026-04-28T09:00:00+00:00"));
        assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
        assert_eq!(output.source_map["sourceProfile"]["unitCount"], serde_json::json!(2));
        assert_eq!(output.source_map["sourceProfile"]["keyCoverage"]["id"], serde_json::json!(1));
        assert!(output.structure_hints.lines[0].source_ordinals.is_empty());
        assert!(
            output.structure_hints.lines[0].signals.contains(&ExtractionLineSignal::SourceProfile)
        );
        assert!(
            output.structure_hints.lines[0]
                .signals
                .contains(&ExtractionLineSignal::MetadataCandidate)
        );
        assert!(
            output
                .structure_hints
                .lines
                .iter()
                .skip(1)
                .all(|line| line.signals.contains(&ExtractionLineSignal::SourceUnit))
        );
        assert!(
            output
                .structure_hints
                .lines
                .iter()
                .skip(1)
                .all(|line| { !line.signals.contains(&ExtractionLineSignal::MetadataCandidate) })
        );
    }

    #[test]
    fn renders_deep_leaf_fields_for_detail_retrieval() {
        // Every leaf — scalars, deeply-nested object values, and scalar arrays —
        // is flattened to dotted-path searchable text. No key is privileged.
        let output = extract_record_jsonl(
            br#"{"id":"row-1","recordedAt":"2026-03-14T12:00:00Z","status":"approved","amount":42,"details":{"provider":"Provider Beta","retry":false},"tags":["alpha","receipt"]}"#,
        )
        .expect("jsonl extraction");

        assert!(output.content_text.contains("occurred_at=2026-03-14T12:00:00+00:00"));
        assert!(output.content_text.contains("fields:"));
        assert!(output.content_text.contains("status=approved"));
        assert!(output.content_text.contains("amount=42"));
        assert!(output.content_text.contains("details.provider=Provider Beta"));
        assert!(output.content_text.contains("details.retry=false"));
        assert!(output.content_text.contains("tags=alpha, receipt"));
    }

    #[test]
    fn rejects_record_without_searchable_content() {
        let error = extract_record_jsonl(br#"{}"#).unwrap_err();
        assert!(error.to_string().contains("non-null scalar"));
    }

    #[test]
    fn extract_chunk_temporal_bounds_returns_none_for_chunks_without_header() {
        assert!(extract_chunk_temporal_bounds("plain text without any temporal header").is_none());
        assert!(extract_chunk_temporal_bounds("").is_none());
    }

    #[test]
    fn extract_chunk_temporal_bounds_handles_single_record_chunk() {
        let text =
            "[unit_ordinal=0 occurred_at=2024-09-06T12:19:38+00:00] fields: body=hello world";
        let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
        assert_eq!(min, max);
        assert_eq!(min.to_rfc3339(), "2024-09-06T12:19:38+00:00");
    }

    #[test]
    fn extract_chunk_temporal_bounds_aggregates_min_and_max_across_records() {
        let text = "\
            [unit_ordinal=0 occurred_at=2024-09-01T09:37:18+00:00] fields: a=one\n\
            [unit_ordinal=1 occurred_at=2026-04-28T10:52:10+00:00] fields: a=two\n\
            [unit_ordinal=2 occurred_at=2025-03-15T18:00:00+00:00] fields: a=three";
        let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
        assert_eq!(min.to_rfc3339(), "2024-09-01T09:37:18+00:00");
        assert_eq!(max.to_rfc3339(), "2026-04-28T10:52:10+00:00");
    }

    #[test]
    fn extract_chunk_temporal_bounds_normalizes_zulu_form() {
        let text = "[occurred_at=2026-03-15T12:00:00Z] body";
        let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
        assert_eq!(min, max);
        assert_eq!(min.timezone(), Utc);
    }

    #[test]
    fn extract_chunk_temporal_bounds_skips_unparseable_timestamps() {
        let text = "[occurred_at=not-a-date occurred_at=2024-09-01T00:00:00+00:00]";
        let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
        assert_eq!(min, max);
        assert_eq!(min.to_rfc3339(), "2024-09-01T00:00:00+00:00");
    }

    #[test]
    fn project_record_unit_values_strips_scaffolding_keeps_values() {
        // A realistic rendered source_unit line: the header (unit_ordinal +
        // occurred_at), the dotted-path KEYS, and the timestamp VALUE are all
        // scaffolding; only the real values must reach the graph LLM.
        let text = "[unit_ordinal=0 occurred_at=2026-04-22T19:05:00+00:00] fields: \
carrier=ZephyrFreight-77; origin=depot-gamma; destination=warehouse-beta; \
eventTime=2026-04-22T19:05:00+00:00; invoice=/srv/billing/invoices/inv-9931.pdf";
        let projected = project_record_unit_values_for_graph(text).expect("projection present");
        // Real values survive verbatim (the prompt requires byte-for-byte labels).
        assert!(projected.contains("ZephyrFreight-77"));
        assert!(projected.contains("depot-gamma"));
        assert!(projected.contains("warehouse-beta"));
        assert!(projected.contains("/srv/billing/invoices/inv-9931.pdf"));
        // Scaffolding is gone: no header, no dotted-path keys, no raw timestamp.
        assert!(!projected.contains("unit_ordinal"));
        assert!(!projected.contains("occurred_at"));
        assert!(!projected.contains("carrier="));
        assert!(!projected.contains("origin="));
        assert!(!projected.contains("eventTime"));
        assert!(!projected.contains("2026-04-22T19:05:00"));
    }

    #[test]
    fn project_record_unit_values_keeps_value_internal_equals_and_deep_keys() {
        // A value carrying its own `=` (a URL/DSN) must survive intact, and a
        // deep dotted-path key (shelves.0.code) must be dropped entirely.
        let text = "[unit_ordinal=3] fields: \
shelves.0.code=A1; shelves.0.contents=postgres://host/app?sslmode=require";
        let projected = project_record_unit_values_for_graph(text).expect("projection present");
        assert!(projected.contains("A1"));
        assert!(projected.contains("postgres://host/app?sslmode=require"));
        assert!(!projected.contains("shelves.0.code"));
        assert!(!projected.contains("shelves.0.contents"));
    }

    #[test]
    fn project_record_unit_values_returns_none_when_only_timestamps_remain() {
        // A record whose sole leaf value is a timestamp projects to nothing —
        // there is no real entity to feed the graph.
        let text = "[unit_ordinal=0 occurred_at=2026-04-22T19:05:00+00:00] fields: \
at=2026-04-22T19:05:00+00:00";
        assert!(project_record_unit_values_for_graph(text).is_none());
    }

    #[test]
    fn project_record_unit_values_returns_none_without_fields_marker() {
        // The source_profile line and any non-record text carry no `fields:`
        // boundary, so they project to nothing.
        assert!(
            project_record_unit_values_for_graph(
                "[source_profile source_format=record_jsonl unit_count=2 top_keys=carrier:1]"
            )
            .is_none()
        );
        assert!(project_record_unit_values_for_graph("plain prose without a marker").is_none());
    }

    #[test]
    fn project_record_unit_values_round_trips_render_record_unit_output() {
        // Locks the projection to exactly what render_record_unit emits: the
        // value reaches the graph, the key and the timestamp do not.
        let payload = br#"{"carrier":"ZephyrFreight-77","shippedAt":"2026-03-15T12:00:00Z"}"#;
        let output = extract_record_jsonl(payload).expect("jsonl extraction");
        let unit_line = output
            .content_text
            .lines()
            .find(|line| line.contains("fields:"))
            .expect("source unit line present");
        let projected =
            project_record_unit_values_for_graph(unit_line).expect("projection present");
        assert_eq!(projected, "ZephyrFreight-77");
    }

    #[test]
    fn extract_chunk_temporal_bounds_round_trips_render_record_unit_output() {
        // Verifies the helper consumes exactly the format render_record_unit
        // produces — keeping the read/write halves locked together. The
        // timestamp lives under an arbitrary key and is detected by shape.
        let payload = br#"{"id":"msg-x","emittedAt":"2026-03-15T12:00:00Z","text":"hello"}"#;
        let output = extract_record_jsonl(payload).expect("jsonl extraction");
        let (min, max) =
            extract_chunk_temporal_bounds(&output.content_text).expect("bounds present");
        assert_eq!(min, max);
        assert_eq!(min.to_rfc3339(), "2026-03-15T12:00:00+00:00");
    }

    #[test]
    fn structured_record_hint_resolves_from_extension_and_mime() {
        use StructuredRecordHint::{Json, Jsonl, Toml, Yaml};
        assert_eq!(structured_record_hint(Some("a.jsonl"), None), Some(Jsonl));
        assert_eq!(structured_record_hint(Some("a.ndjson"), None), Some(Jsonl));
        assert_eq!(structured_record_hint(Some("a.json"), None), Some(Json));
        assert_eq!(structured_record_hint(Some("a.yaml"), None), Some(Yaml));
        assert_eq!(structured_record_hint(Some("a.yml"), None), Some(Yaml));
        assert_eq!(structured_record_hint(Some("a.toml"), None), Some(Toml));
        assert_eq!(structured_record_hint(None, Some("application/json")), Some(Json));
        assert_eq!(structured_record_hint(None, Some("application/yaml")), Some(Yaml));
        assert_eq!(structured_record_hint(None, Some("text/yaml; charset=utf-8")), Some(Yaml));
        assert_eq!(structured_record_hint(None, Some("application/toml")), Some(Toml));
        assert_eq!(structured_record_hint(None, Some("application/x-ndjson")), Some(Jsonl));
        // Arbitrary text-like input must never be offered to the record path.
        assert_eq!(structured_record_hint(Some("notes.md"), Some("text/markdown")), None);
        assert_eq!(structured_record_hint(Some("readme.txt"), None), None);
        assert_eq!(structured_record_hint(None, Some("text/plain")), None);
    }

    #[test]
    fn normalizes_single_json_object_into_one_record() {
        let payload =
            br#"{"id":"alpha-1","status":"approved","detail":{"provider":"Provider Beta"}}"#;
        let output = normalize_structured_records(payload, StructuredRecordHint::Json)
            .expect("json object normalizes");

        assert_eq!(output.source_metadata.source_format, RECORD_JSONL_SOURCE_FORMAT);
        assert_eq!(output.source_map["recordCount"], serde_json::json!(1));
        assert!(output.content_text.contains("unit_count=1"));
        assert!(output.content_text.contains("id=alpha-1"));
        assert!(output.content_text.contains("status=approved"));
        assert!(output.content_text.contains("detail.provider=Provider Beta"));
    }

    #[test]
    fn normalizes_heterogeneous_json_array_into_per_element_records() {
        // Each element carries a DIFFERENT field set; a value present in only
        // one element must still be searchable and attributed to that record.
        let payload = br#"[
            {"id":"alpha-1","status":"approved","region":"north"},
            {"id":"beta-2","amount":4242,"carrier":"Carrier Gamma"},
            {"id":"gamma-3","note":"deferred receipt","priority":7}
        ]"#;
        let output = normalize_structured_records(payload, StructuredRecordHint::Json)
            .expect("json array normalizes");

        assert_eq!(output.source_map["recordCount"], serde_json::json!(3));
        assert!(output.content_text.contains("unit_count=3"));
        assert!(output.content_text.contains("id=alpha-1"));
        assert!(output.content_text.contains("id=beta-2"));
        assert!(output.content_text.contains("id=gamma-3"));
        // Field present in only one record is searchable text.
        assert!(output.content_text.contains("carrier=Carrier Gamma"));
        assert!(output.content_text.contains("region=north"));
        assert!(output.content_text.contains("note=deferred receipt"));
        // The field deep in one record is attributed to that record's line.
        let carrier_line = output
            .structure_hints
            .lines
            .iter()
            .find(|line| line.text.contains("carrier=Carrier Gamma"))
            .expect("carrier line present");
        assert!(carrier_line.text.contains("id=beta-2"));
        assert_eq!(carrier_line.source_ordinals, vec![1]);
    }

    #[test]
    fn normalizes_single_yaml_document_into_one_record() {
        let payload = b"id: alpha-1\nstatus: approved\ndetail:\n  provider: Provider Beta\n";
        let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
            .expect("yaml document normalizes");

        assert_eq!(output.source_map["recordCount"], serde_json::json!(1));
        assert!(output.content_text.contains("id=alpha-1"));
        assert!(output.content_text.contains("status=approved"));
        assert!(output.content_text.contains("detail.provider=Provider Beta"));
    }

    #[test]
    fn normalizes_multi_document_yaml_stream_into_per_document_records() {
        let payload = b"\
id: alpha-1\nregion: north\n---\nid: beta-2\ncarrier: Carrier Gamma\n---\nid: gamma-3\npriority: 7\n";
        let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
            .expect("yaml stream normalizes");

        assert_eq!(output.source_map["recordCount"], serde_json::json!(3));
        assert!(output.content_text.contains("id=alpha-1"));
        assert!(output.content_text.contains("id=beta-2"));
        assert!(output.content_text.contains("id=gamma-3"));
        assert!(output.content_text.contains("carrier=Carrier Gamma"));
        let carrier_line = output
            .structure_hints
            .lines
            .iter()
            .find(|line| line.text.contains("carrier=Carrier Gamma"))
            .expect("carrier line present");
        assert!(carrier_line.text.contains("id=beta-2"));
    }

    #[test]
    fn normalizes_yaml_sequence_of_mappings_into_per_item_records() {
        let payload =
            b"- id: alpha-1\n  status: approved\n- id: beta-2\n  carrier: Carrier Gamma\n";
        let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
            .expect("yaml sequence normalizes");

        assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
        assert!(output.content_text.contains("id=alpha-1"));
        assert!(output.content_text.contains("id=beta-2"));
        assert!(output.content_text.contains("carrier=Carrier Gamma"));
    }

    #[test]
    fn json_hint_falls_back_to_jsonl_when_payload_is_newline_delimited() {
        let payload = br#"{"id":"alpha-1","text":"first"}
{"id":"beta-2","text":"second"}"#;
        let output = normalize_structured_records(payload, StructuredRecordHint::Json)
            .expect("ndjson under a json hint still normalizes");

        assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
        assert!(output.content_text.contains("id=alpha-1"));
        assert!(output.content_text.contains("id=beta-2"));
    }

    #[test]
    fn degrades_non_record_payloads_to_text_path() {
        // Scalar, array of scalars, empty array/object, prose-shaped YAML, and a
        // YAML mapping with a non-string key must all return None so the caller
        // falls through to the existing text extractor.
        assert!(normalize_structured_records(b"42", StructuredRecordHint::Json).is_none());
        assert!(
            normalize_structured_records(br#""just a string""#, StructuredRecordHint::Json)
                .is_none()
        );
        assert!(normalize_structured_records(b"[1, 2, 3]", StructuredRecordHint::Json).is_none());
        assert!(normalize_structured_records(b"[]", StructuredRecordHint::Json).is_none());
        assert!(normalize_structured_records(b"{}", StructuredRecordHint::Json).is_none());
        assert!(
            normalize_structured_records(b"a plain prose sentence\n", StructuredRecordHint::Yaml)
                .is_none()
        );
        assert!(normalize_structured_records(b"- 1\n- 2\n", StructuredRecordHint::Yaml).is_none());
        assert!(normalize_structured_records(b"7\n", StructuredRecordHint::Yaml).is_none());
    }

    #[test]
    fn normalizes_toml_document_into_one_record() {
        let payload = b"id = \"alpha-1\"\nstatus = \"approved\"\n\n[detail]\nprovider = \"Provider Beta\"\nretry = false\n";
        let output = normalize_structured_records(payload, StructuredRecordHint::Toml)
            .expect("toml document normalizes");

        assert_eq!(output.source_map["recordCount"], serde_json::json!(1));
        assert!(output.content_text.contains("id=alpha-1"));
        assert!(output.content_text.contains("status=approved"));
        assert!(output.content_text.contains("detail.provider=Provider Beta"));
        assert!(output.content_text.contains("detail.retry=false"));
    }

    #[test]
    fn toml_with_datetime_value_normalizes_and_stamps_without_panic() {
        // A TOML datetime is preserved as a searchable string value and detected
        // by value shape, stamping the record's temporal bounds. Must not panic.
        let payload = b"id = \"alpha-1\"\nat = 2026-04-28T09:00:00Z\n";
        let output = normalize_structured_records(payload, StructuredRecordHint::Toml)
            .expect("toml with datetime normalizes");
        assert!(output.content_text.contains("id=alpha-1"));
        let (min, max) =
            extract_chunk_temporal_bounds(&output.content_text).expect("temporal bounds present");
        assert_eq!(min, max);
        assert_eq!(min.to_rfc3339(), "2026-04-28T09:00:00+00:00");
    }

    #[test]
    fn surfaces_deep_leaves_from_arbitrary_nested_records() {
        // A deeply-nested config-tree-style record from an unknown system: a
        // path/identifier buried several levels under arbitrary keys must still
        // be searchable text, bounded by the depth cap (no per-field lifting).
        let payload = br#"[
            {"kind":"config","tree":{"service":{"db":{"dsn":"postgres://host/app"}}}},
            {"kind":"action","op":{"tool":{"name":"apply_patch","input":{"path":"/srv/app/etc/settings.ini"}}}}
        ]"#;
        let output = normalize_structured_records(payload, StructuredRecordHint::Json)
            .expect("nested records normalize");

        assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
        // Deep leaves under arbitrary key paths are flattened to searchable text.
        assert!(output.content_text.contains("tree.service.db.dsn=postgres://host/app"));
        assert!(output.content_text.contains("op.tool.name=apply_patch"));
        assert!(output.content_text.contains("op.tool.input.path=/srv/app/etc/settings.ini"));
        // The deep path is attributed to its own record line.
        let path_line = output
            .structure_hints
            .lines
            .iter()
            .find(|line| line.text.contains("op.tool.input.path="))
            .expect("path line present");
        assert_eq!(path_line.source_ordinals, vec![1]);
    }

    #[test]
    fn detects_value_shaped_timestamps_under_arbitrary_key_names() {
        // An event/metrics log where the timestamp lives under different,
        // arbitrary key names per record. All must be detected by VALUE SHAPE
        // and ordered into the chunk's temporal span.
        let payload = br#"[
            {"event":"start","loggedAt":"2026-01-02T03:04:05Z"},
            {"event":"tick","eventTime":"2026-03-04T05:06:07Z"},
            {"event":"stop","when":"2026-06-07T08:09:10Z"}
        ]"#;
        let output = normalize_structured_records(payload, StructuredRecordHint::Json)
            .expect("event log normalizes");

        let (min, max) =
            extract_chunk_temporal_bounds(&output.content_text).expect("temporal bounds present");
        assert_eq!(min.to_rfc3339(), "2026-01-02T03:04:05+00:00");
        assert_eq!(max.to_rfc3339(), "2026-06-07T08:09:10+00:00");
        // Each record carries its own occurred_at derived from its differently
        // named timestamp field.
        let start_line = output
            .structure_hints
            .lines
            .iter()
            .find(|line| line.text.contains("event=start"))
            .expect("start line present");
        assert!(start_line.text.contains("occurred_at=2026-01-02T03:04:05+00:00"));
    }

    #[test]
    fn detects_epoch_timestamps_but_ignores_plain_numbers() {
        // 10-digit seconds / 13-digit millis are stamped; ids/counts/amounts are
        // not. Detection is bounded by magnitude, never by key name.
        let payload = br#"{"id":"alpha-1","seenAt":1767322445,"amount":4242,"count":7}"#;
        let output = normalize_structured_records(payload, StructuredRecordHint::Json)
            .expect("record normalizes");
        let (min, _max) =
            extract_chunk_temporal_bounds(&output.content_text).expect("epoch stamped");
        assert_eq!(min.to_rfc3339(), "2026-01-02T02:54:05+00:00");
        // The plain numbers remain searchable as fields, not mistaken for time.
        assert!(output.content_text.contains("amount=4242"));
        assert!(output.content_text.contains("count=7"));
    }

    #[test]
    fn renders_long_free_text_without_config_width_truncation() {
        // A long prose body (no privileged key) must survive far past the tight
        // single-token field cap.
        let body = "word ".repeat(200);
        let payload =
            serde_json::json!({ "id": "alpha-1", "body": body.trim() }).to_string().into_bytes();
        let output = normalize_structured_records(&payload, StructuredRecordHint::Json)
            .expect("record normalizes");
        // 200 words past RECORD_JSONL_FIELD_VALUE_LIMIT (240 chars) still render.
        assert!(output.content_text.contains(&"word ".repeat(120).trim().to_string()));
    }

    #[test]
    fn normalizes_ragged_yaml_multi_doc_stream() {
        // Multi-document YAML where each document has a ragged, different schema.
        let payload = b"\
order: alpha-1\nlines:\n  - sku: A\n    qty: 2\n---\nshipment: beta-2\ncarrier: Carrier Gamma\neta: 2026-05-01T00:00:00Z\n---\nnote: a free-form remark with several words\n";
        let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
            .expect("ragged yaml normalizes");

        assert_eq!(output.source_map["recordCount"], serde_json::json!(3));
        assert!(output.content_text.contains("order=alpha-1"));
        assert!(output.content_text.contains("lines.0.sku=A"));
        assert!(output.content_text.contains("carrier=Carrier Gamma"));
        assert!(output.content_text.contains("note=a free-form remark with several words"));
        // The shape-based profile observes the ragged union of keys.
        assert!(output.content_text.contains("distinct_key_count="));
        // The only value-shaped timestamp (under `eta`) sets the temporal span.
        let (min, max) =
            extract_chunk_temporal_bounds(&output.content_text).expect("temporal bounds present");
        assert_eq!(min, max);
        assert_eq!(min.to_rfc3339(), "2026-05-01T00:00:00+00:00");
    }

    #[test]
    fn never_panics_on_pathological_inputs() {
        // Robustness floor: huge arrays, deep nesting beyond the cap, mixed
        // types, and empty/garbage inputs must all return cleanly (Some or None),
        // never panic.
        let deep = (0..50).fold(String::from("\"leaf\""), |acc, _| format!("{{\"n\":{acc}}}"));
        let _ = normalize_structured_records(deep.as_bytes(), StructuredRecordHint::Json);
        let wide = format!("[{}]", vec![r#"{"k":1}"#; 5000].join(","));
        let _ = normalize_structured_records(wide.as_bytes(), StructuredRecordHint::Json);
        let _ = normalize_structured_records(b"\xff\xfe not utf8", StructuredRecordHint::Json);
        let _ = normalize_structured_records(b"", StructuredRecordHint::Yaml);
        let _ = normalize_structured_records(b"{ broken json", StructuredRecordHint::Json);
        let _ = normalize_structured_records(b":\n:\n", StructuredRecordHint::Yaml);
    }
}
