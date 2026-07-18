use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::content::{
        ContentDocument, ContentDocumentHead, ContentDocumentPipelineState, ContentDocumentSummary,
        ContentMutation, ContentMutationItem, ContentRevision, ContentRevisionReadiness,
        ContentSourceAccess, DocumentReadinessSummary, WebPageProvenance,
    },
    domains::knowledge::{PreparedSegmentDetail, TypedTechnicalFact},
    interfaces::http::router_support::ApiError,
    services::{
        content::{
            document_accounting::DocumentLifecycleDetail,
            service::{
                ContentMutationAdmission, ReprocessRevisionSource, RevisionAdmissionMetadata,
            },
        },
        ingest::web::RefetchedWebDocumentSource,
    },
};
use ironrag_contracts::documents::{DocumentReadiness, DocumentStatus};

// ============================================================================
// Canonical document-list surface.
//
// The list response is deliberately slim: one compact row per document with
// *only* the fields the documents page actually renders (see the mapper in
// apps/web/src/pages/documents/mappers.ts). Detail-only data such as
// `readiness_summary`, `prepared_revision`, `pipeline.latest_job`,
// `technical_fact_count`, `lifecycle`, and `web_page_provenance` is NOT
// returned here — the inspector panel fetches them separately via
// /content/documents/{id}. This keeps a reference ~5 k-document payload
// under 3 MB instead of 26 MB.
// ============================================================================

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListDocumentsQuery {
    pub include_deleted: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub search: Option<String>,
    pub sort_by: Option<DocumentListSortKey>,
    pub sort_order: Option<DocumentListSortOrder>,
    pub include_total: Option<bool>,
    /// Comma-separated list of status buckets to keep. Accepted values:
    /// `canceled`, `failed`, `processing`, `queued`, `ready`. Empty or
    /// absent = no filter. Matches the canonical `derived_status` column
    /// in the list CTE and the 5 status pills on the documents page.
    pub status: Option<String>,
    /// Comma-separated list of document ids to keep. Empty or absent = no
    /// filter. Lets a caller resolve specific documents (e.g. a deep link
    /// carrying `documentId`) through the same canonical list derivation
    /// that powers the table, instead of a divergent per-id detail mapper.
    pub ids: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentListSortKey {
    UploadedAt,
    FileName,
    FileType,
    FileSize,
    Status,
}

#[derive(Debug, Clone, Copy, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentListSortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentDocumentListItem {
    pub id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub file_name: String,
    pub file_type: Option<String>,
    pub file_size: Option<i64>,
    pub uploaded_at: DateTime<Utc>,
    pub document_state: String,
    /// Canonical connector identity (`content_document.external_key`).
    /// Connectors look this up via list to decide whether to upload or
    /// replace; exposed so a paginated list pass is sufficient instead
    /// of a per-document detail roundtrip.
    pub external_key: String,
    /// Canonical status bucket derived server-side.
    pub status: DocumentStatus,
    /// Canonical readiness bucket derived server-side.
    pub readiness: DocumentReadiness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_finished_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_access: Option<ContentSourceAccess>,
    /// Summed cost across every billable execution attributed to this
    /// document (ingest + graph extraction). Serialized as a decimal
    /// string to avoid IEEE-754 rounding in the browser. Always present
    /// (zero when no billable execution landed) so the frontend can
    /// render the column without a second roundtrip.
    pub cost: String,
    pub cost_currency_code: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentListPageResponse {
    pub items: Vec<ContentDocumentListItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<i64>,
    /// Per-bucket document counts, populated only when the caller sets
    /// `includeTotal=true`. The counts are computed from the same
    /// `derived_status` CASE expression the list CTE uses, so they stay
    /// in sync with what the pills would show if you filtered on each
    /// bucket individually. Used by the documents page filter strip to
    /// render count badges on each pill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_counts: Option<DocumentListStatusCounts>,
}

#[derive(Debug, Serialize, Default, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentListStatusCounts {
    pub total: i64,
    pub ready: i64,
    pub processing: i64,
    pub queued: i64,
    pub failed: i64,
    pub canceled: i64,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListMutationsQuery {
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChunksQuery {
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PreparedDataQuery {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentRequest {
    pub external_key: Option<String>,
    /// Optional connector-declared structural parent identity
    /// (`content_document.external_key` of the parent document). When the
    /// parent already exists in the library it is resolved at admission;
    /// otherwise the deferred resolver reconciles it later.
    pub parent_external_key: Option<String>,
    pub idempotency_key: Option<String>,
    pub content_source_kind: Option<String>,
    pub checksum: Option<String>,
    pub mime_type: Option<String>,
    pub byte_size: Option<i64>,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateMutationRequest {
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
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
}

/// Discriminates the two JSON revision shapes on `POST .../revisions`. File
/// replacement is a third, mutually exclusive shape carried over
/// `multipart/form-data` instead — see `create_revision`'s content
/// negotiation, mirroring `create_document`'s pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RevisionMode {
    Append,
    Replace,
}

/// JSON body for `POST /v1/content/documents/{documentId}/revisions`.
/// `mode = "append"` requires `appendedText`; `mode = "replace"` requires
/// `markdown`. Replacing the underlying file bytes instead of markdown text
/// uses `multipart/form-data` on the same endpoint, not this shape.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateRevisionRequest {
    pub mode: RevisionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub appended_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReprocessDocumentRequest {
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PatchDocumentMetadataRequest {
    pub document_hint: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentDocumentDetailResponse {
    pub document: ContentDocument,
    pub file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_access: Option<ContentSourceAccess>,
    pub head: Option<ContentDocumentHead>,
    pub active_revision: Option<ContentRevision>,
    pub readiness: Option<ContentRevisionReadiness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_summary: Option<DocumentReadinessSummary>,
    // Full prepared revision (~4 MB on PDF docs) is not returned here —
    // the inspector only needs the two count fields below. Use the
    // paginated `/content/documents/{id}/prepared-segments` endpoint
    // for the actual blocks.
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentMutationDetailResponse {
    pub mutation: ContentMutation,
    pub items: Vec<ContentMutationItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentResponse {
    pub document: ContentDocumentDetailResponse,
    pub mutation: ContentMutationDetailResponse,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChunkSummary {
    pub id: Uuid,
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSegmentsPageResponse {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub items: Vec<PreparedSegmentDetail>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactsPageResponse {
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
    // `prepared_revision` is intentionally not copied onto the response
    // — see the field removal above.
    ContentDocumentDetailResponse {
        document: summary.document,
        file_name: summary.file_name,
        source_access: summary.source_access,
        head: summary.head,
        active_revision: summary.active_revision,
        readiness: summary.readiness,
        readiness_summary: summary.readiness_summary,
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
            document_hint: payload.document_hint.clone(),
            storage_key: payload.storage_key.clone(),
        })),
        _ => Err(ApiError::BadRequest(
            "checksum, mimeType, and byteSize must be provided together".to_string(),
        )),
    }
}

pub(super) fn build_reprocess_revision_metadata(
    active_revision: &ContentRevision,
    source: ReprocessRevisionSource,
) -> RevisionAdmissionMetadata {
    RevisionAdmissionMetadata {
        content_source_kind: active_revision.content_source_kind.clone(),
        checksum: source.checksum,
        mime_type: source.mime_type,
        byte_size: source.byte_size,
        title: source.title,
        language_code: active_revision.language_code.clone(),
        source_uri: source.source_uri,
        document_hint: active_revision.document_hint.clone(),
        storage_key: Some(source.storage_key),
    }
}

/// Builds revision metadata for the web-retry path, where the caller just
/// re-fetched the source URL and wants the new mutation to reference the
/// fresh blob instead of the previous capture. `checksum`, `byte_size`,
/// `storage_key` and `mime_type` come from the live fetch; the rest (title,
/// language, and the `source_uri` itself) is carried forward from the previous
/// revision so downstream identity normalization stays stable across retries.
pub(super) fn build_web_refetch_revision_metadata(
    active_revision: &ContentRevision,
    refetched: RefetchedWebDocumentSource,
) -> RevisionAdmissionMetadata {
    RevisionAdmissionMetadata {
        content_source_kind: active_revision.content_source_kind.clone(),
        checksum: refetched.checksum,
        mime_type: refetched.mime_type.unwrap_or_else(|| active_revision.mime_type.clone()),
        byte_size: refetched.byte_size,
        title: active_revision.title.clone(),
        language_code: active_revision.language_code.clone(),
        source_uri: active_revision.source_uri.clone(),
        document_hint: active_revision.document_hint.clone(),
        storage_key: Some(refetched.storage_key),
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

pub(crate) fn normalize_optional_text(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(crate) fn normalize_optional_document_hint(
    value: Option<String>,
) -> Result<Option<String>, ApiError> {
    let Some(value) = value.and_then(normalize_optional_text) else {
        return Ok(None);
    };
    if value.chars().count() > 1024 {
        return Err(ApiError::BadRequest(
            "document_hint must be at most 1024 characters".to_string(),
        ));
    }
    Ok(Some(value))
}

// ============================================================================
// Opaque cursor for /v1/content/documents keyset pagination.
//
// The cursor is base64(json({"t": "<rfc3339 created_at>", "i": "<uuid>"})).
// It is opaque from the client's perspective and only valid against the
// server version that produced it. Any decode failure is surfaced as a
// `BadRequest` — callers are expected to drop the cursor and start from
// the top instead of pretending the page succeeded.
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DocumentListCursor {
    #[serde(rename = "t")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "i")]
    pub document_id: Uuid,
}

pub(super) fn encode_document_list_cursor(cursor: &DocumentListCursor) -> String {
    use base64::Engine;
    // `DocumentListCursor` is a plain struct of `DateTime<Utc>` + `Uuid`, both
    // of which have infallible `Serialize` impls — to_vec can only fail on
    // I/O errors which `Vec<u8>` never produces. A failure here would mean a
    // serde_json upstream regression, which is far out of scope for a cursor
    // encoder; fall back to an empty token so the caller keeps paginating
    // rather than panicking on the hot path.
    let json = serde_json::to_vec(cursor).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub(super) fn decode_document_list_cursor(token: &str) -> Result<DocumentListCursor, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid cursor encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid cursor payload".to_string()))
}
