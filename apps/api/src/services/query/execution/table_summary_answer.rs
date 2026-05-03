use std::collections::{BTreeMap, HashMap, HashSet};

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR};
use crate::shared::extraction::table_summary::{
    TableColumnSummary, TableSummaryValueKind, build_table_column_summaries, format_numeric_value,
    parse_table_column_summary, render_table_column_summary,
};

use super::{
    focused_answer_document_id,
    retrieve::{excerpt_for, score_value},
    table_row_answer::{normalize_table_header, parse_table_row_chunk},
    types::RuntimeMatchedChunk,
};

#[derive(Debug, Clone)]
struct ScoredTableSummary {
    summary: TableColumnSummary,
    score: f32,
    searchable_text: String,
    source_text: String,
}

pub(crate) fn build_table_summary_grounded_answer(
    question: &str,
    ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_asks_table_aggregation(question, ir) {
        return None;
    }

    let focused_document_id = focused_answer_document_id(question, chunks);
    let summaries = collect_scored_table_summaries(chunks, focused_document_id);
    if summaries.is_empty() {
        return None;
    }

    if question_asks_average(ir) {
        let summary = select_best_table_summary(question, &summaries, |summary| {
            summary.value_kind == TableSummaryValueKind::Numeric
                && summary.average.is_some()
                && summary.aggregation_priority > 0
        })?;
        return format_average_table_summary_answer(question, summary);
    }

    if question_asks_most_frequent(ir) {
        let summary = select_best_table_summary(question, &summaries, |summary| {
            summary.value_kind == TableSummaryValueKind::Categorical
                && summary.most_frequent_count > 0
                && summary.aggregation_priority > 0
        })?;
        return format_most_frequent_table_summary_answer(question, summary);
    }

    None
}

fn collect_scored_table_summaries(
    chunks: &[RuntimeMatchedChunk],
    focused_document_id: Option<Uuid>,
) -> Vec<ScoredTableSummary> {
    let scoped_chunks = chunks
        .iter()
        .filter(|chunk| {
            focused_document_id.is_none() || Some(chunk.document_id) == focused_document_id
        })
        .collect::<Vec<_>>();
    if scoped_chunks.is_empty() {
        return Vec::new();
    }

    let mut summaries = scoped_chunks
        .iter()
        .filter_map(|chunk| {
            parse_table_column_summary(&chunk.source_text).map(|summary| ScoredTableSummary {
                searchable_text: build_table_summary_searchable_text(&summary),
                source_text: chunk.source_text.clone(),
                summary,
                score: score_value(chunk.score),
            })
        })
        .collect::<Vec<_>>();
    let mut seen = summaries
        .iter()
        .map(|entry| table_summary_identity_key(&entry.summary))
        .collect::<HashSet<_>>();

    for derived in derive_scored_table_summaries_from_rows(&scoped_chunks) {
        if seen.insert(table_summary_identity_key(&derived.summary)) {
            summaries.push(derived);
        }
    }

    summaries
}

fn derive_scored_table_summaries_from_rows(
    chunks: &[&RuntimeMatchedChunk],
) -> Vec<ScoredTableSummary> {
    #[derive(Debug, Default)]
    struct RowGroup {
        headers: Vec<String>,
        row_values: BTreeMap<usize, HashMap<String, String>>,
        best_score: f32,
    }

    let mut groups = HashMap::<(Uuid, String, Option<String>), RowGroup>::new();
    for row in chunks.iter().filter_map(|chunk| parse_table_row_chunk(chunk)) {
        let group_key = (row.document_id, row.sheet_name.clone(), row.table_name.clone());
        let group = groups.entry(group_key).or_default();
        group.best_score = group.best_score.max(row.score);
        let values = group.row_values.entry(row.row_number).or_default();
        for (header, value) in row.fields {
            let normalized_header = normalize_table_header(&header);
            if !group
                .headers
                .iter()
                .any(|candidate| normalize_table_header(candidate) == normalized_header)
            {
                group.headers.push(header.clone());
            }
            values.insert(normalized_header, value);
        }
    }

    let mut summaries = Vec::new();
    for ((_, sheet_name, table_name), group) in groups {
        if group.headers.is_empty() || group.row_values.len() < 2 {
            continue;
        }
        let rows = group
            .row_values
            .into_values()
            .map(|values| {
                group
                    .headers
                    .iter()
                    .map(|header| {
                        values.get(&normalize_table_header(header)).cloned().unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for summary in build_table_column_summaries(
            Some(sheet_name.as_str()),
            table_name.as_deref(),
            &group.headers,
            &rows,
        ) {
            let source_text = render_table_column_summary(&summary);
            summaries.push(ScoredTableSummary {
                searchable_text: build_table_summary_searchable_text(&summary),
                score: group.best_score,
                summary,
                source_text,
            });
        }
    }

    summaries
}

fn table_summary_identity_key(summary: &TableColumnSummary) -> String {
    format!(
        "{}|{}|{}|{}",
        summary.sheet_name.as_deref().unwrap_or_default(),
        summary.table_name.as_deref().unwrap_or_default(),
        summary.column_name,
        summary.value_kind.as_str(),
    )
}

fn build_table_summary_searchable_text(summary: &TableColumnSummary) -> String {
    [
        summary.sheet_name.as_deref().unwrap_or_default(),
        summary.table_name.as_deref().unwrap_or_default(),
        summary.column_name.as_str(),
        summary.value_kind.as_str(),
    ]
    .join(" ")
    .to_lowercase()
}

fn select_best_table_summary<'a>(
    question: &str,
    summaries: &'a [ScoredTableSummary],
    predicate: impl Fn(&TableColumnSummary) -> bool,
) -> Option<&'a TableColumnSummary> {
    let eligible = summaries.iter().filter(|entry| predicate(&entry.summary)).collect::<Vec<_>>();
    if eligible.len() == 1 {
        return Some(&eligible[0].summary);
    }

    let literals = crate::services::query::planner::extract_keywords_preserving_case(question)
        .into_iter()
        .map(|token| token.to_lowercase())
        .filter(|token| token.len() >= 3)
        .collect::<Vec<_>>();
    let mut ranked = eligible
        .into_iter()
        .map(|entry| {
            let lexical_hits = literals
                .iter()
                .filter(|literal| entry.searchable_text.contains(literal.as_str()))
                .count();
            let lexical_boost = lexical_hits as f32 * 10.0;
            let total_score = entry.score + lexical_boost;
            (entry, total_score, lexical_hits)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.0.summary.column_name.cmp(&right.0.summary.column_name))
    });
    let (best, best_total_score, lexical_hits) = ranked.first().copied()?;
    if lexical_hits == 0 {
        return None;
    }
    if ranked.get(1).is_some_and(|(_, score, hits)| {
        *hits == lexical_hits && (*score - best_total_score).abs() < 0.001
    }) {
        return None;
    }
    Some(&best.summary)
}

fn format_average_table_summary_answer(
    question: &str,
    summary: &TableColumnSummary,
) -> Option<String> {
    let _ = question;
    let average = summary.average?;
    let average_text = format_numeric_value(average);
    Some(format!(
        "The average `{}` is `{}` across `{}` rows.",
        summary.column_name, average_text, summary.non_empty_count
    ))
}

fn format_most_frequent_table_summary_answer(
    question: &str,
    summary: &TableColumnSummary,
) -> Option<String> {
    if summary.most_frequent_count == 0 {
        return None;
    }
    if summary.most_frequent_count <= 1 && summary.distinct_count > 1 {
        return Some(format!(
            "There is no single most frequent `{}` value: every value appears once.",
            summary.column_name
        ));
    }
    if summary.most_frequent_tie_count > 5 {
        return Some(format!(
            "There is no single leading `{}` value: `{}` different values each appear in `{}` rows.",
            summary.column_name, summary.most_frequent_tie_count, summary.most_frequent_count
        ));
    }
    let rendered_values = summary
        .most_frequent_values
        .iter()
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = question;
    if summary.most_frequent_tie_count == 1 {
        Some(format!(
            "The most frequent `{}` value is {} (`{}` rows).",
            summary.column_name, rendered_values, summary.most_frequent_count
        ))
    } else {
        Some(format!(
            "The most frequent `{}` values are {} (`{}` rows each).",
            summary.column_name, rendered_values, summary.most_frequent_count
        ))
    }
}

/// Detect questions that need table-level aggregation from the compiled IR.
pub(crate) fn question_asks_table_aggregation(question: &str, ir: Option<&QueryIR>) -> bool {
    let _ = question;
    ir.is_some_and(|ir| {
        question_asks_average(Some(ir))
            || question_asks_most_frequent(Some(ir))
            || (matches!(ir.act, QueryAct::RetrieveValue | QueryAct::Enumerate)
                && ir.target_types.iter().any(|tag| {
                    matches!(
                        normalized_ir_tag(tag).as_str(),
                        "table_summary" | "table_column_summary" | "table_aggregation"
                    )
                }))
    })
}

pub(crate) fn render_table_summary_chunk_section(
    question: &str,
    ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
) -> String {
    if !question_asks_table_aggregation(question, ir) {
        return String::new();
    }
    let mut summaries =
        collect_scored_table_summaries(chunks, focused_answer_document_id(question, chunks))
            .into_iter()
            .filter(|entry| summary_matches_requested_aggregation(&entry.summary, ir))
            .collect::<Vec<_>>();
    if summaries.is_empty() {
        return String::new();
    }
    if summaries.iter().any(|entry| entry.summary.aggregation_priority > 0) {
        summaries.retain(|entry| entry.summary.aggregation_priority > 0);
    }
    summaries.sort_by(|left, right| {
        right
            .summary
            .aggregation_priority
            .cmp(&left.summary.aggregation_priority)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.summary.column_name.cmp(&right.summary.column_name))
    });
    let lines = summaries
        .into_iter()
        .take(8)
        .map(|entry| format!("- {}", excerpt_for(&entry.source_text, 320)))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }
    format!("Table summaries\n{}", lines.join("\n"))
}

fn question_asks_average(ir: Option<&QueryIR>) -> bool {
    ir.is_some_and(|ir| {
        ir.target_types.iter().any(|tag| {
            matches!(
                normalized_ir_tag(tag).as_str(),
                "average" | "avg" | "mean" | "table_average" | "numeric_aggregate"
            )
        })
    })
}

fn question_asks_most_frequent(ir: Option<&QueryIR>) -> bool {
    ir.is_some_and(|ir| {
        ir.target_types.iter().any(|tag| {
            matches!(
                normalized_ir_tag(tag).as_str(),
                "most_frequent" | "mode" | "frequency" | "table_frequency" | "categorical_mode"
            )
        })
    })
}

pub(crate) fn summary_matches_requested_aggregation(
    summary: &TableColumnSummary,
    ir: Option<&QueryIR>,
) -> bool {
    if question_asks_average(ir) {
        return summary.value_kind == TableSummaryValueKind::Numeric && summary.average.is_some();
    }
    if question_asks_most_frequent(ir) {
        return summary.value_kind == TableSummaryValueKind::Categorical
            && summary.most_frequent_count > 0;
    }
    false
}

fn normalized_ir_tag(tag: &str) -> String {
    tag.trim().to_ascii_lowercase().replace('-', "_")
}
