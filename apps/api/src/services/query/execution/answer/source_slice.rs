use super::{
    BTreeSet, HashMap, HashSet, ParsedSourceUnitText, QueryTargetKind, ReleaseSourceIdentity,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, Uuid, compare_version_desc, excerpt_for,
    extract_release_context_version, extract_semver_like_version, indent_source_unit_body,
    is_structured_source_unit_runtime_chunk, parse_source_unit_text,
    query_requests_latest_versions, requested_latest_version_count,
};
use crate::domains::query_ir::SourceSliceFilter;
use crate::shared::extraction::table_summary::{
    TableSummaryValueKind, format_numeric_value, parse_table_column_summary,
};

const SOURCE_SLICE_COMPACT_BODY_CHARS: usize = 720;
const SOURCE_SLICE_COMPACT_BODY_LINES: usize = 8;
const INTERNAL_TABLE_SUMMARY_PREFIX: &str = "Table Summary | ";

pub(crate) struct OrderedSourceSliceAnswer {
    pub(crate) answer: String,
    pub(crate) unit_count: usize,
}

pub(crate) fn build_ordered_source_slice_answer(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
    context_chunks: &[RuntimeMatchedChunk],
) -> Option<OrderedSourceSliceAnswer> {
    if !query_requests_record_inventory(query_ir) {
        return None;
    }
    if !source_units_align_with_top_ranked_document(source_units, context_chunks) {
        tracing::info!(
            stage = "answer.source_slice_rank_guard",
            source_unit_count = source_units.len(),
            top_ranked_document_id = ?context_chunks.first().map(|chunk| chunk.document_id),
            "ordered source-slice answer rejected: primary source units are absent or not aligned with the top-ranked document"
        );
        return None;
    }
    let mut units = source_slice_answer_units(query_ir, source_units);
    if !retain_record_inventory_units(&mut units) {
        return None;
    }
    dedupe_source_slice_answer_units_by_body(query_ir, &mut units);
    units.retain(|unit| ordered_source_unit_has_display_material(query_ir, unit));
    let answer = build_ordered_source_units_answer(query_ir, &units)?;
    Some(OrderedSourceSliceAnswer { answer, unit_count: units.len() })
}

fn source_units_align_with_top_ranked_document(
    source_units: &[RuntimeMatchedChunk],
    context_chunks: &[RuntimeMatchedChunk],
) -> bool {
    let Some(top_ranked_document_id) = context_chunks.first().map(|chunk| chunk.document_id) else {
        return false;
    };
    !source_units.is_empty()
        && source_units.iter().all(|unit| unit.document_id == top_ranked_document_id)
}

fn retain_record_inventory_units(units: &mut Vec<RuntimeMatchedChunk>) -> bool {
    let matching_count = units.iter().filter(|unit| source_unit_has_record_shape(unit)).count();
    let mismatching_count = units.len().saturating_sub(matching_count);
    if matching_count == 0 || mismatching_count > matching_count {
        tracing::info!(
            stage = "answer.source_slice_shape_guard",
            selected_unit_count = units.len(),
            matching_unit_count = matching_count,
            "ordered source-slice answer rejected: selected units do not have record shape"
        );
        return false;
    }
    units.retain(source_unit_has_record_shape);
    true
}

fn query_requests_record_inventory(query_ir: &crate::domains::query_ir::QueryIR) -> bool {
    query_ir.source_slice.is_some() && !query_requests_release_inventory(query_ir)
}

fn query_requests_release_inventory(query_ir: &crate::domains::query_ir::QueryIR) -> bool {
    query_ir.source_slice.as_ref().is_some_and(|source_slice| {
        matches!(source_slice.filter, SourceSliceFilter::ReleaseMarker)
            || query_ir.targets_any(&[
                QueryTargetKind::Changelog,
                QueryTargetKind::Release,
                QueryTargetKind::Version,
            ])
    })
}

fn source_unit_has_record_shape(unit: &RuntimeMatchedChunk) -> bool {
    parsed_source_unit_has_record_shape(&parse_source_unit_text(&unit.source_text))
}

fn parsed_source_unit_has_record_shape(parsed: &ParsedSourceUnitText) -> bool {
    parsed.field("unit_id").is_some()
        || parsed.field("unit_ordinal").is_some()
        || parsed.field("occurred_at").is_some()
        || parsed.field("actor_id").is_some()
        || parsed.field("actor_label").is_some()
        || parsed.field("actor_role").is_some()
}

fn source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    let latest_version_inventory = query_requests_latest_versions(query_ir);
    if query_ir.source_slice.is_none() && !latest_version_inventory {
        return Vec::new();
    }
    if !source_units.is_empty() && !latest_version_inventory {
        let mut units = source_units.to_vec();
        sort_source_slice_answer_units(query_ir, &mut units);
        let requested_count = super::source_slice_requested_count(query_ir).unwrap_or(units.len());
        if requested_count > 0 && units.len() > requested_count {
            units.truncate(requested_count);
        }
        return units;
    }
    if !latest_version_inventory {
        return Vec::new();
    }

    let requested_count = latest_source_slice_requested_count(query_ir);
    let mut units = source_units
        .iter()
        .filter(|unit| source_slice_answer_unit_evidence(query_ir, unit).is_some())
        .cloned()
        .collect::<Vec<_>>();
    sort_source_slice_answer_units(query_ir, &mut units);

    dedupe_latest_source_slice_answer_units(query_ir, &mut units);
    if requested_count > 0 && units.len() > requested_count {
        units.truncate(requested_count);
    }
    units
}

fn sort_source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    units: &mut [RuntimeMatchedChunk],
) {
    if query_requests_latest_versions(query_ir) {
        units.sort_by(|left, right| latest_source_slice_answer_unit_order(query_ir, left, right));
    } else {
        units
            .sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id));
    }
}

fn latest_source_slice_answer_unit_order(
    query_ir: &crate::domains::query_ir::QueryIR,
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    let left_is_attested = latest_source_slice_answer_unit_is_attested(left);
    let right_is_attested = latest_source_slice_answer_unit_is_attested(right);
    match (left_is_attested, right_is_attested) {
        (true, false) => return std::cmp::Ordering::Less,
        (false, true) => return std::cmp::Ordering::Greater,
        (true, true) | (false, false) => {}
    }

    match (
        source_slice_answer_unit_version(query_ir, left),
        source_slice_answer_unit_version(query_ir, right),
    ) {
        (Some(left_version), Some(right_version)) => {
            let version_order = compare_version_desc(&left_version, &right_version);
            if !version_order.is_eq() {
                return version_order;
            }
        }
        (Some(_), None) => return std::cmp::Ordering::Less,
        (None, Some(_)) => return std::cmp::Ordering::Greater,
        (None, None) => {}
    }

    if left_is_attested && right_is_attested {
        let score_order = score_value(right.score).total_cmp(&score_value(left.score));
        if !score_order.is_eq() {
            return score_order;
        }
    }

    score_value(right.score)
        .total_cmp(&score_value(left.score))
        .then_with(|| left.chunk_index.cmp(&right.chunk_index))
        .then_with(|| left.document_label.cmp(&right.document_label))
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
}

fn latest_source_slice_answer_unit_version(chunk: &RuntimeMatchedChunk) -> Option<Vec<u32>> {
    extract_release_context_version(&chunk.document_label)
}

fn source_slice_answer_unit_version(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<Vec<u32>> {
    source_slice_answer_unit_evidence(query_ir, chunk).map(|evidence| evidence.version)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LatestSourceSliceEvidence {
    version: Vec<u32>,
}

fn source_slice_answer_unit_evidence(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<LatestSourceSliceEvidence> {
    if query_requests_latest_versions(query_ir) {
        return explicit_latest_source_slice_answer_unit_version(chunk)
            .map(|version| LatestSourceSliceEvidence { version });
    }
    latest_source_slice_answer_unit_version(chunk)
        .map(|version| LatestSourceSliceEvidence { version })
}

fn explicit_latest_source_slice_answer_unit_version(
    chunk: &RuntimeMatchedChunk,
) -> Option<Vec<u32>> {
    latest_source_slice_answer_unit_version(chunk).or_else(|| {
        if !latest_source_slice_answer_unit_is_attested(chunk) {
            return None;
        }
        let source = parse_source_unit_text(&chunk.source_text);
        let excerpt = parse_source_unit_text(&chunk.excerpt);
        source
            .field("version")
            .and_then(extract_semver_like_version)
            .or_else(|| excerpt.field("version").and_then(extract_semver_like_version))
            .or_else(|| extract_release_context_version(&chunk.source_text))
            .or_else(|| extract_release_context_version(&chunk.excerpt))
    })
}

fn latest_source_slice_answer_unit_is_attested(chunk: &RuntimeMatchedChunk) -> bool {
    matches!(chunk.score_kind, RuntimeChunkScoreKind::LatestVersion)
        || (matches!(chunk.score_kind, RuntimeChunkScoreKind::SourceContext)
            && is_structured_source_unit_runtime_chunk(chunk))
}

fn dedupe_latest_source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    units: &mut Vec<RuntimeMatchedChunk>,
) {
    if query_requests_latest_versions(query_ir) {
        let mut seen_versions = HashSet::<(ReleaseSourceIdentity, Vec<u32>)>::new();
        let mut seen_revisions = HashSet::<Uuid>::new();
        units.retain(|unit| {
            if let Some(evidence) = source_slice_answer_unit_evidence(query_ir, unit) {
                let source = ReleaseSourceIdentity::new(unit.document_id, unit.revision_id);
                return seen_versions.insert((source, evidence.version));
            }
            seen_revisions.insert(unit.revision_id)
        });
        return;
    }

    let mut seen_revisions = HashSet::<Uuid>::new();
    units.retain(|unit| seen_revisions.insert(unit.revision_id));
}

fn dedupe_source_slice_answer_units_by_body(
    query_ir: &crate::domains::query_ir::QueryIR,
    units: &mut Vec<RuntimeMatchedChunk>,
) {
    let release_inventory = query_requests_release_inventory(query_ir);
    let mut positions = HashMap::<(String, Option<Vec<u32>>), usize>::new();
    let mut deduplicated = Vec::<RuntimeMatchedChunk>::with_capacity(units.len());
    for mut unit in units.drain(..) {
        let Some(body_identity) = normalized_source_unit_body_identity(&unit) else {
            deduplicated.push(unit);
            continue;
        };
        let release_identity = release_inventory
            .then(|| extract_release_context_version(&unit.document_label))
            .flatten();
        let identity = (body_identity, release_identity);
        if let Some(position) = positions.get(&identity).copied() {
            let existing = &mut deduplicated[position];
            if unit.document_label.trim().chars().count()
                > existing.document_label.trim().chars().count()
            {
                existing.document_label = unit.document_label.trim().to_string();
            }
            continue;
        }
        positions.insert(identity, deduplicated.len());
        unit.document_label = unit.document_label.trim().to_string();
        deduplicated.push(unit);
    }
    *units = deduplicated;
}

fn normalized_source_unit_body_identity(unit: &RuntimeMatchedChunk) -> Option<String> {
    let parsed = parse_source_unit_text(&unit.source_text);
    let normalized = parsed.body.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

pub(super) fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(f32::NEG_INFINITY)
}

pub(super) fn build_ordered_source_units_answer(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
) -> Option<String> {
    let latest_version_inventory = query_requests_latest_versions(query_ir);
    if query_ir.source_slice.is_none() && !latest_version_inventory {
        return None;
    }
    if source_units.is_empty() {
        return None;
    }

    let mut units = source_units.to_vec();
    sort_source_slice_answer_units(query_ir, &mut units);
    dedupe_source_slice_answer_units_by_body(query_ir, &mut units);
    units.retain(|unit| ordered_source_unit_has_display_material(query_ir, unit));
    if units.is_empty() {
        return None;
    }
    let requested_count = if query_requests_latest_versions(query_ir) {
        latest_source_slice_requested_count(query_ir)
    } else {
        super::source_slice_requested_count(query_ir).unwrap_or(units.len())
    };
    if requested_count > 0 && units.len() > requested_count {
        units.truncate(requested_count);
    }
    let document_labels = units
        .iter()
        .map(|unit| unit.document_label.trim())
        .filter(|label| !label.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    let source_identities = units
        .iter()
        .map(|unit| ReleaseSourceIdentity::new(unit.document_id, unit.revision_id))
        .collect::<HashSet<_>>();
    let single_source = source_identities.len() == 1;
    let include_document_label = !single_source;
    let mut lines = vec![ordered_source_units_summary(
        &units,
        requested_count,
        single_source,
        &document_labels,
    )];
    lines.push(String::new());
    for (index, unit) in units.iter().enumerate() {
        lines.extend(render_ordered_source_unit(
            index,
            unit,
            latest_version_inventory,
            include_document_label,
        ));
    }
    Some(lines.join("\n"))
}

fn ordered_source_units_summary(
    units: &[RuntimeMatchedChunk],
    requested_count: usize,
    single_source: bool,
    document_labels: &BTreeSet<&str>,
) -> String {
    if !single_source {
        return format!("{}/{} source units returned.", units.len(), requested_count);
    }
    let label = document_labels.iter().next().copied().unwrap_or("source");
    format!("`{label}` - {}/{} source units returned.", units.len(), requested_count)
}

fn render_ordered_source_unit(
    index: usize,
    unit: &RuntimeMatchedChunk,
    latest_version_inventory: bool,
    include_document_label: bool,
) -> Vec<String> {
    let parsed = parse_source_unit_text(&unit.source_text);
    let mut heading_parts = Vec::new();
    if include_document_label {
        heading_parts.push(format!("source=`{}`", unit.document_label.trim()));
    }
    if let Some(heading) =
        latest_inventory_source_unit_heading(latest_version_inventory, include_document_label, unit)
    {
        heading_parts.push(format!("**{heading}**"));
    }
    if let Some(timestamp) = parsed.field("occurred_at") {
        heading_parts.push(format!("**{timestamp}**"));
    }
    if let Some(actor) = parsed
        .field("actor_label")
        .or_else(|| parsed.field("actor_id"))
        .or_else(|| parsed.field("actor_role"))
    {
        heading_parts.push(format!("`{actor}`"));
    } else if let Some(unit_id) = parsed.field("unit_id") {
        heading_parts.push(format!("`unit_id={unit_id}`"));
    }
    if heading_parts.is_empty() {
        heading_parts.push(format!("`ordinal={}`", unit.chunk_index));
    }
    let mut lines = vec![format!("{}. {}", index + 1, heading_parts.join(" - "))];
    if let Some(body) = source_slice_unit_body_for_answer(latest_version_inventory, &parsed)
        && !body.is_empty()
    {
        lines.push(indent_source_unit_body(&body));
    }
    lines
}

fn latest_inventory_source_unit_heading(
    latest_version_inventory: bool,
    include_document_label: bool,
    unit: &RuntimeMatchedChunk,
) -> Option<String> {
    if !latest_version_inventory || include_document_label {
        return None;
    }
    if let Some(label) = compact_source_slice_inventory_line(&unit.document_label)
        && extract_semver_like_version(&label).is_some()
    {
        return Some(excerpt_for(&label, 160));
    }
    explicit_latest_source_slice_answer_unit_version(unit).map(|version| {
        format!("Version {}", version.iter().map(u32::to_string).collect::<Vec<_>>().join("."))
    })
}

fn source_slice_unit_body_for_answer(
    latest_version_inventory: bool,
    parsed: &ParsedSourceUnitText,
) -> Option<String> {
    let body = parsed.body.trim();
    if is_internal_table_summary_serialization(body) {
        return human_table_summary_body(body);
    }
    Some(if latest_version_inventory {
        compact_source_slice_inventory_body(&source_slice_inventory_body_source(parsed))
    } else {
        body.trim().to_string()
    })
}

fn ordered_source_unit_has_display_material(
    query_ir: &crate::domains::query_ir::QueryIR,
    unit: &RuntimeMatchedChunk,
) -> bool {
    source_slice_unit_body_for_answer(
        query_requests_latest_versions(query_ir),
        &parse_source_unit_text(&unit.source_text),
    )
    .is_some()
}

fn is_internal_table_summary_serialization(body: &str) -> bool {
    body.trim_start().starts_with(INTERNAL_TABLE_SUMMARY_PREFIX)
}

fn human_table_summary_body(body: &str) -> Option<String> {
    let summary = parse_table_column_summary(body)?;
    let mut segments = Vec::<String>::new();
    if let Some(sheet_name) = summary.sheet_name {
        segments.push(format!("Sheet: {sheet_name}"));
    }
    if let Some(table_name) = summary.table_name {
        segments.push(format!("Table: {table_name}"));
    }
    segments.push(format!("Column: {}", summary.column_name));
    match summary.value_kind {
        TableSummaryValueKind::Numeric => {
            if let Some(average) = summary.average {
                segments.push(format!("Average: {}", format_numeric_value(average)));
            }
            if let Some(minimum) = summary.min {
                segments.push(format!("Min: {}", format_numeric_value(minimum)));
            }
            if let Some(maximum) = summary.max {
                segments.push(format!("Max: {}", format_numeric_value(maximum)));
            }
        }
        TableSummaryValueKind::Categorical if !summary.most_frequent_values.is_empty() => {
            segments
                .push(format!("Most Frequent Values: {}", summary.most_frequent_values.join("; ")));
        }
        TableSummaryValueKind::Categorical => {}
    }
    (!segments.is_empty()).then(|| segments.join(" | "))
}

fn source_slice_inventory_body_source(parsed: &ParsedSourceUnitText) -> String {
    let body = parsed.body.trim();
    let include_header = parsed
        .header
        .as_deref()
        .is_some_and(|header| extract_semver_like_version(header).is_some() || body.is_empty());
    let mut lines = Vec::<String>::new();
    if include_header
        && let Some(header) =
            parsed.header.as_deref().map(str::trim).filter(|header| !header.is_empty())
    {
        lines.push(format!("[{header}]"));
    }
    if !body.is_empty() {
        lines.push(body.to_string());
    }
    if lines.is_empty() { body.to_string() } else { lines.join("\n") }
}

fn compact_source_slice_inventory_body(body: &str) -> String {
    let mut lines = Vec::<String>::new();
    let mut used_chars = 0usize;
    for line in
        body.lines().filter_map(compact_source_slice_inventory_line).filter(|line| !line.is_empty())
    {
        let line_chars = line.chars().count();
        if lines.is_empty() && line_chars > SOURCE_SLICE_COMPACT_BODY_CHARS {
            lines.push(excerpt_for(&line, SOURCE_SLICE_COMPACT_BODY_CHARS));
            break;
        }
        let projected = used_chars.saturating_add(line_chars).saturating_add(1);
        if (!lines.is_empty() && projected > SOURCE_SLICE_COMPACT_BODY_CHARS)
            || lines.len() >= SOURCE_SLICE_COMPACT_BODY_LINES
        {
            break;
        }
        used_chars = projected;
        lines.push(line);
    }
    if lines.is_empty() {
        excerpt_for(body, SOURCE_SLICE_COMPACT_BODY_CHARS)
    } else {
        lines.join("\n")
    }
}

fn latest_source_slice_requested_count(query_ir: &crate::domains::query_ir::QueryIR) -> usize {
    super::source_slice_requested_count(query_ir)
        .unwrap_or_else(|| requested_latest_version_count(query_ir))
}

fn compact_source_slice_inventory_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty()
        || is_markdown_image_line(trimmed)
        || is_markdown_link_only_line(trimmed)
        || is_markdown_horizontal_rule(trimmed)
    {
        return None;
    }
    let without_heading = trimmed.trim_start_matches('#').trim_start();
    if without_heading.is_empty() { None } else { Some(without_heading.to_string()) }
}

fn is_markdown_image_line(line: &str) -> bool {
    line.starts_with("![") && line.contains("](") && line.ends_with(')')
}

fn is_markdown_link_only_line(line: &str) -> bool {
    line.starts_with('[') && line.contains("](") && line.ends_with(')')
}

fn is_markdown_horizontal_rule(line: &str) -> bool {
    let marker_count = line.chars().filter(|ch| matches!(ch, '-' | '_' | '*')).count();
    marker_count >= 3 && marker_count == line.chars().count()
}
