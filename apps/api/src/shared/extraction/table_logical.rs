//! Canonical logical table model shared by the content consumer.
//!
//! One table shape serves both Markdown pipe tables and the canonical
//! HTML-in-Markdown form produced from model-predicted logical cells. The
//! canonical form is deliberately narrow because it is a product contract, not
//! a general HTML surface:
//!
//! ```text
//! <table data-irg-header-policy="anchor-band-derived-v1" data-irg-rows="5" data-irg-columns="3">
//! <tr><td data-irg-anchor="a0" rowspan="1" colspan="2">Inventory</td><td data-irg-anchor="a1" rowspan="1" colspan="1">Location</td></tr>
//! ...
//! </table>
//! ```
//!
//! The parser accepts only that exact topology. It does not run an HTML parser,
//! so it cannot repair malformed markup, and it never treats escaped or inline
//! tags as structure. Cell text is entity-decoded exactly once.
//!
//! Header roles are not available from the extractor. The declared policy is
//! derived, not a model label: a leading complete nonempty singleton row is the
//! conventional header, and leading spanning rows form a header band that ends
//! with the first such row. Every column path comes from the distinct source
//! anchors covering that column in the band; a missing, crossing or ambiguous
//! band is an error rather than a guess. Native PDF pipe fragments instead use
//! `unknown` roles: every physical row is retained, with positional column labels
//! and no claimed header anchors.

use std::collections::HashSet;

use thiserror::Error;

/// The canonical policy for source-anchor-derived header bands.
pub const CANONICAL_HEADER_POLICY: &str = "anchor-band-derived-v1";
/// Source header roles are unavailable; no physical row is consumed as a header.
pub const UNKNOWN_HEADER_POLICY: &str = "unknown";

const MAX_DIMENSION: usize = 100_000;
const MAX_SLOTS: usize = 1_000_000;
const MAX_ANCHOR_CHARS: usize = 64;
// Reject unbounded derived header repetition before allocating its copies.
const MAX_PROJECTION_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TableLayoutError {
    #[error("canonical table markup is malformed at table line {line}: {detail}")]
    MalformedMarkup { line: usize, detail: String },
    #[error("canonical table has no rows or columns")]
    EmptyTable,
    #[error("canonical table declares {declared} rows but contains {actual}")]
    RowCountMismatch { declared: usize, actual: usize },
    #[error("table is not rectangular: row {row} column {column} is not covered")]
    RaggedTable { row: usize, column: usize },
    #[error("cell {anchor} spans outside the table at row {row} column {column}")]
    OutOfBounds { anchor: String, row: usize, column: usize },
    #[error("cell {anchor} has a zero span")]
    ZeroSpan { anchor: String },
    #[error("table repeats source anchor {anchor}")]
    DuplicateAnchor { anchor: String },
    #[error("table covers slot row {row} column {column} more than once")]
    DuplicateCoveredSlot { row: usize, column: usize },
    #[error("table has no leading header band ending in a complete nonempty row")]
    MissingHeaderBand,
    #[error("header cell {anchor} is empty")]
    EmptyHeaderCell { anchor: String },
    #[error("header cell {anchor} crosses the header band boundary")]
    CrossingHeaderSpan { anchor: String },
    #[error("header band is ambiguous at column {column}: {detail}")]
    AmbiguousHeaderBand { column: usize, detail: String },
    #[error("header band has no source anchor for column {column}")]
    MissingHeaderAnchor { column: usize },
    #[error("body cell {anchor} at row {row} has a span; body-span projection is unsupported")]
    UnsupportedBodySpan { row: usize, anchor: String },
    #[error("derived table projection exceeds its bounded text budget")]
    ProjectionLimit,
    #[error("derived table text does not reference its source Table parent")]
    InvalidSourceReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableHeaderOrigin {
    /// Markdown pipe tables carry the existing convention: the first row is the
    /// declared header.
    DeclaredFirstRow,
    /// Canonical HTML carries spans and header paths derived from source anchors.
    DerivedAnchorBand,
    /// Header roles are unavailable, not evidence that the table has no headers.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalCell {
    anchor: String,
    row: usize,
    column: usize,
    row_span: usize,
    column_span: usize,
    text: String,
}

impl LogicalCell {
    #[must_use]
    pub fn anchor(&self) -> &str {
        &self.anchor
    }

    #[must_use]
    pub const fn row(&self) -> usize {
        self.row
    }

    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }

    #[must_use]
    pub const fn row_span(&self) -> usize {
        self.row_span
    }

    #[must_use]
    pub const fn column_span(&self) -> usize {
        self.column_span
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// One unique source anchor; covered slots never copy its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalCellDraft {
    pub anchor: Option<String>,
    pub row: usize,
    pub column: usize,
    pub row_span: usize,
    pub column_span: usize,
    pub text: String,
}

impl LogicalCellDraft {
    #[must_use]
    pub fn new(row: usize, column: usize, text: impl Into<String>) -> Self {
        Self { anchor: None, row, column, row_span: 1, column_span: 1, text: text.into() }
    }

    #[must_use]
    pub fn with_anchor(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    #[must_use]
    pub const fn with_spans(mut self, row_span: usize, column_span: usize) -> Self {
        self.row_span = row_span;
        self.column_span = column_span;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderPath {
    /// Literal anchor texts in band order, without copied cell values.
    pub segments: Vec<String>,
    /// Source anchor ids the path is derived from.
    pub anchors: Vec<String>,
    pub display: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalHeader {
    pub origin: TableHeaderOrigin,
    pub paths: Vec<HeaderPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableProjection {
    pub header: LogicalHeader,
    pub header_row_count: usize,
    /// Body values per column. Body spans are refused before rows are produced,
    /// so no value is duplicated across slots or into summaries.
    pub body_rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalTable {
    rows: usize,
    columns: usize,
    cells: Vec<LogicalCell>,
    /// Slot to cell index. Times dimensions; the values live once in `cells`.
    slots: Vec<Option<usize>>,
    origin: TableHeaderOrigin,
}

impl LogicalTable {
    /// Builds a canonical (derived-header) table from drafts.
    ///
    /// # Errors
    ///
    /// Returns a [`TableLayoutError`] for zero spans, out-of-bounds cells,
    /// duplicate anchors, overlapping slots or uncovered slots.
    pub fn from_cells(
        rows: usize,
        columns: usize,
        drafts: Vec<LogicalCellDraft>,
    ) -> Result<Self, TableLayoutError> {
        Self::assemble(rows, columns, drafts, TableHeaderOrigin::DerivedAnchorBand)
    }

    /// Builds the Markdown convention: the first row is the declared header.
    ///
    /// Short rows are padded with empty cells, matching the existing consumer
    /// contract for pipe tables.
    ///
    /// # Errors
    ///
    /// Returns a [`TableLayoutError`] when the input has no nonempty row.
    pub fn from_markdown_rows(rows: &[Vec<String>]) -> Result<Self, TableLayoutError> {
        Self::from_rows(rows, TableHeaderOrigin::DeclaredFirstRow)
    }

    /// Retains every native row without assigning source-header roles.
    ///
    /// # Errors
    ///
    /// Returns a [`TableLayoutError`] for empty or oversized input.
    pub fn from_unknown_rows(rows: &[Vec<String>]) -> Result<Self, TableLayoutError> {
        Self::from_rows(rows, TableHeaderOrigin::Unknown)
    }

    fn from_rows(
        rows: &[Vec<String>],
        origin: TableHeaderOrigin,
    ) -> Result<Self, TableLayoutError> {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        checked_slot_count(rows.len(), columns)?;
        let drafts = rows
            .iter()
            .enumerate()
            .flat_map(|(row, values)| {
                (0..columns).map(move |column| {
                    LogicalCellDraft::new(
                        row,
                        column,
                        values.get(column).cloned().unwrap_or_default(),
                    )
                })
            })
            .collect();
        Self::assemble(rows.len(), columns, drafts, origin)
    }

    /// Parses the canonical HTML-in-Markdown table block.
    ///
    /// `lines` are the physical lines of one table, from the opening tag to the
    /// closing tag. Nothing outside that block is read.
    ///
    /// # Errors
    ///
    /// Returns a [`TableLayoutError`] for any markup that is not exactly the
    /// canonical form, and for invalid, overlapping, out-of-bounds or ragged
    /// topology.
    pub fn parse_canonical_lines(lines: &[&str]) -> Result<Self, TableLayoutError> {
        if lines.len() < 3 {
            return Err(TableLayoutError::MalformedMarkup {
                line: 1,
                detail: "a canonical table needs an opening tag, a row and a closing tag"
                    .to_string(),
            });
        }
        let opening = lines[0].trim();
        let (declared_rows, declared_columns, origin) = parse_opening(opening)?;
        let closing = lines[lines.len() - 1].trim();
        if closing != "</table>" {
            return Err(TableLayoutError::MalformedMarkup {
                line: lines.len(),
                detail: "expected the closing table tag".to_string(),
            });
        }
        let row_lines = &lines[1..lines.len() - 1];
        if row_lines.len() != declared_rows {
            return Err(TableLayoutError::RowCountMismatch {
                declared: declared_rows,
                actual: row_lines.len(),
            });
        }

        let slot_count = checked_slot_count(declared_rows, declared_columns)?;
        let mut occupied = vec![false; slot_count];
        let mut drafts = Vec::new();
        for (row, line) in row_lines.iter().enumerate() {
            let cells = parse_row(line.trim(), row + 2)?;
            let mut column = 0_usize;
            for (anchor, row_span, column_span, text) in cells {
                // Omitted slots remain owned by their earlier rowspan anchor.
                while column < declared_columns && occupied[row * declared_columns + column] {
                    column += 1;
                }
                if column >= declared_columns
                    || row_span > declared_rows - row
                    || column_span > declared_columns - column
                {
                    return Err(TableLayoutError::OutOfBounds { anchor, row, column });
                }
                for covered_row in row..row + row_span {
                    for covered_column in column..column + column_span {
                        let slot = covered_row * declared_columns + covered_column;
                        if std::mem::replace(&mut occupied[slot], true) {
                            return Err(TableLayoutError::DuplicateCoveredSlot {
                                row: covered_row,
                                column: covered_column,
                            });
                        }
                    }
                }
                drafts.push(LogicalCellDraft {
                    anchor: Some(anchor),
                    row,
                    column,
                    row_span,
                    column_span,
                    text,
                });
                column += column_span;
            }
        }
        Self::assemble(declared_rows, declared_columns, drafts, origin)
    }

    fn assemble(
        rows: usize,
        columns: usize,
        drafts: Vec<LogicalCellDraft>,
        origin: TableHeaderOrigin,
    ) -> Result<Self, TableLayoutError> {
        let slot_count = checked_slot_count(rows, columns)?;

        let mut cells = Vec::with_capacity(drafts.len());
        let mut anchors = HashSet::with_capacity(drafts.len());
        let mut slots = vec![None; slot_count];

        for draft in drafts {
            let anchor = draft.anchor.unwrap_or_else(|| format!("c{}-{}", draft.row, draft.column));
            if anchor.is_empty()
                || anchor.len() > MAX_ANCHOR_CHARS
                || !anchor
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(TableLayoutError::MalformedMarkup {
                    line: 1,
                    detail: "source anchors must be 1..=64 of [A-Za-z0-9_-]".to_string(),
                });
            }
            if !anchors.insert(anchor.clone()) {
                return Err(TableLayoutError::DuplicateAnchor { anchor });
            }
            if draft.row_span == 0 || draft.column_span == 0 {
                return Err(TableLayoutError::ZeroSpan { anchor });
            }
            let cell = LogicalCell {
                anchor,
                row: draft.row,
                column: draft.column,
                row_span: draft.row_span,
                column_span: draft.column_span,
                text: draft.text,
            };
            if cell.row >= rows
                || cell.column >= columns
                || cell.row_span > rows - cell.row
                || cell.column_span > columns - cell.column
            {
                return Err(TableLayoutError::OutOfBounds {
                    anchor: cell.anchor.clone(),
                    row: cell.row,
                    column: cell.column,
                });
            }
            let cell_index = cells.len();
            for slot_row in cell.row..cell.row + cell.row_span {
                for slot_column in cell.column..cell.column + cell.column_span {
                    let slot_index = slot_row * columns + slot_column;
                    if slots[slot_index].replace(cell_index).is_some() {
                        return Err(TableLayoutError::DuplicateCoveredSlot {
                            row: slot_row,
                            column: slot_column,
                        });
                    }
                }
            }
            cells.push(cell);
        }

        for row in 0..rows {
            for column in 0..columns {
                if slots[row * columns + column].is_none() {
                    return Err(TableLayoutError::RaggedTable { row, column });
                }
            }
        }

        Ok(Self { rows, columns, cells, slots, origin })
    }

    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    #[must_use]
    pub fn cells(&self) -> &[LogicalCell] {
        &self.cells
    }

    #[must_use]
    pub fn cell_by_anchor(&self, anchor: &str) -> Option<&LogicalCell> {
        self.cells.iter().find(|cell| cell.anchor == anchor)
    }

    #[must_use]
    pub fn cell_at(&self, row: usize, column: usize) -> Option<&LogicalCell> {
        if row >= self.rows || column >= self.columns {
            return None;
        }
        self.slots[row * self.columns + column].map(|index| &self.cells[index])
    }

    #[must_use]
    pub fn render_canonical(&self) -> String {
        self.render_canonical_bounded(usize::MAX).expect("unbounded String rendering")
    }

    /// Renders the same canonical contract without exceeding an admitted budget.
    ///
    /// # Errors
    ///
    /// Returns [`TableLayoutError::ProjectionLimit`] before appending excess text.
    pub fn render_canonical_bounded(&self, max_bytes: usize) -> Result<String, TableLayoutError> {
        let mut output = BoundedTableText { text: String::new(), max_bytes };
        self.write_canonical(&mut output).map_err(|_| TableLayoutError::ProjectionLimit)?;
        Ok(output.text)
    }

    fn write_canonical(&self, output: &mut impl std::fmt::Write) -> std::fmt::Result {
        let policy = match self.origin {
            TableHeaderOrigin::Unknown => UNKNOWN_HEADER_POLICY,
            TableHeaderOrigin::DeclaredFirstRow | TableHeaderOrigin::DerivedAnchorBand => {
                CANONICAL_HEADER_POLICY
            }
        };
        writeln!(
            output,
            "<table data-irg-header-policy=\"{policy}\" data-irg-rows=\"{}\" data-irg-columns=\"{}\">",
            self.rows, self.columns
        )?;
        for row in 0..self.rows {
            output.write_str("<tr>")?;
            for column in 0..self.columns {
                let cell = self.cell_at(row, column).expect("validated table topology");
                if cell.row != row || cell.column != column {
                    continue;
                }
                write!(
                    output,
                    "<td data-irg-anchor=\"{}\" rowspan=\"{}\" colspan=\"{}\">",
                    cell.anchor, cell.row_span, cell.column_span
                )?;
                write_entity_text(output, &cell.text)?;
                output.write_str("</td>")?;
            }
            output.write_str("</tr>\n")?;
        }
        output.write_str("</table>")
    }

    /// Projects header paths and body rows through the declared header policy.
    ///
    /// # Errors
    ///
    /// Returns a [`TableLayoutError`] when the header band is missing, crossing
    /// or ambiguous, or when a body cell carries a span. Body spans are refused
    /// instead of being expanded, so no observation is duplicated.
    pub fn project(
        &self,
        context: super::table_markdown::SemanticTableContext<'_>,
    ) -> Result<TableProjection, TableLayoutError> {
        let (header_row_count, origin) = match self.origin {
            TableHeaderOrigin::DeclaredFirstRow => (1, TableHeaderOrigin::DeclaredFirstRow),
            TableHeaderOrigin::DerivedAnchorBand => {
                (self.derive_header_band_end()? + 1, TableHeaderOrigin::DerivedAnchorBand)
            }
            TableHeaderOrigin::Unknown => (0, TableHeaderOrigin::Unknown),
        };

        for cell in &self.cells {
            if cell.row < header_row_count {
                if cell.row + cell.row_span > header_row_count {
                    return Err(TableLayoutError::CrossingHeaderSpan {
                        anchor: cell.anchor.clone(),
                    });
                }
                if origin == TableHeaderOrigin::DerivedAnchorBand && cell.text.trim().is_empty() {
                    return Err(TableLayoutError::EmptyHeaderCell { anchor: cell.anchor.clone() });
                }
            } else if cell.row_span > 1 || cell.column_span > 1 {
                return Err(TableLayoutError::UnsupportedBodySpan {
                    row: cell.row,
                    anchor: cell.anchor.clone(),
                });
            }
        }

        if origin == TableHeaderOrigin::DerivedAnchorBand {
            self.validate_header_band(header_row_count)?;
        }

        self.check_projection_budget(header_row_count, origin, context)?;

        let paths = (0..self.columns)
            .map(|column| self.header_path(column, header_row_count, origin))
            .collect::<Result<Vec<_>, _>>()?;

        let body_rows = (header_row_count..self.rows)
            .map(|row| {
                (0..self.columns)
                    .map(|column| {
                        self.cell_at(row, column).map_or_else(String::new, |cell| cell.text.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        Ok(TableProjection { header: LogicalHeader { origin, paths }, header_row_count, body_rows })
    }

    fn check_projection_budget(
        &self,
        header_rows: usize,
        origin: TableHeaderOrigin,
        context: super::table_markdown::SemanticTableContext<'_>,
    ) -> Result<(), TableLayoutError> {
        use super::table_markdown::semantic_encoded_len;
        let mut used = 0_usize;
        let mut header_bytes = Vec::with_capacity(self.columns);
        for column in 0..self.columns {
            let mut previous = None;
            let mut bytes = if origin == TableHeaderOrigin::Unknown {
                format!("col_{}", column + 1).len()
            } else {
                0_usize
            };
            for row in 0..header_rows {
                let cell = self
                    .cell_at(row, column)
                    .ok_or(TableLayoutError::MissingHeaderAnchor { column })?;
                if previous == Some(cell.anchor.as_str()) {
                    continue;
                }
                if previous.is_some() {
                    bytes = bytes.saturating_add(3);
                }
                bytes = bytes.saturating_add(
                    if cell.text.trim().is_empty() && origin == TableHeaderOrigin::DeclaredFirstRow
                    {
                        format!("col_{}", column + 1).len()
                    } else {
                        semantic_encoded_len(cell.text.trim())
                    },
                );
                previous = Some(cell.anchor.as_str());
            }
            // Header paths keep segments/display and the emitter normalizes a
            // copy. Charge those, plus summary metadata, before cloning text.
            charge_projection(&mut used, bytes.saturating_mul(3))?;
            charge_projection(&mut used, context.encoded_prefix_len().saturating_add(1024))?;
            header_bytes.push(bytes);
        }
        for row in header_rows..self.rows {
            charge_projection(&mut used, context.row_prefix_len(row - header_rows))?;
            for (column, header_bytes) in header_bytes.iter().enumerate() {
                let cell = self
                    .cell_at(row, column)
                    .ok_or(TableLayoutError::RaggedTable { row, column })?;
                charge_projection(&mut used, cell.text.len())?;
                if !cell.text.trim().is_empty() {
                    // Each nonempty field repeats its ENCODED header. The
                    // additional value copy also bounds categorical summaries.
                    let value_bytes = semantic_encoded_len(cell.text.trim());
                    charge_projection(
                        &mut used,
                        3_usize
                            .saturating_add(*header_bytes)
                            .saturating_add(2)
                            .saturating_add(value_bytes.saturating_mul(2)),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn derive_header_band_end(&self) -> Result<usize, TableLayoutError> {
        for row in 0..self.rows {
            if self.row_is_complete_nonempty_singleton(row) {
                return Ok(row);
            }
        }
        Err(TableLayoutError::MissingHeaderBand)
    }

    fn row_is_complete_nonempty_singleton(&self, row: usize) -> bool {
        (0..self.columns).all(|column| {
            self.cell_at(row, column).is_some_and(|cell| {
                cell.row == row
                    && cell.column == column
                    && cell.row_span == 1
                    && cell.column_span == 1
                    && !cell.text.trim().is_empty()
            })
        })
    }

    fn validate_header_band(&self, header_row_count: usize) -> Result<(), TableLayoutError> {
        let mut band =
            self.cells.iter().filter(|cell| cell.row < header_row_count).collect::<Vec<_>>();
        band.sort_by_key(|cell| {
            (cell.column, std::cmp::Reverse(cell.column + cell.column_span), cell.row)
        });
        let mut nested: Vec<&LogicalCell> = Vec::new();
        for cell in band {
            while nested
                .last()
                .is_some_and(|previous| cell.column >= previous.column + previous.column_span)
            {
                nested.pop();
            }
            if let Some(previous) = nested.last()
                && cell.column + cell.column_span > previous.column + previous.column_span
            {
                return Err(TableLayoutError::AmbiguousHeaderBand {
                    column: cell.column + 1,
                    detail: format!(
                        "header spans {} and {} cross column boundaries",
                        previous.anchor, cell.anchor
                    ),
                });
            }
            nested.push(cell);
        }
        Ok(())
    }

    fn header_path(
        &self,
        column: usize,
        header_row_count: usize,
        origin: TableHeaderOrigin,
    ) -> Result<HeaderPath, TableLayoutError> {
        if origin == TableHeaderOrigin::Unknown {
            return Ok(HeaderPath {
                segments: Vec::new(),
                anchors: Vec::new(),
                display: format!("col_{}", column + 1),
            });
        }
        let mut anchors = Vec::<&LogicalCell>::new();
        for row in 0..header_row_count {
            let Some(cell) = self.cell_at(row, column) else {
                return Err(TableLayoutError::MissingHeaderAnchor { column });
            };
            if anchors.last().is_some_and(|last| last.anchor == cell.anchor) {
                continue;
            }
            if anchors.iter().any(|seen| seen.anchor == cell.anchor) {
                return Err(TableLayoutError::AmbiguousHeaderBand {
                    column,
                    detail: format!("source anchor {} appears twice in the path", cell.anchor),
                });
            }
            anchors.push(cell);
        }
        if anchors.is_empty() {
            return Err(TableLayoutError::MissingHeaderAnchor { column });
        }

        if origin == TableHeaderOrigin::DeclaredFirstRow {
            let cell = anchors[0];
            let display = if cell.text.trim().is_empty() {
                format!("col_{}", column + 1)
            } else {
                cell.text.trim().to_string()
            };
            return Ok(HeaderPath {
                segments: vec![display.clone()],
                anchors: vec![cell.anchor.clone()],
                display,
            });
        }

        let segments = anchors.iter().map(|cell| cell.text.trim().to_string()).collect::<Vec<_>>();
        if segments.iter().any(String::is_empty) {
            return Err(TableLayoutError::MissingHeaderAnchor { column });
        }
        Ok(HeaderPath {
            anchors: anchors.iter().map(|cell| cell.anchor.clone()).collect(),
            display: segments.join(" / "),
            segments,
        })
    }
}

fn checked_slot_count(rows: usize, columns: usize) -> Result<usize, TableLayoutError> {
    if rows == 0 || columns == 0 {
        return Err(TableLayoutError::EmptyTable);
    }
    if rows > MAX_DIMENSION || columns > MAX_DIMENSION {
        return Err(TableLayoutError::MalformedMarkup {
            line: 1,
            detail: "table dimensions exceed the supported bound".to_string(),
        });
    }
    rows.checked_mul(columns).filter(|count| *count <= MAX_SLOTS).ok_or_else(|| {
        TableLayoutError::MalformedMarkup {
            line: 1,
            detail: "table slot count exceeds the supported bound".to_string(),
        }
    })
}

fn charge_projection(used: &mut usize, bytes: usize) -> Result<(), TableLayoutError> {
    *used = used
        .checked_add(bytes)
        .filter(|total| *total <= MAX_PROJECTION_BYTES)
        .ok_or(TableLayoutError::ProjectionLimit)?;
    Ok(())
}

fn parse_opening(line: &str) -> Result<(usize, usize, TableHeaderOrigin), TableLayoutError> {
    let malformed =
        |detail: &str| TableLayoutError::MalformedMarkup { line: 1, detail: detail.to_string() };
    let rest = line.strip_prefix("<table data-irg-header-policy=\"").ok_or_else(|| {
        malformed("opening tag must declare the canonical header policy and row count")
    })?;
    let (policy, rest) = rest
        .split_once("\" data-irg-rows=\"")
        .ok_or_else(|| malformed("opening tag must declare the row count"))?;
    let origin = match policy {
        CANONICAL_HEADER_POLICY => TableHeaderOrigin::DerivedAnchorBand,
        UNKNOWN_HEADER_POLICY => TableHeaderOrigin::Unknown,
        _ => return Err(malformed("unsupported canonical header policy")),
    };
    let (rows, rest) = rest
        .split_once("\" data-irg-columns=\"")
        .ok_or_else(|| malformed("opening tag must declare the column count"))?;
    let (columns, rest) =
        rest.split_once("\">").ok_or_else(|| malformed("opening tag is not closed"))?;
    if !rest.is_empty() {
        return Err(malformed("unexpected trailing attributes after the column count"));
    }
    Ok((parse_dimension(rows)?, parse_dimension(columns)?, origin))
}

fn parse_dimension(value: &str) -> Result<usize, TableLayoutError> {
    if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
        return Err(TableLayoutError::MalformedMarkup {
            line: 1,
            detail: "table dimensions must be decimal integers".to_string(),
        });
    }
    let parsed = value.parse::<usize>().map_err(|_| TableLayoutError::MalformedMarkup {
        line: 1,
        detail: "table dimension is out of range".to_string(),
    })?;
    if parsed == 0 || parsed > MAX_DIMENSION {
        return Err(TableLayoutError::MalformedMarkup {
            line: 1,
            detail: "table dimension is outside the supported bound".to_string(),
        });
    }
    Ok(parsed)
}

type ParsedRowCell = (String, usize, usize, String);

fn parse_row(line: &str, line_number: usize) -> Result<Vec<ParsedRowCell>, TableLayoutError> {
    let malformed = |detail: &str| TableLayoutError::MalformedMarkup {
        line: line_number,
        detail: detail.to_string(),
    };
    let inner = line
        .strip_prefix("<tr>")
        .and_then(|rest| rest.strip_suffix("</tr>"))
        .ok_or_else(|| malformed("a table row must be a plain <tr>...</tr> line"))?;
    let mut cells = Vec::new();
    let mut rest = inner;
    while !rest.is_empty() {
        let remainder = rest
            .strip_prefix("<td data-irg-anchor=\"")
            .ok_or_else(|| malformed("every cell must start with the canonical anchor tag"))?;
        let (anchor, remainder) = remainder
            .split_once("\" rowspan=\"")
            .ok_or_else(|| malformed("cell anchor is not followed by a row span"))?;
        if anchor.is_empty()
            || anchor.chars().count() > MAX_ANCHOR_CHARS
            || !anchor.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(malformed("cell anchors must be 1..=64 of [A-Za-z0-9_-]"));
        }
        let (row_span, remainder) = remainder
            .split_once("\" colspan=\"")
            .ok_or_else(|| malformed("cell row span is not followed by a column span"))?;
        let (column_span, remainder) =
            remainder.split_once("\">").ok_or_else(|| malformed("cell span tag is not closed"))?;
        let row_span = parse_span(row_span, line_number)?;
        let column_span = parse_span(column_span, line_number)?;
        let (encoded_text, remainder) =
            remainder.split_once("</td>").ok_or_else(|| malformed("cell text is not closed"))?;
        if encoded_text.contains('<') || encoded_text.contains('>') {
            return Err(malformed("raw angle brackets in cell text must be entity-escaped"));
        }
        cells.push((
            anchor.to_string(),
            row_span,
            column_span,
            decode_entity_text(encoded_text, line_number)?,
        ));
        rest = remainder;
    }
    Ok(cells)
}

fn parse_span(value: &str, line: usize) -> Result<usize, TableLayoutError> {
    if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
        return Err(TableLayoutError::MalformedMarkup {
            line,
            detail: "cell spans must be decimal integers".to_string(),
        });
    }
    let parsed = value.parse::<usize>().map_err(|_| TableLayoutError::MalformedMarkup {
        line,
        detail: "cell span is out of range".to_string(),
    })?;
    if parsed == 0 || parsed > MAX_DIMENSION {
        return Err(TableLayoutError::MalformedMarkup {
            line,
            detail: "cell spans must be positive and within the supported bound".to_string(),
        });
    }
    Ok(parsed)
}

struct BoundedTableText {
    text: String,
    max_bytes: usize,
}

impl std::fmt::Write for BoundedTableText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if text.len() > self.max_bytes.saturating_sub(self.text.len()) {
            return Err(std::fmt::Error);
        }
        self.text.push_str(text);
        Ok(())
    }
}

fn write_entity_text(output: &mut impl std::fmt::Write, text: &str) -> std::fmt::Result {
    for character in text.chars() {
        match character {
            '&' => output.write_str("&amp;"),
            '<' => output.write_str("&lt;"),
            '>' => output.write_str("&gt;"),
            '"' => output.write_str("&quot;"),
            '\n' => output.write_str("&#10;"),
            '\r' => output.write_str("&#13;"),
            _ => output.write_char(character),
        }?;
    }
    Ok(())
}

fn decode_entity_text(text: &str, line: usize) -> Result<String, TableLayoutError> {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        decoded.push_str(&rest[..index]);
        let tail = &rest[index..];
        let (replacement, consumed) = if tail.starts_with("&amp;") {
            ('&', 5)
        } else if tail.starts_with("&lt;") {
            ('<', 4)
        } else if tail.starts_with("&gt;") {
            ('>', 4)
        } else if tail.starts_with("&quot;") {
            ('"', 6)
        } else if tail.starts_with("&#10;") {
            ('\n', 5)
        } else if tail.starts_with("&#13;") {
            ('\r', 5)
        } else {
            return Err(TableLayoutError::MalformedMarkup {
                line,
                detail:
                    "cell text contains an unsupported entity; escape a literal ampersand as &amp;"
                        .to_string(),
            });
        };
        decoded.push(replacement);
        rest = &rest[index + consumed..];
    }
    decoded.push_str(rest);
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::{
        CANONICAL_HEADER_POLICY, LogicalCellDraft, LogicalTable, TableHeaderOrigin,
        TableLayoutError,
    };

    fn merged_table() -> LogicalTable {
        LogicalTable::from_cells(
            5,
            3,
            vec![
                LogicalCellDraft::new(0, 0, "Inventory").with_anchor("inventory").with_spans(1, 2),
                LogicalCellDraft::new(0, 2, "Location").with_anchor("location"),
                LogicalCellDraft::new(1, 0, "Material").with_anchor("material"),
                LogicalCellDraft::new(1, 1, "Stock").with_anchor("stock"),
                LogicalCellDraft::new(1, 2, "Bin").with_anchor("bin"),
                LogicalCellDraft::new(2, 0, "Copper"),
                LogicalCellDraft::new(2, 1, "7"),
                LogicalCellDraft::new(2, 2, "Amber"),
                LogicalCellDraft::new(3, 0, "Silver"),
                LogicalCellDraft::new(3, 1, "19"),
                LogicalCellDraft::new(3, 2, "Violet"),
                LogicalCellDraft::new(4, 0, "Zinc"),
                LogicalCellDraft::new(4, 1, "31"),
                LogicalCellDraft::new(4, 2, "Indigo"),
            ],
        )
        .expect("merged table")
    }

    #[test]
    fn native_table_consumer_sparse_markdown_rejects_before_rectangularization() {
        let mut rows = vec![vec!["H".to_string(); 10_000]];
        rows.extend((1..10_000).map(|_| vec!["x".to_string()]));
        assert!(matches!(
            LogicalTable::from_markdown_rows(&rows),
            Err(TableLayoutError::MalformedMarkup { .. })
        ));
    }

    #[test]
    fn native_table_consumer_repeated_encoded_headers_and_context_are_budgeted() {
        use crate::shared::extraction::table_markdown::SemanticTableContext;
        for (rows, header, context) in [
            (10_000, "a".repeat(1024 * 1024), SemanticTableContext::default()),
            (100, "|".repeat(200_000), SemanticTableContext::default()),
        ] {
            let mut cells = vec![
                LogicalCellDraft::new(0, 0, header.clone()),
                LogicalCellDraft::new(0, 1, header),
            ];
            cells.extend((1..rows).flat_map(|row| {
                [LogicalCellDraft::new(row, 0, "x"), LogicalCellDraft::new(row, 1, "x")]
            }));
            let table = LogicalTable::from_cells(rows, 2, cells).expect("valid bounded input");
            assert!(matches!(table.project(context), Err(TableLayoutError::ProjectionLimit)));
        }
        let cells = (0..10_000).map(|row| LogicalCellDraft::new(row, 0, "x")).collect();
        let table = LogicalTable::from_cells(10_000, 1, cells).expect("context source");
        let name = "n".repeat(8192);
        assert!(matches!(
            table.project(SemanticTableContext {
                sheet_name: Some(&name),
                table_ref: Some(7),
                ..Default::default()
            }),
            Err(TableLayoutError::ProjectionLimit)
        ));
    }

    #[test]
    fn native_table_consumer_budget_counts_only_emitted_nonempty_body_fields() {
        let cells = vec![
            LogicalCellDraft::new(0, 0, "a".repeat(1024 * 1024)),
            LogicalCellDraft::new(1, 0, ""),
            LogicalCellDraft::new(2, 0, " \n "),
        ];
        let table = LogicalTable::from_cells(3, 1, cells).expect("source");
        assert!(table.project(Default::default()).is_ok());
    }

    #[test]
    fn native_table_consumer_header_amplification_is_bounded_before_allocation() {
        let mut cells = vec![LogicalCellDraft::new(0, 0, "a".repeat(32768)).with_spans(1, 4096)];
        cells.extend((0..4096).map(|column| LogicalCellDraft::new(1, column, "leaf")));
        let table = LogicalTable::from_cells(2, 4096, cells).expect("bounded source grid");
        assert!(matches!(
            table.project(Default::default()),
            Err(TableLayoutError::ProjectionLimit)
        ));
    }

    #[test]
    fn native_table_consumer_vertical_span_round_trips_without_duplicating_slots() {
        let table = LogicalTable::from_cells(
            3,
            2,
            vec![
                LogicalCellDraft::new(0, 0, "Group"),
                LogicalCellDraft::new(0, 1, "Value"),
                LogicalCellDraft::new(1, 0, "one\nline\r& text").with_spans(2, 1),
                LogicalCellDraft::new(1, 1, "7"),
                LogicalCellDraft::new(2, 1, "9"),
            ],
        )
        .expect("valid source topology");
        let rendered = table.render_canonical();
        let parsed = LogicalTable::parse_canonical_lines(&rendered.lines().collect::<Vec<_>>())
            .expect("rowspan topology round trip");
        assert_eq!(parsed.cells(), table.cells());
        assert_eq!(parsed.cell_at(2, 0).unwrap().text(), "one\nline\r& text");
        assert_eq!(parsed.cell_at(2, 1).unwrap().text(), "9");
        assert!(matches!(
            parsed.project(Default::default()),
            Err(TableLayoutError::UnsupportedBodySpan { .. })
        ));
    }

    #[test]
    fn native_table_consumer_constructor_refuses_overflow_and_attribute_injection() {
        for cell in [
            LogicalCellDraft::new(usize::MAX, 0, "x"),
            LogicalCellDraft::new(0, 0, "x").with_spans(usize::MAX, 1),
            LogicalCellDraft::new(0, 0, "x").with_anchor("x\" onclick=\"evil"),
        ] {
            assert!(LogicalTable::from_cells(1, 1, vec![cell]).is_err());
        }
    }

    #[test]
    fn native_table_consumer_crossing_rowspan_slots_are_not_repaired() {
        let lines = [
            "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"2\" data-irg-columns=\"2\">",
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\">A</td><td data-irg-anchor=\"b\" rowspan=\"2\" colspan=\"1\">B</td></tr>",
            "<tr><td data-irg-anchor=\"c\" rowspan=\"1\" colspan=\"2\">cross</td></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&lines),
            Err(TableLayoutError::DuplicateCoveredSlot { .. })
        ));
    }

    #[test]
    fn canonical_round_trip_keeps_one_spanning_source_anchor() {
        let table = merged_table();
        let rendered = table.render_canonical();
        assert!(rendered.contains(CANONICAL_HEADER_POLICY));
        let lines = rendered.lines().collect::<Vec<_>>();
        let parsed = LogicalTable::parse_canonical_lines(&lines).expect("canonical table");

        let inventory = parsed.cell_by_anchor("inventory").expect("inventory anchor");
        assert_eq!(inventory.row_span(), 1);
        assert_eq!(inventory.column_span(), 2);
        assert_eq!(parsed.cells().iter().filter(|cell| cell.text() == "Inventory").count(), 1);

        let projection = parsed.project(Default::default()).expect("projection");
        assert_eq!(projection.header.origin, TableHeaderOrigin::DerivedAnchorBand);
        assert_eq!(projection.header.paths[0].display, "Inventory / Material");
        assert_eq!(projection.header.paths[1].display, "Inventory / Stock");
        assert_eq!(projection.header.paths[2].display, "Location / Bin");
        assert_eq!(projection.header.paths[0].anchors, vec!["inventory", "material"]);
        assert_eq!(projection.header.paths[1].anchors, vec!["inventory", "stock"]);
        assert_eq!(projection.header_row_count, 2);
        assert_eq!(projection.body_rows.len(), 3);
    }

    #[test]
    fn canonical_parser_rejects_overlap_out_of_bounds_ragged_and_duplicate_slots() {
        let opening = "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"2\" data-irg-columns=\"2\">";
        let overlap = [
            opening,
            "<tr><td data-irg-anchor=\"a\" rowspan=\"2\" colspan=\"1\">A</td><td data-irg-anchor=\"b\" rowspan=\"1\" colspan=\"1\">B</td></tr>",
            "<tr><td data-irg-anchor=\"c\" rowspan=\"1\" colspan=\"1\">C</td><td data-irg-anchor=\"d\" rowspan=\"1\" colspan=\"1\">D</td></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&overlap),
            Err(TableLayoutError::OutOfBounds { .. })
        ));

        let out_of_bounds = [
            opening,
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"3\">A</td></tr>",
            "<tr><td data-irg-anchor=\"b\" rowspan=\"1\" colspan=\"1\">B</td><td data-irg-anchor=\"c\" rowspan=\"1\" colspan=\"1\">C</td></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&out_of_bounds),
            Err(TableLayoutError::OutOfBounds { .. })
        ));

        let ragged = [
            opening,
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\">A</td><td data-irg-anchor=\"b\" rowspan=\"1\" colspan=\"1\">B</td></tr>",
            "<tr><td data-irg-anchor=\"c\" rowspan=\"1\" colspan=\"1\">C</td></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&ragged),
            Err(TableLayoutError::RaggedTable { .. })
        ));

        let duplicate_anchor = [
            opening,
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\">A</td><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\">B</td></tr>",
            "<tr><td data-irg-anchor=\"c\" rowspan=\"1\" colspan=\"1\">C</td><td data-irg-anchor=\"d\" rowspan=\"1\" colspan=\"1\">D</td></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&duplicate_anchor),
            Err(TableLayoutError::DuplicateAnchor { .. })
        ));
    }

    #[test]
    fn canonical_parser_rejects_malformed_markup_instead_of_repairing_it() {
        let escaped_tags = [
            "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"1\" data-irg-columns=\"1\">",
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\">&lt;table&gt;</td></tr>",
            "</table>",
        ];
        let parsed =
            LogicalTable::parse_canonical_lines(&escaped_tags).expect("escaped text is literal");
        assert_eq!(parsed.cells()[0].text(), "<table>");

        let inline = ["<table data-irg-header-policy=\"anchor-band-derived-v1\">x</table>"];
        assert!(LogicalTable::parse_canonical_lines(&inline).is_err());

        let raw_tag = [
            "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"1\" data-irg-columns=\"1\">",
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\"><b>x</b></td></tr>",
            "</table>",
        ];
        assert!(LogicalTable::parse_canonical_lines(&raw_tag).is_err());
    }

    #[test]
    fn canonical_parser_rejects_crossing_and_ambiguous_header_bands() {
        let crossing = [
            "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"3\" data-irg-columns=\"3\">",
            "<tr><td data-irg-anchor=\"inventory\" rowspan=\"1\" colspan=\"2\">Inventory</td><td data-irg-anchor=\"location\" rowspan=\"1\" colspan=\"1\">Location</td></tr>",
            "<tr><td data-irg-anchor=\"material\" rowspan=\"1\" colspan=\"1\">Material</td><td data-irg-anchor=\"stock\" rowspan=\"1\" colspan=\"2\">Stock</td></tr>",
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\">Copper</td><td data-irg-anchor=\"b\" rowspan=\"1\" colspan=\"1\">7</td><td data-irg-anchor=\"c\" rowspan=\"1\" colspan=\"1\">Amber</td></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&crossing)
                .map(|table| table.project(Default::default())),
            Ok(Err(TableLayoutError::AmbiguousHeaderBand { .. }))
        ));

        let spanning_into_body = [
            "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"2\" data-irg-columns=\"1\">",
            "<tr><td data-irg-anchor=\"head\" rowspan=\"2\" colspan=\"1\">Head</td></tr>",
            "<tr></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&spanning_into_body)
                .map(|table| table.project(Default::default())),
            Ok(Err(TableLayoutError::MissingHeaderBand))
        ));

        let missing_band = [
            "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"2\" data-irg-columns=\"2\">",
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"2\">A</td></tr>",
            "<tr><td data-irg-anchor=\"b\" rowspan=\"1\" colspan=\"2\">B</td></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&missing_band)
                .map(|table| table.project(Default::default())),
            Ok(Err(TableLayoutError::MissingHeaderBand))
        ));

        let empty_header = [
            "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"2\" data-irg-columns=\"1\">",
            "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\"></td></tr>",
            "<tr><td data-irg-anchor=\"b\" rowspan=\"1\" colspan=\"1\">Body</td></tr>",
            "</table>",
        ];
        assert!(matches!(
            LogicalTable::parse_canonical_lines(&empty_header)
                .map(|table| table.project(Default::default())),
            Ok(Err(TableLayoutError::EmptyHeaderCell { .. }))
        ));
    }

    #[test]
    fn body_spans_are_refused_instead_of_duplicating_observations() {
        let body_span = [
            "<table data-irg-header-policy=\"anchor-band-derived-v1\" data-irg-rows=\"3\" data-irg-columns=\"2\">",
            "<tr><td data-irg-anchor=\"h1\" rowspan=\"1\" colspan=\"1\">Material</td><td data-irg-anchor=\"h2\" rowspan=\"1\" colspan=\"1\">Stock</td></tr>",
            "<tr><td data-irg-anchor=\"row2\" rowspan=\"2\" colspan=\"1\">Copper</td><td data-irg-anchor=\"n2\" rowspan=\"1\" colspan=\"1\">7</td></tr>",
            "<tr><td data-irg-anchor=\"n3\" rowspan=\"1\" colspan=\"1\">19</td></tr>",
            "</table>",
        ];
        let table = LogicalTable::parse_canonical_lines(&body_span).expect("valid topology");
        assert!(matches!(
            table.project(Default::default()),
            Err(TableLayoutError::UnsupportedBodySpan { .. })
        ));
    }
    #[test]
    fn native_pdf_unknown_header_round_trip_keeps_rows_without_header_anchors() {
        let rows = vec![
            vec!["currency".into(), "<USD> & \\".into()],
            vec!["created_at".into(), "TIMESTAMPTZ".into()],
        ];
        let table = LogicalTable::from_unknown_rows(&rows).unwrap();
        let rendered = table.render_canonical();
        let parsed =
            LogicalTable::parse_canonical_lines(&rendered.lines().collect::<Vec<_>>()).unwrap();
        let projection = parsed.project(Default::default()).unwrap();
        assert_eq!(projection.header.origin, TableHeaderOrigin::Unknown);
        assert_eq!(projection.header_row_count, 0);
        assert_eq!(projection.body_rows, rows);
        assert!(
            projection
                .header
                .paths
                .iter()
                .all(|path| path.anchors.is_empty() && path.segments.is_empty())
        );
        assert_eq!(projection.header.paths[0].display, "col_1");
        assert_eq!(table.render_canonical_bounded(rendered.len()).unwrap(), rendered);
        assert_eq!(
            table.render_canonical_bounded(rendered.len() - 1),
            Err(TableLayoutError::ProjectionLimit)
        );
        assert!(LogicalTable::from_unknown_rows(&[]).is_err());
        assert!(
            LogicalTable::parse_canonical_lines(
                &rendered.replace("\"unknown\"", "\"guessed\"").lines().collect::<Vec<_>>()
            )
            .is_err()
        );
    }
}
