mod document;
mod mutation;
mod pipeline;
mod revision;
pub mod snapshot;

pub use document::{
    ContentDocumentListEntry, ContentDocumentListPageResult, DocumentListCursorValue,
    ListDocumentsPageCommand,
};

use std::collections::HashMap;

use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    domains::content::{
        ContentChunk, ContentDocument, ContentDocumentPipelineJob, ContentDocumentSummary,
        ContentMutation, ContentMutationItem, ContentRevision, ContentRevisionReadiness,
        WebPageProvenance,
    },
    domains::ingest::IngestStageEvent,
    domains::knowledge::StructuredDocumentRevision,
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
        KnowledgeStructuredRevisionRow,
    },
    infra::repositories::{content_repository, ingest_repository},
    interfaces::http::router_support::ApiError,
    services::ingest::service::IngestJobHandle,
};

#[derive(Debug, Clone)]
pub struct CreateDocumentCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub file_name: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
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
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
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
    pub text_readable_at: Option<chrono::DateTime<chrono::Utc>>,
    pub vector_ready_at: Option<chrono::DateTime<chrono::Utc>>,
    pub graph_ready_at: Option<chrono::DateTime<chrono::Utc>>,
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
    pub storage_key: Option<String>,
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
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
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
/// successful call does NOT necessarily create a new document — when
/// the fetched body hashes to content that already lives in the
/// library, the candidate is recorded as a duplicate and the existing
/// document id is returned. Caller (web-ingest single_page) branches
/// on the variant: `Ingested` → candidate_state = processed,
/// `DuplicateContent` → candidate_state = duplicate with
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
pub(super) struct InlineMutationContext {
    mutation_id: Uuid,
    job_id: Uuid,
    item_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
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
    pub extracted_entities: usize,
    pub extracted_relations: usize,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub usage_json: serde_json::Value,
    /// Number of chunks whose extraction output was reused from a previous
    /// revision because their text checksum was identical (diff-aware ingest).
    /// These chunks did not trigger an LLM call.
    pub reused_chunks: usize,
    /// Entities carried over from the previous revision via reuse.
    pub reused_entities: usize,
    /// Relations carried over from the previous revision via reuse.
    pub reused_relations: usize,
}

#[derive(Debug, Clone)]
pub(super) struct EditableDocumentContext {
    current_content: String,
    mime_type: String,
    title: Option<String>,
    language_code: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingChunkInsert {
    chunk_index: i32,
    start_offset: i32,
    end_offset: i32,
    token_count: Option<i32>,
    chunk_kind: Option<String>,
    content_text: String,
    normalized_text: String,
    text_checksum: String,
    support_block_ids: Vec<Uuid>,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    literal_digest: Option<String>,
    quality_score: Option<f32>,
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
    /// Time spent on chunk persistence (Postgres + Arango).
    pub chunk_content_elapsed_ms: i64,
    /// Time spent on technical-fact extraction.
    pub extract_technical_facts_elapsed_ms: i64,
}

#[derive(Clone, Default)]
pub struct ContentService;

pub(super) struct PrefetchedDocumentSummaryData {
    revisions_by_id: HashMap<Uuid, KnowledgeRevisionRow>,
    structured_revisions_by_revision_id: HashMap<Uuid, KnowledgeStructuredRevisionRow>,
    web_pages_by_result_revision_id: HashMap<Uuid, ingest_repository::WebDiscoveredPageRow>,
}

impl ContentService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

// --- Shared mapping and helper functions ---

pub(crate) fn derive_failed_revision_readiness(
    revision: &KnowledgeRevisionRow,
    stage_events: &[IngestStageEvent],
) -> FailedRevisionReadiness {
    let now = Utc::now();
    let extract_completed = has_completed_stage(stage_events, "extract_content");
    let embed_completed = has_completed_stage(stage_events, "embed_chunk");
    let graph_completed = has_completed_stage(stage_events, "extract_graph");

    let text_state = if revision.text_state == "text_readable" || extract_completed {
        "text_readable"
    } else {
        "failed"
    };
    let vector_state =
        if revision.vector_state == "ready" || embed_completed { "ready" } else { "failed" };
    let graph_state =
        if revision.graph_state == "ready" || graph_completed { "ready" } else { "failed" };

    FailedRevisionReadiness {
        text_state: text_state.to_string(),
        vector_state: vector_state.to_string(),
        graph_state: graph_state.to_string(),
        text_readable_at: (text_state == "text_readable")
            .then(|| revision.text_readable_at.unwrap_or(now)),
        vector_ready_at: (vector_state == "ready").then(|| revision.vector_ready_at.unwrap_or(now)),
        graph_ready_at: (graph_state == "ready").then(|| revision.graph_ready_at.unwrap_or(now)),
    }
}

fn has_completed_stage(stage_events: &[IngestStageEvent], stage_name: &str) -> bool {
    stage_events
        .iter()
        .any(|event| event.stage_name == stage_name && event.stage_state == "completed")
}

fn segment_excerpt(text: &str) -> String {
    const EXCERPT_LIMIT: usize = 180;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= EXCERPT_LIMIT {
        compact
    } else {
        let prefix = compact.chars().take(EXCERPT_LIMIT).collect::<String>();
        format!("{prefix}...")
    }
}

fn map_knowledge_document_row(row: &KnowledgeDocumentRow) -> ContentDocument {
    ContentDocument {
        id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key.clone(),
        document_state: row.document_state.clone(),
        created_at: row.created_at,
    }
}

fn map_document_row(row: content_repository::ContentDocumentRow) -> ContentDocument {
    ContentDocument {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key,
        document_state: row.document_state,
        created_at: row.created_at,
    }
}

pub(super) fn map_revision_row(row: content_repository::ContentRevisionRow) -> ContentRevision {
    ContentRevision {
        id: row.id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        revision_number: row.revision_number,
        parent_revision_id: row.parent_revision_id,
        content_source_kind: row.content_source_kind,
        checksum: row.checksum,
        mime_type: row.mime_type,
        byte_size: row.byte_size,
        title: row.title,
        language_code: row.language_code,
        source_uri: row.source_uri,
        storage_key: row.storage_key,
        created_by_principal_id: row.created_by_principal_id,
        created_at: row.created_at,
    }
}

fn map_knowledge_revision_row(row: KnowledgeRevisionRow) -> ContentRevision {
    ContentRevision {
        id: row.revision_id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        revision_number: i32::try_from(row.revision_number).unwrap_or(i32::MAX),
        parent_revision_id: None,
        content_source_kind: row.revision_kind,
        checksum: row.checksum,
        mime_type: row.mime_type,
        byte_size: row.byte_size,
        title: row.title,
        language_code: None,
        source_uri: row.source_uri,
        storage_key: row.storage_ref,
        created_by_principal_id: None,
        created_at: row.created_at,
    }
}

fn map_knowledge_revision_readiness(row: KnowledgeRevisionRow) -> ContentRevisionReadiness {
    ContentRevisionReadiness {
        revision_id: row.revision_id,
        text_state: row.text_state,
        vector_state: row.vector_state,
        graph_state: row.graph_state,
        text_readable_at: row.text_readable_at,
        vector_ready_at: row.vector_ready_at,
        graph_ready_at: row.graph_ready_at,
    }
}

fn map_structured_revision_row(row: KnowledgeStructuredRevisionRow) -> StructuredDocumentRevision {
    let outline = serde_json::from_value(row.outline_json).unwrap_or_default();
    StructuredDocumentRevision {
        revision_id: row.revision_id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        preparation_state: row.preparation_state,
        normalization_profile: row.normalization_profile,
        source_format: row.source_format,
        language_code: row.language_code,
        block_count: row.block_count,
        chunk_count: row.chunk_count,
        typed_fact_count: row.typed_fact_count,
        outline,
        prepared_at: row.prepared_at,
    }
}

fn map_web_page_provenance_row(row: &ingest_repository::WebDiscoveredPageRow) -> WebPageProvenance {
    WebPageProvenance {
        run_id: Some(row.run_id),
        candidate_id: Some(row.id),
        source_uri: row.final_url.clone().or(row.discovered_url.clone()),
        canonical_url: row.canonical_url.clone().or(row.final_url.clone()),
    }
}

fn map_structured_revision_data(
    data: &crate::shared::extraction::structured_document::StructuredDocumentRevisionData,
) -> StructuredDocumentRevision {
    StructuredDocumentRevision {
        revision_id: data.revision_id,
        document_id: data.document_id,
        workspace_id: data.workspace_id,
        library_id: data.library_id,
        preparation_state: data.preparation_state.clone(),
        normalization_profile: data.normalization_profile.clone(),
        source_format: data.source_format.clone(),
        language_code: data.language_code.clone(),
        block_count: data.block_count,
        chunk_count: data.chunk_count,
        typed_fact_count: data.typed_fact_count,
        outline: data.outline.clone(),
        prepared_at: data.prepared_at,
    }
}

fn map_knowledge_chunk_row(row: KnowledgeChunkRow) -> ContentChunk {
    let start_offset = row.span_start.unwrap_or(0);
    let end_offset = row.span_end.unwrap_or_else(|| {
        start_offset.saturating_add(i32::try_from(row.normalized_text.len()).unwrap_or(0))
    });
    let checksum =
        format!("sha256:{}", hex::encode(Sha256::digest(row.normalized_text.as_bytes())));
    ContentChunk {
        id: row.chunk_id,
        revision_id: row.revision_id,
        chunk_index: row.chunk_index,
        start_offset,
        end_offset,
        token_count: row.token_count,
        normalized_text: row.normalized_text,
        text_checksum: checksum,
    }
}

fn map_mutation_row(row: content_repository::ContentMutationRow) -> ContentMutation {
    ContentMutation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        operation_kind: row.operation_kind,
        mutation_state: row.mutation_state,
        requested_at: row.requested_at,
        completed_at: row.completed_at,
        requested_by_principal_id: row.requested_by_principal_id,
        request_surface: row.request_surface,
        idempotency_key: row.idempotency_key,
        source_identity: row.source_identity,
        failure_code: row.failure_code,
        conflict_code: row.conflict_code,
    }
}

fn map_document_pipeline_job(handle: IngestJobHandle) -> ContentDocumentPipelineJob {
    let latest_attempt = handle.latest_attempt;
    ContentDocumentPipelineJob {
        id: handle.job.id,
        workspace_id: handle.job.workspace_id,
        library_id: handle.job.library_id,
        mutation_id: handle.job.mutation_id,
        async_operation_id: handle.job.async_operation_id,
        job_kind: handle.job.job_kind,
        queue_state: handle.job.queue_state,
        queued_at: handle.job.queued_at,
        available_at: handle.job.available_at,
        completed_at: handle.job.completed_at,
        claimed_at: latest_attempt.as_ref().map(|attempt| attempt.started_at),
        last_activity_at: latest_attempt
            .as_ref()
            .and_then(|attempt| {
                attempt.heartbeat_at.or(attempt.finished_at).or(Some(attempt.started_at))
            })
            .or(handle.job.completed_at),
        current_stage: latest_attempt.as_ref().and_then(|attempt| attempt.current_stage.clone()),
        failure_code: latest_attempt.as_ref().and_then(|attempt| attempt.failure_code.clone()),
        retryable: latest_attempt.as_ref().is_some_and(|attempt| attempt.retryable),
    }
}

fn map_mutation_item_row(row: content_repository::ContentMutationItemRow) -> ContentMutationItem {
    ContentMutationItem {
        id: row.id,
        mutation_id: row.mutation_id,
        document_id: row.document_id,
        base_revision_id: row.base_revision_id,
        result_revision_id: row.result_revision_id,
        item_state: row.item_state,
        message: row.message,
    }
}

fn ensure_existing_mutation_matches_request(
    existing: &content_repository::ContentMutationRow,
    request_workspace_id: Uuid,
    request_library_id: Uuid,
    request_operation_kind: &str,
    request_source_identity: Option<&str>,
) -> Result<(), ApiError> {
    if existing.workspace_id != request_workspace_id
        || existing.library_id != request_library_id
        || existing.operation_kind != request_operation_kind
    {
        return Err(ApiError::idempotency_conflict(
            "the same idempotency key was already used for a different mutation request",
        ));
    }
    if let Some(request_source_identity) = request_source_identity {
        match existing.source_identity.as_deref() {
            Some(existing_source_identity)
                if existing_source_identity != request_source_identity =>
            {
                return Err(ApiError::idempotency_conflict(
                    "the same idempotency key was already used with a different payload",
                ));
            }
            None => {
                return Err(ApiError::idempotency_conflict(
                    "the same idempotency key was already used before payload identity tracking was available; retry with a new idempotency key",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_content_mutation_idempotency_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            database_error.constraint() == Some("idx_content_mutation_idempotency")
        }
        _ => false,
    }
}

fn source_uri_for_inline_payload(
    operation_kind: &str,
    source_identity: Option<&str>,
    file_name: Option<&str>,
) -> String {
    if let Some(source_identity) = source_identity {
        return format!("mcp://payload/{source_identity}");
    }

    match file_name {
        Some(file_name) => format!("{operation_kind}://{file_name}"),
        None => format!("{operation_kind}://inline"),
    }
}

fn infer_inline_mime_type(
    requested_mime_type: Option<&str>,
    file_name: Option<&str>,
    fallback_kind: &str,
) -> String {
    if let Some(mime_type) = requested_mime_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !value.eq_ignore_ascii_case("application/octet-stream"))
    {
        return mime_type.to_string();
    }

    match file_name.and_then(file_extension) {
        Some(extension) if extension == "pdf" => "application/pdf".to_string(),
        Some(extension) if extension == "docx" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
        }
        Some(extension) if extension == "xls" => "application/vnd.ms-excel".to_string(),
        Some(extension) if extension == "xlsx" => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()
        }
        Some(extension) if extension == "xlsb" => {
            "application/vnd.ms-excel.sheet.binary.macroenabled.12".to_string()
        }
        Some(extension) if extension == "ods" => {
            "application/vnd.oasis.opendocument.spreadsheet".to_string()
        }
        Some(extension) if extension == "csv" => "text/csv".to_string(),
        Some(extension) if extension == "tsv" => "text/tab-separated-values".to_string(),
        Some(extension) if extension == "pptx" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string()
        }
        Some(extension) if extension == "md" => "text/markdown".to_string(),
        Some(extension) if extension == "txt" => "text/plain".to_string(),
        Some(extension) if extension == "json" => "application/json".to_string(),
        Some(extension) if extension == "png" => "image/png".to_string(),
        Some(extension) if extension == "jpg" || extension == "jpeg" => "image/jpeg".to_string(),
        Some(extension) if extension == "gif" => "image/gif".to_string(),
        Some(extension) if extension == "bmp" => "image/bmp".to_string(),
        Some(extension) if extension == "webp" => "image/webp".to_string(),
        Some(extension) if extension == "svg" => "image/svg+xml".to_string(),
        Some(extension) if extension == "tif" || extension == "tiff" => "image/tiff".to_string(),
        _ if fallback_kind == "append" => "text/plain".to_string(),
        _ if fallback_kind == "edit" => "text/markdown".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn file_extension(file_name: &str) -> Option<String> {
    let (_, extension) = file_name.rsplit_once('.')?;
    Some(extension.trim().to_ascii_lowercase())
}

fn edited_markdown_file_name(title: Option<&str>, document_id: Uuid) -> String {
    let base = title.map(str::trim).filter(|value| !value.is_empty()).unwrap_or("document");
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem.trim());
    let normalized_stem =
        if stem.is_empty() { format!("document-{document_id}") } else { stem.to_string() };
    format!("{normalized_stem}.md")
}

fn sha256_hex_text(value: &str) -> String {
    sha256_hex_bytes(value.as_bytes())
}

fn sha256_hex_bytes(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    hex::encode(hasher.finalize())
}

fn merge_appended_text(current_content: &str, appended_text: &str) -> String {
    let current = current_content.trim_end();
    let append = appended_text.trim();
    if current.is_empty() {
        append.to_string()
    } else if append.is_empty() {
        current.to_string()
    } else {
        format!("{current}\n\n{append}")
    }
}

fn locate_chunk_offsets(text: &str, chunk_text: &str, next_search_char: usize) -> (usize, usize) {
    let start_byte = char_offset_to_byte_index(text, next_search_char);
    if let Some(relative_start) = text[start_byte..].find(chunk_text) {
        let chunk_start_byte = start_byte + relative_start;
        let chunk_end_byte = chunk_start_byte + chunk_text.len();
        let chunk_start = text[..chunk_start_byte].chars().count();
        let chunk_end = text[..chunk_end_byte].chars().count();
        return (chunk_start, chunk_end);
    }

    let chunk_start = next_search_char;
    let chunk_end = chunk_start.saturating_add(chunk_text.chars().count());
    (chunk_start, chunk_end)
}

fn char_offset_to_byte_index(text: &str, char_offset: usize) -> usize {
    text.char_indices().nth(char_offset).map_or(text.len(), |(byte_index, _)| byte_index)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{edited_markdown_file_name, infer_inline_mime_type, source_uri_for_inline_payload};

    #[test]
    fn infers_spreadsheet_inline_mime_type_from_file_name() {
        assert_eq!(
            infer_inline_mime_type(None, Some("inventory.xlsx"), "replace"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(
            infer_inline_mime_type(None, Some("inventory.xls"), "replace"),
            "application/vnd.ms-excel"
        );
        assert_eq!(
            infer_inline_mime_type(None, Some("inventory.ods"), "replace"),
            "application/vnd.oasis.opendocument.spreadsheet"
        );
        assert_eq!(infer_inline_mime_type(None, Some("inventory.csv"), "replace"), "text/csv");
        assert_eq!(
            infer_inline_mime_type(None, Some("inventory.tsv"), "replace"),
            "text/tab-separated-values"
        );
    }

    #[test]
    fn builds_canonical_markdown_file_name_for_edited_sources() {
        assert_eq!(edited_markdown_file_name(Some("Inventory.xlsx"), Uuid::nil()), "Inventory.md");
        assert_eq!(
            edited_markdown_file_name(Some("Quarterly report"), Uuid::nil()),
            "Quarterly report.md"
        );
    }

    #[test]
    fn edit_inline_sources_use_canonical_inline_uri() {
        assert_eq!(source_uri_for_inline_payload("edit", None, None), "edit://inline");
    }
}
