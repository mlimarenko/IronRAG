use std::collections::HashSet;

use uuid::Uuid;

use crate::infra::knowledge_rows::KnowledgeChunkRow;

use super::{
    source_excerpt::{focused_declaration_depth, focused_declaration_reach_score},
    source_profile::is_source_profile_chunk_row,
    technical_literals::technical_chunk_selection_score,
};

#[derive(Debug, Clone)]
pub(super) struct SelectedQueryFocusedSourceRow {
    pub(super) row: KnowledgeChunkRow,
    pub(super) additional_declaration: bool,
}

#[derive(Debug, Clone, Copy)]
struct RankedQueryFocusedSourceRow<'a> {
    declaration_depth: Option<usize>,
    score: isize,
    declaration_reach_score: usize,
    row: &'a KnowledgeChunkRow,
}

/// Per-document selection limits for query-focused source rows: the declared
/// focus limit plus the extra declaration-reach slots.
pub(super) struct SourceFocusLimits {
    pub(super) limit: usize,
    pub(super) additional_limit: usize,
}

pub(super) fn select_query_focused_source_rows_with_declaration_reach(
    rows: &[KnowledgeChunkRow],
    focus_keywords: &[String],
    pagination_requested: bool,
    anchor_indexes: &[i32],
    limits: SourceFocusLimits,
    existing_chunk_ids: &HashSet<Uuid>,
) -> Vec<SelectedQueryFocusedSourceRow> {
    let SourceFocusLimits { limit, additional_limit } = limits;
    if limit == 0 {
        return Vec::new();
    }
    let candidates = ranked_query_focused_source_rows(
        rows,
        focus_keywords,
        pagination_requested,
        anchor_indexes,
    );
    let baseline_chunk_ids = candidates
        .iter()
        .take(limit)
        .map(|candidate| candidate.row.chunk_id)
        .collect::<HashSet<_>>();
    let mut selection_candidates = candidates
        .iter()
        .filter_map(|candidate| {
            let baseline = baseline_chunk_ids.contains(&candidate.row.chunk_id);
            let additional_declaration = !baseline
                && candidate.declaration_reach_score > 0
                && !existing_chunk_ids.contains(&candidate.row.chunk_id);
            (baseline || additional_declaration).then_some((*candidate, additional_declaration))
        })
        .collect::<Vec<_>>();
    selection_candidates.sort_by(|left, right| declaration_reach_order(*left, *right));
    let mut additional_declaration_count = 0usize;
    selection_candidates
        .into_iter()
        .filter(|(_, additional_declaration)| {
            if !*additional_declaration {
                return true;
            }
            if additional_declaration_count >= additional_limit {
                return false;
            }
            additional_declaration_count = additional_declaration_count.saturating_add(1);
            true
        })
        .take(limit)
        .map(|(candidate, additional_declaration)| SelectedQueryFocusedSourceRow {
            row: candidate.row.clone(),
            additional_declaration,
        })
        .collect()
}

fn ranked_query_focused_source_rows<'a>(
    rows: &'a [KnowledgeChunkRow],
    focus_keywords: &[String],
    pagination_requested: bool,
    anchor_indexes: &[i32],
) -> Vec<RankedQueryFocusedSourceRow<'a>> {
    if rows.is_empty() || focus_keywords.is_empty() {
        return Vec::new();
    }
    let mut candidates = rows
        .iter()
        .filter(|row| !is_source_profile_chunk_row(row))
        .filter(|row| !anchor_indexes.contains(&row.chunk_index))
        .filter_map(|row| {
            let text =
                format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
            let score =
                technical_chunk_selection_score(&text, focus_keywords, pagination_requested);
            let document_start_offset = row
                .span_start
                .and_then(|offset| usize::try_from(offset.max(0)).ok())
                .unwrap_or_else(|| usize::try_from(row.chunk_index.max(0)).unwrap_or_default());
            (score > 0).then_some(RankedQueryFocusedSourceRow {
                declaration_depth: focused_declaration_depth(&text, focus_keywords),
                score,
                declaration_reach_score: focused_declaration_reach_score(
                    &text,
                    focus_keywords,
                    document_start_offset,
                ),
                row,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        declaration_depth_order(left.declaration_depth, right.declaration_depth)
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.row.chunk_index.cmp(&right.row.chunk_index))
            .then_with(|| left.row.chunk_id.cmp(&right.row.chunk_id))
    });
    candidates
}

fn declaration_reach_order(
    left: (RankedQueryFocusedSourceRow<'_>, bool),
    right: (RankedQueryFocusedSourceRow<'_>, bool),
) -> std::cmp::Ordering {
    let left_reach = if left.1 { left.0.declaration_reach_score } else { 0 };
    let right_reach = if right.1 { right.0.declaration_reach_score } else { 0 };
    let left_total = left.0.score.saturating_add(isize::try_from(left_reach).unwrap_or(isize::MAX));
    let right_total =
        right.0.score.saturating_add(isize::try_from(right_reach).unwrap_or(isize::MAX));
    declaration_depth_order(left.0.declaration_depth, right.0.declaration_depth)
        .then_with(|| right.1.cmp(&left.1))
        .then_with(|| right_total.cmp(&left_total))
        .then_with(|| right_reach.cmp(&left_reach))
        .then_with(|| right.0.score.cmp(&left.0.score))
        .then_with(|| left.0.row.chunk_index.cmp(&right.0.row.chunk_index))
        .then_with(|| left.0.row.chunk_id.cmp(&right.0.row.chunk_id))
}

fn declaration_depth_order(
    left_depth: Option<usize>,
    right_depth: Option<usize>,
) -> std::cmp::Ordering {
    match (left_depth, right_depth) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}
