use std::collections::HashSet;

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR};

use super::{
    focused_answer_document_id, requested_initial_table_row_count, retrieve::score_value,
    types::RuntimeMatchedChunk,
};

#[derive(Debug, Clone)]
pub(crate) struct ParsedTableRow {
    pub(crate) document_id: Uuid,
    pub(crate) sheet_name: String,
    pub(crate) table_name: Option<String>,
    pub(crate) row_number: usize,
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) flattened_text: String,
    pub(crate) score: f32,
}

pub(crate) fn build_table_row_grounded_answer(
    question: &str,
    ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let focused_document_id = focused_answer_document_id(question, chunks);
    let rows = chunks
        .iter()
        .filter(|chunk| {
            focused_document_id.is_none() || Some(chunk.document_id) == focused_document_id
        })
        .filter_map(parse_table_row_chunk)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }

    if let Some(row_count) = requested_initial_table_row_count(question) {
        return build_initial_table_rows_answer(&rows, row_count);
    }

    if question_asks_table_value_inventory(question, ir) {
        return build_table_value_inventory_answer(&rows);
    }

    build_focused_table_row_field_answer(question, &rows)
}

pub(crate) fn parse_table_row_chunk(chunk: &RuntimeMatchedChunk) -> Option<ParsedTableRow> {
    if !chunk.source_text.starts_with("Sheet: ") || !chunk.source_text.contains(" | Row ") {
        return None;
    }
    let mut fields = Vec::new();
    let mut sheet_name = None::<String>;
    let mut table_name = None::<String>;
    let mut row_number = None::<usize>;
    for part in chunk.source_text.split(" | ") {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix("Row ") {
            row_number = value.trim().parse::<usize>().ok();
            continue;
        }
        let Some((key, value)) = part.split_once(": ") else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("row") {
            row_number = value.parse::<usize>().ok();
            continue;
        }
        if key.eq_ignore_ascii_case("sheet") {
            sheet_name = Some(value.to_string());
            continue;
        }
        if key.eq_ignore_ascii_case("table") {
            table_name = Some(value.to_string());
            continue;
        }
        fields.push((key.to_string(), value.to_string()));
    }
    let row_number = row_number?;
    Some(ParsedTableRow {
        document_id: chunk.document_id,
        sheet_name: sheet_name.unwrap_or_else(|| "Sheet".to_string()),
        table_name,
        row_number,
        fields,
        flattened_text: chunk.source_text.to_lowercase(),
        score: score_value(chunk.score),
    })
}

fn build_initial_table_rows_answer(rows: &[ParsedTableRow], row_count: usize) -> Option<String> {
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        left.sheet_name
            .cmp(&right.sheet_name)
            .then_with(|| left.table_name.cmp(&right.table_name))
            .then_with(|| left.row_number.cmp(&right.row_number))
    });
    rows.dedup_by(|left, right| {
        left.document_id == right.document_id
            && left.sheet_name == right.sheet_name
            && left.table_name == right.table_name
            && left.row_number == right.row_number
    });
    let selected = rows.into_iter().take(row_count).collect::<Vec<_>>();
    if selected.len() != row_count {
        return None;
    }

    let mut lines = Vec::with_capacity(selected.len());
    for row in selected {
        let rendered = row
            .fields
            .iter()
            .map(|(header, value)| format!("{header} = `{value}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- Row {}: {}", row.row_number, rendered));
    }
    Some(lines.join("\n"))
}

fn build_focused_table_row_field_answer(question: &str, rows: &[ParsedTableRow]) -> Option<String> {
    let best_row = best_matching_table_row(question, rows)?;
    let requested_headers = requested_table_headers(question, best_row);
    if requested_headers.is_empty() {
        return None;
    }

    let values = requested_headers
        .into_iter()
        .filter_map(|header| {
            best_row
                .fields
                .iter()
                .find(|(candidate, _)| normalize_table_header(candidate) == header)
                .map(|(candidate, value)| format!("{candidate}: `{value}`"))
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    Some(values.join("; "))
}

fn build_table_value_inventory_answer(rows: &[ParsedTableRow]) -> Option<String> {
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        left.sheet_name.cmp(&right.sheet_name).then_with(|| left.row_number.cmp(&right.row_number))
    });
    rows.dedup_by(|left, right| {
        left.document_id == right.document_id
            && left.sheet_name == right.sheet_name
            && left.row_number == right.row_number
    });
    if rows.is_empty() {
        return None;
    }

    let mut lines = Vec::with_capacity(rows.len().min(16));
    for row in rows.into_iter().take(16) {
        let rendered =
            if row.fields.len() == 1 && normalize_table_header(&row.fields[0].0) == "col_1" {
                format!("`{}`", row.fields[0].1)
            } else {
                row.fields
                    .iter()
                    .map(|(header, value)| format!("{header} = `{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
        lines.push(format!("- {} row {}: {}", row.sheet_name, row.row_number, rendered));
    }

    Some(lines.join("\n"))
}

/// Does the user want the full inventory of values from a table?
///
/// The canonical signal is the compiled IR: an `Enumerate` act scoped
/// to a `table_row` target type means "list values from this table".
/// When IR is not available we fall back to the previous 10-marker
/// trigger list so older callers keep working unchanged.
pub(crate) fn question_asks_table_value_inventory(question: &str, ir: Option<&QueryIR>) -> bool {
    if let Some(ir) = ir {
        return matches!(ir.act, QueryAct::Enumerate)
            && ir.target_types.iter().any(|tag| tag == "table_row");
    }
    let lowered = question.to_lowercase();
    [
        "какие значения",
        "какие данные",
        "какие строки",
        "покажи значения",
        "что за значения",
        "what values",
        "which values",
        "list values",
        "show values",
        "show rows",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

fn best_matching_table_row<'a>(
    question: &str,
    rows: &'a [ParsedTableRow],
) -> Option<&'a ParsedTableRow> {
    let literals = crate::services::query::planner::extract_keywords_preserving_case(question)
        .into_iter()
        .map(|token| token.to_lowercase())
        .filter(|token| token.len() >= 3)
        .collect::<Vec<_>>();
    if literals.is_empty() {
        return None;
    }

    let mut ranked = rows
        .iter()
        .map(|row| {
            let score = literals
                .iter()
                .filter(|literal| row.flattened_text.contains(literal.as_str()))
                .map(|literal| {
                    if literal.contains('@')
                        || literal.contains('.')
                        || literal.chars().any(|character| character.is_ascii_digit())
                    {
                        12usize
                    } else {
                        3usize
                    }
                })
                .sum::<usize>();
            (row, score)
        })
        .filter(|(_, score)| *score > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| left.0.row_number.cmp(&right.0.row_number))
    });
    let (best_row, best_score) = ranked.first().copied()?;
    if best_score < 6 {
        return None;
    }
    if ranked.get(1).is_some_and(|(_, second_score)| *second_score == best_score) {
        return None;
    }
    Some(best_row)
}

fn requested_table_headers(question: &str, row: &ParsedTableRow) -> Vec<String> {
    const HEADER_MARKERS: &[(&[&str], &[&str])] = &[
        (&["должност", "job title", "position", "role"], &["job title"]),
        (&["стран", "country"], &["country"]),
        (&["город", "city"], &["city"]),
        (&["компан", "company"], &["company"]),
        (&["отрасл", "индустр", "industry"], &["industry"]),
        (&["цен", "price"], &["price"]),
        (&["stock", "остат", "inventory"], &["stock"]),
        (&["сотруд", "employee", "employees", "headcount"], &["number of employees"]),
        (&["availability", "налич", "доступ"], &["availability"]),
        (&["source", "источ"], &["source"]),
        (&["deal stage", "stage", "этап", "стад"], &["deal stage"]),
        (&["email", "почт"], &["email", "email 1", "email 2"]),
        (&["phone", "телефон"], &["phone", "phone 1", "phone 2"]),
    ];

    let lowered = question.to_lowercase();
    let available_headers =
        row.fields.iter().map(|(header, _)| normalize_table_header(header)).collect::<HashSet<_>>();
    let mut requested = Vec::new();

    for (markers, aliases) in HEADER_MARKERS {
        if !markers.iter().any(|marker| lowered.contains(marker)) {
            continue;
        }
        for alias in *aliases {
            let normalized = normalize_table_header(alias);
            if available_headers.contains(&normalized) && !requested.contains(&normalized) {
                requested.push(normalized);
                break;
            }
        }
    }

    if !requested.is_empty() {
        return requested;
    }

    row.fields
        .iter()
        .map(|(header, _)| normalize_table_header(header))
        .filter(|header| lowered.contains(header.as_str()))
        .collect()
}

pub(crate) fn normalize_table_header(value: &str) -> String {
    value.trim().to_lowercase()
}
