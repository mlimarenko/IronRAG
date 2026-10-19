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

mod blocks;
#[cfg(test)]
mod tests;

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
        let mut ordered_blocks = blocks::build_structured_blocks(&command, cancellation_token)?;
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
            outline: blocks::build_outline(&ordered_blocks),
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
