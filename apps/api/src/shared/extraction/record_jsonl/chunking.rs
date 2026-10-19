use super::rendering::{join_record_fields, split_record_fields};
use super::*;

/// Splits over-budget rendered record source units into multiple source-unit
/// blocks while keeping the record grammar owned by this module.
///
/// Chunking treats every `SourceUnit` as an atomic chunk. Wide structured
/// records therefore need to spill into multiple source units before chunk
/// windows are built; otherwise a single vector is forced to represent a large
/// heterogeneous object and late fields become harder to retrieve.
#[must_use]
pub fn split_large_record_units(
    blocks: &[StructuredBlockData],
    max_chars: usize,
) -> Vec<StructuredBlockData> {
    let max_chars = max_chars.max(1);
    let mut result = Vec::<StructuredBlockData>::with_capacity(blocks.len());
    for block in blocks {
        if block.block_kind != StructuredBlockKind::SourceUnit
            || block.normalized_text.chars().count() <= max_chars
        {
            result.push(block.clone());
            continue;
        }
        let segments = split_record_unit_segments(&block.text, &block.normalized_text, max_chars);
        if segments.len() < 2 {
            result.push(block.clone());
            continue;
        }
        for segment in segments {
            if segment.text.trim().is_empty() && segment.normalized_text.trim().is_empty() {
                continue;
            }
            result.push(StructuredBlockData {
                block_id: uuid::Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::SourceUnit,
                text: segment.text,
                normalized_text: segment.normalized_text,
                heading_trail: block.heading_trail.clone(),
                section_path: block.section_path.clone(),
                page_number: block.page_number,
                source_span: None,
                parent_block_id: block.parent_block_id,
                table_coordinates: None,
                code_language: None,
                is_boilerplate: block.is_boilerplate,
            });
        }
    }
    for (index, block) in result.iter_mut().enumerate() {
        block.ordinal = i32::try_from(index).unwrap_or(i32::MAX);
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordUnitSegment {
    text: String,
    normalized_text: String,
}

fn split_record_unit_segments(
    text: &str,
    normalized_text: &str,
    max_chars: usize,
) -> Vec<RecordUnitSegment> {
    const FIELDS_MARKER: &str = "fields: ";
    let Some((header, fields_body)) = text.split_once(FIELDS_MARKER) else {
        return vec![RecordUnitSegment {
            text: text.to_string(),
            normalized_text: normalized_text.to_string(),
        }];
    };
    let Some((normalized_header, normalized_fields_body)) =
        normalized_text.split_once(FIELDS_MARKER)
    else {
        return vec![RecordUnitSegment {
            text: text.to_string(),
            normalized_text: normalized_text.to_string(),
        }];
    };

    let fields = split_record_fields(fields_body);
    let normalized_fields = split_record_fields(normalized_fields_body);
    if fields.len() < 2 || normalized_fields.len() != fields.len() {
        return vec![RecordUnitSegment {
            text: text.to_string(),
            normalized_text: normalized_text.to_string(),
        }];
    }

    let mut segments = Vec::<RecordUnitSegment>::new();
    let mut current_fields = Vec::<&str>::new();
    let mut current_normalized_fields = Vec::<&str>::new();

    for (field, normalized_field) in fields.iter().zip(normalized_fields.iter()) {
        if current_fields.is_empty() {
            current_fields.push(field);
            current_normalized_fields.push(normalized_field);
            continue;
        }

        let projected = render_record_unit_segment_text(header, &current_fields, Some(field));
        if projected.chars().count() > max_chars {
            push_record_unit_segment(
                &mut segments,
                header,
                normalized_header,
                &mut current_fields,
                &mut current_normalized_fields,
            );
        }
        current_fields.push(field);
        current_normalized_fields.push(normalized_field);
    }

    push_record_unit_segment(
        &mut segments,
        header,
        normalized_header,
        &mut current_fields,
        &mut current_normalized_fields,
    );

    if segments.is_empty() {
        vec![RecordUnitSegment {
            text: text.to_string(),
            normalized_text: normalized_text.to_string(),
        }]
    } else {
        segments
    }
}

fn push_record_unit_segment(
    out: &mut Vec<RecordUnitSegment>,
    header: &str,
    normalized_header: &str,
    fields: &mut Vec<&str>,
    normalized_fields: &mut Vec<&str>,
) {
    if fields.is_empty() && normalized_fields.is_empty() {
        return;
    }
    out.push(RecordUnitSegment {
        text: render_record_unit_segment_text(header, fields, None),
        normalized_text: render_record_unit_segment_text(
            normalized_header,
            normalized_fields,
            None,
        ),
    });
    fields.clear();
    normalized_fields.clear();
}

fn render_record_unit_segment_text(
    header: &str,
    fields: &[&str],
    next_field: Option<&str>,
) -> String {
    const FIELDS_MARKER: &str = "fields: ";
    let mut rendered = String::with_capacity(header.len().saturating_add(16));
    rendered.push_str(header);
    rendered.push_str(FIELDS_MARKER);
    let owned_fields = fields.iter().map(|field| (*field).to_string()).collect::<Vec<_>>();
    rendered.push_str(&join_record_fields(&owned_fields));
    if let Some(field) = next_field {
        if !fields.is_empty() {
            rendered.push_str(
                if fields.iter().any(|field| field.contains('\n')) || field.contains('\n') {
                    "\n;\n"
                } else {
                    "; "
                },
            );
        }
        rendered.push_str(field);
    }
    rendered
}

/// Returns a bounded, field-aware excerpt for the shared `render_record_unit`
/// grammar. Matching fields stay verbatim; no field name or corpus term is
/// special-cased.
#[must_use]
pub fn focused_record_unit_excerpt(
    text: &str,
    focus_terms: &[String],
    max_chars: usize,
) -> Option<String> {
    let trimmed = text.trim();
    let (header, fields, normalized_focus) =
        prepare_focused_excerpt(trimmed, focus_terms, max_chars)?;
    if trimmed.chars().count() <= max_chars {
        return Some(trimmed.to_string());
    }
    let scored = score_record_fields(&fields, &normalized_focus);
    let selected = select_excerpt_fields(header, &fields, &scored, max_chars);
    fit_excerpt_to_budget(header, &fields, &scored, selected, max_chars)
}

/// Header, matching field lines, and normalized focus terms prepared for
/// excerpt scoring. The tuple keeps the three borrowed pieces together under one
/// name because they are produced and consumed as a unit.
type FocusedExcerptFields<'text> = (&'text str, Vec<&'text str>, Vec<String>);

fn prepare_focused_excerpt<'text>(
    trimmed: &'text str,
    focus_terms: &[String],
    max_chars: usize,
) -> Option<FocusedExcerptFields<'text>> {
    const FIELDS_MARKER: &str = "fields: ";
    if trimmed.is_empty() || max_chars == 0 {
        return None;
    }
    if trimmed.chars().count() <= max_chars {
        return Some((trimmed, Vec::new(), Vec::new()));
    }
    let (header, fields_body) = trimmed.split_once(FIELDS_MARKER)?;
    let fields = split_record_fields(fields_body)
        .into_iter()
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let normalized_focus = focus_terms
        .iter()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| term.chars().count() >= 2)
        .collect::<Vec<_>>();
    (!fields.is_empty() && !normalized_focus.is_empty()).then_some((
        header,
        fields,
        normalized_focus,
    ))
}

fn score_record_fields(fields: &[&str], normalized_focus: &[String]) -> Vec<(usize, usize)> {
    let mut scored = fields
        .iter()
        .enumerate()
        .filter_map(|(index, field)| {
            let lowered = field.to_lowercase();
            let score = normalized_focus
                .iter()
                .filter(|term| lowered.contains(term.as_str()))
                .map(|term| term.chars().count().min(24))
                .sum::<usize>();
            (score > 0).then_some((index, score))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_index, left_score), (right_index, right_score)| {
        right_score.cmp(left_score).then_with(|| left_index.cmp(right_index))
    });
    scored
}

fn select_excerpt_fields(
    header: &str,
    fields: &[&str],
    scored: &[(usize, usize)],
    max_chars: usize,
) -> BTreeSet<usize> {
    let mut selected = BTreeSet::new();
    for (index, _) in scored {
        selected.insert(*index);
        selected.extend(index.checked_sub(1));
        selected.extend((index + 1 < fields.len()).then_some(index + 1));
        if render_record_unit_excerpt_text(header, fields, &selected).chars().count() >= max_chars {
            break;
        }
    }
    selected
}

fn fit_excerpt_to_budget(
    header: &str,
    fields: &[&str],
    scored: &[(usize, usize)],
    mut selected: BTreeSet<usize>,
    max_chars: usize,
) -> Option<String> {
    while !selected.is_empty() {
        let rendered = render_record_unit_excerpt_text(header, fields, &selected);
        if rendered.chars().count() <= max_chars {
            return Some(rendered);
        }
        selected.remove(&lowest_priority_selected_field(&selected, scored)?);
    }
    None
}

fn lowest_priority_selected_field(
    selected: &BTreeSet<usize>,
    scored: &[(usize, usize)],
) -> Option<usize> {
    selected
        .iter()
        .copied()
        .rfind(|index| !scored.iter().any(|(scored_index, _)| scored_index == index))
        .or_else(|| selected.iter().next_back().copied())
}

fn render_record_unit_excerpt_text(
    header: &str,
    fields: &[&str],
    selected: &BTreeSet<usize>,
) -> String {
    const FIELDS_MARKER: &str = "fields: ";
    let selected_fields =
        selected.iter().filter_map(|index| fields.get(*index).copied()).collect::<Vec<_>>();
    render_record_unit_segment_text(header, &selected_fields, None)
        .replace(&format!("{FIELDS_MARKER}{FIELDS_MARKER}"), FIELDS_MARKER)
}
