use super::*;

/// Shared renderer over the common record stream. Every wire format funnels
/// here so JSON/YAML/JSONL all get identical field-aware rendering, the
/// cross-record schema profile, and temporal bounds. No second renderer.
pub(super) fn render_record_stream(
    records: &[Map<String, Value>],
    comment_context: Option<(&str, ConfigSyntax, &[config_comments::ConfigCommentRun])>,
) -> Result<ExtractionOutput> {
    let mut rendered_records = Vec::<RenderedRecord>::with_capacity(records.len());
    let mut profile = RecordSourceProfile::default();
    let fields_by_record = records.iter().map(render_generic_fields).collect::<Vec<_>>();
    let document_header_text = comment_context
        .and_then(|(raw_text, syntax, runs)| extract_document_header_text(raw_text, syntax, runs));
    let comments_by_field = comment_context.map_or_else(
        || fields_by_record.iter().map(|fields| vec![Vec::new(); fields.len()]).collect(),
        |(raw_text, syntax, runs)| anchor_comment_runs(raw_text, syntax, runs, &fields_by_record),
    );

    for (record_index, ((object, fields), field_comments)) in
        records.iter().zip(fields_by_record.iter()).zip(comments_by_field.iter()).enumerate()
    {
        let rendered =
            render_record_unit_from_fields(object, record_index, fields, field_comments)?;
        profile.observe(object);
        rendered_records.push(RenderedRecord { source_ordinal: record_index, text: rendered });
    }

    if rendered_records.is_empty() {
        return Err(anyhow!("record stream payload contains no records"));
    }

    let mut rendered_lines = Vec::<String>::with_capacity(
        rendered_records.len() + 1 + usize::from(document_header_text.is_some()),
    );
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
    if let Some(document_header_text) = document_header_text {
        push_rendered_line(
            &mut rendered_lines,
            &mut hints,
            &mut offset,
            document_header_text,
            Vec::new(),
            Vec::new(),
        );
    }
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
/// anywhere in the record (so the shared temporal extractor can aggregate
/// it), and every leaf value flattened to `dotted.path=value` so any deep
/// identifier/path/name stays searchable text. Free-text leaves render with a
/// generous cap so message bodies are not truncated to a config-value width.
#[cfg(test)]
pub(super) fn render_record_unit(
    record: &Map<String, Value>,
    record_index: usize,
) -> Result<String> {
    let fields = render_generic_fields(record);
    let comments = vec![Vec::new(); fields.len()];
    render_record_unit_from_fields(record, record_index, &fields, &comments)
}

fn render_record_unit_from_fields(
    record: &Map<String, Value>,
    record_index: usize,
    fields: &[String],
    comments_by_field: &[Vec<String>],
) -> Result<String> {
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
    let annotated_fields = fields
        .iter()
        .enumerate()
        .map(|(field_index, field)| {
            let Some(comments) = comments_by_field.get(field_index) else {
                return field.clone();
            };
            if comments.is_empty() {
                return field.clone();
            }
            format!("{field}\n{}", comments.join("\n"))
        })
        .collect::<Vec<_>>();
    rendered.push_str(&join_record_fields(&annotated_fields));
    Ok(rendered)
}

pub(super) fn join_record_fields(fields: &[String]) -> String {
    let separator = if fields.iter().any(|field| field.contains('\n')) { "\n;\n" } else { "; " };
    fields.join(separator)
}

pub(super) fn split_record_fields(fields_body: &str) -> Vec<&str> {
    let separator = if fields_body.contains("\n;\n") { "\n;\n" } else { "; " };
    fields_body.split(separator).filter(|field| !field.trim().is_empty()).collect()
}

/// Detects a record-level timestamp by VALUE SHAPE, not by key name: the
/// earliest value-shaped timestamp found anywhere in the record (any depth,
/// under any key) is used as the record's normalized `occurred_at`. Returns
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
pub(super) fn parse_value_timestamp_string(value: &str) -> Option<DateTime<Utc>> {
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
    }
    fields
}

fn collect_leaf_fields(
    value: &Value,
    path: &mut Vec<String>,
    fields: &mut Vec<String>,
    depth: usize,
) {
    if path.is_empty() {
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

pub(super) fn sanitize_field_path_segment(value: &str) -> String {
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
