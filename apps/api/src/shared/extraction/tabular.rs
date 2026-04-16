use std::{ffi::OsStr, io::Cursor, path::Path};

use anyhow::{Context, Result};
use calamine::{Data, Range, Reader, Sheets, Table, open_workbook_auto_from_rs};
use csv::{ReaderBuilder, StringRecord};

use crate::shared::extraction::{
    ExtractionOutput, ExtractionSourceMetadata, RawExtractionPage, build_text_layout,
    table_markdown::render_markdown_table,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabularFormat {
    Csv,
    Tsv,
    Xls,
    Xlsx,
    Xlsb,
    Ods,
}

impl TabularFormat {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Xls => "xls",
            Self::Xlsx => "xlsx",
            Self::Xlsb => "xlsb",
            Self::Ods => "ods",
        }
    }

    const fn is_delimited(self) -> bool {
        matches!(self, Self::Csv | Self::Tsv)
    }
}

#[derive(Debug, Clone)]
struct ExtractedTable {
    sheet_name: String,
    table_name: Option<String>,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    header_policy: String,
    source_kind: String,
}

impl ExtractedTable {
    fn column_count(&self) -> usize {
        self.headers.len().max(self.rows.iter().map(Vec::len).max().unwrap_or(0))
    }

    fn row_count(&self) -> usize {
        self.rows.len()
    }
}

#[derive(Debug, Clone)]
struct InferredTabularShape {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    header_policy: String,
}

pub fn extract_tabular(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> Result<ExtractionOutput> {
    let format = detect_tabular_format(file_name, mime_type, file_bytes)?;
    let mut warnings = Vec::new();
    let tables = if format.is_delimited() {
        extract_delimited_tables(format, file_name, file_bytes)?
    } else {
        extract_workbook_tables(format, file_bytes, &mut warnings)?
    };

    let mut paragraph_lines = Vec::new();
    for (index, table) in tables.iter().enumerate() {
        if index > 0 {
            paragraph_lines.push(String::new());
        }
        paragraph_lines.push(format!("# {}", table.sheet_name.trim()));
        if let Some(table_name) =
            table.table_name.as_deref().filter(|value| !value.trim().is_empty())
        {
            paragraph_lines.push(format!("## {}", table_name.trim()));
        }
        if let Some(markdown) = render_markdown_table(&table.headers, &table.rows) {
            paragraph_lines.extend(markdown.lines().map(str::to_string));
        }
    }

    let layout =
        build_text_layout(&[RawExtractionPage { page_number: None, lines: paragraph_lines }]);

    Ok(ExtractionOutput {
        extraction_kind: "tabular_text".into(),
        content_text: layout.content_text,
        page_count: None,
        warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: format.as_str().to_string(),
            page_count: None,
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "tabular_format": format.as_str(),
            "sheet_count": tables.iter().map(|table| table.sheet_name.as_str()).collect::<std::collections::BTreeSet<_>>().len(),
            "table_count": tables.len(),
            "tables": tables.iter().map(|table| serde_json::json!({
                "sheetName": table.sheet_name,
                "tableName": table.table_name,
                "rowCount": table.row_count(),
                "columnCount": table.column_count(),
                "headerPolicy": table.header_policy,
                "sourceKind": table.source_kind,
            })).collect::<Vec<_>>(),
            "delimiter": match format {
                TabularFormat::Csv => Some(","),
                TabularFormat::Tsv => Some("\\t"),
                _ => None,
            },
        }),
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images: Vec::new(),
    })
}

fn detect_tabular_format(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> Result<TabularFormat> {
    if let Some(extension) = file_name
        .and_then(|value| Path::new(value).extension())
        .and_then(OsStr::to_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        return match extension.as_str() {
            "csv" => Ok(TabularFormat::Csv),
            "tsv" => Ok(TabularFormat::Tsv),
            "xls" => Ok(TabularFormat::Xls),
            "xlsx" => Ok(TabularFormat::Xlsx),
            "xlsb" => Ok(TabularFormat::Xlsb),
            "ods" => Ok(TabularFormat::Ods),
            _ => Err(anyhow::anyhow!("unsupported tabular extension: {extension}")),
        };
    }

    if let Some(mime_type) = mime_type.map(str::trim).filter(|value| !value.is_empty()) {
        let essence =
            mime_type.split(';').next().map(str::trim).unwrap_or(mime_type).to_ascii_lowercase();
        return match essence.as_str() {
            "text/csv" | "application/csv" | "application/vnd.ms-excel" => {
                if std::str::from_utf8(file_bytes).is_ok() {
                    Ok(TabularFormat::Csv)
                } else {
                    Ok(TabularFormat::Xls)
                }
            }
            "text/tab-separated-values" => Ok(TabularFormat::Tsv),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
                Ok(TabularFormat::Xlsx)
            }
            "application/vnd.ms-excel.sheet.binary.macroenabled.12" => Ok(TabularFormat::Xlsb),
            "application/vnd.oasis.opendocument.spreadsheet" => Ok(TabularFormat::Ods),
            other => Err(anyhow::anyhow!("unsupported tabular mime type: {other}")),
        };
    }

    if std::str::from_utf8(file_bytes).is_ok() {
        return Ok(TabularFormat::Csv);
    }

    Ok(TabularFormat::Xlsx)
}

fn extract_delimited_tables(
    format: TabularFormat,
    file_name: Option<&str>,
    file_bytes: &[u8],
) -> Result<Vec<ExtractedTable>> {
    let decoded =
        std::str::from_utf8(file_bytes).context("failed to decode tabular text as UTF-8")?;
    let stripped = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
    let delimiter = if format == TabularFormat::Tsv { b'\t' } else { detect_delimiter(stripped) };
    let mut reader = ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(stripped.as_bytes());
    let rows = reader
        .records()
        .collect::<std::result::Result<Vec<StringRecord>, csv::Error>>()
        .context("failed to parse delimited table")?
        .into_iter()
        .map(|record| record.iter().map(|cell| cell.to_string()).collect::<Vec<_>>())
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .collect::<Vec<_>>();
    let inferred = infer_tabular_shape(&rows);
    if inferred.headers.is_empty() {
        return Ok(Vec::new());
    }

    let sheet_name = file_name
        .and_then(|value| Path::new(value).file_stem())
        .and_then(OsStr::to_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Sheet1")
        .to_string();

    Ok(vec![ExtractedTable {
        sheet_name,
        table_name: None,
        headers: inferred.headers,
        rows: inferred.rows,
        header_policy: inferred.header_policy,
        source_kind: "delimited_text".to_string(),
    }])
}

fn detect_delimiter(text: &str) -> u8 {
    let candidates = [b',', b';', b'\t', b'|'];
    candidates.into_iter().max_by_key(|delimiter| score_delimiter(text, *delimiter)).unwrap_or(b',')
}

fn score_delimiter(text: &str, delimiter: u8) -> (usize, usize, usize) {
    let mut reader = ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut non_empty_rows = 0usize;
    let mut multi_column_rows = 0usize;
    let mut repeated_width_rows = 0usize;
    let mut previous_width = None::<usize>;
    for record in reader.records().flatten().take(16) {
        let width = record.len();
        if width == 0 || record.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        non_empty_rows += 1;
        if width > 1 {
            multi_column_rows += 1;
        }
        if previous_width == Some(width) && width > 1 {
            repeated_width_rows += 1;
        }
        previous_width = Some(width);
    }
    (multi_column_rows, repeated_width_rows, non_empty_rows)
}

fn extract_workbook_tables(
    format: TabularFormat,
    file_bytes: &[u8],
    warnings: &mut Vec<String>,
) -> Result<Vec<ExtractedTable>> {
    let mut workbook = open_workbook_auto_from_rs(Cursor::new(file_bytes))
        .context("failed to open spreadsheet workbook")?;
    if let Sheets::Xlsx(xlsx) = &mut workbook
        && let Err(error) = xlsx.load_tables()
    {
        warnings.push(format!(
            "spreadsheet table metadata unavailable; used worksheet ranges ({error})"
        ));
    }

    let sheet_names = workbook.sheet_names().to_owned();
    let mut tables = Vec::new();
    for sheet_name in &sheet_names {
        let explicit_tables = extract_explicit_sheet_tables(&mut workbook, sheet_name, warnings);
        if !explicit_tables.is_empty() {
            tables.extend(explicit_tables);
            continue;
        }

        let range = workbook.worksheet_range(sheet_name).with_context(|| {
            format!("failed to read {} worksheet '{sheet_name}'", format.as_str())
        })?;
        let rows = build_used_range_rows(&range);
        let inferred = infer_tabular_shape(&rows);
        if inferred.headers.is_empty() {
            continue;
        }
        tables.push(ExtractedTable {
            sheet_name: sheet_name.trim().to_string(),
            table_name: None,
            headers: inferred.headers,
            rows: inferred.rows,
            header_policy: inferred.header_policy,
            source_kind: "worksheet_range".to_string(),
        });
    }
    Ok(tables)
}

fn extract_explicit_sheet_tables(
    workbook: &mut Sheets<Cursor<&[u8]>>,
    sheet_name: &str,
    warnings: &mut Vec<String>,
) -> Vec<ExtractedTable> {
    let Sheets::Xlsx(xlsx) = workbook else {
        return Vec::new();
    };
    let table_names =
        xlsx.table_names_in_sheet(sheet_name).into_iter().cloned().collect::<Vec<_>>();
    table_names
        .into_iter()
        .filter_map(|table_name| match xlsx.table_by_name(&table_name) {
            Ok(table) => table_to_extracted_table(sheet_name, &table),
            Err(error) => {
                warnings.push(format!(
                    "failed to read spreadsheet table '{table_name}' in sheet '{sheet_name}'; skipped explicit table ({error})"
                ));
                None
            }
        })
        .collect()
}

fn table_to_extracted_table(sheet_name: &str, table: &Table<Data>) -> Option<ExtractedTable> {
    let rows = table
        .data()
        .rows()
        .map(|row| row.iter().map(spreadsheet_cell_to_text).collect::<Vec<_>>())
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .collect::<Vec<_>>();
    let headers = table.columns().to_vec();
    if headers.is_empty() && rows.is_empty() {
        return None;
    }
    Some(ExtractedTable {
        sheet_name: sheet_name.trim().to_string(),
        table_name: Some(table.name().to_string()),
        headers,
        rows,
        header_policy: "explicit_table_headers".to_string(),
        source_kind: "excel_table".to_string(),
    })
}

fn build_used_range_rows(range: &Range<Data>) -> Vec<Vec<String>> {
    range
        .rows()
        .map(|row| row.iter().map(spreadsheet_cell_to_text).collect::<Vec<_>>())
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .collect()
}

fn infer_tabular_shape(rows: &[Vec<String>]) -> InferredTabularShape {
    if rows.is_empty() {
        return InferredTabularShape {
            headers: Vec::new(),
            rows: Vec::new(),
            header_policy: "empty_table".to_string(),
        };
    }

    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return InferredTabularShape {
            headers: Vec::new(),
            rows: Vec::new(),
            header_policy: "empty_table".to_string(),
        };
    }

    let normalized_rows = rows
        .iter()
        .map(|row| {
            let mut next = row.clone();
            next.resize(width, String::new());
            next
        })
        .collect::<Vec<_>>();
    let first_row = normalized_rows.first().cloned().unwrap_or_default();

    if normalized_rows.len() == 1 {
        return InferredTabularShape {
            headers: synthesized_headers(width),
            rows: normalized_rows,
            header_policy: "synthetic_single_row".to_string(),
        };
    }
    if width == 1 {
        return InferredTabularShape {
            headers: synthesized_headers(width),
            rows: normalized_rows,
            header_policy: "synthetic_single_column".to_string(),
        };
    }
    if should_treat_first_row_as_header(&first_row, &normalized_rows[1..]) {
        return InferredTabularShape {
            headers: fill_blank_headers(&first_row),
            rows: normalized_rows.into_iter().skip(1).collect(),
            header_policy: "inferred_first_row_header".to_string(),
        };
    }

    InferredTabularShape {
        headers: synthesized_headers(width),
        rows: normalized_rows,
        header_policy: "synthetic_data_like_header".to_string(),
    }
}

fn should_treat_first_row_as_header(first_row: &[String], sample_rows: &[Vec<String>]) -> bool {
    let non_empty_cells = first_row
        .iter()
        .map(|cell| cell.trim())
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>();
    if non_empty_cells.is_empty() {
        return false;
    }

    let label_like_count =
        non_empty_cells.iter().filter(|cell| looks_like_header_label(cell)).count();
    let data_like_count = non_empty_cells.iter().filter(|cell| looks_like_data_value(cell)).count();
    if data_like_count > 0 || label_like_count.saturating_mul(2) < non_empty_cells.len() {
        return false;
    }

    let distinct_count = non_empty_cells
        .iter()
        .map(|cell| cell.to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if distinct_count != non_empty_cells.len() {
        return false;
    }

    sample_rows.iter().take(4).any(|row| {
        row.iter()
            .map(|cell| cell.trim())
            .filter(|cell| !cell.is_empty())
            .any(|cell| looks_like_data_value(cell) || !looks_like_header_label(cell))
    })
}

fn looks_like_header_label(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 48 {
        return false;
    }
    if looks_like_data_value(trimmed) {
        return false;
    }
    trimmed.chars().any(char::is_alphabetic)
}

fn looks_like_data_value(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }

    let lowercase = trimmed.to_ascii_lowercase();
    let numeric_like = trimmed.chars().all(|character| {
        character.is_ascii_digit() || matches!(character, '.' | ',' | '-' | '+' | '/' | ':' | '%')
    });
    numeric_like
        || lowercase.contains('@')
        || lowercase.starts_with("http://")
        || lowercase.starts_with("https://")
        || lowercase == "true"
        || lowercase == "false"
}

fn synthesized_headers(width: usize) -> Vec<String> {
    (0..width).map(|index| format!("col_{}", index + 1)).collect()
}

fn fill_blank_headers(headers: &[String]) -> Vec<String> {
    headers
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let trimmed = cell.trim();
            if trimmed.is_empty() { format!("col_{}", index + 1) } else { trimmed.to_string() }
        })
        .collect()
}

fn spreadsheet_cell_to_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;

    use super::{detect_delimiter, extract_tabular};

    #[test]
    fn extracts_csv_as_markdown_table() {
        let output = extract_tabular(
            Some("people.csv"),
            Some("text/csv"),
            b"Name,Email\nAlice,alice@example.com\nBob,bob@example.com\n",
        )
        .expect("csv extraction");

        assert_eq!(output.extraction_kind, "tabular_text");
        assert_eq!(output.source_metadata.source_format, "csv");
        assert!(output.content_text.contains("# people"));
        assert!(output.content_text.contains("| Name | Email |"));
        assert!(output.content_text.contains("| Alice | alice@example.com |"));
        assert_eq!(output.source_map["delimiter"], serde_json::json!(","));
    }

    #[test]
    fn keeps_single_row_sheet_as_data_with_synthetic_headers() {
        let output =
            extract_tabular(Some("single.csv"), Some("text/csv"), b"test1\n").expect("csv");

        assert!(output.content_text.contains("| col_1 |"));
        assert!(output.content_text.contains("| test1 |"));
        assert_eq!(
            output.source_map["tables"][0]["headerPolicy"],
            serde_json::json!("synthetic_single_row")
        );
    }

    #[test]
    fn keeps_numeric_first_row_as_data_for_single_column_tables() {
        let output =
            extract_tabular(Some("numbers.csv"), Some("text/csv"), b"1\n2\n3\n").expect("csv");

        assert!(output.content_text.contains("| col_1 |"));
        assert!(output.content_text.contains("| 1 |"));
        assert!(output.content_text.contains("| 2 |"));
        assert_eq!(
            output.source_map["tables"][0]["headerPolicy"],
            serde_json::json!("synthetic_single_column")
        );
    }

    #[test]
    fn extracts_xlsx_used_range_as_markdown_table() {
        let output = extract_tabular(
            Some("inventory.xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            &build_minimal_xlsx_bytes(false),
        )
        .expect("spreadsheet extraction");

        assert_eq!(output.source_metadata.source_format, "xlsx");
        assert!(output.content_text.contains("# Sheet1"));
        assert!(output.content_text.contains("| Name | Value |"));
        assert!(output.content_text.contains("| Acme | 42 |"));
    }

    #[test]
    fn extracts_xlsx_explicit_tables_before_used_range_fallback() {
        let output = extract_tabular(
            Some("inventory.xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            &build_minimal_xlsx_bytes(true),
        )
        .expect("spreadsheet extraction");

        assert!(output.content_text.contains("# Sheet1"));
        assert!(output.content_text.contains("## InventoryTable"));
        assert!(output.content_text.contains("| Item | Quantity |"));
        assert!(output.content_text.contains("| Widget | 7 |"));
        assert_eq!(
            output.source_map["tables"][0]["headerPolicy"],
            serde_json::json!("explicit_table_headers")
        );
    }

    #[test]
    fn extracts_multi_sheet_workbook_with_top_level_sheet_headings() {
        let output = extract_tabular(
            Some("multi-sheet.xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            &build_two_sheet_xlsx_bytes(),
        )
        .expect("spreadsheet extraction");

        assert!(output.content_text.contains("# Sheet1"));
        assert!(output.content_text.contains("# Sheet2"));
        assert!(output.content_text.contains("| Name | Value |"));
        assert!(output.content_text.contains("| Acme | 42 |"));
        assert!(output.content_text.contains("| Status |"));
        assert!(output.content_text.contains("| Ready |"));
        assert!(!output.content_text.contains("## Sheet2"));
    }

    #[test]
    fn detects_pipe_and_tab_delimiters() {
        assert_eq!(detect_delimiter("name|value\nacme|42\n"), b'|');
        assert_eq!(detect_delimiter("name\tvalue\nacme\t42\n"), b'\t');
    }

    fn build_minimal_xlsx_bytes(include_table: bool) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = SimpleFileOptions::default();

        writer.start_file("[Content_Types].xml", options).expect("content types");
        writer.write_all(content_types_xml(include_table).as_bytes()).expect("write content types");

        writer.start_file("_rels/.rels", options).expect("rels");
        writer.write_all(root_rels_xml().as_bytes()).expect("write rels");

        writer.start_file("xl/workbook.xml", options).expect("workbook");
        writer.write_all(workbook_xml().as_bytes()).expect("write workbook");

        writer.start_file("xl/_rels/workbook.xml.rels", options).expect("workbook rels");
        writer.write_all(workbook_rels_xml().as_bytes()).expect("write workbook rels");

        writer.start_file("xl/worksheets/sheet1.xml", options).expect("sheet1");
        writer.write_all(sheet_xml(include_table).as_bytes()).expect("write sheet1");

        if include_table {
            writer.start_file("xl/worksheets/_rels/sheet1.xml.rels", options).expect("sheet1 rels");
            writer.write_all(sheet_rels_xml().as_bytes()).expect("write sheet rels");

            writer.start_file("xl/tables/table1.xml", options).expect("table xml");
            writer.write_all(table_xml().as_bytes()).expect("write table xml");
        }

        writer.finish().expect("finish xlsx");
        cursor.into_inner()
    }

    fn build_two_sheet_xlsx_bytes() -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = SimpleFileOptions::default();

        writer.start_file("[Content_Types].xml", options).expect("content types");
        writer.write_all(two_sheet_content_types_xml().as_bytes()).expect("write content types");

        writer.start_file("_rels/.rels", options).expect("rels");
        writer.write_all(root_rels_xml().as_bytes()).expect("write rels");

        writer.start_file("xl/workbook.xml", options).expect("workbook");
        writer.write_all(two_sheet_workbook_xml().as_bytes()).expect("write workbook");

        writer.start_file("xl/_rels/workbook.xml.rels", options).expect("workbook rels");
        writer.write_all(two_sheet_workbook_rels_xml().as_bytes()).expect("write workbook rels");

        writer.start_file("xl/worksheets/sheet1.xml", options).expect("sheet1");
        writer.write_all(sheet_xml(false).as_bytes()).expect("write sheet1");

        writer.start_file("xl/worksheets/sheet2.xml", options).expect("sheet2");
        writer.write_all(second_sheet_xml().as_bytes()).expect("write sheet2");

        writer.finish().expect("finish xlsx");
        cursor.into_inner()
    }

    fn content_types_xml(include_table: bool) -> String {
        let table_override = if include_table {
            r#"<Override PartName="/xl/tables/table1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml"/>"#
        } else {
            ""
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  {table_override}
</Types>"#
        )
    }

    fn two_sheet_content_types_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#
    }

    fn root_rels_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#
    }

    fn workbook_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#
    }

    fn two_sheet_workbook_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
    <sheet name="Sheet2" sheetId="2" r:id="rId2"/>
  </sheets>
</workbook>"#
    }

    fn workbook_rels_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#
    }

    fn two_sheet_workbook_rels_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
</Relationships>"#
    }

    fn sheet_xml(include_table: bool) -> String {
        let table_part = if include_table {
            r#"
  <tableParts count="1">
    <tablePart r:id="rId1"/>
  </tableParts>"#
        } else {
            ""
        };
        let rows = if include_table {
            r#"
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>Item</t></is></c>
      <c r="B1" t="inlineStr"><is><t>Quantity</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="inlineStr"><is><t>Widget</t></is></c>
      <c r="B2"><v>7</v></c>
    </row>
  </sheetData>"#
        } else {
            r#"
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>Name</t></is></c>
      <c r="B1" t="inlineStr"><is><t>Value</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="inlineStr"><is><t>Acme</t></is></c>
      <c r="B2"><v>42</v></c>
    </row>
  </sheetData>"#
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <dimension ref="A1:B2"/>
  {rows}
  {table_part}
</worksheet>"#
        )
    }

    fn second_sheet_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>Status</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="inlineStr"><is><t>Ready</t></is></c>
    </row>
  </sheetData>
</worksheet>"#
    }

    fn sheet_rels_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/table" Target="../tables/table1.xml"/>
</Relationships>"#
    }

    fn table_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 id="1"
 name="InventoryTable"
 displayName="InventoryTable"
 ref="A1:B2"
 totalsRowShown="0">
  <autoFilter ref="A1:B2"/>
  <tableColumns count="2">
    <tableColumn id="1" name="Item"/>
    <tableColumn id="2" name="Quantity"/>
  </tableColumns>
  <tableStyleInfo name="TableStyleMedium2" showFirstColumn="0" showLastColumn="0" showRowStripes="1" showColumnStripes="0"/>
</table>"#
    }
}
