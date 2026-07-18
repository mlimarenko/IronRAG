use std::collections::{BTreeMap, BTreeSet, HashMap};

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR, QueryTargetKind};
use crate::services::query::text_match::normalized_alnum_tokens;
use crate::shared::extraction::table_markdown::{
    is_markdown_separator_row, parse_markdown_table_row,
};

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
    let scoped_chunks = chunks
        .iter()
        .filter(|chunk| {
            focused_document_id.is_none() || Some(chunk.document_id) == focused_document_id
        })
        .collect::<Vec<_>>();
    let rows =
        scoped_chunks.iter().filter_map(|chunk| parse_table_row_chunk(chunk)).collect::<Vec<_>>();
    if rows.is_empty() && scoped_chunks.is_empty() {
        return None;
    }

    if let Some(row_count) = requested_initial_table_row_count(ir) {
        if rows.is_empty() {
            return None;
        }
        return build_initial_table_rows_answer(&rows, row_count);
    }

    if query_ir_requests_table_column_inventory(ir) {
        if let Some(answer) = build_table_column_inventory_answer(ir, &rows) {
            return Some(answer);
        }
        if let Some(answer) = build_raw_pipe_table_column_inventory_answer(ir, &scoped_chunks) {
            return Some(answer);
        }
        return None;
    }

    if rows.is_empty() {
        return None;
    }

    if question_asks_table_value_inventory(question, ir) {
        return build_table_value_inventory_answer(&rows);
    }

    build_focused_table_row_field_answer(question, &rows)
}

/// Decodes the canonical row protocol emitted by
/// [`crate::shared::extraction::table_markdown::build_semantic_table_row_text`].
///
/// `Sheet`, `Table`, and `Row` are format markers here, not natural-language
/// content classifiers. The round-trip regression below keeps this parser tied
/// to the emitter instead of allowing these markers to spread into routing.
pub(crate) fn parse_table_row_chunk(chunk: &RuntimeMatchedChunk) -> Option<ParsedTableRow> {
    if !chunk.source_text.starts_with("Sheet: ") || !chunk.source_text.contains(" | Row ") {
        return parse_raw_pipe_table_row_chunk(chunk);
    }
    let mut fields = Vec::new();
    let mut sheet_name = None::<String>;
    let mut table_name = None::<String>;
    let mut row_number = None::<usize>;
    let mut seen_row_marker = false;
    for part in chunk.source_text.split(" | ") {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix("Row ")
            && let Ok(parsed) = value.trim().parse::<usize>()
        {
            row_number = Some(parsed);
            seen_row_marker = true;
            continue;
        }
        let Some((key, value)) = part.split_once(": ") else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if !seen_row_marker && key.eq_ignore_ascii_case("row") {
            row_number = value.parse::<usize>().ok();
            seen_row_marker = true;
            continue;
        }
        if !seen_row_marker && key.eq_ignore_ascii_case("sheet") {
            sheet_name = Some(value.to_string());
            continue;
        }
        if !seen_row_marker && key.eq_ignore_ascii_case("table") {
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

fn parse_raw_pipe_table_row_chunk(chunk: &RuntimeMatchedChunk) -> Option<ParsedTableRow> {
    if chunk.chunk_kind.as_deref() != Some("table_row") {
        return None;
    }
    let cells = parse_raw_pipe_cells(&chunk.source_text)?;
    if cells.len() < 2 || is_markdown_separator_row(&cells) {
        return None;
    }

    let fields = cells
        .iter()
        .enumerate()
        .map(|(index, value)| (format!("col_{}", index + 1), value.clone()))
        .collect::<Vec<_>>();
    Some(ParsedTableRow {
        document_id: chunk.document_id,
        sheet_name: chunk.document_label.clone(),
        table_name: None,
        row_number: usize::try_from(chunk.chunk_index).ok()?.saturating_add(1),
        flattened_text: cells.join(" ").to_lowercase(),
        fields,
        score: score_value(chunk.score),
    })
}

fn parse_raw_pipe_cells(source_text: &str) -> Option<Vec<String>> {
    let mut non_empty_lines = source_text.lines().map(str::trim).filter(|line| !line.is_empty());
    let line = non_empty_lines.next()?;
    if non_empty_lines.next().is_some() || !line.starts_with('|') || !line.ends_with('|') {
        return None;
    }
    let cells = parse_markdown_table_row(line)
        .into_iter()
        .map(|cell| cell.trim().to_string())
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>();
    (!cells.is_empty()).then_some(cells)
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
        if raw_pipe_table_row(best_row) {
            return build_raw_pipe_table_row_answer(question, best_row);
        }
        return None;
    }
    let mut selected_headers = row_identifier_headers_from_question(question, best_row);
    selected_headers.extend(requested_headers);
    selected_headers.dedup();

    let values = selected_headers
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

fn build_raw_pipe_table_row_answer(question: &str, row: &ParsedTableRow) -> Option<String> {
    if row_identifier_headers_from_question(question, row).is_empty() {
        return None;
    }
    let values = row
        .fields
        .iter()
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>();
    if values.len() < 2 {
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

fn build_table_column_inventory_answer(
    ir: Option<&QueryIR>,
    rows: &[ParsedTableRow],
) -> Option<String> {
    let target_tokens = typed_table_inventory_target_tokens(ir);
    let groups = table_column_inventory_groups(rows);
    if groups.is_empty() {
        return None;
    }

    let table_token_counts = table_inventory_token_counts(&groups);
    let ranked = rank_table_column_inventory_groups(groups, &target_tokens, &table_token_counts);
    let (_, _, _, table_name, rows) = unique_top_table_inventory_group(&ranked)?;
    render_table_column_inventory(table_name, rows)
}

type TableColumnInventoryGroup<'a> = ((Uuid, String, String), Vec<&'a ParsedTableRow>);
type RankedTableColumnInventoryGroup<'a> = (usize, f32, String, String, Vec<&'a ParsedTableRow>);

fn table_column_inventory_groups(
    rows: &[ParsedTableRow],
) -> HashMap<(Uuid, String, String), Vec<&ParsedTableRow>> {
    let mut groups: HashMap<(Uuid, String, String), Vec<&ParsedTableRow>> = HashMap::new();
    for row in rows {
        let Some(table_name) = row.table_name.as_ref().filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        if table_row_has_inventory_fields(row) {
            groups
                .entry((row.document_id, row.sheet_name.clone(), table_name.clone()))
                .or_default()
                .push(row);
        }
    }
    groups
}

fn table_row_has_inventory_fields(row: &ParsedTableRow) -> bool {
    row.fields.iter().any(|(header, value)| !header.trim().is_empty() && !value.trim().is_empty())
}

fn table_inventory_token_counts(
    groups: &HashMap<(Uuid, String, String), Vec<&ParsedTableRow>>,
) -> BTreeMap<String, usize> {
    let mut table_token_counts = BTreeMap::new();
    for (_, _, table_name) in groups.keys() {
        for token in normalized_alnum_tokens(table_name, 2) {
            table_token_counts.entry(token).and_modify(|count| *count += 1).or_insert(1);
        }
    }
    table_token_counts
}

fn rank_table_column_inventory_groups<'a>(
    groups: HashMap<(Uuid, String, String), Vec<&'a ParsedTableRow>>,
    target_tokens: &BTreeSet<String>,
    table_token_counts: &BTreeMap<String, usize>,
) -> Vec<RankedTableColumnInventoryGroup<'a>> {
    let group_count = groups.len();
    let mut ranked = groups
        .into_iter()
        .filter_map(|group| {
            rank_table_column_inventory_group(group, target_tokens, table_token_counts, group_count)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(compare_ranked_table_column_inventory_groups);
    ranked
}

fn rank_table_column_inventory_group<'a>(
    ((_, sheet_name, table_name), rows): TableColumnInventoryGroup<'a>,
    target_tokens: &BTreeSet<String>,
    table_token_counts: &BTreeMap<String, usize>,
    group_count: usize,
) -> Option<RankedTableColumnInventoryGroup<'a>> {
    let table_tokens = normalized_alnum_tokens(&table_name, 2);
    let distinctive_table_hits = distinctive_table_target_hits(
        &table_tokens,
        target_tokens,
        table_token_counts,
        group_count,
    )?;
    let sheet_hits = table_inventory_sheet_hits(&sheet_name, target_tokens);
    let best_score = rows.iter().map(|row| row.score).fold(0.0, f32::max);
    let score = distinctive_table_hits.saturating_mul(8).saturating_add(sheet_hits);
    Some((score, best_score, sheet_name, table_name, rows))
}

fn table_inventory_sheet_hits(sheet_name: &str, target_tokens: &BTreeSet<String>) -> usize {
    if target_tokens.is_empty() {
        return 0;
    }
    normalized_alnum_tokens(sheet_name, 2)
        .iter()
        .filter(|token| target_tokens.contains(token.as_str()))
        .count()
}

fn compare_ranked_table_column_inventory_groups(
    left: &RankedTableColumnInventoryGroup<'_>,
    right: &RankedTableColumnInventoryGroup<'_>,
) -> std::cmp::Ordering {
    right
        .0
        .cmp(&left.0)
        .then_with(|| right.1.total_cmp(&left.1))
        .then_with(|| left.2.cmp(&right.2))
        .then_with(|| left.3.cmp(&right.3))
}

fn unique_top_table_inventory_group<'group, 'row>(
    ranked: &'group [RankedTableColumnInventoryGroup<'row>],
) -> Option<&'group RankedTableColumnInventoryGroup<'row>> {
    let top = ranked.first()?;
    ranked.get(1).is_none_or(|candidate| candidate.0 != top.0).then_some(top)
}

fn render_table_column_inventory(table_name: &str, rows: &[&ParsedTableRow]) -> Option<String> {
    let mut rows_by_number = BTreeMap::<usize, &ParsedTableRow>::new();
    for row in rows {
        rows_by_number.entry(row.row_number).or_insert(*row);
    }
    if rows_by_number.len() < 2 {
        return None;
    }

    let mut lines = vec![format!("`{table_name}`:")];
    for row in rows_by_number.into_values().take(32) {
        if let Some(rendered) = render_structured_inventory_row(row) {
            lines.push(rendered);
        }
    }
    (lines.len() > 1).then(|| lines.join("\n"))
}

fn render_structured_inventory_row(row: &ParsedTableRow) -> Option<String> {
    let fields = row
        .fields
        .iter()
        .filter_map(|(header, value)| {
            let header = header.trim();
            let value = value.trim();
            (!header.is_empty() && !value.is_empty()).then(|| format!("`{header}` = `{value}`"))
        })
        .collect::<Vec<_>>();
    (!fields.is_empty()).then(|| format!("- {}", fields.join("; ")))
}

#[derive(Debug)]
struct RawPipeTableSectionRow {
    document_id: Uuid,
    table_name: String,
    row_number: usize,
    cells: Vec<String>,
    score: f32,
}

fn build_raw_pipe_table_column_inventory_answer(
    ir: Option<&QueryIR>,
    chunks: &[&RuntimeMatchedChunk],
) -> Option<String> {
    let target_tokens = typed_table_inventory_target_tokens(ir);
    let rows = collect_raw_pipe_section_rows(chunks);
    if rows.is_empty() {
        return None;
    }

    let mut groups = HashMap::<(Uuid, String), Vec<&RawPipeTableSectionRow>>::new();
    for row in &rows {
        groups.entry((row.document_id, row.table_name.clone())).or_default().push(row);
    }
    let group_count = groups.len();
    let mut table_token_counts = BTreeMap::<String, usize>::new();
    for (_, table_name) in groups.keys() {
        for token in normalized_alnum_tokens(table_name, 2) {
            table_token_counts.entry(token).and_modify(|count| *count += 1).or_insert(1);
        }
    }

    let mut ranked = groups
        .into_iter()
        .filter_map(|((_, table_name), rows)| {
            let table_tokens = normalized_alnum_tokens(&table_name, 2);
            let distinctive_table_hits = distinctive_table_target_hits(
                &table_tokens,
                &target_tokens,
                &table_token_counts,
                group_count,
            )?;
            let best_score = rows.iter().map(|row| row.score).fold(0.0, f32::max);
            Some((distinctive_table_hits, best_score, table_name, rows))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let (score, _, table_name, rows) = ranked.first()?;
    if ranked.get(1).is_some_and(|candidate| candidate.0 == *score) {
        return None;
    }

    let mut rows_by_number = BTreeMap::<usize, &RawPipeTableSectionRow>::new();
    for row in rows {
        rows_by_number.entry(row.row_number).or_insert(row);
    }
    if rows_by_number.is_empty() {
        return None;
    }

    let mut lines = vec![format!("`{table_name}`:")];
    for row in rows_by_number.into_values().take(32) {
        let cells = row
            .cells
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>();
        if !cells.is_empty() {
            lines.push(format!("- {}", cells.join("; ")));
        }
    }
    (lines.len() > 1).then(|| lines.join("\n"))
}

fn collect_raw_pipe_section_rows(chunks: &[&RuntimeMatchedChunk]) -> Vec<RawPipeTableSectionRow> {
    let mut ordered = chunks
        .iter()
        .filter(|chunk| matches!(chunk.chunk_kind.as_deref(), Some("heading") | Some("table_row")))
        .copied()
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.document_id
            .cmp(&right.document_id)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
    });

    let mut rows = Vec::new();
    let mut current_document_id = None::<Uuid>;
    let mut current_table_name = None::<String>;
    for chunk in ordered {
        if current_document_id != Some(chunk.document_id) {
            current_document_id = Some(chunk.document_id);
            current_table_name = None;
        }
        if chunk.chunk_kind.as_deref() == Some("heading") {
            current_table_name = extract_structural_section_label(&chunk.source_text);
            continue;
        }
        if chunk.chunk_kind.as_deref() != Some("table_row") {
            continue;
        }
        let Some(table_name) = current_table_name.as_ref() else {
            continue;
        };
        let Some(cells) = parse_raw_pipe_cells(&chunk.source_text) else {
            continue;
        };
        if cells.len() < 2 || is_markdown_separator_row(&cells) {
            continue;
        }
        rows.push(RawPipeTableSectionRow {
            document_id: chunk.document_id,
            table_name: table_name.clone(),
            row_number: usize::try_from(chunk.chunk_index).unwrap_or_default(),
            cells,
            score: score_value(chunk.score),
        });
    }
    rows
}

fn extract_structural_section_label(source_text: &str) -> Option<String> {
    source_text.lines().find_map(|line| {
        let label = line
            .trim()
            .trim_start_matches('#')
            .trim()
            .split(['|', '\t'])
            .next()
            .unwrap_or_default()
            .trim();
        (!label.is_empty()).then(|| label.to_string())
    })
}

fn typed_table_inventory_target_tokens(ir: Option<&QueryIR>) -> BTreeSet<String> {
    let mut tokens = ir
        .into_iter()
        .flat_map(|query_ir| {
            query_ir
                .target_entities
                .iter()
                .flat_map(|entity| normalized_alnum_tokens(&entity.label, 2))
                .chain(
                    query_ir
                        .document_focus
                        .iter()
                        .flat_map(|focus| normalized_alnum_tokens(&focus.hint, 2)),
                )
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    if tokens.is_empty()
        && let Some(query_ir) = ir
        && let Some(retrieval_query) = query_ir.retrieval_query.as_deref()
    {
        tokens.extend(normalized_alnum_tokens(retrieval_query, 2));
    }
    tokens
}

fn distinctive_table_target_hits(
    table_tokens: &BTreeSet<String>,
    target_tokens: &BTreeSet<String>,
    table_token_counts: &BTreeMap<String, usize>,
    group_count: usize,
) -> Option<usize> {
    if target_tokens.is_empty() {
        // No compiled target is safe only when the evidence has one possible
        // section. Multiple sections are semantically ambiguous, so abstain.
        return (group_count == 1).then_some(1);
    }
    let hits = table_tokens
        .iter()
        .filter(|token| {
            target_tokens.contains(token.as_str())
                && table_token_counts.get(token.as_str()).copied().unwrap_or_default() == 1
        })
        .count();
    (hits > 0).then_some(hits)
}

fn query_ir_requests_table_column_inventory(ir: Option<&QueryIR>) -> bool {
    if let Some(ir) = ir {
        if ir.source_slice.is_some() {
            return false;
        }
        if matches!(ir.act, QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue)
            && ir.targets(QueryTargetKind::TableRow)
            && ir.targets(QueryTargetKind::TableSummary)
        {
            return true;
        }
    }
    false
}

/// Does the user want the full inventory of values from a table?
///
/// The canonical signal is the compiled IR: an `Enumerate` act scoped
/// to a `table_row` target type means "list values from this table".
pub(crate) fn question_asks_table_value_inventory(question: &str, ir: Option<&QueryIR>) -> bool {
    let _ = question;
    if let Some(ir) = ir
        && matches!(ir.act, QueryAct::Enumerate)
        && ir.targets(QueryTargetKind::TableRow)
    {
        return true;
    }
    false
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
            let literal_score = literals
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
            let matched_cell_score =
                row_identifier_headers_from_question(question, row).len().saturating_mul(12);
            let score = literal_score.saturating_add(matched_cell_score);
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

fn raw_pipe_table_row(row: &ParsedTableRow) -> bool {
    !row.fields.is_empty()
        && row
            .fields
            .iter()
            .enumerate()
            .all(|(index, (header, _))| header == &format!("col_{}", index + 1))
}

fn row_identifier_headers_from_question(question: &str, row: &ParsedTableRow) -> Vec<String> {
    let lowered = question.to_lowercase();
    let question_tokens = normalized_alnum_tokens(question, 3);
    row.fields
        .iter()
        .filter(|(_, value)| {
            let normalized_value = value.trim().to_lowercase();
            if normalized_value.chars().count() < 3 {
                return false;
            }
            lowered.contains(&normalized_value)
                || normalized_alnum_tokens(value, 3)
                    .iter()
                    .any(|token| question_tokens.contains(token))
        })
        .map(|(header, _)| normalize_table_header(header))
        .collect()
}

fn requested_table_headers(question: &str, row: &ParsedTableRow) -> Vec<String> {
    let lowered = question.to_lowercase();
    let question_tokens = normalized_alnum_tokens(question, 2);
    row.fields
        .iter()
        .filter(|(header, _)| table_header_matches_question(header, &lowered, &question_tokens))
        .map(|(header, _)| normalize_table_header(header))
        .collect()
}

pub(crate) fn normalize_table_header(value: &str) -> String {
    value.trim().to_lowercase()
}

fn table_header_matches_question(
    header: &str,
    lowered_question: &str,
    question_tokens: &BTreeSet<String>,
) -> bool {
    let normalized_header = normalize_table_header(header);
    if lowered_question.contains(&normalized_header) {
        return true;
    }
    let tokenized_header = split_table_header_for_token_match(header);
    let header_tokens = normalized_alnum_tokens(&tokenized_header, 2);
    if header_tokens.is_empty() {
        return false;
    }
    if header_tokens.iter().all(|token| question_tokens.contains(token)) {
        return true;
    }
    header_tokens.len() >= 2
        && header_tokens.iter().filter(|token| question_tokens.contains(*token)).count() >= 1
}

fn split_table_header_for_token_match(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    for (idx, ch) in chars.iter().copied().enumerate() {
        if idx > 0 && ch.is_uppercase() {
            let prev = chars[idx - 1];
            let next = chars.get(idx + 1).copied();
            if prev.is_lowercase()
                || prev.is_numeric()
                || (prev.is_uppercase() && next.is_some_and(|candidate| candidate.is_lowercase()))
            {
                out.push(' ');
            }
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
#[path = "table_row_answer_tests.rs"]
mod tests;
