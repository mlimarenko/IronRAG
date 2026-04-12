use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::content::{
        ContentDocument, ContentDocumentHead, ContentDocumentPipelineState, ContentDocumentSummary,
        ContentMutation, ContentMutationItem, ContentRevision, ContentRevisionReadiness,
        ContentSourceAccess, DocumentReadinessSummary, WebPageProvenance,
    },
    domains::knowledge::{PreparedSegmentDetail, StructuredDocumentRevision, TypedTechnicalFact},
    interfaces::http::router_support::ApiError,
    services::content::{
        document_accounting::DocumentLifecycleDetail,
        service::{ContentMutationAdmission, RevisionAdmissionMetadata},
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListDocumentsQuery {
    pub library_id: Option<Uuid>,
    pub include_deleted: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListMutationsQuery {
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChunksQuery {
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreparedDataQuery {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateDocumentRequest {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub idempotency_key: Option<String>,
    pub content_source_kind: Option<String>,
    pub checksum: Option<String>,
    pub mime_type: Option<String>,
    pub byte_size: Option<i64>,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub storage_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateMutationRequest {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub operation_kind: String,
    pub idempotency_key: Option<String>,
    pub content_source_kind: Option<String>,
    pub checksum: Option<String>,
    pub mime_type: Option<String>,
    pub byte_size: Option<i64>,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub storage_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AppendDocumentBodyRequest {
    pub appended_text: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EditDocumentRequest {
    pub markdown: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReprocessDocumentRequest {
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ContentDocumentDetailResponse {
    pub document: ContentDocument,
    pub file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_access: Option<ContentSourceAccess>,
    pub head: Option<ContentDocumentHead>,
    pub active_revision: Option<ContentRevision>,
    pub readiness: Option<ContentRevisionReadiness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_summary: Option<DocumentReadinessSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepared_revision: Option<StructuredDocumentRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepared_segment_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub technical_fact_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_page_provenance: Option<WebPageProvenance>,
    pub pipeline: ContentDocumentPipelineState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<DocumentLifecycleDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ContentMutationDetailResponse {
    pub mutation: ContentMutation,
    pub items: Vec<ContentMutationItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateDocumentResponse {
    pub document: ContentDocumentDetailResponse,
    pub mutation: ContentMutationDetailResponse,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChunkSummary {
    pub id: Uuid,
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreparedSegmentsPageResponse {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub items: Vec<PreparedSegmentDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TechnicalFactsPageResponse {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub items: Vec<TypedTechnicalFact>,
}

pub(super) fn map_document_summary(
    summary: ContentDocumentSummary,
) -> ContentDocumentDetailResponse {
    let prepared_segment_count =
        summary.prepared_revision.as_ref().map(|revision| revision.block_count);
    let technical_fact_count =
        summary.prepared_revision.as_ref().map(|revision| revision.typed_fact_count);
    ContentDocumentDetailResponse {
        document: summary.document,
        file_name: summary.file_name,
        source_access: summary.source_access,
        head: summary.head,
        active_revision: summary.active_revision,
        readiness: summary.readiness,
        readiness_summary: summary.readiness_summary,
        prepared_revision: summary.prepared_revision,
        prepared_segment_count,
        technical_fact_count,
        web_page_provenance: summary.web_page_provenance,
        pipeline: summary.pipeline,
        lifecycle: None,
    }
}

pub(super) fn build_revision_metadata(
    payload: &CreateDocumentRequest,
) -> Result<Option<RevisionAdmissionMetadata>, ApiError> {
    let checksum = payload.checksum.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let mime_type = payload.mime_type.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let byte_size = payload.byte_size;

    match (checksum, mime_type, byte_size) {
        (None, None, None) => Ok(None),
        (Some(checksum), Some(mime_type), Some(byte_size)) => Ok(Some(RevisionAdmissionMetadata {
            content_source_kind: payload
                .content_source_kind
                .clone()
                .unwrap_or_else(|| "upload".to_string()),
            checksum: checksum.to_string(),
            mime_type: mime_type.to_string(),
            byte_size,
            title: payload.title.clone(),
            language_code: payload.language_code.clone(),
            source_uri: payload.source_uri.clone(),
            storage_key: payload.storage_key.clone(),
        })),
        _ => Err(ApiError::BadRequest(
            "checksum, mimeType, and byteSize must be provided together".to_string(),
        )),
    }
}

pub(super) fn build_reprocess_revision_metadata(
    active_revision: &ContentRevision,
    storage_key: Option<String>,
) -> RevisionAdmissionMetadata {
    RevisionAdmissionMetadata {
        content_source_kind: active_revision.content_source_kind.clone(),
        checksum: active_revision.checksum.clone(),
        mime_type: active_revision.mime_type.clone(),
        byte_size: active_revision.byte_size,
        title: active_revision.title.clone(),
        language_code: active_revision.language_code.clone(),
        source_uri: active_revision.source_uri.clone(),
        storage_key: storage_key.or_else(|| active_revision.storage_key.clone()),
    }
}

pub(super) fn map_mutation_admission(
    admission: ContentMutationAdmission,
) -> ContentMutationDetailResponse {
    ContentMutationDetailResponse {
        mutation: admission.mutation,
        items: admission.items,
        job_id: admission.job_id,
        async_operation_id: admission.async_operation_id,
    }
}

pub(super) fn normalize_page_window(offset: Option<usize>, limit: Option<usize>) -> (usize, usize) {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(100).clamp(1, 500);
    (offset, limit)
}

pub(super) fn paginate_items<T>(items: Vec<T>, offset: usize, limit: usize) -> Vec<T> {
    items.into_iter().skip(offset).take(limit).collect()
}
