use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::knowledge::StructuredDocumentRevision;

pub use crate::domains::runtime_ingestion::RuntimeDocumentActivityStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDocument {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub document_state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDocumentHead {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_mutation_id: Option<Uuid>,
    pub latest_successful_attempt_id: Option<Uuid>,
    pub head_updated_at: DateTime<Utc>,
    pub document_summary: Option<String>,
}

impl ContentDocumentHead {
    /// Returns the best revision available for serving content: prefers the last
    /// successfully-ingested (`readable`) revision, falls back to `active` if no
    /// readable revision exists yet.
    #[must_use]
    pub fn effective_revision_id(&self) -> Option<Uuid> {
        self.readable_revision_id.or(self.active_revision_id)
    }

    /// Returns the most recent revision pointer (active first, then readable)
    /// for use as the base revision when creating new mutations.
    #[must_use]
    pub fn latest_revision_id(&self) -> Option<Uuid> {
        self.active_revision_id.or(self.readable_revision_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentRevision {
    pub id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_number: i32,
    pub parent_revision_id: Option<Uuid>,
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub storage_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSourceAccessKind {
    StoredDocument,
    ExternalUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSourceAccess {
    pub kind: ContentSourceAccessKind,
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentRevisionReadiness {
    pub revision_id: Uuid,
    pub text_state: String,
    pub vector_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub vector_ready_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentMutation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub mutation_state: String,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
    pub source_identity: Option<String>,
    pub failure_code: Option<String>,
    pub conflict_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentChunk {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub token_count: Option<i32>,
    pub normalized_text: String,
    pub text_checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentMutationItem {
    pub id: Uuid,
    pub mutation_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: String,
    pub message: Option<String>,
}

#[must_use]
pub fn revision_text_state_is_readable(text_state: &str) -> bool {
    matches!(text_state.trim(), "readable" | "ready" | "text_readable")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebPageProvenance {
    pub run_id: Option<Uuid>,
    pub candidate_id: Option<Uuid>,
    pub source_uri: Option<String>,
    pub canonical_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDocumentPipelineJob {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub queued_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub current_stage: Option<String>,
    pub failure_code: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDocumentPipelineState {
    pub latest_mutation: Option<ContentMutation>,
    pub latest_job: Option<ContentDocumentPipelineJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentReadinessSummary {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readiness_kind: String,
    pub activity_status: RuntimeDocumentActivityStatus,
    pub stalled_reason: Option<String>,
    pub preparation_state: String,
    pub graph_coverage_kind: String,
    pub typed_fact_coverage: Option<f64>,
    pub last_mutation_id: Option<Uuid>,
    pub last_job_stage: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryKnowledgeCoverage {
    pub library_id: Uuid,
    pub document_counts_by_readiness: BTreeMap<String, i64>,
    pub graph_ready_document_count: i64,
    pub graph_sparse_document_count: i64,
    pub typed_fact_document_count: i64,
    pub last_generation_id: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDocumentSummary {
    pub document: ContentDocument,
    pub file_name: String,
    pub head: Option<ContentDocumentHead>,
    pub active_revision: Option<ContentRevision>,
    pub source_access: Option<ContentSourceAccess>,
    pub readiness: Option<ContentRevisionReadiness>,
    pub readiness_summary: Option<DocumentReadinessSummary>,
    pub prepared_revision: Option<StructuredDocumentRevision>,
    pub web_page_provenance: Option<WebPageProvenance>,
    pub pipeline: ContentDocumentPipelineState,
}

#[cfg(test)]
mod tests {
    use super::revision_text_state_is_readable;

    #[test]
    fn revision_text_state_is_readable_accepts_canonical_ready_states() {
        assert!(revision_text_state_is_readable("readable"));
        assert!(revision_text_state_is_readable("ready"));
        assert!(revision_text_state_is_readable("text_readable"));
        assert!(!revision_text_state_is_readable("processing"));
    }
}
