mod chunking;
mod contracts;
mod document;
mod internal;
mod mappers;
mod mutation;
mod pipeline;
mod readiness;
mod revision;
pub mod snapshot;
mod source_bytes;

pub use crate::infra::repositories::admission_repository::RevisionAdmission;
pub use contracts::{
    AdmitDocumentCommand, AdmitMutationCommand, AppendInlineMutationCommand,
    ContentMutationAdmission, ContentService, CreateDocumentAdmission, CreateDocumentCommand,
    CreateMutationItemCommand, CreateRevisionCommand, MaterializeRevisionGraphCandidatesCommand,
    MaterializeWebCaptureCommand, MaterializedWebCapture, PreparedRevisionPersistenceSummary,
    PromoteHeadCommand, ReconcileFailedIngestMutationCommand, ReplaceInlineMutationCommand,
    ReprocessRevisionSource, RevisionGraphCandidateMaterialization, UpdateMutationCommand,
    UpdateMutationItemCommand, UploadInlineDocumentCommand,
};
pub use document::{
    ContentDocumentListEntry, ContentDocumentListPageResult, DocumentListCursorValue,
    ListDocumentsPageCommand,
};

pub(crate) use contracts::FailedRevisionReadiness;
pub(crate) use readiness::{
    GRAPH_STATE_DEGRADED, derive_failed_revision_readiness, graph_extract_success_message,
    graph_extract_success_stage_details, graph_state_after_successful_extract,
};
pub(crate) use revision::preparation_fence_text;
pub(crate) use revision::{DoclingPdfExtractionPlanInput, PersistedRevisionSourceGuard};

use chunking::{PendingChunkInsert, locate_chunk_offsets};
use internal::{InlineMutationContext, PrefetchedDocumentSummaryData};
use mappers::{
    map_document_pipeline_job, map_document_row, map_knowledge_document_row,
    map_knowledge_revision_readiness, map_knowledge_revision_row, map_mutation_item_row,
    map_mutation_row, map_revision_row, map_structured_revision_data, map_structured_revision_row,
    map_web_page_provenance_row, segment_excerpt,
};
use source_bytes::{
    AppendableDocumentSource, infer_inline_mime_type, is_appendable_text_mime,
    merge_appended_bytes, sha256_hex_bytes, sha256_hex_text, source_uri_for_inline_payload,
};
