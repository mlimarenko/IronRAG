use super::rendering::render_record_stream;
use super::*;

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
/// through the single shared record renderer.
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
    let comment_context = match hint {
        StructuredRecordHint::Yaml => Some((ConfigSyntax::Yaml, extract_comment_runs(raw_text))),
        StructuredRecordHint::Toml => Some((ConfigSyntax::Toml, extract_comment_runs(raw_text))),
        StructuredRecordHint::Jsonl | StructuredRecordHint::Json => None,
    };
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
    render_record_stream(
        &records,
        comment_context.as_ref().map(|(syntax, runs)| (raw_text, *syntax, runs.as_slice())),
    )
    .ok()
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
    render_record_stream(&records, None)
}
