use super::{
    HashSet, KnowledgeChunkRow, QueryIR, RuntimeMatchedChunk, Uuid,
    extract_config_assignment_literals, extract_config_section_literals,
    extract_explicit_path_literals, extract_package_command_literals, extract_parameter_literals,
    is_source_profile_chunk_row, line_has_key_value_literal_surface,
    technical_chunk_selection_score, technical_literal_focus_keywords,
    types::{
        CODE_BLOCK_CHUNK_KIND, HEADING_CHUNK_KIND, KEY_VALUE_BLOCK_CHUNK_KIND,
        METADATA_BLOCK_CHUNK_KIND, SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
        SOURCE_CONTEXT_NEIGHBOR_PENALTY, SOURCE_CONTEXT_PROCEDURAL_SETUP_LIMIT_PER_DOCUMENT,
        SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD, SOURCE_CONTEXT_SETUP_PATH_SCORE_BONUS,
        SourceContextAnchor, SourceContextNeighborSpan, TABLE_ROW_CHUNK_KIND, source_anchor_window,
    },
};

#[cfg(test)]
use super::SourceFocusLimits;
#[cfg(test)]
use super::types::SOURCE_CONTEXT_FOCUSED_DECLARATION_LIMIT_PER_DOCUMENT;

pub(super) fn source_context_neighbor_windows(
    anchors: &[SourceContextAnchor],
    span: SourceContextNeighborSpan,
) -> Vec<(i32, i32)> {
    anchors
        .iter()
        .map(|anchor| source_anchor_window(anchor.chunk_index, span.backward, span.forward))
        .collect()
}

pub(super) fn procedural_structured_sibling_windows(
    anchors: &[SourceContextAnchor],
    forward: i32,
) -> Vec<(i32, i32)> {
    anchors.iter().map(|anchor| source_anchor_window(anchor.chunk_index, 0, forward)).collect()
}

pub(super) fn source_context_best_neighbor_score(
    anchors: &[SourceContextAnchor],
    chunk_index: i32,
    span: SourceContextNeighborSpan,
) -> Option<f32> {
    anchors
        .iter()
        .filter_map(|anchor| {
            let min_index = anchor.chunk_index.saturating_sub(span.backward.max(0));
            let max_index = anchor.chunk_index.saturating_add(span.forward.max(0));
            (chunk_index >= min_index && chunk_index <= max_index).then(|| {
                let distance = chunk_index.abs_diff(anchor.chunk_index) as f32;
                source_context_neighbor_score(anchor.score, distance, chunk_index)
            })
        })
        .max_by(f32::total_cmp)
}

pub(super) fn source_context_focus_keywords(
    question: &str,
    query_ir: Option<&QueryIR>,
    plan_keywords: &[String],
) -> Vec<String> {
    let mut keywords = technical_literal_focus_keywords(question, query_ir);
    if let Some(query_ir) = query_ir {
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            keywords.push(document_focus.hint.clone());
        }
        keywords.extend(query_ir.target_entities.iter().map(|entity| entity.label.clone()));
        keywords.extend(query_ir.literal_constraints.iter().map(|literal| literal.text.clone()));
    }
    keywords.extend(plan_keywords.iter().cloned());
    let mut seen = std::collections::BTreeSet::new();
    keywords
        .into_iter()
        .filter_map(|keyword| {
            let normalized = keyword.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                return None;
            }
            let key = normalized.to_lowercase();
            seen.insert(key).then_some(normalized)
        })
        .collect()
}

#[cfg(test)]
pub(super) fn select_query_focused_source_rows(
    rows: &[KnowledgeChunkRow],
    focus_keywords: &[String],
    pagination_requested: bool,
    anchors: &[SourceContextAnchor],
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    let anchor_indexes = anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>();
    super::super::source_focus_selection::select_query_focused_source_rows_with_declaration_reach(
        rows,
        focus_keywords,
        pagination_requested,
        &anchor_indexes,
        SourceFocusLimits {
            limit,
            additional_limit: SOURCE_CONTEXT_FOCUSED_DECLARATION_LIMIT_PER_DOCUMENT,
        },
        &HashSet::new(),
    )
    .into_iter()
    .map(|selected| selected.row)
    .collect()
}

pub(super) fn select_fallback_structured_search_rows(
    rows: Vec<(KnowledgeChunkRow, f32)>,
    focus_keywords: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
    limit: usize,
) -> Vec<(KnowledgeChunkRow, isize)> {
    if limit == 0 || rows.is_empty() || focus_keywords.is_empty() {
        return Vec::new();
    }
    let mut candidates = rows
        .into_iter()
        .filter(|(row, _)| !existing_chunk_ids.contains(&row.chunk_id))
        .filter(|(row, _)| !is_source_profile_chunk_row(row))
        .filter_map(|(row, search_score)| {
            let structural_score = fallback_structured_search_score(&row, focus_keywords)?;
            Some((structural_score, search_score, row))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_structural, left_search, left), (right_structural, right_search, right)| {
            right_structural
                .cmp(left_structural)
                .then_with(|| right_search.total_cmp(left_search))
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        },
    );
    candidates
        .into_iter()
        .take(limit)
        .map(|(structural_score, _, row)| (row, structural_score))
        .collect()
}

pub(super) fn fallback_structured_search_score(
    row: &KnowledgeChunkRow,
    focus_keywords: &[String],
) -> Option<isize> {
    let text = format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
    if !text_has_structured_literal_surface(&text) {
        return None;
    }
    let focus_score = technical_chunk_selection_score(&text, focus_keywords, false);
    if focus_score == 0 {
        return None;
    }
    let literal_score = extract_parameter_literals(&text, 8).len().saturating_mul(6)
        + extract_config_section_literals(&text, 4).len().saturating_mul(4)
        + extract_explicit_path_literals(&text, 4).len().saturating_mul(3)
        + extract_package_command_literals(&text, 2).len().saturating_mul(2);
    let literal_score = isize::try_from(literal_score).unwrap_or(isize::MAX);
    Some(focus_score.saturating_add(literal_score))
}

pub(super) fn select_procedural_structured_sibling_rows(
    rows: &[KnowledgeChunkRow],
    anchors: &[SourceContextAnchor],
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    if limit == 0 || rows.is_empty() || anchors.is_empty() {
        return Vec::new();
    }
    let anchor_indexes = anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>();
    let mut eligible = rows
        .iter()
        .filter(|row| !is_source_profile_chunk_row(row))
        .filter(|row| !anchor_indexes.contains(&row.chunk_index))
        .filter(|row| is_procedural_structured_sibling_row(row))
        .filter(|row| {
            !row.content_text.trim().is_empty()
                || row.window_text.as_deref().is_some_and(|text| !text.trim().is_empty())
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });

    let mut selected = Vec::<&KnowledgeChunkRow>::new();
    let mut selected_ids = std::collections::BTreeSet::<Uuid>::new();
    select_head_procedural_rows(&eligible, anchors, limit, &mut selected, &mut selected_ids);

    let non_head_anchors = anchors
        .iter()
        .copied()
        .filter(|anchor| anchor.chunk_index != SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX)
        .collect::<Vec<_>>();
    let distance_anchors =
        if non_head_anchors.is_empty() { anchors } else { non_head_anchors.as_slice() };

    let mut candidates = eligible
        .iter()
        .copied()
        .filter(|row| !selected_ids.contains(&row.chunk_id))
        .filter_map(|row| {
            let distance = distance_anchors
                .iter()
                .filter_map(|anchor| {
                    (row.chunk_index >= anchor.chunk_index)
                        .then_some(row.chunk_index.saturating_sub(anchor.chunk_index))
                })
                .min()?;
            Some((distance, row))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_distance, left), (right_distance, right)| {
        left_distance
            .cmp(right_distance)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    for (_, row) in candidates {
        if selected.len() >= limit {
            break;
        }
        if selected_ids.insert(row.chunk_id) {
            selected.push(row);
        }
    }
    selected.into_iter().cloned().collect()
}

pub(super) fn select_table_structured_sibling_rows(
    rows: &[KnowledgeChunkRow],
    anchors: &[SourceContextAnchor],
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    if limit == 0 || rows.is_empty() || anchors.is_empty() {
        return Vec::new();
    }
    let anchor_indexes = anchors.iter().map(|anchor| anchor.chunk_index).collect::<HashSet<_>>();
    let anchor_section_paths = rows
        .iter()
        .filter(|row| anchor_indexes.contains(&row.chunk_index))
        .map(|row| row.section_path.clone())
        .filter(|section_path| !section_path.is_empty())
        .collect::<HashSet<_>>();
    let mut eligible = rows
        .iter()
        .filter(|row| !is_source_profile_chunk_row(row))
        .filter(|row| {
            row.chunk_kind.as_deref() == Some(TABLE_ROW_CHUNK_KIND)
                || (!anchor_section_paths.is_empty()
                    && row.chunk_kind.as_deref() == Some(HEADING_CHUNK_KIND))
        })
        .filter(|row| chunk_row_has_visible_text(row))
        .filter(|row| {
            anchor_section_paths.is_empty() || anchor_section_paths.contains(&row.section_path)
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    eligible.into_iter().take(limit).cloned().collect()
}

pub(super) fn chunk_row_has_visible_text(row: &KnowledgeChunkRow) -> bool {
    !row.content_text.trim().is_empty()
        || row.window_text.as_deref().is_some_and(|text| !text.trim().is_empty())
}

pub(super) fn select_head_procedural_rows<'a>(
    eligible: &[&'a KnowledgeChunkRow],
    anchors: &[SourceContextAnchor],
    limit: usize,
    selected: &mut Vec<&'a KnowledgeChunkRow>,
    selected_ids: &mut std::collections::BTreeSet<Uuid>,
) {
    if !anchors.iter().any(|anchor| anchor.chunk_index == SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX)
    {
        return;
    }
    let max_index = SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX
        .saturating_add(SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD);
    for row in eligible
        .iter()
        .copied()
        .filter(|row| {
            row.chunk_index > SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX
                && row.chunk_index <= max_index
        })
        .take(limit.min(SOURCE_CONTEXT_PROCEDURAL_SETUP_LIMIT_PER_DOCUMENT))
    {
        if selected_ids.insert(row.chunk_id) {
            selected.push(row);
        }
    }
}

pub(super) fn is_procedural_structured_sibling_row(row: &KnowledgeChunkRow) -> bool {
    let text = format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
    if row.chunk_kind.as_deref() == Some(METADATA_BLOCK_CHUNK_KIND) {
        return text_has_structured_literal_surface(&text);
    }
    row_chunk_kind_is_structured(row.chunk_kind.as_deref())
        || text_has_structured_literal_surface(&text)
}

pub(super) fn runtime_chunk_has_explicit_structured_literal_surface(
    chunk: &RuntimeMatchedChunk,
) -> bool {
    text_has_setup_literal_surface(&format!("{}\n{}", chunk.excerpt, chunk.source_text))
}

pub(super) fn row_chunk_kind_is_structured(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            TABLE_ROW_CHUNK_KIND
                | CODE_BLOCK_CHUNK_KIND
                | KEY_VALUE_BLOCK_CHUNK_KIND
                | METADATA_BLOCK_CHUNK_KIND
        )
    )
}

pub(super) fn text_has_structured_literal_surface(text: &str) -> bool {
    !extract_parameter_literals(text, 2).is_empty()
        || !extract_config_assignment_literals(text, 2).is_empty()
        || !extract_config_section_literals(text, 2).is_empty()
        || !extract_explicit_path_literals(text, 2).is_empty()
        || !extract_package_command_literals(text, 1).is_empty()
}

pub(super) fn text_has_setup_literal_surface(text: &str) -> bool {
    !extract_config_section_literals(text, 2).is_empty()
        || !extract_explicit_path_literals(text, 2).is_empty()
        || !extract_package_command_literals(text, 1).is_empty()
        || text.lines().any(line_has_key_value_literal_surface)
}

pub(super) fn select_path_source_rows(
    rows: &[KnowledgeChunkRow],
    anchors: &[SourceContextAnchor],
    limit: usize,
    prioritize_module_setup_paths: bool,
) -> Vec<KnowledgeChunkRow> {
    if limit == 0 || rows.is_empty() {
        return Vec::new();
    }
    let anchor_indexes = anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>();
    let mut candidates = rows
        .iter()
        .filter(|row| !is_source_profile_chunk_row(row))
        .filter(|row| prioritize_module_setup_paths || !anchor_indexes.contains(&row.chunk_index))
        .filter_map(|row| {
            let text =
                format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
            let path_count = extract_explicit_path_literals(&text, 4).len();
            let setup_score = usize::from(
                prioritize_module_setup_paths
                    && path_count > 0
                    && !extract_package_command_literals(&text, 1).is_empty(),
            );
            (path_count > 0).then_some((setup_score, path_count, row))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_setup, left_count, left), (right_setup, right_count, right)| {
        right_setup
            .cmp(left_setup)
            .then_with(|| right_count.cmp(left_count))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates.into_iter().take(limit).map(|(_, _, row)| row.clone()).collect()
}

pub(super) fn setup_path_source_score_bonus(row: &KnowledgeChunkRow) -> f32 {
    let text = format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
    let has_command_object_literal = !extract_package_command_literals(&text, 1).is_empty();
    let has_configuration_path = extract_explicit_path_literals(&text, 8).into_iter().any(|path| {
        let lowered = path.to_ascii_lowercase();
        lowered.ends_with(".conf") || lowered.ends_with(".ini")
    });
    if has_command_object_literal && has_configuration_path {
        SOURCE_CONTEXT_SETUP_PATH_SCORE_BONUS
    } else {
        0.0
    }
}

pub(super) fn source_context_neighbor_score(
    anchor_score: f32,
    distance: f32,
    chunk_index: i32,
) -> f32 {
    // Source companions expand the evidence around an anchor; they must not become
    // stronger anchors than the retrieval hit that caused the expansion.
    anchor_score
        - SOURCE_CONTEXT_NEIGHBOR_PENALTY
        - distance * 0.001
        - chunk_index.max(0) as f32 * 0.000_001
}
