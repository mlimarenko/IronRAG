mod chunking;
mod contracts;
mod document;
mod idempotency;
mod internal;
mod mappers;
mod mutation;
mod pipeline;
mod readiness;
mod revision;
pub mod snapshot;
mod source_bytes;

pub use contracts::{
    AcceptMutationCommand, AdmitDocumentCommand, AdmitMutationCommand, AppendInlineMutationCommand,
    ContentMutationAdmission, ContentService, CreateDocumentAdmission, CreateDocumentCommand,
    CreateMutationItemCommand, CreateRevisionCommand, EditInlineMutationCommand,
    MaterializeRevisionGraphCandidatesCommand, MaterializeWebCaptureCommand,
    MaterializedWebCapture, PreparedRevisionPersistenceSummary, PromoteHeadCommand,
    ReconcileFailedIngestMutationCommand, ReplaceInlineMutationCommand, ReprocessRevisionSource,
    RevisionAdmissionMetadata, RevisionGraphCandidateMaterialization, UpdateMutationCommand,
    UpdateMutationItemCommand, UploadInlineDocumentCommand,
};
pub use document::{
    ContentDocumentListEntry, ContentDocumentListPageResult, DocumentListCursorValue,
    ListDocumentsPageCommand,
};

pub(crate) use contracts::FailedRevisionReadiness;
pub(crate) use readiness::{
    GRAPH_STATE_DEGRADED, derive_failed_revision_readiness, fail_revision_vector_graph_readiness,
    graph_extract_success_message, graph_state_after_successful_extract,
};

use chunking::{PendingChunkInsert, locate_chunk_offsets};
use idempotency::{
    ensure_existing_mutation_matches_request, is_content_mutation_idempotency_violation,
};
use internal::{EditableDocumentContext, InlineMutationContext, PrefetchedDocumentSummaryData};
use mappers::{
    map_document_pipeline_job, map_document_row, map_knowledge_chunk_row,
    map_knowledge_document_row, map_knowledge_revision_readiness, map_knowledge_revision_row,
    map_mutation_item_row, map_mutation_row, map_revision_row, map_structured_revision_data,
    map_structured_revision_row, map_web_page_provenance_row, segment_excerpt,
};
use source_bytes::{
    AppendableDocumentSource, edited_markdown_file_name, infer_inline_mime_type,
    is_appendable_text_mime, merge_appended_bytes, sha256_hex_bytes, sha256_hex_text,
    source_uri_for_inline_payload,
};
