#[must_use]
pub fn normalize_table_cell_text(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn escape_markdown_table_cell(value: &str) -> String {
    normalize_table_cell_text(value).replace('|', "\\|")
}

fn padded_markdown_row(cells: &[String], width: usize) -> Vec<String> {
    let mut padded = cells.iter().map(|cell| escape_markdown_table_cell(cell)).collect::<Vec<_>>();
    padded.resize(width, String::new());
    padded
}

#[must_use]
pub fn canonicalize_table_headers(headers: &[String], width: usize) -> Vec<String> {
    (0..width)
        .map(|index| {
            headers
                .get(index)
                .map(|cell| escape_markdown_table_cell(cell))
                .filter(|cell| !cell.trim().is_empty())
                .unwrap_or_else(|| format!("col_{}", index + 1))
        })
        .collect()
}

fn markdown_row_line(cells: &[String]) -> String {
    format!("| {} |", cells.join(" | "))
}

#[must_use]
pub fn parse_markdown_table_row(row_text: &str) -> Vec<String> {
    let trimmed = row_text.trim();
    if !trimmed.contains('|') {
        return Vec::new();
    }

    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escaped = false;

    for character in inner.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '|' => {
                cells.push(current.trim().replace("<br>", "\n").replace("<br />", "\n"));
                current.clear();
            }
            _ => current.push(character),
        }
    }
    cells.push(current.trim().replace("<br>", "\n").replace("<br />", "\n"));
    cells
}

#[must_use]
pub fn is_markdown_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let trimmed = cell.trim();
            !trimmed.is_empty() && trimmed.chars().all(|character| matches!(character, ':' | '-'))
        })
}

#[must_use]
pub fn parse_markdown_table_rows(table_text: &str) -> Vec<Vec<String>> {
    table_text
        .lines()
        .map(parse_markdown_table_row)
        .filter(|cells| !cells.is_empty() && !is_markdown_separator_row(cells))
        .collect()
}

#[must_use]
pub fn build_semantic_table_row_text(
    sheet_name: Option<&str>,
    table_name: Option<&str>,
    row_index: usize,
    headers: &[String],
    row: &[String],
) -> String {
    let width = headers.len().max(row.len());
    let normalized_headers = canonicalize_table_headers(headers, width);
    let mut segments = Vec::with_capacity(width.saturating_add(3));
    if let Some(sheet_name) = sheet_name.map(str::trim).filter(|value| !value.is_empty()) {
        segments.push(format!("Sheet: {sheet_name}"));
    }
    if let Some(table_name) = table_name.map(str::trim).filter(|value| !value.is_empty()) {
        segments.push(format!("Table: {table_name}"));
    }
    segments.push(format!("Row {}", row_index + 1));
    for (index, header) in normalized_headers.iter().enumerate() {
        let value = row.get(index).map_or("", |cell| cell.trim());
        if value.is_empty() {
            continue;
        }
        segments.push(format!(
            "{}: {}",
            header.replace("\\|", "|"),
            normalize_table_cell_text(value)
        ));
    }
    segments.join(" | ")
}

#[must_use]
pub fn render_markdown_table(headers: &[String], rows: &[Vec<String>]) -> Option<String> {
    let width = headers.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if width == 0 {
        return None;
    }

    let header_row = canonicalize_table_headers(headers, width);
    let mut lines = Vec::with_capacity(rows.len().saturating_add(2));
    lines.push(markdown_row_line(&header_row));
    lines.push(markdown_row_line(&(0..width).map(|_| "---".to_string()).collect::<Vec<_>>()));
    for row in rows {
        lines.push(markdown_row_line(&padded_markdown_row(row, width)));
    }

    Some(lines.join("\n"))
}

#[must_use]
pub fn render_markdown_table_from_rows(rows: &[Vec<String>]) -> Option<String> {
    if rows.len() < 2 {
        return None;
    }

    render_markdown_table(&rows[0], &rows[1..])
}

#[must_use]
pub fn render_plain_table_rows(rows: &[Vec<String>], separator: &str) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|cell| normalize_table_cell_text(cell))
                .filter(|cell| !cell.is_empty())
                .collect::<Vec<_>>()
                .join(separator)
        })
        .filter(|line| !line.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        build_semantic_table_row_text, is_markdown_separator_row, parse_markdown_table_row,
        parse_markdown_table_rows, render_markdown_table, render_markdown_table_from_rows,
        render_plain_table_rows,
    };

    #[test]
    fn renders_markdown_table_with_synthesized_blank_headers() {
        let table = render_markdown_table(
            &[String::new(), "Value".to_string()],
            &[vec!["Alpha".to_string(), "42".to_string()]],
        )
        .expect("markdown table");

        assert_eq!(table, "| col_1 | Value |\n| --- | --- |\n| Alpha | 42 |");
    }

    #[test]
    fn renders_markdown_table_from_header_row() {
        let table = render_markdown_table_from_rows(&[
            vec!["Name".to_string(), "Value".to_string()],
            vec!["Alpha".to_string(), "42".to_string()],
        ])
        .expect("markdown table");

        assert_eq!(table, "| Name | Value |\n| --- | --- |\n| Alpha | 42 |");
    }

    #[test]
    fn renders_plain_rows_for_non_table_shapes() {
        assert_eq!(
            render_plain_table_rows(&[vec!["Only".to_string(), "Row".to_string()]], " "),
            vec!["Only Row".to_string()]
        );
    }

    #[test]
    fn parses_markdown_table_rows_and_ignores_separator_lines() {
        let rows = parse_markdown_table_rows(
            "| Name | Value |\n| --- | --- |\n| Alpha | 42 |\n| Beta | 7 |",
        );

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec!["Name".to_string(), "Value".to_string()]);
        assert_eq!(rows[2], vec!["Beta".to_string(), "7".to_string()]);
    }

    #[test]
    fn parses_escaped_pipe_cells() {
        let cells = parse_markdown_table_row(r"| A \| B | 42 |");

        assert_eq!(cells, vec!["A | B".to_string(), "42".to_string()]);
        assert!(is_markdown_separator_row(&["---".to_string(), ":---:".to_string()]));
    }

    #[test]
    fn builds_semantic_table_row_text_with_context() {
        let text = build_semantic_table_row_text(
            Some("people"),
            None,
            0,
            &["Name".to_string(), "Email".to_string()],
            &["Alice".to_string(), "alice@example.com".to_string()],
        );

        assert_eq!(text, "Sheet: people | Row 1 | Name: Alice | Email: alice@example.com");
    }
}
