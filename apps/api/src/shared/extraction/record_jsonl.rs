use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::shared::extraction::{
    ExtractionLineHint, ExtractionLineSignal, ExtractionOutput, ExtractionSourceMetadata,
    ExtractionStructureHints,
};

pub const RECORD_JSONL_SOURCE_FORMAT: &str = "record_jsonl";
const RECORD_JSONL_FIELD_LIMIT: usize = 32;
const RECORD_JSONL_FIELD_VALUE_LIMIT: usize = 240;
const RECORD_JSONL_ARRAY_ITEM_LIMIT: usize = 8;
const RECORD_JSONL_OBJECT_DEPTH_LIMIT: usize = 4;

pub fn extract_record_jsonl(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let raw_text = std::str::from_utf8(file_bytes)
        .map_err(|_| anyhow!("invalid utf-8 record jsonl payload"))?;
    let mut rendered_records = Vec::<RenderedRecord>::new();
    let mut profile = RecordSourceProfile::default();

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
        let rendered = render_record_unit(object, line_index)?;
        profile.observe(object);
        rendered_records.push(RenderedRecord { source_ordinal: line_index, text: rendered });
    }

    if rendered_records.is_empty() {
        return Err(anyhow!("record jsonl payload contains no records"));
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

#[derive(Debug, Default)]
struct RecordSourceProfile {
    record_count: usize,
    text_record_count: usize,
    attachment_count: usize,
    unit_kind_counts: BTreeMap<String, usize>,
    actor_role_counts: BTreeMap<String, usize>,
    actor_label_counts: BTreeMap<String, usize>,
    thread_keys: BTreeSet<String>,
    time_start: Option<DateTime<Utc>>,
    time_end: Option<DateTime<Utc>>,
}

impl RecordSourceProfile {
    fn observe(&mut self, record: &Map<String, Value>) {
        self.record_count += 1;
        let text = first_string(record, &["text", "payloadText", "payload_text", "content"])
            .or_else(|| record.get("payload").and_then(render_payload_value))
            .unwrap_or_default();
        if !text.trim().is_empty() {
            self.text_record_count += 1;
        }
        self.attachment_count += attachment_refs(record.get("attachments")).len();

        let unit_kind = first_string(record, &["kind", "unitKind", "unit_kind"])
            .unwrap_or_else(|| "record".to_string());
        increment_count(&mut self.unit_kind_counts, sanitize_header_value(&unit_kind));

        if let Some(actor) = record.get("actor").and_then(Value::as_object) {
            if let Some(role) = first_string(actor, &["role", "kind"]) {
                increment_count(&mut self.actor_role_counts, sanitize_header_value(&role));
            }
            if let Some(label) = first_string(actor, &["label", "display", "name"]) {
                increment_count(&mut self.actor_label_counts, sanitize_header_value(&label));
            }
        }

        if let Some(thread_key) = first_string(record, &["threadKey", "thread_key"]) {
            self.thread_keys.insert(sanitize_header_value(&thread_key));
        }

        if let Some(occurred_at) = first_string(record, &["occurredAt", "occurred_at", "timestamp"])
            && let Ok(parsed) = DateTime::parse_from_rfc3339(&occurred_at)
        {
            let parsed = parsed.with_timezone(&Utc);
            self.time_start = Some(self.time_start.map_or(parsed, |current| current.min(parsed)));
            self.time_end = Some(self.time_end.map_or(parsed, |current| current.max(parsed)));
        }
    }

    fn render_header(&self) -> String {
        let mut parts = vec![
            format!("source_format={RECORD_JSONL_SOURCE_FORMAT}"),
            "sequence_kind=record_stream".to_string(),
            format!("unit_count={}", self.record_count),
            format!("text_unit_count={}", self.text_record_count),
            format!("attachment_count={}", self.attachment_count),
        ];
        if !self.unit_kind_counts.is_empty() {
            parts.push(format!("unit_kinds={}", render_count_map(&self.unit_kind_counts, 8)));
        }
        if !self.actor_role_counts.is_empty() {
            parts.push(format!("actor_roles={}", render_count_map(&self.actor_role_counts, 8)));
        }
        if !self.actor_label_counts.is_empty() {
            parts.push(format!("actor_labels={}", render_count_map(&self.actor_label_counts, 8)));
        }
        if !self.thread_keys.is_empty() {
            parts.push(format!("thread_count={}", self.thread_keys.len()));
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
            "attachmentCount": self.attachment_count,
            "unitKinds": self.unit_kind_counts,
            "actorRoles": self.actor_role_counts,
            "actorLabels": self.actor_label_counts,
            "threadCount": self.thread_keys.len(),
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

fn increment_count(counts: &mut BTreeMap<String, usize>, key: String) {
    if key.trim().is_empty() {
        return;
    }
    *counts.entry(key).or_default() += 1;
}

fn render_count_map(counts: &BTreeMap<String, usize>, limit: usize) -> String {
    counts
        .iter()
        .take(limit)
        .map(|(key, count)| format!("{key}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn render_record_unit(record: &Map<String, Value>, line_index: usize) -> Result<String> {
    let text = first_string(record, &["text", "payloadText", "payload_text", "content"])
        .or_else(|| record.get("payload").and_then(render_payload_value))
        .unwrap_or_default();
    let attachment_refs = attachment_refs(record.get("attachments"));
    let scalar_fields = render_generic_scalar_fields(record);
    if text.trim().is_empty() && attachment_refs.is_empty() && scalar_fields.is_empty() {
        return Err(anyhow!(
            "record jsonl line {} must contain text, payloadText, content, payload, attachments, or scalar fields",
            line_index + 1
        ));
    }

    let mut header_parts = Vec::<String>::new();
    if let Some(id) = first_string(record, &["id", "unitId", "unit_id"]) {
        header_parts.push(format!("unit_id={}", sanitize_header_value(&id)));
    }
    let kind =
        first_string(record, &["kind", "unitKind", "unit_kind"]).unwrap_or_else(|| "record".into());
    header_parts.push(format!("unit_kind={}", sanitize_header_value(&kind)));
    if let Some(occurred_at) = first_string(record, &["occurredAt", "occurred_at", "timestamp"]) {
        header_parts.push(format!("occurred_at={}", normalize_timestamp_header(&occurred_at)));
    }
    if let Some(actor) = record.get("actor").and_then(Value::as_object) {
        if let Some(role) = first_string(actor, &["role", "kind"]) {
            header_parts.push(format!("actor_role={}", sanitize_header_value(&role)));
        }
        if let Some(id) = first_string(actor, &["id", "ref"]) {
            header_parts.push(format!("actor_id={}", sanitize_header_value(&id)));
        }
        if let Some(label) = first_string(actor, &["label", "display", "name"]) {
            header_parts.push(format!("actor_label={}", sanitize_header_value(&label)));
        }
    }
    if let Some(thread_key) = first_string(record, &["threadKey", "thread_key"]) {
        header_parts.push(format!("thread_key={}", sanitize_header_value(&thread_key)));
    }
    if let Some(parent_unit_id) =
        first_string(record, &["parentUnitId", "parent_unit_id", "replyTo", "reply_to"])
    {
        header_parts.push(format!("parent_unit_id={}", sanitize_header_value(&parent_unit_id)));
    }
    if !attachment_refs.is_empty() {
        header_parts.push(format!("attachment_count={}", attachment_refs.len()));
        header_parts
            .push(format!("attachments={}", sanitize_header_value(&attachment_refs.join(","))));
    }

    let mut rendered = format!("[{}]", header_parts.join(" "));
    if !text.trim().is_empty() {
        rendered.push(' ');
        rendered.push_str(text.trim());
    }
    if !scalar_fields.is_empty() {
        rendered.push(' ');
        rendered.push_str("json_fields: ");
        rendered.push_str(&scalar_fields.join("; "));
    }
    Ok(rendered)
}

fn render_generic_scalar_fields(record: &Map<String, Value>) -> Vec<String> {
    let mut fields = Vec::<String>::new();
    let mut path = Vec::<String>::new();
    for (key, value) in record {
        if is_primary_record_text_key(key) {
            continue;
        }
        path.push(sanitize_field_path_segment(key));
        collect_scalar_fields(value, &mut path, &mut fields, 0);
        path.pop();
        if fields.len() >= RECORD_JSONL_FIELD_LIMIT {
            break;
        }
    }
    fields
}

fn collect_scalar_fields(
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
                value_to_string(value).and_then(|scalar| render_scalar_value(&scalar))
            {
                fields.push(format!("{}={rendered}", path.join(".")));
            }
        }
        Value::Array(items) => {
            let scalar_values = items
                .iter()
                .take(RECORD_JSONL_ARRAY_ITEM_LIMIT)
                .filter_map(value_to_string)
                .filter_map(|scalar| render_scalar_value(&scalar))
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
                collect_scalar_fields(item, path, fields, depth + 1);
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
                collect_scalar_fields(nested, path, fields, depth + 1);
                path.pop();
                if fields.len() >= RECORD_JSONL_FIELD_LIMIT {
                    break;
                }
            }
        }
        Value::Null => {}
    }
}

fn is_primary_record_text_key(key: &str) -> bool {
    matches!(key, "text" | "payloadText" | "payload_text" | "content")
}

fn render_scalar_value(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let mut rendered = normalized
        .chars()
        .take(RECORD_JSONL_FIELD_VALUE_LIMIT)
        .collect::<String>()
        .replace(';', ",");
    if normalized.chars().count() > RECORD_JSONL_FIELD_VALUE_LIMIT {
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

fn first_string(record: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| record.get(*key))
        .find_map(value_to_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn render_payload_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items.iter().filter_map(value_to_string).collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(_) => Some(value.to_string()),
        _ => value_to_string(value),
    }
}

fn attachment_refs(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            Value::String(text) => Some(text.clone()),
            Value::Object(object) => render_attachment_object(object)
                .or_else(|| first_string(object, &["id", "uri", "sourceUri", "source_uri"]))
                .or_else(|| first_string(object, &["type", "mimeType", "mime_type"]))
                .map(|value| format!("{}:{value}", index + 1)),
            _ => None,
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn render_attachment_object(object: &Map<String, Value>) -> Option<String> {
    let fields = [
        ("id", first_string(object, &["id"])),
        ("uri", first_string(object, &["uri", "sourceUri", "source_uri"])),
        ("type", first_string(object, &["type"])),
        ("mime_type", first_string(object, &["mimeType", "mime_type"])),
        ("file_name", first_string(object, &["fileName", "file_name", "name"])),
        ("label", first_string(object, &["label", "title"])),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        value.map(|value| format!("{key}={}", sanitize_attachment_value(&value)))
    })
    .collect::<Vec<_>>();
    (!fields.is_empty()).then(|| fields.join("|"))
}

fn normalize_timestamp_header(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc).to_rfc3339())
        .unwrap_or_else(|_| sanitize_header_value(value))
}

/// Scans rendered chunk text for `occurred_at=ISO` substrings emitted by
/// `render_record_unit` (line 251) and returns the (min, max) inclusive
/// temporal bounds of the records the chunk aggregates.
///
/// Returns `None` for chunks that never appeared in a `record_jsonl`
/// extraction — they have no canonical temporal interpretation. Single-record
/// chunks return `Some((ts, ts))`. Chunks combining records across a month
/// boundary return the wider span so retrieval-side overlap filters
/// (`occurred_at < @t_end AND occurred_until >= @t_start`) work correctly.
///
/// This is the canonical temporal extractor: shared by the ingest write path
/// (so new chunks are temporally indexed) and the runtime backfill binary
/// (so legacy chunks become indexed without re-ingest). No second
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

fn sanitize_header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join("_")
}

fn sanitize_attachment_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join("_").replace('|', "/").replace(',', ";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_generic_record_jsonl_units() {
        let payload = br#"{"id":"msg-1","kind":"message","occurredAt":"2026-04-28T09:00:00Z","actor":{"role":"user","id":"u1","label":"User One"},"threadKey":"root","text":"How do I configure search?","attachments":[{"id":"file-1","type":"image/png"}]}
{"id":"msg-2","kind":"message","actor":{"role":"assistant"},"text":"Use the retrieval settings."}"#;

        let output = extract_record_jsonl(payload).expect("jsonl extraction");

        assert_eq!(output.extraction_kind, RECORD_JSONL_SOURCE_FORMAT);
        assert_eq!(output.source_metadata.source_format, RECORD_JSONL_SOURCE_FORMAT);
        assert_eq!(output.structure_hints.lines.len(), 3);
        assert!(output.content_text.starts_with("[source_profile source_format=record_jsonl"));
        assert!(output.content_text.contains("unit_count=2"));
        assert!(output.content_text.contains("unit_kinds=message:2"));
        assert!(output.content_text.contains("actor_roles=assistant:1,user:1"));
        assert!(output.content_text.contains("unit_id=msg-1"));
        assert!(output.content_text.contains("actor_role=user"));
        assert!(output.content_text.contains("attachment_count=1"));
        assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
        assert_eq!(output.source_map["sourceProfile"]["unitCount"], serde_json::json!(2));
        assert_eq!(
            output.source_map["sourceProfile"]["actorRoles"]["assistant"],
            serde_json::json!(1)
        );
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
                .all(|line| !line.signals.contains(&ExtractionLineSignal::MetadataCandidate))
        );
    }

    #[test]
    fn renders_generic_scalar_fields_for_detail_retrieval() {
        let output = extract_record_jsonl(
            br#"{"id":"row-1","occurredAt":"2026-03-14T12:00:00Z","status":"approved","amount":42,"details":{"provider":"Provider Beta","retry":false},"tags":["alpha","receipt"]}"#,
        )
        .expect("jsonl extraction");

        assert!(output.content_text.contains("occurred_at=2026-03-14T12:00:00+00:00"));
        assert!(output.content_text.contains("json_fields:"));
        assert!(output.content_text.contains("status=approved"));
        assert!(output.content_text.contains("amount=42"));
        assert!(output.content_text.contains("details.provider=Provider Beta"));
        assert!(output.content_text.contains("details.retry=false"));
        assert!(output.content_text.contains("tags=alpha, receipt"));
    }

    #[test]
    fn rejects_record_without_searchable_content() {
        let error = extract_record_jsonl(br#"{}"#).unwrap_err();
        assert!(error.to_string().contains("must contain text"));
    }

    #[test]
    fn extract_chunk_temporal_bounds_returns_none_for_chunks_without_header() {
        assert!(extract_chunk_temporal_bounds("plain text without any temporal header").is_none());
        assert!(extract_chunk_temporal_bounds("").is_none());
    }

    #[test]
    fn extract_chunk_temporal_bounds_handles_single_record_chunk() {
        let text = "[unit_id=msg-1 unit_kind=message occurred_at=2024-09-06T12:19:38+00:00 \
                    actor_role=user] hello world";
        let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
        assert_eq!(min, max);
        assert_eq!(min.to_rfc3339(), "2024-09-06T12:19:38+00:00");
    }

    #[test]
    fn extract_chunk_temporal_bounds_aggregates_min_and_max_across_records() {
        let text = "\
            [unit_id=msg-1 occurred_at=2024-09-01T09:37:18+00:00 actor_role=user] one\n\
            [unit_id=msg-2 occurred_at=2026-04-28T10:52:10+00:00 actor_role=user] two\n\
            [unit_id=msg-3 occurred_at=2025-03-15T18:00:00+00:00 actor_role=user] three";
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
    fn extract_chunk_temporal_bounds_round_trips_render_record_unit_output() {
        // Verifies the helper consumes exactly the format render_record_unit
        // produces — keeping the read/write halves locked together.
        let payload = br#"{"id":"msg-x","kind":"message","occurredAt":"2026-03-15T12:00:00Z","actor":{"role":"user","label":"u"},"text":"hello"}"#;
        let output = extract_record_jsonl(payload).expect("jsonl extraction");
        let (min, max) =
            extract_chunk_temporal_bounds(&output.content_text).expect("bounds present");
        assert_eq!(min, max);
        assert_eq!(min.to_rfc3339(), "2026-03-15T12:00:00+00:00");
    }
}
