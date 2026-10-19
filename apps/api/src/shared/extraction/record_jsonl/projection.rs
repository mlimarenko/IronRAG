use super::rendering::{parse_value_timestamp_string, split_record_fields};
use super::*;

/// Scans rendered chunk text for `occurred_at=ISO` substrings emitted by
/// `render_record_unit` and returns the (min, max) inclusive temporal bounds of
/// the records the chunk aggregates. The `occurred_at=` token is derived from
/// value-shaped timestamp detection (any key, any depth), not an input key
/// name, so it is fully format- and schema-agnostic.
///
/// Returns `None` for chunks that never appeared in a `record_jsonl`
/// extraction — they have no defined temporal interpretation. Single-record
/// chunks return `Some((ts, ts))`. Chunks combining records across a month
/// boundary return the wider span so retrieval-side overlap filters
/// (`occurred_at < @t_end AND occurred_until >= @t_start`) work correctly.
///
/// This temporal extractor is shared by the ingest write path
/// (so new chunks are temporally indexed) and the runtime backfill binary
/// (so older chunks become indexed without re-ingest). No second
/// implementation may parse this header — keep this fn as the only authority.
#[must_use]
pub fn extract_chunk_temporal_bounds(text: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    const HEADER_KEY: &str = "occurred_at=";
    let mut min_ts: Option<DateTime<Utc>> = None;
    let mut max_ts: Option<DateTime<Utc>> = None;

    for line in text.lines().filter(|line| !line.trim_start().starts_with('#')) {
        for fragment in line.split(HEADER_KEY).skip(1) {
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
/// normalized `occurred_at`/`occurred_until` temporal bounds, so it must not also
/// become an entity node).
///
/// Fully format- and schema-agnostic: it keys off the structural shape
/// guaranteed by `render_record_unit` — the literal `fields: ` boundary, the
/// field separator (`; ` normally, or a standalone `;` line when annotations
/// are present), and the first `=` per field. Values never contain a raw `;`
/// because `render_leaf_value` rewrites it to `,`; path segments are sanitized
/// to alnum/`_`/`-`, so a key never contains `=`, and a value's own `=`, e.g. in
/// a URL, survives. No field name is ever special-cased.
///
/// Returns `None` when the projection has no non-timestamp value left (e.g. a
/// record whose only leaves were timestamps), so the caller skips an empty graph
/// chunk rather than feeding bare scaffolding.
#[must_use]
pub fn project_record_unit_values_for_graph(text: &str) -> Option<String> {
    const FIELDS_MARKER: &str = "fields: ";
    let mut value_lines = Vec::<String>::new();

    if text.contains("\n;\n") {
        let (_, body) = text.split_once(FIELDS_MARKER)?;
        let values = project_record_field_values(body);
        return (!values.is_empty()).then(|| values.join("; "));
    }

    for line in text.lines() {
        let Some(body) = line.split_once(FIELDS_MARKER).map(|(_, body)| body) else {
            continue;
        };
        let values = project_record_field_values(body);
        if !values.is_empty() {
            value_lines.push(values.join("; "));
        }
    }

    (!value_lines.is_empty()).then(|| value_lines.join("\n"))
}

fn project_record_field_values(fields_body: &str) -> Vec<String> {
    split_record_fields(fields_body)
        .into_iter()
        .filter_map(|field| field.lines().next())
        .filter_map(|field| field.split_once('=').map(|(_, value)| value.trim()))
        .filter(|value| !value.is_empty())
        .filter(|value| parse_value_timestamp_string(value).is_none())
        .map(str::to_string)
        .collect()
}
