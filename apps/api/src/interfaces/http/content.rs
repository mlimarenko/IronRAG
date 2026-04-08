use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        multipart::{Field, Multipart},
    },
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{
        ContentDocument, ContentDocumentHead, ContentDocumentPipelineState, ContentDocumentSummary,
        ContentMutation, ContentMutationItem, ContentRevision, ContentRevisionReadiness,
        DocumentReadinessSummary, WebPageProvenance,
    },
    domains::ingest::{WebDiscoveredPage, WebIngestRun, WebIngestRunReceipt, WebIngestRunSummary},
    domains::knowledge::{PreparedSegmentDetail, StructuredDocumentRevision, TypedTechnicalFact},
    infra::repositories::ingest_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_READ, POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ,
            POLICY_LIBRARY_WRITE, load_content_document_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::content_service::{
        AdmitDocumentCommand, AdmitMutationCommand, AppendInlineMutationCommand,
        ContentMutationAdmission, CreateDocumentAdmission, ReplaceInlineMutationCommand,
        RevisionAdmissionMetadata, UploadInlineDocumentCommand,
    },
    services::web_ingest_service::CreateWebIngestRunCommand,
    shared::file_extract::{UploadAdmissionError, classify_multipart_file_body_error},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDocumentsQuery {
    pub library_id: Option<Uuid>,
    pub include_deleted: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListMutationsQuery {
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunksQuery {
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedDataQuery {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentRequest {
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
    pub storage_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendDocumentBodyRequest {
    pub appended_text: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReprocessDocumentRequest {
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListWebIngestRunsQuery {
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWebIngestRunRequest {
    pub library_id: Uuid,
    pub seed_url: String,
    pub mode: String,
    pub boundary_policy: Option<String>,
    pub max_depth: Option<i32>,
    pub max_pages: Option<i32>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDocumentDetailResponse {
    pub document: ContentDocument,
    pub file_name: String,
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMutationDetailResponse {
    pub mutation: ContentMutation,
    pub items: Vec<ContentMutationItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentResponse {
    pub document: ContentDocumentDetailResponse,
    pub mutation: ContentMutationDetailResponse,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkSummary {
    pub id: Uuid,
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSegmentsPageResponse {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub items: Vec<PreparedSegmentDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactsPageResponse {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub items: Vec<TypedTechnicalFact>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteResponse {
    pub deleted_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchDeleteResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteResult {
    pub document_id: Uuid,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCancelRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCancelResponse {
    pub cancelled_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchCancelResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCancelResult {
    pub document_id: Uuid,
    pub jobs_cancelled: u64,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchReprocessRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchReprocessResponse {
    pub reprocessed_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchReprocessResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchReprocessResult {
    pub document_id: Uuid,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation: Option<ContentMutationDetailResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

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
        .route("/content/documents", get(list_documents).post(create_document))
        .route("/content/documents/upload", axum::routing::post(upload_document))
        .route("/content/documents/{document_id}", get(get_document).delete(delete_document))
        .route("/content/documents/{document_id}/append", axum::routing::post(append_document))
        .route("/content/documents/{document_id}/replace", axum::routing::post(replace_document))
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
        .route("/content/documents/{document_id}/revisions", get(list_revisions))
        .route("/content/mutations", get(list_mutations).post(create_mutation))
        .route("/content/mutations/{mutation_id}", get(get_mutation))
        .route("/content/libraries/{library_id}/export", get(export_library))
        .route("/content/libraries/{library_id}/import", post(import_library))
}

async fn create_web_ingest_run(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<CreateWebIngestRunRequest>,
) -> Result<(StatusCode, Json<WebIngestRunReceipt>), ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, request.library_id, POLICY_LIBRARY_WRITE).await?;
    let run = state
        .canonical_services
        .web_ingest
        .create_run(
            &state,
            CreateWebIngestRunCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                seed_url: request.seed_url,
                mode: request.mode,
                boundary_policy: request.boundary_policy,
                max_depth: request.max_depth,
                max_pages: request.max_pages,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                idempotency_key: request.idempotency_key,
            },
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(WebIngestRunReceipt {
            run_id: run.run_id,
            library_id: run.library_id,
            mode: run.mode,
            run_state: run.run_state,
            async_operation_id: run.async_operation_id,
            counts: run.counts,
            failure_code: run.failure_code,
            cancel_requested_at: run.cancel_requested_at,
        }),
    ))
}

async fn list_web_ingest_runs(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListWebIngestRunsQuery>,
) -> Result<Json<Vec<WebIngestRunSummary>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let runs = state.canonical_services.web_ingest.list_runs(&state, library.id).await?;
    Ok(Json(runs))
}

async fn get_web_ingest_run(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<WebIngestRun>, ApiError> {
    let run = state.canonical_services.web_ingest.get_run(&state, run_id).await?;
    let _library =
        load_library_and_authorize(&auth, &state, run.library_id, POLICY_LIBRARY_READ).await?;
    Ok(Json(run))
}

async fn list_web_ingest_run_pages(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<Vec<WebDiscoveredPage>>, ApiError> {
    let run = state.canonical_services.web_ingest.get_run(&state, run_id).await?;
    let _library =
        load_library_and_authorize(&auth, &state, run.library_id, POLICY_LIBRARY_READ).await?;
    let pages = state.canonical_services.web_ingest.list_pages(&state, run_id).await?;
    Ok(Json(pages))
}

async fn cancel_web_ingest_run(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<(StatusCode, Json<WebIngestRunReceipt>), ApiError> {
    let run = state.canonical_services.web_ingest.get_run(&state, run_id).await?;
    let _library =
        load_library_and_authorize(&auth, &state, run.library_id, POLICY_LIBRARY_WRITE).await?;
    let receipt = state.canonical_services.web_ingest.cancel_run(&state, run_id).await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}

async fn list_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListDocumentsQuery>,
) -> Result<Json<Vec<ContentDocumentDetailResponse>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let include_deleted = query.include_deleted.unwrap_or(false);

    let items = state
        .canonical_services
        .content
        .list_documents(&state, library.id)
        .await?
        .into_iter()
        .filter(|summary| include_deleted || summary.document.document_state != "deleted")
        .map(map_document_summary)
        .collect();
    Ok(Json(items))
}

async fn list_chunks(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ChunksQuery>,
) -> Result<Json<Vec<ChunkSummary>>, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_READ)?;

    let document_id =
        query.document_id.ok_or_else(|| ApiError::BadRequest("documentId is required".into()))?;
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
            .await?;
    let head = state.canonical_services.content.get_document_head(&state, document_id).await?;
    let revision_id = head.and_then(|row| row.effective_revision_id());
    let items = match revision_id {
        Some(revision_id) => {
            state.canonical_services.content.list_chunks(&state, revision_id).await?
        }
        None => Vec::new(),
    };

    Ok(Json(
        items
            .into_iter()
            .map(|chunk| ChunkSummary {
                id: chunk.id,
                document_id,
                library_id: document.library_id,
                ordinal: chunk.chunk_index,
                content: chunk.normalized_text,
                token_count: chunk.token_count,
            })
            .collect(),
    ))
}

async fn create_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateDocumentRequest>,
) -> Result<Json<CreateDocumentResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, payload.library_id, POLICY_LIBRARY_WRITE).await?;
    if library.workspace_id != payload.workspace_id {
        return Err(ApiError::BadRequest(
            "workspaceId does not match the target library".to_string(),
        ));
    }

    let admission = state
        .canonical_services
        .content
        .admit_document(
            &state,
            AdmitDocumentCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                external_key: payload.external_key.clone(),
                idempotency_key: payload.idempotency_key.clone(),
                created_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: build_revision_metadata(&payload)?,
            },
        )
        .await?;
    let CreateDocumentAdmission { document, mutation } = admission;
    Ok(Json(CreateDocumentResponse {
        document: map_document_summary(document),
        mutation: map_mutation_admission(mutation),
    }))
}

async fn upload_document(
    auth: AuthContext,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<CreateDocumentResponse>, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let payload = parse_upload_multipart(&state, multipart).await?;
    let library =
        load_library_and_authorize(&auth, &state, payload.library_id, POLICY_LIBRARY_WRITE).await?;
    let response = state
        .canonical_services
        .content
        .upload_inline_document(
            &state,
            UploadInlineDocumentCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                external_key: None,
                idempotency_key: payload.idempotency_key.clone(),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                file_name: payload.file_name,
                title: payload.title,
                mime_type: payload.mime_type,
                file_bytes: payload.file_bytes,
            },
        )
        .await?;
    let CreateDocumentAdmission { document, mutation } = response;
    Ok(Json(CreateDocumentResponse {
        document: map_document_summary(document),
        mutation: map_mutation_admission(mutation),
    }))
}

async fn get_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentDocumentDetailResponse>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let summary = state.canonical_services.content.get_document(&state, document_id).await?;
    Ok(Json(map_document_summary(summary)))
}

async fn get_document_head(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentDocumentHead>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let head = state.canonical_services.content.get_document_head(&state, document_id).await?;
    head.map(Json).ok_or_else(|| ApiError::resource_not_found("document_head", document_id))
}

async fn get_document_prepared_segments(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Query(query): Query<PreparedDataQuery>,
) -> Result<Json<PreparedSegmentsPageResponse>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let revision_id = resolve_readable_revision_id(&state, document_id).await?;
    let (offset, limit) = normalize_page_window(query.offset, query.limit);
    let items = match revision_id {
        Some(revision_id) => {
            state.canonical_services.content.list_prepared_segments(&state, revision_id).await?
        }
        None => Vec::new(),
    };
    let total = items.len();
    Ok(Json(PreparedSegmentsPageResponse {
        document_id,
        revision_id,
        total,
        offset,
        limit,
        items: paginate_items(items, offset, limit),
    }))
}

async fn get_document_technical_facts(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Query(query): Query<PreparedDataQuery>,
) -> Result<Json<TechnicalFactsPageResponse>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let revision_id = resolve_readable_revision_id(&state, document_id).await?;
    let (offset, limit) = normalize_page_window(query.offset, query.limit);
    let items = match revision_id {
        Some(revision_id) => {
            state.canonical_services.content.list_technical_facts(&state, revision_id).await?
        }
        None => Vec::new(),
    };
    let total = items.len();
    Ok(Json(TechnicalFactsPageResponse {
        document_id,
        revision_id,
        total,
        offset,
        limit,
        items: paginate_items(items, offset, limit),
    }))
}

async fn delete_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let admission = state
        .canonical_services
        .content
        .admit_mutation(
            &state,
            AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "delete".to_string(),
                idempotency_key: None,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: None,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

async fn append_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<AppendDocumentBodyRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let admission = state
        .canonical_services
        .content
        .append_inline_mutation(
            &state,
            AppendInlineMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                idempotency_key: payload.idempotency_key,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                appended_text: payload.appended_text,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

async fn replace_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    multipart: Multipart,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let payload = parse_replace_multipart(&state, multipart).await?;
    let admission = state
        .canonical_services
        .content
        .replace_inline_mutation(
            &state,
            ReplaceInlineMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                idempotency_key: payload.idempotency_key,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                file_name: payload.file_name,
                mime_type: payload.mime_type,
                file_bytes: payload.file_bytes,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

async fn list_revisions(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<Vec<ContentRevision>>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let revisions = state.canonical_services.content.list_revisions(&state, document_id).await?;
    Ok(Json(revisions))
}

async fn create_mutation(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateMutationRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document = load_content_document_and_authorize(
        &auth,
        &state,
        payload.document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    if document.workspace_id != payload.workspace_id || document.library_id != payload.library_id {
        return Err(ApiError::BadRequest(
            "workspaceId or libraryId does not match the target document".to_string(),
        ));
    }

    let admission = state
        .canonical_services
        .content
        .admit_mutation(
            &state,
            AdmitMutationCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                document_id: document.id,
                operation_kind: payload.operation_kind.clone(),
                idempotency_key: payload.idempotency_key.clone(),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: build_revision_metadata(&CreateDocumentRequest {
                    workspace_id: payload.workspace_id,
                    library_id: payload.library_id,
                    external_key: None,
                    idempotency_key: payload.idempotency_key.clone(),
                    content_source_kind: payload.content_source_kind.clone(),
                    checksum: payload.checksum.clone(),
                    mime_type: payload.mime_type.clone(),
                    byte_size: payload.byte_size,
                    title: payload.title.clone(),
                    language_code: payload.language_code.clone(),
                    source_uri: payload.source_uri.clone(),
                    storage_key: payload.storage_key.clone(),
                })?,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

async fn list_mutations(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListMutationsQuery>,
) -> Result<Json<Vec<ContentMutationDetailResponse>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let admissions = state
        .canonical_services
        .content
        .list_mutation_admissions(&state, library.workspace_id, library.id)
        .await?;
    Ok(Json(admissions.into_iter().map(map_mutation_admission).collect()))
}

async fn get_mutation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(mutation_id): Path<Uuid>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let admission =
        state.canonical_services.content.get_mutation_admission(&state, mutation_id).await?;
    let mutation = &admission.mutation;
    let library =
        load_library_and_authorize(&auth, &state, mutation.library_id, POLICY_LIBRARY_READ).await?;
    if library.workspace_id != mutation.workspace_id {
        return Err(ApiError::Unauthorized);
    }
    Ok(Json(map_mutation_admission(admission)))
}

fn map_document_summary(summary: ContentDocumentSummary) -> ContentDocumentDetailResponse {
    let prepared_segment_count =
        summary.prepared_revision.as_ref().map(|revision| revision.block_count);
    let technical_fact_count =
        summary.prepared_revision.as_ref().map(|revision| revision.typed_fact_count);
    let file_name = summary
        .active_revision
        .as_ref()
        .and_then(|revision| revision.title.clone())
        .unwrap_or_else(|| summary.document.external_key.clone());
    ContentDocumentDetailResponse {
        document: summary.document,
        file_name,
        head: summary.head,
        active_revision: summary.active_revision,
        readiness: summary.readiness,
        readiness_summary: summary.readiness_summary,
        prepared_revision: summary.prepared_revision,
        prepared_segment_count,
        technical_fact_count,
        web_page_provenance: summary.web_page_provenance,
        pipeline: summary.pipeline,
    }
}

fn build_revision_metadata(
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

#[derive(Debug)]
struct ParsedUploadMultipart {
    library_id: Uuid,
    idempotency_key: Option<String>,
    title: Option<String>,
    file_name: String,
    mime_type: Option<String>,
    file_bytes: Vec<u8>,
}

#[derive(Debug)]
struct ParsedReplaceMultipart {
    idempotency_key: Option<String>,
    file_name: String,
    mime_type: Option<String>,
    file_bytes: Vec<u8>,
}

async fn parse_upload_multipart(
    state: &AppState,
    mut multipart: Multipart,
) -> Result<ParsedUploadMultipart, ApiError> {
    let mut library_id = None;
    let mut idempotency_key = None;
    let mut title = None;
    let mut file_name = None;
    let mut mime_type = None;
    let mut file_bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(|error| {
        warn!(error = %error, "rejecting canonical content upload with invalid multipart payload");
        map_content_multipart_payload_error(state, &error)
    })? {
        match field.name().unwrap_or_default() {
            "library_id" => {
                let raw = field
                    .text()
                    .await
                    .map_err(|_| ApiError::BadRequest("invalid library_id".to_string()))?;
                library_id =
                    Some(raw.parse().map_err(|_| {
                        ApiError::BadRequest("library_id must be uuid".to_string())
                    })?);
            }
            "idempotency_key" => {
                idempotency_key =
                    Some(field.text().await.map_err(|_| {
                        ApiError::BadRequest("invalid idempotency_key".to_string())
                    })?);
            }
            "title" => {
                title = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| ApiError::BadRequest("invalid title".to_string()))?,
                );
            }
            "file" => {
                let parsed_file = read_multipart_file_field(state, field).await?;
                file_name = Some(parsed_file.file_name);
                mime_type = parsed_file.mime_type;
                file_bytes = Some(parsed_file.file_bytes);
            }
            _ => {}
        }
    }

    Ok(ParsedUploadMultipart {
        library_id: library_id
            .ok_or_else(|| ApiError::BadRequest("missing library_id".to_string()))?,
        idempotency_key: idempotency_key.and_then(normalize_optional_text),
        title: title.and_then(normalize_optional_text),
        file_name: file_name.unwrap_or_else(|| format!("upload-{}", Uuid::now_v7())),
        mime_type,
        file_bytes: file_bytes.ok_or_else(|| {
            ApiError::from_upload_admission(UploadAdmissionError::missing_upload_file(
                "missing file",
            ))
        })?,
    })
}

async fn reprocess_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<ReprocessDocumentRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let summary = state.canonical_services.content.get_document(&state, document_id).await?;
    let active_revision = summary.active_revision.ok_or_else(|| {
        ApiError::BadRequest("document has no active revision to reprocess".to_string())
    })?;
    let resolved_storage_key = state
        .canonical_services
        .content
        .resolve_revision_storage_key(&state, active_revision.id)
        .await?;
    if active_revision.storage_key.is_none() && resolved_storage_key.is_none() {
        return Err(ApiError::BadRequest("document has no stored source to reprocess".to_string()));
    }
    let admission = state
        .canonical_services
        .content
        .admit_mutation(
            &state,
            AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "reprocess".to_string(),
                idempotency_key: payload.idempotency_key,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: Some(build_reprocess_revision_metadata(
                    &active_revision,
                    resolved_storage_key,
                )),
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

fn build_reprocess_revision_metadata(
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

async fn parse_replace_multipart(
    state: &AppState,
    mut multipart: Multipart,
) -> Result<ParsedReplaceMultipart, ApiError> {
    let mut idempotency_key = None;
    let mut file_name = None;
    let mut mime_type = None;
    let mut file_bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(|error| {
        warn!(error = %error, "rejecting canonical replace mutation with invalid multipart payload");
        map_content_multipart_payload_error(state, &error)
    })? {
        match field.name().unwrap_or_default() {
            "idempotency_key" => {
                idempotency_key = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| ApiError::BadRequest("invalid idempotency_key".to_string()))?,
                );
            }
            "file" => {
                let parsed_file = read_multipart_file_field(state, field).await?;
                file_name = Some(parsed_file.file_name);
                mime_type = parsed_file.mime_type;
                file_bytes = Some(parsed_file.file_bytes);
            }
            _ => {}
        }
    }

    Ok(ParsedReplaceMultipart {
        idempotency_key: idempotency_key.and_then(normalize_optional_text),
        file_name: file_name.unwrap_or_else(|| format!("replace-{}", Uuid::now_v7())),
        mime_type,
        file_bytes: file_bytes.ok_or_else(|| {
            ApiError::from_upload_admission(UploadAdmissionError::missing_upload_file(
                "missing file",
            ))
        })?,
    })
}

struct ParsedMultipartFile {
    file_name: String,
    mime_type: Option<String>,
    file_bytes: Vec<u8>,
}

async fn read_multipart_file_field(
    state: &AppState,
    mut field: Field<'_>,
) -> Result<ParsedMultipartFile, ApiError> {
    let file_name = field
        .file_name()
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("upload-{}", Uuid::now_v7()));
    let mime_type = field.content_type().map(ToString::to_string);
    let mut file_bytes = Vec::new();

    while let Some(chunk) = field.chunk().await.map_err(|error| {
        map_content_multipart_file_body_error(state, Some(&file_name), mime_type.as_deref(), &error)
    })? {
        file_bytes.extend_from_slice(&chunk);
    }

    Ok(ParsedMultipartFile { file_name, mime_type, file_bytes })
}

fn map_content_multipart_payload_error(
    state: &AppState,
    error: &axum::extract::multipart::MultipartError,
) -> ApiError {
    let message = error.to_string();
    let rejection = if message.trim().is_empty() {
        UploadAdmissionError::invalid_multipart_payload()
    } else {
        classify_multipart_file_body_error(
            None,
            None,
            state.ui_runtime.upload_max_size_mb,
            &message,
        )
    };
    ApiError::from_upload_admission(rejection)
}

fn map_content_multipart_file_body_error(
    state: &AppState,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    error: &axum::extract::multipart::MultipartError,
) -> ApiError {
    ApiError::from_upload_admission(classify_multipart_file_body_error(
        file_name,
        mime_type,
        state.ui_runtime.upload_max_size_mb,
        &error.to_string(),
    ))
}

fn normalize_optional_text(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

const BATCH_MAX_DOCUMENTS: usize = 100;

async fn batch_delete_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchDeleteRequest>,
) -> Result<Json<BatchDeleteResponse>, ApiError> {
    if request.document_ids.len() > BATCH_MAX_DOCUMENTS {
        return Err(ApiError::BadRequest(format!(
            "batch size exceeds maximum of {BATCH_MAX_DOCUMENTS} documents"
        )));
    }

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut deleted_count = 0usize;
    let mut failed_count = 0usize;

    for document_id in &request.document_ids {
        match load_content_document_and_authorize(
            &auth,
            &state,
            *document_id,
            POLICY_DOCUMENTS_WRITE,
        )
        .await
        {
            Ok(document) => {
                match state
                    .canonical_services
                    .content
                    .admit_mutation(
                        &state,
                        AdmitMutationCommand {
                            workspace_id: document.workspace_id,
                            library_id: document.library_id,
                            document_id: *document_id,
                            operation_kind: "delete".to_string(),
                            idempotency_key: None,
                            requested_by_principal_id: Some(auth.principal_id),
                            request_surface: "rest".to_string(),
                            source_identity: None,
                            revision: None,
                        },
                    )
                    .await
                {
                    Ok(_) => {
                        deleted_count += 1;
                        results.push(BatchDeleteResult {
                            document_id: *document_id,
                            success: true,
                            error: None,
                        });
                    }
                    Err(error) => {
                        failed_count += 1;
                        results.push(BatchDeleteResult {
                            document_id: *document_id,
                            success: false,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(error) => {
                failed_count += 1;
                results.push(BatchDeleteResult {
                    document_id: *document_id,
                    success: false,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(Json(BatchDeleteResponse { deleted_count, failed_count, results }))
}

async fn batch_cancel_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchCancelRequest>,
) -> Result<Json<BatchCancelResponse>, ApiError> {
    if request.document_ids.len() > BATCH_MAX_DOCUMENTS {
        return Err(ApiError::BadRequest(format!(
            "batch size exceeds maximum of {BATCH_MAX_DOCUMENTS} documents"
        )));
    }

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut cancelled_count = 0usize;
    let mut failed_count = 0usize;

    for document_id in &request.document_ids {
        match load_content_document_and_authorize(
            &auth,
            &state,
            *document_id,
            POLICY_DOCUMENTS_WRITE,
        )
        .await
        {
            Ok(_) => {
                match ingest_repository::cancel_queued_jobs_for_document(
                    &state.persistence.postgres,
                    *document_id,
                )
                .await
                {
                    Ok(jobs_cancelled) => {
                        cancelled_count += 1;
                        results.push(BatchCancelResult {
                            document_id: *document_id,
                            jobs_cancelled,
                            success: true,
                            error: None,
                        });
                    }
                    Err(error) => {
                        failed_count += 1;
                        results.push(BatchCancelResult {
                            document_id: *document_id,
                            jobs_cancelled: 0,
                            success: false,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(error) => {
                failed_count += 1;
                results.push(BatchCancelResult {
                    document_id: *document_id,
                    jobs_cancelled: 0,
                    success: false,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(Json(BatchCancelResponse { cancelled_count, failed_count, results }))
}

async fn batch_reprocess_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchReprocessRequest>,
) -> Result<Json<BatchReprocessResponse>, ApiError> {
    if request.document_ids.len() > BATCH_MAX_DOCUMENTS {
        return Err(ApiError::BadRequest(format!(
            "batch size exceeds maximum of {BATCH_MAX_DOCUMENTS} documents"
        )));
    }

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut reprocessed_count = 0usize;
    let mut failed_count = 0usize;

    for document_id in &request.document_ids {
        match reprocess_single_document(&auth, &state, *document_id).await {
            Ok(admission) => {
                reprocessed_count += 1;
                results.push(BatchReprocessResult {
                    document_id: *document_id,
                    success: true,
                    mutation: Some(map_mutation_admission(admission)),
                    error: None,
                });
            }
            Err(error) => {
                failed_count += 1;
                results.push(BatchReprocessResult {
                    document_id: *document_id,
                    success: false,
                    mutation: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(Json(BatchReprocessResponse { reprocessed_count, failed_count, results }))
}

async fn reprocess_single_document(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
) -> Result<ContentMutationAdmission, ApiError> {
    let document =
        load_content_document_and_authorize(auth, state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let summary = state.canonical_services.content.get_document(state, document_id).await?;
    let active_revision = summary.active_revision.ok_or_else(|| {
        ApiError::BadRequest("document has no active revision to reprocess".to_string())
    })?;
    let resolved_storage_key = state
        .canonical_services
        .content
        .resolve_revision_storage_key(state, active_revision.id)
        .await?;
    if active_revision.storage_key.is_none() && resolved_storage_key.is_none() {
        return Err(ApiError::BadRequest("document has no stored source to reprocess".to_string()));
    }
    state
        .canonical_services
        .content
        .admit_mutation(
            state,
            AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "reprocess".to_string(),
                idempotency_key: None,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: Some(build_reprocess_revision_metadata(
                    &active_revision,
                    resolved_storage_key,
                )),
            },
        )
        .await
}

async fn export_library(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;

    let summaries = state.canonical_services.content.list_documents(&state, library.id).await?;

    let mut export_docs = Vec::new();
    for summary in &summaries {
        if summary.document.document_state == "deleted" {
            continue;
        }
        let revision_id = summary.head.as_ref().and_then(|h| h.effective_revision_id());
        let Some(revision_id) = revision_id else {
            continue;
        };

        let arango_rev = state.arango_document_store.get_revision(revision_id).await.ok().flatten();
        let content = arango_rev.and_then(|r| r.normalized_text).unwrap_or_default();
        if content.is_empty() {
            continue;
        }

        let title = summary
            .active_revision
            .as_ref()
            .and_then(|rev| rev.title.clone())
            .unwrap_or_else(|| summary.document.external_key.clone());
        let source_uri = summary.active_revision.as_ref().and_then(|rev| rev.source_uri.clone());
        let mime_type =
            summary.active_revision.as_ref().map(|rev| rev.mime_type.clone()).unwrap_or_default();

        export_docs.push(serde_json::json!({
            "title": title,
            "sourceUri": source_uri,
            "mimeType": mime_type,
            "content": content,
        }));
    }

    let export = serde_json::json!({
        "version": "1.0",
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "library": {
            "displayName": library.display_name,
            "description": library.description,
            "extractionPrompt": library.extraction_prompt,
        },
        "documentCount": export_docs.len(),
        "documents": export_docs,
    });

    let filename = format!("{}.json", library.slug);
    let disposition = format!("attachment; filename=\"{filename}\"");
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        Json(export),
    ))
}

async fn import_library(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let docs = payload
        .get("documents")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::BadRequest("missing documents array".to_string()))?;

    let mut imported = 0;
    for doc in docs {
        let title = doc.get("title").and_then(|v| v.as_str()).unwrap_or("Imported document");
        let content = doc.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() {
            continue;
        }
        let mime_type = doc.get("mimeType").and_then(|v| v.as_str()).unwrap_or("text/plain");

        let file_bytes = content.as_bytes().to_vec();
        let file_name = format!("{title}.txt");

        state
            .canonical_services
            .content
            .upload_inline_document(
                &state,
                UploadInlineDocumentCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    external_key: None,
                    idempotency_key: None,
                    requested_by_principal_id: Some(auth.principal_id),
                    request_surface: "rest-import".to_string(),
                    source_identity: None,
                    file_name,
                    title: Some(title.to_string()),
                    mime_type: Some(mime_type.to_string()),
                    file_bytes,
                },
            )
            .await?;

        imported += 1;
    }

    Ok(Json(serde_json::json!({
        "importedDocuments": imported,
        "libraryId": library_id,
    })))
}

fn map_mutation_admission(admission: ContentMutationAdmission) -> ContentMutationDetailResponse {
    ContentMutationDetailResponse {
        mutation: admission.mutation,
        items: admission.items,
        job_id: admission.job_id,
        async_operation_id: admission.async_operation_id,
    }
}

async fn resolve_readable_revision_id(
    state: &AppState,
    document_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    let head = state.canonical_services.content.get_document_head(state, document_id).await?;
    Ok(head.and_then(|row| row.effective_revision_id()))
}

fn normalize_page_window(offset: Option<usize>, limit: Option<usize>) -> (usize, usize) {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(100).clamp(1, 500);
    (offset, limit)
}

fn paginate_items<T>(items: Vec<T>, offset: usize, limit: usize) -> Vec<T> {
    items.into_iter().skip(offset).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::build_reprocess_revision_metadata;
    use crate::domains::content::ContentRevision;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn reprocess_metadata_preserves_active_revision_source_kind() {
        let revision = ContentRevision {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 1,
            parent_revision_id: None,
            content_source_kind: "upload".to_string(),
            checksum: "sha256:test".to_string(),
            mime_type: "application/pdf".to_string(),
            byte_size: 636,
            title: Some("runtime-upload-check.pdf".to_string()),
            language_code: Some("ru".to_string()),
            source_uri: Some("upload://runtime-upload-check.pdf".to_string()),
            storage_key: Some("storage/runtime-upload-check.pdf".to_string()),
            created_by_principal_id: None,
            created_at: Utc::now(),
        };

        let metadata = build_reprocess_revision_metadata(&revision, None);

        assert_eq!(metadata.content_source_kind, "upload");
        assert_eq!(metadata.checksum, revision.checksum);
        assert_eq!(metadata.mime_type, revision.mime_type);
        assert_eq!(metadata.byte_size, revision.byte_size);
        assert_eq!(metadata.title, revision.title);
        assert_eq!(metadata.language_code, revision.language_code);
        assert_eq!(metadata.source_uri, revision.source_uri);
        assert_eq!(metadata.storage_key, revision.storage_key);
    }
}
