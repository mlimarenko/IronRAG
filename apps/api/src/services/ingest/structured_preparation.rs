use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::shared::extraction::{
    ExtractionLineHint, ExtractionLineSignal, ExtractionStructureHints,
    chunking::{StructuredChunkingProfile, build_structured_chunk_windows},
    structured_document::{
        StructuredBlockData, StructuredBlockKind, StructuredChunkWindow,
        StructuredDocumentRevisionData, StructuredDocumentValidationError, StructuredOutlineEntry,
        StructuredSourceSpan, StructuredTableCoordinates,
    },
    table_markdown::{
        build_semantic_table_row_text, is_markdown_separator_row, parse_markdown_table_row,
    },
    table_summary::{build_table_column_summaries, render_table_column_summary},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareStructuredRevisionCommand {
    pub revision_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub preparation_state: String,
    pub normalization_profile: String,
    pub source_format: String,
    pub language_code: Option<String>,
    pub source_text: String,
    pub normalized_text: String,
    pub structure_hints: ExtractionStructureHints,
    pub typed_fact_count: i32,
    pub prepared_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedStructuredRevision {
    pub prepared_revision: StructuredDocumentRevisionData,
    pub ordered_blocks: Vec<StructuredBlockData>,
    pub chunk_windows: Vec<StructuredChunkWindow>,
}

#[derive(Debug, Error)]
pub enum StructuredPreparationError {
    #[error(transparent)]
    Validation(#[from] StructuredDocumentValidationError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredPreparationFailureCode {
    InvalidStructuredRevision,
}

impl StructuredPreparationFailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidStructuredRevision => "invalid_structured_revision",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredPreparationFailure {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, Default)]
pub struct StructuredPreparationService {
    chunking_profile: StructuredChunkingProfile,
}

impl StructuredPreparationService {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            chunking_profile: StructuredChunkingProfile { max_chars: 2_800, overlap_chars: 280 },
        }
    }

    /// Create a service with chunking parameters sourced from application config.
    #[must_use]
    pub const fn with_chunking(max_chars: usize, overlap_chars: usize) -> Self {
        Self { chunking_profile: StructuredChunkingProfile { max_chars, overlap_chars } }
    }

    pub fn prepare_revision(
        &self,
        command: PrepareStructuredRevisionCommand,
    ) -> Result<PreparedStructuredRevision, StructuredPreparationError> {
        let mut ordered_blocks = build_structured_blocks(&command)?;
        // Filter out blocks with empty text — code files can produce empty lines/blocks
        ordered_blocks
            .retain(|b| !b.text.trim().is_empty() || !b.normalized_text.trim().is_empty());
        // Re-number ordinals after filtering
        for (i, block) in ordered_blocks.iter_mut().enumerate() {
            block.ordinal = i32::try_from(i).unwrap_or(i32::MAX);
        }
        let chunk_windows = build_structured_chunk_windows(&ordered_blocks, self.chunking_profile);
        let prepared_revision = StructuredDocumentRevisionData {
            revision_id: command.revision_id,
            document_id: command.document_id,
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            preparation_state: command.preparation_state,
            normalization_profile: command.normalization_profile,
            source_format: command.source_format,
            language_code: command.language_code,
            block_count: i32::try_from(ordered_blocks.len()).unwrap_or(i32::MAX),
            chunk_count: i32::try_from(chunk_windows.len()).unwrap_or(i32::MAX),
            typed_fact_count: command.typed_fact_count,
            outline: build_outline(&ordered_blocks),
            blocks: ordered_blocks.clone(),
            chunk_windows: chunk_windows.clone(),
            prepared_at: command.prepared_at,
        };
        prepared_revision.validate()?;
        Ok(PreparedStructuredRevision { prepared_revision, ordered_blocks, chunk_windows })
    }

    pub fn prepare_runtime_stage(
        &self,
        command: PrepareStructuredRevisionCommand,
    ) -> Result<PreparedStructuredRevision, StructuredPreparationFailure> {
        self.prepare_revision(command).map_err(|error| StructuredPreparationFailure {
            code: StructuredPreparationFailureCode::InvalidStructuredRevision.as_str().to_string(),
            summary: error.to_string(),
        })
    }
}

fn build_structured_blocks(
    command: &PrepareStructuredRevisionCommand,
) -> Result<Vec<StructuredBlockData>, StructuredPreparationError> {
    let lines = if command.structure_hints.lines.is_empty() {
        fallback_line_hints(&command.normalized_text)
    } else {
        command.structure_hints.lines.clone()
    };
    let mut blocks = Vec::<StructuredBlockData>::new();
    let mut heading_stack = Vec::<String>::new();
    let mut ordinal = 0_i32;
    let mut index = 0_usize;

    while index < lines.len() {
        let line = &lines[index];
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        if is_code_fence(line) {
            let language = trimmed.trim_start_matches('`').trim();
            let start = index;
            index += 1;
            let mut code_lines = Vec::<ExtractionLineHint>::new();
            while index < lines.len() && !is_code_fence(&lines[index]) {
                if !lines[index].text.trim().is_empty() {
                    code_lines.push(lines[index].clone());
                }
                index += 1;
            }
            if index < lines.len() && is_code_fence(&lines[index]) {
                index += 1;
            }
            if !code_lines.is_empty() {
                // If the fenced block has no language tag (` ``` ` with
                // no annotation), auto-detect the language via tree-sitter
                // probing so the extraction pipeline can still produce
                // AST-based identifiers.
                let resolved_language = if language.is_empty() {
                    let code_text: String =
                        code_lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
                    crate::shared::ast_extraction::detect_language(&code_text).map(str::to_string)
                } else {
                    Some(language.to_string())
                };
                blocks.push(build_block(
                    ordinal,
                    StructuredBlockKind::CodeBlock,
                    &code_lines,
                    &heading_stack,
                    None,
                    resolved_language,
                    None,
                    None,
                ));
                ordinal += 1;
            } else if start == index.saturating_sub(1) {
                continue;
            }
            continue;
        }

        if is_heading_line(line) {
            let heading_text = normalize_heading_text(trimmed);
            update_heading_stack(&mut heading_stack, heading_depth(trimmed), &heading_text);
            blocks.push(build_block(
                ordinal,
                StructuredBlockKind::Heading,
                std::slice::from_ref(line),
                &heading_stack,
                None,
                None,
                None,
                None,
            ));
            ordinal += 1;
            index += 1;
            continue;
        }

        if is_table_row_line(line) {
            let start = index;
            while index < lines.len() && is_table_row_line(&lines[index]) {
                index += 1;
            }
            let row_lines = &lines[start..index];
            let table_block = build_block(
                ordinal,
                StructuredBlockKind::Table,
                row_lines,
                &heading_stack,
                None,
                None,
                None,
                None,
            );
            let table_block_id = table_block.block_id;
            blocks.push(table_block);
            ordinal += 1;
            let header_cells = row_lines
                .first()
                .map(|row| parse_markdown_table_row(&row.text))
                .unwrap_or_default();
            let (sheet_name, table_name) = table_context_from_heading_stack(&heading_stack);
            let mut data_row_index = 0usize;
            let mut data_rows = Vec::<Vec<String>>::new();
            for row_line in row_lines.iter().skip(1) {
                let row_cells = parse_markdown_table_row(&row_line.text);
                if row_cells.is_empty() || is_markdown_separator_row(&row_cells) {
                    continue;
                }
                data_rows.push(row_cells.clone());
                blocks.push(build_block(
                    ordinal,
                    StructuredBlockKind::TableRow,
                    std::slice::from_ref(row_line),
                    &heading_stack,
                    Some(table_block_id),
                    None,
                    Some(StructuredTableCoordinates {
                        row_index: i32::try_from(data_row_index).unwrap_or(i32::MAX),
                        column_index: 0,
                        row_span: 1,
                        column_span: 1,
                    }),
                    Some(build_semantic_table_row_text(
                        sheet_name,
                        table_name,
                        data_row_index,
                        &header_cells,
                        &row_cells,
                    )),
                ));
                ordinal += 1;
                data_row_index += 1;
            }
            for summary in
                build_table_column_summaries(sheet_name, table_name, &header_cells, &data_rows)
            {
                blocks.push(build_block(
                    ordinal,
                    StructuredBlockKind::MetadataBlock,
                    &[],
                    &heading_stack,
                    Some(table_block_id),
                    None,
                    None,
                    Some(render_table_column_summary(&summary)),
                ));
                ordinal += 1;
            }
            continue;
        }

        let block_kind = classify_scalar_block_kind(line);
        blocks.push(build_block(
            ordinal,
            block_kind,
            std::slice::from_ref(line),
            &heading_stack,
            None,
            None,
            None,
            None,
        ));
        ordinal += 1;
        index += 1;
    }

    for block in &mut blocks {
        if detect_boilerplate(&block.text) {
            block.is_boilerplate = true;
        }
    }

    Ok(blocks)
}

fn detect_boilerplate(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();

    // 5+ HTTP links
    let http_count = lower.matches("http://").count() + lower.matches("https://").count();
    if http_count >= 5 {
        return true;
    }

    // Breadcrumb patterns: contains " > " or " › " with 3+ segments
    for sep in [" > ", " › "] {
        if lower.contains(sep) {
            let segment_count = lower.split(sep).count();
            if segment_count >= 3 {
                return true;
            }
        }
    }

    // Common boilerplate phrases
    const BOILERPLATE_PHRASES: &[&str] = &[
        "skip to content",
        "cookie",
        "accept cookies",
        "privacy policy",
        "terms of service",
        "all rights reserved",
        "copyright ©",
        "powered by",
        "follow us on",
    ];
    for phrase in BOILERPLATE_PHRASES {
        if lower.contains(phrase) {
            return true;
        }
    }

    // Pure navigation: only short words separated by "|" or "•"
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        let is_nav = if trimmed.contains('|') {
            trimmed
                .split('|')
                .all(|segment| !segment.trim().is_empty() && segment.trim().len() <= 20)
                && trimmed.split('|').count() >= 3
        } else if trimmed.contains('•') {
            trimmed
                .split('•')
                .all(|segment| !segment.trim().is_empty() && segment.trim().len() <= 20)
                && trimmed.split('•').count() >= 3
        } else {
            false
        };
        if is_nav {
            return true;
        }
    }

    false
}

fn fallback_line_hints(content: &str) -> Vec<ExtractionLineHint> {
    crate::shared::extraction::build_text_layout_from_content(content).structure_hints.lines
}

fn classify_scalar_block_kind(line: &ExtractionLineHint) -> StructuredBlockKind {
    let trimmed = line.text.trim();
    if looks_like_docs_navigation_link(trimmed) || has_signal(line, ExtractionLineSignal::ListItem)
    {
        StructuredBlockKind::ListItem
    } else if has_signal(line, ExtractionLineSignal::EndpointCandidate)
        && !looks_like_docs_navigation_link(trimmed)
    {
        StructuredBlockKind::EndpointBlock
    } else if has_signal(line, ExtractionLineSignal::Quote) {
        StructuredBlockKind::QuoteBlock
    } else if has_signal(line, ExtractionLineSignal::MetadataCandidate)
        && !looks_like_compound_product_label(trimmed)
    {
        StructuredBlockKind::MetadataBlock
    } else if has_signal(line, ExtractionLineSignal::CodeLine) {
        StructuredBlockKind::CodeBlock
    } else {
        StructuredBlockKind::Paragraph
    }
}

fn build_block(
    ordinal: i32,
    block_kind: StructuredBlockKind,
    lines: &[ExtractionLineHint],
    heading_stack: &[String],
    parent_block_id: Option<Uuid>,
    code_language: Option<String>,
    table_coordinates: Option<StructuredTableCoordinates>,
    normalized_text_override: Option<String>,
) -> StructuredBlockData {
    let block_id = Uuid::now_v7();
    let raw_text = lines.iter().map(|line| line.text.trim_end()).collect::<Vec<_>>().join("\n");
    let normalized_text = normalized_text_override.unwrap_or_else(|| match block_kind {
        StructuredBlockKind::Heading => {
            heading_stack.last().cloned().unwrap_or_else(|| raw_text.trim().to_string())
        }
        _ => raw_text.trim().to_string(),
    });
    let heading_trail = heading_stack.to_vec();
    let section_path = heading_stack
        .iter()
        .map(|heading| {
            crate::services::graph::identity::normalize_graph_identity_component(heading)
        })
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let page_number = lines.iter().find_map(|line| line.page_number);
    let source_span = match (lines.first(), lines.last()) {
        (Some(first), Some(last)) => Some(StructuredSourceSpan {
            start_offset: first.start_offset.unwrap_or_default(),
            end_offset: last.end_offset.unwrap_or(first.end_offset.unwrap_or_default()),
        }),
        _ => None,
    };

    StructuredBlockData {
        block_id,
        ordinal,
        block_kind,
        text: raw_text.trim().to_string(),
        normalized_text,
        heading_trail,
        section_path,
        page_number,
        source_span,
        parent_block_id,
        table_coordinates,
        code_language,
        is_boilerplate: false,
    }
}

fn build_outline(blocks: &[StructuredBlockData]) -> Vec<StructuredOutlineEntry> {
    blocks
        .iter()
        .filter(|block| matches!(block.block_kind, StructuredBlockKind::Heading))
        .map(|block| StructuredOutlineEntry {
            block_id: block.block_id,
            block_ordinal: block.ordinal,
            depth: i32::try_from(block.heading_trail.len().saturating_sub(1)).unwrap_or(i32::MAX),
            heading: block.normalized_text.clone(),
            heading_trail: block.heading_trail.clone(),
            section_path: block.section_path.clone(),
        })
        .collect()
}

fn table_context_from_heading_stack(heading_stack: &[String]) -> (Option<&str>, Option<&str>) {
    match heading_stack {
        [] => (None, None),
        [sheet] => (Some(sheet.as_str()), None),
        [rest @ .., last] => {
            let sheet = rest.first().map(String::as_str).or(Some(last.as_str()));
            (sheet, Some(last.as_str()))
        }
    }
}

fn is_code_fence(line: &ExtractionLineHint) -> bool {
    has_signal(line, ExtractionLineSignal::CodeFence) || line.text.trim().starts_with("```")
}

fn is_heading_line(line: &ExtractionLineHint) -> bool {
    has_signal(line, ExtractionLineSignal::Heading) || line.text.trim().starts_with('#')
}

fn is_table_row_line(line: &ExtractionLineHint) -> bool {
    has_signal(line, ExtractionLineSignal::TableRow)
}

fn has_signal(line: &ExtractionLineHint, signal: ExtractionLineSignal) -> bool {
    line.signals.contains(&signal)
}

fn normalize_heading_text(text: &str) -> String {
    text.trim_start_matches('#').trim().to_string()
}

fn heading_depth(text: &str) -> usize {
    let trimmed = text.trim_start();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    usize::max(hashes, 1)
}

fn update_heading_stack(stack: &mut Vec<String>, depth: usize, heading: &str) {
    while stack.len() >= depth {
        stack.pop();
    }
    stack.push(heading.to_string());
}

fn looks_like_compound_product_label(text: &str) -> bool {
    let Some((left, right)) = text.split_once(':') else {
        return false;
    };
    !left.trim().contains(' ')
        && !right.trim().is_empty()
        && (right.contains('–') || right.contains('-'))
        && !text.contains(": ")
}

fn looks_like_docs_navigation_link(line: &str) -> bool {
    let lowercase = line.to_ascii_lowercase();
    (lowercase.contains("http://") || lowercase.contains("https://"))
        && (lowercase.contains("/x/")
            || lowercase.contains("/display/")
            || lowercase.contains("/pages/viewpage.action"))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{PrepareStructuredRevisionCommand, StructuredPreparationService};
    use crate::shared::extraction::{
        build_text_layout_from_content, structured_document::StructuredBlockKind,
    };

    #[test]
    fn prepare_revision_derives_outline_from_heading_blocks() {
        let text = "# REST API\n\n## Authentication\n\nGET /v1/status\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "md".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            })
            .expect("prepared revision");

        assert!(prepared.prepared_revision.outline.iter().any(|entry| entry.heading == "REST API"));
        assert!(
            prepared
                .prepared_revision
                .outline
                .iter()
                .any(|entry| entry.heading == "Authentication")
        );
        assert!(prepared.chunk_windows.iter().any(|chunk| !chunk.heading_trail.is_empty()));
    }

    #[test]
    fn prepare_revision_classifies_lists_tables_and_endpoints() {
        // Tables must use canonical markdown table syntax with header separator;
        // informal pipe-delimited text is no longer auto-classified as a table.
        let text = "# Products\n\n- Control Center\n\n| Method | Path |\n| --- | --- |\n| GET | /v1/status |\n\nGET /v1/status\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "md".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            })
            .expect("prepared revision");

        assert!(
            prepared
                .ordered_blocks
                .iter()
                .any(|block| matches!(block.block_kind, StructuredBlockKind::ListItem))
        );
        assert!(
            prepared
                .ordered_blocks
                .iter()
                .any(|block| matches!(block.block_kind, StructuredBlockKind::Table))
        );
        assert!(
            prepared
                .ordered_blocks
                .iter()
                .any(|block| matches!(block.block_kind, StructuredBlockKind::TableRow))
        );
        assert!(
            prepared
                .ordered_blocks
                .iter()
                .any(|block| matches!(block.block_kind, StructuredBlockKind::EndpointBlock))
        );
    }

    #[test]
    fn prepare_revision_builds_semantic_table_row_text_and_preserves_raw_row_text() {
        let text = "## people\n\n| Name | Email |\n| --- | --- |\n| Alice | alice@example.com |\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "csv".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            })
            .expect("prepared revision");

        let row_block = prepared
            .ordered_blocks
            .iter()
            .find(|block| matches!(block.block_kind, StructuredBlockKind::TableRow))
            .expect("table row block");

        assert_eq!(row_block.text, "| Alice | alice@example.com |");
        assert_eq!(
            row_block.normalized_text,
            "Sheet: people | Row 1 | Name: Alice | Email: alice@example.com"
        );
    }

    #[test]
    fn prepare_revision_keeps_single_column_markdown_tables_as_table_blocks() {
        let text = "## test1\n\n| col_1 |\n| --- |\n| test1 |\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "xls".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            })
            .expect("prepared revision");

        let block_kinds =
            prepared.ordered_blocks.iter().map(|block| block.block_kind).collect::<Vec<_>>();
        assert_eq!(
            block_kinds,
            vec![
                StructuredBlockKind::Heading,
                StructuredBlockKind::Table,
                StructuredBlockKind::TableRow,
            ]
        );

        let row_block = prepared
            .ordered_blocks
            .iter()
            .find(|block| block.block_kind == StructuredBlockKind::TableRow)
            .expect("table row block");
        assert_eq!(row_block.text, "| test1 |");
        assert_eq!(row_block.normalized_text, "Sheet: test1 | Row 1 | col_1: test1");
    }

    #[test]
    fn prepare_revision_builds_table_summary_metadata_blocks() {
        let text = "## organizations\n\n| Country | Employees |\n| --- | --- |\n| Sweden | 10 |\n| Benin | 20 |\n| Sweden | 30 |\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "csv".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            })
            .expect("prepared revision");

        let summary_blocks = prepared
            .ordered_blocks
            .iter()
            .filter(|block| block.block_kind == StructuredBlockKind::MetadataBlock)
            .collect::<Vec<_>>();

        assert_eq!(summary_blocks.len(), 2);
        assert!(summary_blocks.iter().any(|block| {
            block.normalized_text.contains("Table Summary")
                && block.normalized_text.contains("Column: Country")
                && block.parent_block_id.is_some()
        }));
        assert!(summary_blocks.iter().any(|block| {
            block.normalized_text.contains("Table Summary")
                && block.normalized_text.contains("Column: Employees")
                && block.parent_block_id.is_some()
        }));
    }

    #[test]
    fn prepare_revision_allows_empty_normalized_content() {
        let prepared = StructuredPreparationService::new()
            .prepare_revision(PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "verbatim_v1".to_string(),
                source_format: "image".to_string(),
                language_code: None,
                source_text: String::new(),
                normalized_text: String::new(),
                structure_hints: build_text_layout_from_content("").structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            })
            .expect("prepared empty revision");

        assert_eq!(prepared.prepared_revision.block_count, 0);
        assert_eq!(prepared.prepared_revision.chunk_count, 0);
        assert!(prepared.ordered_blocks.is_empty());
        assert!(prepared.chunk_windows.is_empty());
    }

    #[test]
    fn detect_boilerplate_catches_nav_links() {
        assert!(
            super::detect_boilerplate("Home | About | Contact | Blog | FAQ | Support"),
            "pipe-separated nav links should be detected as boilerplate"
        );
    }

    #[test]
    fn detect_boilerplate_catches_breadcrumbs() {
        assert!(
            super::detect_boilerplate("Documentation > API Reference > Authentication > OAuth"),
            "breadcrumb pattern should be detected as boilerplate"
        );
    }

    #[test]
    fn detect_boilerplate_catches_cookie_banner() {
        assert!(
            super::detect_boilerplate("We use cookies to improve your experience. Accept cookies"),
            "cookie banner text should be detected as boilerplate"
        );
    }

    #[test]
    fn detect_boilerplate_skips_normal_text() {
        assert!(
            !super::detect_boilerplate(
                "FastAPI is a modern, fast web framework for building APIs with Python."
            ),
            "normal technical text should not be detected as boilerplate"
        );
    }

    #[test]
    fn detect_boilerplate_catches_copyright() {
        assert!(
            super::detect_boilerplate("Copyright © 2024 Acme Inc. All rights reserved."),
            "copyright notice should be detected as boilerplate"
        );
    }
}
