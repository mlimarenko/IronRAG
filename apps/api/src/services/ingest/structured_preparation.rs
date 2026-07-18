use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domains::catalog::ChunkingTemplate;
use crate::services::ingest::cancellation::{StageError, ensure_not_cancelled};
use crate::shared::extraction::{
    ExtractionLineHint, ExtractionLineSignal, ExtractionStructureHints,
    chunking::split_large_code_blocks,
    chunking::{StructuredChunkingProfile, build_structured_chunk_windows},
    record_jsonl::split_large_record_units,
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

const MIN_DENSE_LINK_COUNT: usize = 3;
const MIN_STRUCTURAL_SEGMENT_COUNT: usize = 4;
const MAX_STRUCTURAL_SEGMENT_CHARS: usize = 40;
const MIN_REPEATED_BLOCK_PAGE_COUNT: usize = 5;
const MAX_REPEATED_BLOCK_CHARS: usize = 160;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
    #[error(transparent)]
    Cancelled(#[from] StageError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

    /// Create a service whose chunking profile is driven by a `ChunkingTemplate`.
    #[must_use]
    pub const fn with_template(template: ChunkingTemplate) -> Self {
        let (max_chars, overlap_chars) = template.chunking_params();
        Self { chunking_profile: StructuredChunkingProfile { max_chars, overlap_chars } }
    }

    pub fn prepare_revision(
        &self,
        command: PrepareStructuredRevisionCommand,
        cancellation_token: &CancellationToken,
    ) -> Result<PreparedStructuredRevision, StructuredPreparationError> {
        ensure_not_cancelled(cancellation_token)?;
        let mut ordered_blocks = build_structured_blocks(&command, cancellation_token)?;
        ensure_not_cancelled(cancellation_token)?;
        // Filter out blocks with empty text — code files can produce empty lines/blocks
        ordered_blocks
            .retain(|b| !b.text.trim().is_empty() || !b.normalized_text.trim().is_empty());
        ensure_not_cancelled(cancellation_token)?;
        ordered_blocks = split_large_code_blocks(&ordered_blocks, self.chunking_profile.max_chars);
        ordered_blocks = split_large_record_units(&ordered_blocks, self.chunking_profile.max_chars);
        ensure_not_cancelled(cancellation_token)?;
        // Re-number ordinals after filtering
        for (i, block) in ordered_blocks.iter_mut().enumerate() {
            ensure_not_cancelled(cancellation_token)?;
            block.ordinal = i32::try_from(i).unwrap_or(i32::MAX);
        }
        ensure_not_cancelled(cancellation_token)?;
        let chunk_windows = build_structured_chunk_windows(&ordered_blocks, self.chunking_profile);
        ensure_not_cancelled(cancellation_token)?;
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
        cancellation_token: &CancellationToken,
    ) -> Result<PreparedStructuredRevision, StructuredPreparationFailure> {
        self.prepare_revision(command, cancellation_token).map_err(|error| {
            StructuredPreparationFailure {
                code: StructuredPreparationFailureCode::InvalidStructuredRevision
                    .as_str()
                    .to_string(),
                summary: error.to_string(),
            }
        })
    }
}

fn build_structured_blocks(
    command: &PrepareStructuredRevisionCommand,
    cancellation_token: &CancellationToken,
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
        ensure_not_cancelled(cancellation_token)?;
        let line = &lines[index];
        if line.text.trim().is_empty() {
            index += 1;
            continue;
        }
        if is_code_fence(line) {
            append_code_block(
                &lines,
                &mut index,
                &mut ordinal,
                &heading_stack,
                &mut blocks,
                cancellation_token,
            )?;
            continue;
        }
        if is_heading_line(line) {
            append_heading_block(line, &mut index, &mut ordinal, &mut heading_stack, &mut blocks);
            continue;
        }
        if is_table_row_line(line) {
            append_table_blocks(
                &lines,
                &mut index,
                &mut ordinal,
                &heading_stack,
                &mut blocks,
                cancellation_token,
            )?;
            continue;
        }
        append_scalar_block(line, &mut index, &mut ordinal, &heading_stack, &mut blocks);
    }

    mark_structural_boilerplate(&mut blocks, cancellation_token)?;
    Ok(blocks)
}

fn append_code_block(
    lines: &[ExtractionLineHint],
    index: &mut usize,
    ordinal: &mut i32,
    heading_stack: &[String],
    blocks: &mut Vec<StructuredBlockData>,
    cancellation_token: &CancellationToken,
) -> Result<(), StructuredPreparationError> {
    let language = lines[*index].text.trim().trim_start_matches('`').trim().to_string();
    *index += 1;
    let mut code_lines = Vec::<ExtractionLineHint>::new();
    while *index < lines.len() && !is_code_fence(&lines[*index]) {
        ensure_not_cancelled(cancellation_token)?;
        if !lines[*index].text.trim().is_empty() {
            code_lines.push(lines[*index].clone());
        }
        *index += 1;
    }
    if *index < lines.len() && is_code_fence(&lines[*index]) {
        *index += 1;
    }
    if code_lines.is_empty() {
        return Ok(());
    }
    let resolved_language = if language.is_empty() {
        let code_text =
            code_lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
        crate::shared::ast_extraction::detect_language(&code_text).map(str::to_string)
    } else {
        Some(language)
    };
    blocks.push(build_block(
        *ordinal,
        StructuredBlockKind::CodeBlock,
        &code_lines,
        heading_stack,
        None,
        resolved_language,
        None,
        None,
    ));
    *ordinal += 1;
    Ok(())
}

fn append_heading_block(
    line: &ExtractionLineHint,
    index: &mut usize,
    ordinal: &mut i32,
    heading_stack: &mut Vec<String>,
    blocks: &mut Vec<StructuredBlockData>,
) {
    let trimmed = line.text.trim();
    let heading_text = normalize_heading_text(trimmed);
    update_heading_stack(heading_stack, heading_depth(trimmed), &heading_text);
    blocks.push(build_block(
        *ordinal,
        StructuredBlockKind::Heading,
        std::slice::from_ref(line),
        heading_stack,
        None,
        None,
        None,
        None,
    ));
    *ordinal += 1;
    *index += 1;
}

fn append_table_blocks(
    lines: &[ExtractionLineHint],
    index: &mut usize,
    ordinal: &mut i32,
    heading_stack: &[String],
    blocks: &mut Vec<StructuredBlockData>,
    cancellation_token: &CancellationToken,
) -> Result<(), StructuredPreparationError> {
    let start = *index;
    while *index < lines.len() && is_table_row_line(&lines[*index]) {
        ensure_not_cancelled(cancellation_token)?;
        *index += 1;
    }
    let row_lines = &lines[start..*index];
    let table_block = build_block(
        *ordinal,
        StructuredBlockKind::Table,
        row_lines,
        heading_stack,
        None,
        None,
        None,
        None,
    );
    let table_block_id = table_block.block_id;
    blocks.push(table_block);
    *ordinal += 1;
    let header_cells =
        row_lines.first().map(|row| parse_markdown_table_row(&row.text)).unwrap_or_default();
    let (sheet_name, table_name) = table_context_from_heading_stack(heading_stack);
    let data_rows = append_table_row_blocks(
        row_lines,
        *ordinal,
        heading_stack,
        table_block_id,
        sheet_name,
        table_name,
        blocks,
        cancellation_token,
    )?;
    *ordinal += i32::try_from(data_rows.len()).unwrap_or(i32::MAX);
    append_table_summary_blocks(
        &header_cells,
        &data_rows,
        sheet_name,
        table_name,
        heading_stack,
        table_block_id,
        ordinal,
        blocks,
        cancellation_token,
    )
}

fn append_table_row_blocks(
    row_lines: &[ExtractionLineHint],
    first_ordinal: i32,
    heading_stack: &[String],
    table_block_id: Uuid,
    sheet_name: Option<&str>,
    table_name: Option<&str>,
    blocks: &mut Vec<StructuredBlockData>,
    cancellation_token: &CancellationToken,
) -> Result<Vec<Vec<String>>, StructuredPreparationError> {
    let header_cells =
        row_lines.first().map(|row| parse_markdown_table_row(&row.text)).unwrap_or_default();
    let mut data_rows = Vec::new();
    for row_line in row_lines.iter().skip(1) {
        ensure_not_cancelled(cancellation_token)?;
        let row_cells = parse_markdown_table_row(&row_line.text);
        if row_cells.is_empty() || is_markdown_separator_row(&row_cells) {
            continue;
        }
        let row_index = data_rows.len();
        blocks.push(build_block(
            first_ordinal.saturating_add(i32::try_from(row_index).unwrap_or(i32::MAX)),
            StructuredBlockKind::TableRow,
            std::slice::from_ref(row_line),
            heading_stack,
            Some(table_block_id),
            None,
            Some(StructuredTableCoordinates {
                row_index: i32::try_from(row_index).unwrap_or(i32::MAX),
                column_index: 0,
                row_span: 1,
                column_span: 1,
            }),
            Some(build_semantic_table_row_text(
                sheet_name,
                table_name,
                row_index,
                &header_cells,
                &row_cells,
            )),
        ));
        data_rows.push(row_cells);
    }
    Ok(data_rows)
}

fn append_table_summary_blocks(
    header_cells: &[String],
    data_rows: &[Vec<String>],
    sheet_name: Option<&str>,
    table_name: Option<&str>,
    heading_stack: &[String],
    table_block_id: Uuid,
    ordinal: &mut i32,
    blocks: &mut Vec<StructuredBlockData>,
    cancellation_token: &CancellationToken,
) -> Result<(), StructuredPreparationError> {
    for summary in build_table_column_summaries(sheet_name, table_name, header_cells, data_rows) {
        ensure_not_cancelled(cancellation_token)?;
        blocks.push(build_block(
            *ordinal,
            StructuredBlockKind::MetadataBlock,
            &[],
            heading_stack,
            Some(table_block_id),
            None,
            None,
            Some(render_table_column_summary(&summary)),
        ));
        *ordinal += 1;
    }
    Ok(())
}

fn append_scalar_block(
    line: &ExtractionLineHint,
    index: &mut usize,
    ordinal: &mut i32,
    heading_stack: &[String],
    blocks: &mut Vec<StructuredBlockData>,
) {
    blocks.push(build_block(
        *ordinal,
        classify_scalar_block_kind(line),
        std::slice::from_ref(line),
        heading_stack,
        None,
        None,
        None,
        None,
    ));
    *ordinal += 1;
    *index += 1;
}

fn detect_boilerplate(block_kind: StructuredBlockKind, text: &str) -> bool {
    if !is_boilerplate_candidate_kind(block_kind) {
        return false;
    }
    has_dense_link_shape(text)
        || ['|', '•', '>', '›']
            .into_iter()
            .any(|separator| has_dense_delimiter_shape(text, separator))
}

const fn is_boilerplate_candidate_kind(block_kind: StructuredBlockKind) -> bool {
    matches!(block_kind, StructuredBlockKind::Paragraph | StructuredBlockKind::ListItem)
}

fn has_dense_link_shape(text: &str) -> bool {
    let (token_count, link_count) =
        text.split_whitespace().fold((0_usize, 0_usize), |(token_count, link_count), token| {
            (token_count + 1, link_count + usize::from(token.contains("://")))
        });
    link_count >= MIN_DENSE_LINK_COUNT && link_count.saturating_mul(2) >= token_count
}

fn has_dense_delimiter_shape(text: &str, separator: char) -> bool {
    let mut segment_count = 0_usize;
    for segment in text.split(separator).map(str::trim) {
        if segment.is_empty() || segment.chars().count() > MAX_STRUCTURAL_SEGMENT_CHARS {
            return false;
        }
        segment_count += 1;
    }
    segment_count >= MIN_STRUCTURAL_SEGMENT_COUNT
}

fn mark_structural_boilerplate(
    blocks: &mut [StructuredBlockData],
    cancellation_token: &CancellationToken,
) -> Result<(), StructuredPreparationError> {
    let mut occurrences_by_key = BTreeMap::<String, (BTreeSet<i32>, Vec<usize>)>::new();
    for (index, block) in blocks.iter_mut().enumerate() {
        ensure_not_cancelled(cancellation_token)?;
        block.is_boilerplate = detect_boilerplate(block.block_kind, &block.text);
        if let (Some(key), Some(page_number)) = (repeated_block_key(block), block.page_number) {
            let (pages, indexes) = occurrences_by_key.entry(key).or_default();
            pages.insert(page_number);
            indexes.push(index);
        }
    }
    for (pages, indexes) in occurrences_by_key.into_values() {
        ensure_not_cancelled(cancellation_token)?;
        if pages.len() >= MIN_REPEATED_BLOCK_PAGE_COUNT {
            for index in indexes {
                blocks[index].is_boilerplate = true;
            }
        }
    }
    if !blocks.is_empty() && blocks.iter().all(|block| block.is_boilerplate) {
        for block in blocks {
            ensure_not_cancelled(cancellation_token)?;
            block.is_boilerplate = false;
        }
    }
    Ok(())
}

fn repeated_block_key(block: &StructuredBlockData) -> Option<String> {
    if !is_boilerplate_candidate_kind(block.block_kind) {
        return None;
    }
    let key = block.normalized_text.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = key.chars().count();
    (length > 0 && length <= MAX_REPEATED_BLOCK_CHARS).then_some(key)
}

fn fallback_line_hints(content: &str) -> Vec<ExtractionLineHint> {
    crate::shared::extraction::build_text_layout_from_content(content).structure_hints.lines
}

fn classify_scalar_block_kind(line: &ExtractionLineHint) -> StructuredBlockKind {
    let trimmed = line.text.trim();
    if has_signal(line, ExtractionLineSignal::ListItem) {
        StructuredBlockKind::ListItem
    } else if has_signal(line, ExtractionLineSignal::EndpointCandidate) {
        StructuredBlockKind::EndpointBlock
    } else if has_signal(line, ExtractionLineSignal::Quote) {
        StructuredBlockKind::QuoteBlock
    } else if has_signal(line, ExtractionLineSignal::SourceProfile) {
        StructuredBlockKind::SourceProfile
    } else if has_signal(line, ExtractionLineSignal::SourceUnit) {
        StructuredBlockKind::SourceUnit
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
            end_offset: last.end_offset.unwrap_or_else(|| first.end_offset.unwrap_or_default()),
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use chrono::Utc;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::{PrepareStructuredRevisionCommand, StructuredPreparationService};
    use crate::shared::extraction::{
        ExtractionLineHint, ExtractionLineSignal, ExtractionStructureHints,
        build_text_layout_from_content, record_jsonl::extract_record_jsonl,
        structured_document::StructuredBlockKind,
    };

    fn prepare_page_lines(entries: &[(i32, &str)]) -> super::PreparedStructuredRevision {
        let lines = entries
            .iter()
            .enumerate()
            .map(|(ordinal, (page_number, text))| ExtractionLineHint {
                ordinal: i32::try_from(ordinal).unwrap_or(i32::MAX),
                page_number: Some(*page_number),
                text: (*text).to_string(),
                ..ExtractionLineHint::default()
            })
            .collect::<Vec<_>>();
        let text = lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
        StructuredPreparationService::new()
            .prepare_revision(
                PrepareStructuredRevisionCommand {
                    revision_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    workspace_id: Uuid::now_v7(),
                    library_id: Uuid::now_v7(),
                    preparation_state: "prepared".to_string(),
                    normalization_profile: "default".to_string(),
                    source_format: "pdf".to_string(),
                    language_code: None,
                    source_text: text.clone(),
                    normalized_text: text,
                    structure_hints: ExtractionStructureHints { lines },
                    typed_fact_count: 0,
                    prepared_at: Utc::now(),
                },
                &CancellationToken::new(),
            )
            .expect("prepared revision")
    }

    #[test]
    fn prepare_revision_derives_outline_from_heading_blocks() {
        let text = "# REST API\n\n## Authentication\n\nGET /v1/status\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(
                PrepareStructuredRevisionCommand {
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
                },
                &CancellationToken::new(),
            )
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
        let text = "# Products\n\n- Operations Console\n\n| Method | Path |\n| --- | --- |\n| GET | /v1/status |\n\nGET /v1/status\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(
                PrepareStructuredRevisionCommand {
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
                },
                &CancellationToken::new(),
            )
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
    fn endpoint_signal_is_not_overridden_by_url_path_vocabulary() {
        let line = ExtractionLineHint {
            text: "GET https://service.example/x/resource".to_string(),
            signals: vec![ExtractionLineSignal::EndpointCandidate],
            ..ExtractionLineHint::default()
        };

        assert_eq!(super::classify_scalar_block_kind(&line), StructuredBlockKind::EndpointBlock);
    }

    #[test]
    fn prepare_revision_preserves_record_source_profile_as_structural_chunk() {
        let extracted = extract_record_jsonl(
            br#"{"id":"unit-1","kind":"message","occurredAt":"2026-04-28T09:00:00Z","actor":{"role":"user","label":"User One"},"text":"First unit"}
{"id":"unit-2","kind":"message","occurredAt":"2026-04-28T10:00:00Z","actor":{"role":"assistant","label":"Assistant"},"text":"Second unit"}"#,
        )
        .expect("record jsonl extraction");

        let prepared = StructuredPreparationService::new()
            .prepare_revision(
                PrepareStructuredRevisionCommand {
                    revision_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    workspace_id: Uuid::now_v7(),
                    library_id: Uuid::now_v7(),
                    preparation_state: "prepared".to_string(),
                    normalization_profile: "default".to_string(),
                    source_format: "record_jsonl".to_string(),
                    language_code: None,
                    source_text: extracted.content_text.clone(),
                    normalized_text: extracted.content_text,
                    structure_hints: extracted.structure_hints,
                    typed_fact_count: 0,
                    prepared_at: Utc::now(),
                },
                &CancellationToken::new(),
            )
            .expect("prepared revision");

        assert_eq!(prepared.ordered_blocks[0].block_kind, StructuredBlockKind::SourceProfile);
        assert_eq!(prepared.ordered_blocks[1].block_kind, StructuredBlockKind::SourceUnit);
        assert_eq!(prepared.ordered_blocks[2].block_kind, StructuredBlockKind::SourceUnit);
        assert_eq!(prepared.chunk_windows[0].chunk_kind, StructuredBlockKind::SourceProfile);
        assert_eq!(prepared.chunk_windows[1].chunk_kind, StructuredBlockKind::SourceUnit);
        assert_eq!(prepared.chunk_windows[2].chunk_kind, StructuredBlockKind::SourceUnit);
        assert!(prepared.chunk_windows[0].content_text.contains("unit_count=2"));
        assert!(prepared.chunk_windows[1].content_text.contains("First unit"));
        assert!(prepared.chunk_windows[2].content_text.contains("Second unit"));
    }

    #[test]
    fn prepare_revision_builds_semantic_table_row_text_and_preserves_raw_row_text() {
        let text = "## people\n\n| Name | Email |\n| --- | --- |\n| Alice | alice@example.com |\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(
                PrepareStructuredRevisionCommand {
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
                },
                &CancellationToken::new(),
            )
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
        assert!(
            prepared
                .chunk_windows
                .iter()
                .any(|chunk| chunk.chunk_kind == StructuredBlockKind::TableRow
                    && chunk.normalized_text.contains("Name: Alice")),
            "table rows must produce queryable chunks"
        );
    }

    #[test]
    fn prepare_revision_keeps_single_column_markdown_tables_as_table_blocks() {
        let text = "## test1\n\n| col_1 |\n| --- |\n| test1 |\n";
        let prepared = StructuredPreparationService::new()
            .prepare_revision(
                PrepareStructuredRevisionCommand {
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
                },
                &CancellationToken::new(),
            )
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
            .prepare_revision(
                PrepareStructuredRevisionCommand {
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
                },
                &CancellationToken::new(),
            )
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
    fn prepare_revision_persists_split_code_blocks_before_chunking() {
        let mut code = String::new();
        for function_index in 0..8 {
            code.push_str(&format!("fn func_{function_index}() {{\n"));
            for line_index in 0..12 {
                code.push_str(&format!(
                    "    let value_{line_index} = \"synthetic segment {function_index}-{line_index}\";\n"
                ));
            }
            code.push_str("}\n\n");
        }
        let text = format!("# Code\n\n```\n{code}```\n");
        let prepared = StructuredPreparationService::with_chunking(220, 0)
            .prepare_revision(
                PrepareStructuredRevisionCommand {
                    revision_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    workspace_id: Uuid::now_v7(),
                    library_id: Uuid::now_v7(),
                    preparation_state: "prepared".to_string(),
                    normalization_profile: "default".to_string(),
                    source_format: "md".to_string(),
                    language_code: Some("en".to_string()),
                    source_text: text.clone(),
                    normalized_text: text.clone(),
                    structure_hints: build_text_layout_from_content(&text).structure_hints,
                    typed_fact_count: 0,
                    prepared_at: Utc::now(),
                },
                &CancellationToken::new(),
            )
            .expect("prepared revision");

        let known_block_ids =
            prepared.ordered_blocks.iter().map(|block| block.block_id).collect::<HashSet<_>>();
        let code_blocks = prepared
            .ordered_blocks
            .iter()
            .filter(|block| block.block_kind == StructuredBlockKind::CodeBlock)
            .collect::<Vec<_>>();

        assert!(code_blocks.len() > 1, "large code block should be split before chunking");
        assert!(
            code_blocks.iter().all(|block| block.parent_block_id.is_none()),
            "split code blocks must not point at a discarded parent block"
        );
        assert!(
            prepared
                .chunk_windows
                .iter()
                .flat_map(|chunk| chunk.support_block_ids.iter())
                .all(|block_id| known_block_ids.contains(block_id)),
            "chunk support ids must all reference persisted structured blocks"
        );
        assert_eq!(
            prepared.prepared_revision.block_count,
            i32::try_from(prepared.ordered_blocks.len()).unwrap_or(i32::MAX)
        );
        assert_eq!(
            prepared.prepared_revision.chunk_count,
            i32::try_from(prepared.chunk_windows.len()).unwrap_or(i32::MAX)
        );
    }

    #[test]
    fn prepare_revision_allows_empty_normalized_content() {
        let prepared = StructuredPreparationService::new()
            .prepare_revision(
                PrepareStructuredRevisionCommand {
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
                },
                &CancellationToken::new(),
            )
            .expect("prepared empty revision");

        assert_eq!(prepared.prepared_revision.block_count, 0);
        assert_eq!(prepared.prepared_revision.chunk_count, 0);
        assert!(prepared.ordered_blocks.is_empty());
        assert!(prepared.chunk_windows.is_empty());
    }

    #[test]
    fn detect_boilerplate_catches_nav_links() {
        assert!(
            super::detect_boilerplate(
                StructuredBlockKind::Paragraph,
                "Home | About | Contact | Blog | FAQ | Support",
            ),
            "pipe-separated nav links should be detected as boilerplate"
        );
    }

    #[test]
    fn detect_boilerplate_catches_breadcrumbs() {
        assert!(
            super::detect_boilerplate(
                StructuredBlockKind::Paragraph,
                "Documentation > API Reference > Authentication > OAuth",
            ),
            "breadcrumb pattern should be detected as boilerplate"
        );
    }

    #[test]
    fn detect_boilerplate_preserves_phrase_only_prose() {
        assert!(
            !super::detect_boilerplate(
                StructuredBlockKind::Paragraph,
                "We use cookies to improve your experience. Accept cookies",
            ),
            "natural-language wording alone must not discard evidence"
        );
    }

    #[test]
    fn detect_boilerplate_skips_normal_text() {
        assert!(
            !super::detect_boilerplate(
                StructuredBlockKind::Paragraph,
                "FastAPI is a modern, fast web framework for building APIs with Python."
            ),
            "normal technical text should not be detected as boilerplate"
        );
    }

    #[test]
    fn detect_boilerplate_uses_link_density_instead_of_absolute_link_count() {
        let text = "A long evidence paragraph references https://one.test, https://two.test, \
            https://three.test, https://four.test, and https://five.test while retaining enough \
            surrounding source material that the links are not the dominant block structure.";

        assert!(
            !super::detect_boilerplate(StructuredBlockKind::Paragraph, text),
            "link count alone must not discard an evidence-rich paragraph"
        );
    }

    #[test]
    fn detect_boilerplate_marks_a_dense_link_block() {
        assert!(super::detect_boilerplate(
            StructuredBlockKind::Paragraph,
            "https://one.test https://two.test https://three.test",
        ));
    }

    #[test]
    fn prepare_revision_marks_exact_short_blocks_repeated_across_pages() {
        let repeated = "Repeated structural block";
        let prepared = prepare_page_lines(&[
            (1, repeated),
            (2, repeated),
            (3, repeated),
            (4, repeated),
            (5, repeated),
            (3, "Unique evidence block"),
        ]);

        assert_eq!(
            prepared
                .ordered_blocks
                .iter()
                .filter(|block| block.text == repeated && block.is_boilerplate)
                .count(),
            5
        );
        assert!(
            prepared
                .ordered_blocks
                .iter()
                .any(|block| block.text == "Unique evidence block" && !block.is_boilerplate)
        );
    }

    #[test]
    fn prepare_revision_keeps_content_when_every_block_matches_a_structural_shape() {
        let prepared = prepare_page_lines(&[(1, "A | B | C | D")]);

        assert!(prepared.ordered_blocks.iter().all(|block| !block.is_boilerplate));
    }

    #[test]
    fn detect_boilerplate_preserves_pipe_tables() {
        assert!(
            !super::detect_boilerplate(StructuredBlockKind::TableRow, "| Name | Value | Status |"),
            "table rows must not be dropped as navigation boilerplate"
        );
    }

    #[test]
    fn detect_boilerplate_preserves_record_stream_units() {
        let unit = "[unit_ordinal=0] fields: \
            services.api.environment.PUBLIC_URL=http://api.local; \
            services.api.healthcheck.test=CMD-SHELL, curl -f http://localhost:8000/health || exit 1; \
            services.auth.environment.AUTH_URL=http://auth.local; \
            services.user.environment.AUTH_SERVICE_URL=http://auth-service:3001; \
            services.notification.environment.WEBHOOK_URL=https://hooks.example.test/events";

        assert!(
            !super::detect_boilerplate(StructuredBlockKind::SourceUnit, unit),
            "structured record units are canonical evidence, not web navigation boilerplate"
        );
    }
}
