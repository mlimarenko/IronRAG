pub mod batch;
pub mod multipart;
pub mod snapshot;
pub mod source_download;
pub mod types;
pub mod web_runs;

mod documents;
mod revisions;

use axum::{
    Json, Router,
    extract::{FromRequest, Path, Query, State, multipart::Multipart},
    response::IntoResponse,
    routing::{get, post},
};
use ironrag_contracts::documents::DocumentStatus;
use uuid::Uuid;

use self::{
    batch::{batch_cancel_documents, batch_delete_documents, batch_reprocess_documents},
    multipart::{parse_replace_multipart, parse_upload_multipart, resolve_upload_external_key},
    source_download::download_document_source,
    types::{
        ChunkSummary, ChunksQuery, ContentDocumentDetailResponse, ContentDocumentListItem,
        ContentMutationDetailResponse, CreateDocumentMultipartRequest, CreateDocumentRequest,
        CreateDocumentResponse, CreateRevisionMultipartRequest, CreateRevisionRequest,
        DocumentColumnFilters, DocumentFacetColumn, DocumentFacetCursor, DocumentFacetValue,
        DocumentFacetsResponse, DocumentIndexingFilter, DocumentListCursor,
        DocumentListPageResponse, DocumentListSortKey, DocumentListSortOrder,
        DocumentListStatusCounts, DocumentSearchRanges, FacetValue, ListDocumentFacetsQuery,
        ListDocumentsQuery, PatchDocumentMetadataRequest, PreparedDataQuery,
        PreparedSegmentsPageResponse, ReprocessDocumentRequest, TechnicalFactsPageResponse,
        build_revision_metadata, decode_document_facet_cursor, decode_document_list_cursor,
        encode_document_facet_cursor, encode_document_list_cursor, map_document_summary,
        map_mutation_admission, normalize_optional_document_hint, normalize_page_window,
        paginate_items,
    },
    web_runs::{
        cancel_web_ingest_run, create_web_ingest_run, get_web_ingest_run,
        list_web_ingest_run_pages, list_web_ingest_runs,
    },
};
use crate::{
    app::state::AppState,
    domains::content::{ContentDocumentHead, ContentRevision},
    infra::repositories::content_document_list_repository::{self, DocumentListSortColumn},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_READ, POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ,
            POLICY_LIBRARY_WRITE, load_content_document_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::content::service::{
        AdmitDocumentCommand, AdmitMutationCommand, AppendInlineMutationCommand,
        ContentDocumentListEntry, CreateDocumentAdmission, ListDocumentsPageCommand,
        ReplaceInlineMutationCommand, UploadInlineDocumentCommand,
    },
};

pub use self::{documents::*, revisions::*};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/chunks", get(list_chunks))
        .route("/content/web-runs", get(list_web_ingest_runs).post(create_web_ingest_run))
        .route("/content/web-runs/{run_id}", get(get_web_ingest_run))
        .route("/content/web-runs/{run_id}/pages", get(list_web_ingest_run_pages))
        .route("/content/web-runs/{run_id}/cancel", post(cancel_web_ingest_run))
        .route("/content/documents/batch-delete", post(batch_delete_documents))
        .route("/content/documents/batch-cancel", post(batch_cancel_documents))
        .route("/content/documents/batch-reprocess", post(batch_reprocess_documents))
        .route(
            "/content/libraries/{library_id}/documents",
            get(list_documents).post(create_document),
        )
        .route("/content/libraries/{library_id}/documents/facets", get(list_document_facets))
        .route(
            "/content/documents/{document_id}",
            get(get_document).patch(patch_document_metadata).delete(delete_document),
        )
        .route("/content/documents/{document_id}/source", get(download_document_source))
        .route("/content/documents/{document_id}/head", get(get_document_head))
        .route(
            "/content/documents/{document_id}/prepared-segments",
            get(get_document_prepared_segments),
        )
        .route(
            "/content/documents/{document_id}/technical-facts",
            get(get_document_technical_facts),
        )
        .route("/content/documents/{document_id}/reprocess", post(reprocess_document))
        .route(
            "/content/documents/{document_id}/visibility-intents",
            post(create_visibility_intent),
        )
        .route(
            "/content/documents/{document_id}/revisions",
            get(list_revisions).post(create_revision),
        )
        .merge(snapshot::routes())
}

#[cfg(test)]
mod tests;
