use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredBlockKind {
    Heading,
    Paragraph,
    ListItem,
    Table,
    TableRow,
    CodeBlock,
    EndpointBlock,
    QuoteBlock,
    MetadataBlock,
}

impl StructuredBlockKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Heading => "heading",
            Self::Paragraph => "paragraph",
            Self::ListItem => "list_item",
            Self::Table => "table",
            Self::TableRow => "table_row",
            Self::CodeBlock => "code_block",
            Self::EndpointBlock => "endpoint_block",
            Self::QuoteBlock => "quote_block",
            Self::MetadataBlock => "metadata_block",
        }
    }
}

impl std::str::FromStr for StructuredBlockKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "heading" => Ok(Self::Heading),
            "paragraph" => Ok(Self::Paragraph),
            "list_item" => Ok(Self::ListItem),
            "table" => Ok(Self::Table),
            "table_row" => Ok(Self::TableRow),
            "code_block" => Ok(Self::CodeBlock),
            "endpoint_block" => Ok(Self::EndpointBlock),
            "quote_block" => Ok(Self::QuoteBlock),
            "metadata_block" => Ok(Self::MetadataBlock),
            other => Err(format!("unsupported structured block kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredSourceSpan {
    pub start_offset: i32,
    pub end_offset: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredTableCoordinates {
    pub row_index: i32,
    pub column_index: i32,
    pub row_span: i32,
    pub column_span: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredOutlineEntry {
    pub block_id: Uuid,
    pub block_ordinal: i32,
    pub depth: i32,
    pub heading: String,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredBlockData {
    pub block_id: Uuid,
    pub ordinal: i32,
    pub block_kind: StructuredBlockKind,
    pub text: String,
    pub normalized_text: String,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
    pub page_number: Option<i32>,
    pub source_span: Option<StructuredSourceSpan>,
    pub parent_block_id: Option<Uuid>,
    pub table_coordinates: Option<StructuredTableCoordinates>,
    pub code_language: Option<String>,
    pub is_boilerplate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredChunkWindow {
    pub chunk_index: i32,
    pub chunk_kind: StructuredBlockKind,
    pub support_block_ids: Vec<Uuid>,
    pub content_text: String,
    pub normalized_text: String,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
    pub token_count: Option<i32>,
    pub literal_digest: Option<String>,
    pub quality_score: f32,
    pub simhash_fingerprint: Option<u64>,
    pub is_near_duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredDocumentRevisionData {
    pub revision_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub preparation_state: String,
    pub normalization_profile: String,
    pub source_format: String,
    pub language_code: Option<String>,
    pub block_count: i32,
    pub chunk_count: i32,
    pub typed_fact_count: i32,
    pub outline: Vec<StructuredOutlineEntry>,
    pub blocks: Vec<StructuredBlockData>,
    pub chunk_windows: Vec<StructuredChunkWindow>,
    pub prepared_at: DateTime<Utc>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StructuredDocumentValidationError {
    #[error("structured block ordinals must be contiguous; expected {expected}, found {found}")]
    NonContiguousBlockOrdinal { expected: i32, found: i32 },
    #[error("structured block {block_id} must contain text")]
    EmptyBlockText { block_id: Uuid },
    #[error("structured block {block_id} references unknown parent block {parent_block_id}")]
    UnknownParentBlock { block_id: Uuid, parent_block_id: Uuid },
    #[error("structured block {block_id} has an invalid source span")]
    InvalidSourceSpan { block_id: Uuid },
    #[error("structured chunk indices must be contiguous; expected {expected}, found {found}")]
    NonContiguousChunkIndex { expected: i32, found: i32 },
    #[error("structured chunk {chunk_index} must reference at least one block")]
    EmptyChunkSupport { chunk_index: i32 },
    #[error("structured chunk {chunk_index} must contain text")]
    EmptyChunkText { chunk_index: i32 },
    #[error("structured chunk {chunk_index} references unknown support block {support_block_id}")]
    UnknownChunkSupportBlock { chunk_index: i32, support_block_id: Uuid },
    #[error(
        "structured revision block count {declared} does not match actual block count {actual}"
    )]
    BlockCountMismatch { declared: i32, actual: i32 },
    #[error(
        "structured revision chunk count {declared} does not match actual chunk count {actual}"
    )]
    ChunkCountMismatch { declared: i32, actual: i32 },
}

impl StructuredDocumentRevisionData {
    /// Validates the ordered block and chunk windows inside this structured revision.
    ///
    /// # Errors
    ///
    /// Returns a [`StructuredDocumentValidationError`] when the revision has
    /// non-contiguous ordinals, empty content, invalid spans, or count mismatches.
    pub fn validate(&self) -> Result<(), StructuredDocumentValidationError> {
        validate_ordered_semantic_blocks(&self.blocks)?;
        validate_structured_chunk_windows(&self.blocks, &self.chunk_windows)?;
        validate_declared_count(self.block_count, self.blocks.len(), true)?;
        validate_declared_count(self.chunk_count, self.chunk_windows.len(), false)?;
        Ok(())
    }
}

/// Validates that structured blocks are contiguous and internally consistent.
///
/// # Errors
///
/// Returns a [`StructuredDocumentValidationError`] when ordinals, text, spans, or
/// parent references are invalid.
pub fn validate_ordered_semantic_blocks(
    blocks: &[StructuredBlockData],
) -> Result<(), StructuredDocumentValidationError> {
    if blocks.is_empty() {
        return Ok(());
    }

    let known_ids = blocks.iter().map(|block| block.block_id).collect::<HashSet<_>>();
    let mut expected = 0_i32;

    for block in blocks {
        if block.ordinal != expected {
            return Err(StructuredDocumentValidationError::NonContiguousBlockOrdinal {
                expected,
                found: block.ordinal,
            });
        }
        if block.text.trim().is_empty() && block.normalized_text.trim().is_empty() {
            return Err(StructuredDocumentValidationError::EmptyBlockText {
                block_id: block.block_id,
            });
        }
        match &block.source_span {
            Some(source_span) if source_span.end_offset < source_span.start_offset => {
                return Err(StructuredDocumentValidationError::InvalidSourceSpan {
                    block_id: block.block_id,
                });
            }
            _ => {}
        }
        match block.parent_block_id {
            Some(parent_block_id) if !known_ids.contains(&parent_block_id) => {
                return Err(StructuredDocumentValidationError::UnknownParentBlock {
                    block_id: block.block_id,
                    parent_block_id,
                });
            }
            _ => {}
        }
        expected = expected.saturating_add(1);
    }

    Ok(())
}

/// Validates that structured chunk windows are contiguous and reference known blocks.
///
/// # Errors
///
/// Returns a [`StructuredDocumentValidationError`] when chunk indices, text, or
/// support references are invalid.
pub fn validate_structured_chunk_windows(
    blocks: &[StructuredBlockData],
    chunk_windows: &[StructuredChunkWindow],
) -> Result<(), StructuredDocumentValidationError> {
    let known_block_ids = blocks.iter().map(|block| block.block_id).collect::<HashSet<_>>();
    let mut expected = 0_i32;

    for chunk_window in chunk_windows {
        if chunk_window.chunk_index != expected {
            return Err(StructuredDocumentValidationError::NonContiguousChunkIndex {
                expected,
                found: chunk_window.chunk_index,
            });
        }
        if chunk_window.support_block_ids.is_empty() {
            return Err(StructuredDocumentValidationError::EmptyChunkSupport {
                chunk_index: chunk_window.chunk_index,
            });
        }
        if chunk_window.content_text.trim().is_empty()
            && chunk_window.normalized_text.trim().is_empty()
        {
            return Err(StructuredDocumentValidationError::EmptyChunkText {
                chunk_index: chunk_window.chunk_index,
            });
        }
        for support_block_id in &chunk_window.support_block_ids {
            if !known_block_ids.contains(support_block_id) {
                return Err(StructuredDocumentValidationError::UnknownChunkSupportBlock {
                    chunk_index: chunk_window.chunk_index,
                    support_block_id: *support_block_id,
                });
            }
        }
        expected = expected.saturating_add(1);
    }

    Ok(())
}

/// Verifies declared revision counts against the actual number of blocks or chunks.
///
/// # Errors
///
/// Returns a [`StructuredDocumentValidationError`] when the declared count differs
/// from the actual count.
fn validate_declared_count(
    declared: i32,
    actual: usize,
    is_block_count: bool,
) -> Result<(), StructuredDocumentValidationError> {
    let actual = i32::try_from(actual).unwrap_or(i32::MAX);
    if declared == actual {
        return Ok(());
    }

    if is_block_count {
        Err(StructuredDocumentValidationError::BlockCountMismatch { declared, actual })
    } else {
        Err(StructuredDocumentValidationError::ChunkCountMismatch { declared, actual })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        StructuredBlockData, StructuredBlockKind, StructuredChunkWindow,
        StructuredDocumentRevisionData, StructuredDocumentValidationError, StructuredOutlineEntry,
    };

    fn build_block(ordinal: i32) -> StructuredBlockData {
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal,
            block_kind: StructuredBlockKind::Paragraph,
            text: format!("Block {ordinal}"),
            normalized_text: format!("Block {ordinal}"),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        }
    }

    #[test]
    fn validates_structured_revision_with_ordered_blocks_and_chunks() {
        let block = build_block(0);
        let block_id = block.block_id;
        let content_text = block.text.clone();
        let normalized_text = block.normalized_text.clone();
        let revision = StructuredDocumentRevisionData {
            revision_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            preparation_state: "prepared".to_string(),
            normalization_profile: "default".to_string(),
            source_format: "pdf".to_string(),
            language_code: Some("ru".to_string()),
            block_count: 1,
            chunk_count: 1,
            typed_fact_count: 0,
            outline: vec![StructuredOutlineEntry {
                block_id: block.block_id,
                block_ordinal: 0,
                depth: 0,
                heading: "Block 0".to_string(),
                heading_trail: Vec::new(),
                section_path: Vec::new(),
            }],
            blocks: vec![block],
            chunk_windows: vec![StructuredChunkWindow {
                chunk_index: 0,
                chunk_kind: StructuredBlockKind::Paragraph,
                support_block_ids: vec![block_id],
                content_text,
                normalized_text,
                heading_trail: Vec::new(),
                section_path: Vec::new(),
                token_count: None,
                literal_digest: None,
                quality_score: 1.0,
                simhash_fingerprint: None,
                is_near_duplicate: false,
            }],
            prepared_at: Utc::now(),
        };

        assert_eq!(revision.validate(), Ok(()));
    }

    #[test]
    fn rejects_non_contiguous_block_ordinals() {
        let revision = StructuredDocumentRevisionData {
            revision_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            preparation_state: "prepared".to_string(),
            normalization_profile: "default".to_string(),
            source_format: "pdf".to_string(),
            language_code: None,
            block_count: 2,
            chunk_count: 0,
            typed_fact_count: 0,
            outline: Vec::new(),
            blocks: vec![build_block(0), build_block(2)],
            chunk_windows: Vec::new(),
            prepared_at: Utc::now(),
        };

        assert_eq!(
            revision.validate(),
            Err(StructuredDocumentValidationError::NonContiguousBlockOrdinal {
                expected: 1,
                found: 2,
            })
        );
    }

    #[test]
    fn validates_empty_structured_revision() {
        let revision = StructuredDocumentRevisionData {
            revision_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            preparation_state: "prepared".to_string(),
            normalization_profile: "default".to_string(),
            source_format: "image".to_string(),
            language_code: None,
            block_count: 0,
            chunk_count: 0,
            typed_fact_count: 0,
            outline: Vec::new(),
            blocks: Vec::new(),
            chunk_windows: Vec::new(),
            prepared_at: Utc::now(),
        };

        assert_eq!(revision.validate(), Ok(()));
    }
}
