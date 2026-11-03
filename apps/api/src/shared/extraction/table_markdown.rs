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

/// Escapes one value of the semantic row/summary protocol.
///
/// The protocol joins segments with ` | ` and separates a key from its value at
/// the first `: `. A literal backslash, pipe or colon in source text therefore
/// has to travel escaped, otherwise it would split a field or a summary.
#[must_use]
pub fn escape_semantic_segment(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '|' => escaped.push_str("\\|"),
            ':' => escaped.push_str("\\:"),
            ';' => escaped.push_str("\\;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// Decodes one semantic segment value or key.
#[must_use]
pub fn unescape_semantic_segment(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => decoded.push('\\'),
            Some('|') => decoded.push('|'),
            Some(':') => decoded.push(':'),
            Some(';') => decoded.push(';'),
            Some(other) => {
                decoded.push('\\');
                decoded.push(other);
            }
            None => decoded.push('\\'),
        }
    }
    decoded
}

/// Splits a semantic row or summary into raw segments at unescaped ` | `.
#[must_use]
pub fn split_semantic_segments(text: &str) -> Vec<&str> {
    split_unescaped(text, b" | ")
}

/// Splits the nested categorical-summary list before decoding its values.
#[must_use]
pub fn split_semantic_list(text: &str) -> Vec<&str> {
    split_unescaped(text, b"; ")
}

fn split_unescaped<'a>(text: &'a str, separator: &[u8]) -> Vec<&'a str> {
    let bytes = text.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0_usize;
    let mut index = 0_usize;
    while index + separator.len() <= bytes.len() {
        if &bytes[index..index + separator.len()] == separator && !is_escaped_at(bytes, index) {
            segments.push(&text[start..index]);
            start = index + separator.len();
            index = start;
            continue;
        }
        index += 1;
    }
    segments.push(&text[start..]);
    segments
}

/// Splits one semantic segment at the first unescaped `: ` key separator.
#[must_use]
pub fn split_semantic_key_value(segment: &str) -> Option<(&str, &str)> {
    let bytes = segment.as_bytes();
    for index in 0..bytes.len().saturating_sub(1) {
        if bytes[index] == b':' && bytes[index + 1] == b' ' && !is_escaped_at(bytes, index) {
            return Some((&segment[..index], &segment[index + 2..]));
        }
    }
    None
}

fn is_escaped_at(bytes: &[u8], index: usize) -> bool {
    let mut backslashes = 0_usize;
    let mut cursor = index;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        backslashes += 1;
        cursor -= 1;
    }
    backslashes % 2 == 1
}

fn escape_markdown_table_cell(value: &str) -> String {
    normalize_table_cell_text(value)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\\', "\\\\")
        .replace('|', "\\|")
}

fn decode_markdown_cell(value: &str) -> String {
    const MARKUP: [(&str, &str); 8] = [
        ("<br>", "\n"),
        ("<br />", "\n"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#10;", "\n"),
        ("&#13;", "\r"),
        ("&amp;", "&"),
    ];
    let mut remaining = value.trim();
    let mut decoded = String::with_capacity(remaining.len());
    while let Some(character) = remaining.chars().next() {
        if character == '\\'
            && let Some(escaped) = remaining[1..].chars().next()
            && escaped.is_ascii_punctuation()
        {
            // Escaped tags/entities are literal text, not a second markup pass.
            decoded.push(escaped);
            remaining = &remaining[1 + escaped.len_utf8()..];
        } else if let Some((encoded, literal)) =
            MARKUP.iter().find(|(encoded, _)| remaining.starts_with(*encoded))
        {
            decoded.push_str(literal);
            remaining = &remaining[encoded.len()..];
        } else {
            decoded.push(character);
            remaining = &remaining[character.len_utf8()..];
        }
    }
    decoded
}

fn padded_markdown_row(cells: &[String], width: usize) -> Vec<String> {
    let mut padded = cells.iter().map(|cell| escape_markdown_table_cell(cell)).collect::<Vec<_>>();
    padded.resize(width, String::new());
    padded
}

/// Header labels as literal text, with the existing `col_N` fallback for a
/// missing Markdown header. Nothing is escaped here: this is the plain value
/// consumed by summaries and the semantic row protocol.
#[must_use]
pub fn plain_table_headers(headers: &[String], width: usize) -> Vec<String> {
    (0..width)
        .map(|index| {
            headers
                .get(index)
                .map(|cell| normalize_table_cell_text(cell))
                .filter(|cell| !cell.is_empty())
                .unwrap_or_else(|| format!("col_{}", index + 1))
        })
        .collect()
}

#[must_use]
pub fn normalize_table_headers(headers: &[String], width: usize) -> Vec<String> {
    plain_table_headers(headers, width)
        .iter()
        .map(|cell| escape_markdown_table_cell(cell))
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
            current.push('\\');
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '|' => {
                cells.push(decode_markdown_cell(&current));
                current.clear();
            }
            _ => current.push(character),
        }
    }
    if escaped {
        current.push('\\');
    }
    cells.push(decode_markdown_cell(&current));
    cells
}

/// Tests cell shape, not the source row's role in a table.
#[must_use]
pub fn is_markdown_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let trimmed = cell.trim();
            !trimmed.is_empty() && trimmed.chars().all(|character| matches!(character, ':' | '-'))
        })
}

/// Recognizes the unescaped formatting delimiter after a table's first row.
/// Later dash/colon-only rows are physical data, not additional delimiters.
#[must_use]
pub fn is_markdown_table_delimiter(row_index: usize, row_text: &str) -> bool {
    if row_index != 1 {
        return false;
    }
    let trimmed = row_text.trim();
    if !trimmed.contains('|') {
        return false;
    }
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').all(|cell| {
        let cell = cell.trim();
        let cell = cell.strip_prefix(':').unwrap_or(cell);
        let cell = cell.strip_suffix(':').unwrap_or(cell);
        !cell.is_empty() && cell.chars().all(|character| character == '-')
    })
}

#[must_use]
pub fn parse_markdown_table_rows(table_text: &str) -> Vec<Vec<String>> {
    table_text
        .lines()
        .filter(|line| line.contains('|'))
        .enumerate()
        .filter(|(index, line)| !is_markdown_table_delimiter(*index, line))
        .map(|(_, line)| parse_markdown_table_row(line))
        .filter(|cells| !cells.is_empty())
        .collect()
}

/// Source names, when present, and an independent revision-local table identity.
/// `table_ref` is the owning Table block ordinal, never a caption-derived name.
#[derive(Debug, Default, Clone, Copy)]
pub struct SemanticTableContext<'a> {
    pub sheet_name: Option<&'a str>,
    pub table_name: Option<&'a str>,
    pub table_ref: Option<i32>,
}

impl SemanticTableContext<'_> {
    #[must_use]
    pub fn segments(self) -> Vec<String> {
        let mut segments = Vec::new();
        for (label, value) in [("Sheet", self.sheet_name), ("Table", self.table_name)] {
            if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
                segments.push(format!("{label}: {}", escape_semantic_segment(value)));
            }
        }
        if let Some(table_ref) = self.table_ref {
            segments.push(format!("Table Ref: {table_ref}"));
        }
        segments
    }

    /// Encoded metadata upper bound including each trailing separator and the
    /// maximum final ordinal width after code/record splitting.
    /// Used before allocation by the logical projection's output budget.
    #[must_use]
    pub fn encoded_prefix_len(self) -> usize {
        let mut length = 0_usize;
        for (label, value) in [("Sheet", self.sheet_name), ("Table", self.table_name)] {
            if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
                length = length
                    .saturating_add(label.len() + 2 + 3)
                    .saturating_add(semantic_encoded_len(value));
            }
        }
        if self.table_ref.is_some() {
            // Splitting preceding code/record blocks can widen the final
            // ordinal. Reserve the full i32 width before allocating rows.
            length = length.saturating_add("Table Ref: ".len() + 11 + 3);
        }
        length
    }

    #[must_use]
    pub fn row_prefix_len(self, row_index: usize) -> usize {
        self.encoded_prefix_len().saturating_add(4 + row_index.saturating_add(1).to_string().len())
    }
}

/// Rebinds only the structural source reference, before Row/Column fields.
/// Literal cell/header values remain byte-for-byte encoded exactly once.
#[must_use]
pub fn rebind_semantic_table_ref(text: &str, table_ref: i32) -> Option<std::borrow::Cow<'_, str>> {
    if table_ref < 0 {
        return None;
    }
    let mut offset = 0;
    for segment in split_semantic_segments(text) {
        if segment.starts_with("Row ") || segment.starts_with("Column: ") {
            return None;
        }
        if let Some(value) = segment.strip_prefix("Table Ref: ") {
            let previous = value.parse::<i32>().ok().filter(|value| *value >= 0)?;
            if previous == table_ref {
                return Some(std::borrow::Cow::Borrowed(text));
            }
            let mut rebound = String::new();
            rebound.push_str(&text[..offset]);
            rebound.push_str(&format!("Table Ref: {table_ref}"));
            rebound.push_str(&text[offset + segment.len()..]);
            return Some(std::borrow::Cow::Owned(rebound));
        }
        offset += segment.len() + 3;
    }
    None
}

/// The escaped byte length, without allocating a second copy of source text.
#[must_use]
pub fn semantic_encoded_len(value: &str) -> usize {
    value.len().saturating_add(
        value.bytes().filter(|byte| matches!(byte, b'\\' | b'|' | b':' | b';')).count(),
    )
}

#[must_use]
pub fn build_semantic_table_row_text(
    context: SemanticTableContext<'_>,
    row_index: usize,
    headers: &[String],
    row: &[String],
) -> String {
    let width = headers.len().max(row.len());
    let normalized_headers = plain_table_headers(headers, width);
    let mut segments = context.segments();
    segments.push(format!("Row {}", row_index + 1));
    for (index, header) in normalized_headers.iter().enumerate() {
        let value = row.get(index).map_or("", |cell| cell.trim());
        if value.is_empty() {
            continue;
        }
        segments.push(format!(
            "{}: {}",
            escape_semantic_segment(header.trim()),
            escape_semantic_segment(&normalize_table_cell_text(value))
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

    let header_row = normalize_table_headers(headers, width);
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
        SemanticTableContext, build_semantic_table_row_text, escape_semantic_segment,
        is_markdown_separator_row, is_markdown_table_delimiter, parse_markdown_table_row,
        parse_markdown_table_rows, render_markdown_table, render_markdown_table_from_rows,
        render_plain_table_rows, split_semantic_key_value, split_semantic_segments,
        unescape_semantic_segment,
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
    fn escaped_cell_markup_is_literal_and_actual_markup_decodes_once() {
        assert_eq!(
            parse_markdown_table_row(r"| \<br\> | \&amp; | <br> | &amp; | &amp;lt; |"),
            ["<br>", "&amp;", "\n", "&", "&lt;"]
        );
    }

    #[test]
    fn only_the_positioned_unescaped_table_delimiter_is_removed() {
        let rows = parse_markdown_table_rows(
            "\n\n| key | value |\n| --- | --- |\n| - | - |\n| : | --- |\n| \\- | \\- |",
        );
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1], ["-", "-"]);
        assert_eq!(rows[2], [":", "---"]);
        assert_eq!(rows[3], ["-", "-"]);
        assert!(!is_markdown_table_delimiter(1, r"| \- | \- |"));
        assert!(!is_markdown_table_delimiter(0, "| --- | --- |"));
        assert!(!is_markdown_table_delimiter(2, "| --- | --- |"));
    }

    #[test]
    fn builds_semantic_table_row_text_with_context() {
        let text = build_semantic_table_row_text(
            SemanticTableContext { sheet_name: Some("people"), ..Default::default() },
            0,
            &["Name".to_string(), "Email".to_string()],
            &["Alice".to_string(), "alice@example.com".to_string()],
        );

        assert_eq!(text, "Sheet: people | Row 1 | Name: Alice | Email: alice@example.com");
    }

    #[test]
    fn native_table_consumer_rebinding_preserves_encoded_cells_and_context() {
        let source = build_semantic_table_row_text(
            SemanticTableContext {
                sheet_name: Some("Table Ref: 1 | Row 2"),
                table_ref: Some(1),
                ..Default::default()
            },
            0,
            &["Table Ref".into(), "Other".into()],
            &["literal | 1".into(), r"ns\:key".into()],
        );
        let rebound = super::rebind_semantic_table_ref(&source, 12).expect("reference");
        let expected = build_semantic_table_row_text(
            SemanticTableContext {
                sheet_name: Some("Table Ref: 1 | Row 2"),
                table_ref: Some(12),
                ..Default::default()
            },
            0,
            &["Table Ref".into(), "Other".into()],
            &["literal | 1".into(), r"ns\:key".into()],
        );
        assert_eq!(rebound, expected);
        assert!(super::rebind_semantic_table_ref("Row 1 | Table Ref: literal", 12).is_none());
    }

    #[test]
    fn semantic_segment_codec_round_trips_literal_delimiters() {
        for value in ["A | B", r"C\D", "R&D", "<br>", "<table>", "key: value", "a\\|b: c"] {
            let encoded = escape_semantic_segment(value);
            assert_eq!(unescape_semantic_segment(&encoded), value, "escaped {encoded}");
        }

        let source = build_semantic_table_row_text(
            SemanticTableContext {
                sheet_name: Some("sheet | one"),
                table_name: Some("table: one"),
                table_ref: None,
            },
            0,
            &["Note: type".to_string(), r"Back\slash".to_string()],
            &["A | B".to_string(), r"C\D: E".to_string()],
        );
        let segments = split_semantic_segments(&source);
        assert_eq!(segments.len(), 5);
        let (key, value) = split_semantic_key_value(segments[0].trim()).expect("sheet split");
        assert_eq!((key, unescape_semantic_segment(value)), ("Sheet", "sheet | one".to_string()));
        let (key, value) = split_semantic_key_value(segments[1].trim()).expect("table split");
        assert_eq!((key, unescape_semantic_segment(value)), ("Table", "table: one".to_string()));
        assert_eq!(segments[2].trim(), "Row 1");
        let (key, value) = split_semantic_key_value(segments[3]).expect("field split");
        assert_eq!(unescape_semantic_segment(key), "Note: type");
        assert_eq!(unescape_semantic_segment(value), "A | B");
        let (key, value) = split_semantic_key_value(segments[4]).expect("field split");
        assert_eq!(unescape_semantic_segment(key), r"Back\slash");
        assert_eq!(unescape_semantic_segment(value), r"C\D: E");
    }

    #[test]
    fn markdown_cells_round_trip_literal_backslashes_and_pipes() {
        let table = render_markdown_table(
            &["Header | one".to_string(), r"Header\two".to_string()],
            &[vec!["A | B".to_string(), r"C\D".to_string()]],
        )
        .expect("markdown table");
        let rows = parse_markdown_table_rows(&table);
        assert_eq!(rows[0], vec!["Header | one".to_string(), r"Header\two".to_string()]);
        assert_eq!(rows[1], vec!["A | B".to_string(), r"C\D".to_string()]);
    }
}
