use super::XbergExtractionError;
use crate::shared::extraction::{
    ExtractionLineSignal, infer_line_signals, is_canonical_table_start,
    table_logical::LogicalTable,
    table_markdown::{is_markdown_table_delimiter, parse_markdown_table_row},
};

/// Native PDF pipe separators are extractor formatting, not source-header
/// declarations. Keep all rows, without inferring cross-fragment identity.
pub(super) fn project_pdf_table_lines(
    content: &str,
    max_bytes: usize,
) -> Result<Vec<String>, XbergExtractionError> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let source = normalized.split('\n').collect::<Vec<_>>();
    let mut lines = Vec::new();
    let mut index = 0;
    let mut in_code = false;
    let mut in_canonical = false;
    let mut used = 0_usize;
    while index < source.len() {
        let line = source[index];
        let signals = infer_line_signals(line);
        if signals.contains(&ExtractionLineSignal::CodeFence) {
            in_code = !in_code;
        }
        in_canonical |= !in_code && is_canonical_table_start(line);
        if in_code || in_canonical || !plain_table_row(line) {
            charge(
                &mut used,
                line.len().saturating_add(usize::from(!lines.is_empty())),
                max_bytes,
            )?;
            lines.push(line.to_owned());
            if in_canonical && line.trim() == "</table>" {
                in_canonical = false;
            }
            index += 1;
            continue;
        }
        let start = index;
        let mut rows = Vec::new();
        while index < source.len() && plain_table_row(source[index]) {
            if !is_markdown_table_delimiter(index - start, source[index]) {
                rows.push(parse_markdown_table_row(source[index]));
            }
            index += 1;
        }
        if rows.is_empty() {
            for line in &source[start..index] {
                charge(
                    &mut used,
                    line.len().saturating_add(usize::from(!lines.is_empty())),
                    max_bytes,
                )?;
                lines.push((*line).to_owned());
            }
            continue;
        }
        let table = LogicalTable::from_unknown_rows(&rows)
            .map_err(|error| XbergExtractionError::Contract(error.to_string()))?;
        let separator = usize::from(!lines.is_empty());
        let rendered = table
            .render_canonical_bounded(max_bytes.saturating_sub(used).saturating_sub(separator))
            .map_err(|_| {
                XbergExtractionError::Contract("projected content budget exceeded".into())
            })?;
        charge(&mut used, rendered.len().saturating_add(separator), max_bytes)?;
        lines.extend(rendered.lines().map(str::to_owned));
    }
    Ok(lines)
}

fn charge(used: &mut usize, bytes: usize, limit: usize) -> Result<(), XbergExtractionError> {
    *used = used.checked_add(bytes).filter(|total| *total <= limit).ok_or_else(|| {
        XbergExtractionError::Contract("projected content budget exceeded".into())
    })?;
    Ok(())
}

fn plain_table_row(line: &str) -> bool {
    !is_canonical_table_start(line)
        && infer_line_signals(line).contains(&ExtractionLineSignal::TableRow)
}
