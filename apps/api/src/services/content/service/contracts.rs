use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domains::{
    content::{
        ContentDocument, ContentDocumentSummary, ContentMutation, ContentMutationItem,
        ContentRevision,
    },
    knowledge::StructuredDocumentRevision,
};

#[derive(Debug, Clone)]
pub struct CreateDocumentCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub file_name: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
    /// Connector-declared structural parent identity. When set and a sibling
    /// document with this `external_key` already exists in the same library,
    /// admission resolves it to `parent_document_id`; otherwise it is recorded
    /// as a pending key for the deferred resolver to reconcile later.
    pub parent_external_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateRevisionCommand {
    pub document_id: Uuid,
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct PromoteHeadCommand {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_mutation_id: Option<Uuid>,
    pub latest_successful_attempt_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct AcceptMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
    pub source_identity: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateMutationItemCommand {
    pub mutation_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateMutationCommand {
    pub mutation_id: Uuid,
    pub mutation_state: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub conflict_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateMutationItemCommand {
    pub item_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReconcileFailedIngestMutationCommand {
    pub mutation_id: Uuid,
    pub failure_code: String,
    pub failure_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailedRevisionReadiness {
    pub text_state: String,
    pub vector_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub vector_ready_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct RevisionAdmissionMetadata {
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReprocessRevisionSource {
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub source_uri: Option<String>,
    pub storage_key: String,
}

#[derive(Debug, Clone)]
pub struct AdmitDocumentCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub file_name: Option<String>,
    pub idempotency_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub revision: Option<RevisionAdmissionMetadata>,
    /// Connector-declared structural parent identity, threaded into
    /// [`CreateDocumentCommand::parent_external_key`] at admission.
    pub parent_external_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AdmitMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub operation_kind: String,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub revision: Option<RevisionAdmissionMetadata>,
    /// When this mutation is part of a canonical batch operation, carries
    /// the parent batch `ops_async_operation.id`. The child mutation's own
    /// `ops_async_operation` row is linked to the parent via
    /// `parent_async_operation_id`, which lets progress polling aggregate
    /// child counts with a single indexed query.
    pub parent_async_operation_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UploadInlineDocumentCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub file_name: String,
    pub title: Option<String>,
    pub document_hint: Option<String>,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
    /// Optional connector-declared structural parent identity from the
    /// multipart `parent_external_key` field; threaded through admission.
    pub parent_external_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppendInlineMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub appended_text: String,
}

#[derive(Debug, Clone)]
pub struct ReplaceInlineMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub file_name: String,
    pub document_hint: Option<String>,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct EditInlineMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub markdown: String,
}

#[derive(Debug, Clone)]
pub struct MaterializeWebCaptureCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Uuid,
    pub requested_by_principal_id: Option<Uuid>,
    pub final_url: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub storage_key: String,
}

#[derive(Debug, Clone)]
pub struct ContentMutationAdmission {
    pub mutation: ContentMutation,
    pub items: Vec<ContentMutationItem>,
    pub job_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

/// Result of `materialize_web_capture`. Content-dedup means a
/// successful call does NOT necessarily create a new document - when
/// the fetched body hashes to content that already lives in the
/// library, the candidate is recorded as a duplicate and the existing
/// document id is returned. Caller (web-ingest single_page) branches
/// on the variant: `Ingested` -> candidate_state = processed,
/// `DuplicateContent` -> candidate_state = duplicate with
/// classification_reason = `duplicate_content`.
#[derive(Debug, Clone)]
pub enum MaterializedWebCapture {
    Ingested {
        document: ContentDocument,
        revision: ContentRevision,
        mutation_item: ContentMutationItem,
        job_id: Uuid,
    },
    /// Web-ingest fetched a body whose SHA-256 already matches a
    /// non-deleted document in the library. No new document, revision,
    /// or ingest job is created. `mutation_item` records the skip
    /// linked to the `existing_document_id` so the enclosing
    /// `web_capture` mutation still settles (otherwise the mutation
    /// would dangle).
    DuplicateContent { existing_document_id: Uuid, mutation_item: ContentMutationItem },
}

#[derive(Debug, Clone)]
pub struct CreateDocumentAdmission {
    pub document: ContentDocumentSummary,
    pub mutation: ContentMutationAdmission,
}

#[derive(Debug, Clone)]
pub struct MaterializeRevisionGraphCandidatesCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub attempt_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionGraphCandidateMaterialization {
    pub chunk_count: usize,
    pub selected_graph_chunks: usize,
    pub extracted_entities: usize,
    pub extracted_relations: usize,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub usage_json: serde_json::Value,
    /// Number of chunks whose extraction output was reused from the persistent
    /// graph extraction cache. These chunks did not trigger an LLM call.
    pub reused_chunks: usize,
    /// Cached graph extractions reused from the same semantic extraction version
    /// but produced with a different prompt hash.
    pub reused_prompt_hash_mismatches: usize,
    /// Entities carried over from cached graph extraction output.
    pub reused_entities: usize,
    /// Relations carried over from cached graph extraction output.
    pub reused_relations: usize,
    pub record_stream_source_units_skipped: usize,
}

#[derive(Debug, Clone)]
pub struct PreparedRevisionPersistenceSummary {
    pub prepared_revision: StructuredDocumentRevision,
    pub chunk_count: usize,
    pub technical_fact_count: usize,
    pub technical_conflict_count: usize,
    pub normalization_profile: String,
    /// Time spent on the structured preparation step (block extraction + chunking).
    pub prepare_structure_elapsed_ms: i64,
    /// Time spent on chunk persistence.
    pub chunk_content_elapsed_ms: i64,
    /// Time spent on technical-fact extraction.
    pub extract_technical_facts_elapsed_ms: i64,
}

#[derive(Clone, Default)]
pub struct ContentService;

impl ContentService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
