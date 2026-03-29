use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        multipart::{Field, Multipart},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{
        ContentDocument, ContentDocumentHead, ContentDocumentPipelineState,
        ContentDocumentSummary, ContentMutation, ContentMutationItem, ContentRevision,
        ContentRevisionReadiness,
    },
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDocumentDetailResponse {
    pub document: ContentDocument,
    pub file_name: String,
    pub head: Option<ContentDocumentHead>,
    pub active_revision: Option<ContentRevision>,
    pub readiness: Option<ContentRevisionReadiness>,
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
    pub project_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/chunks", get(list_chunks))
        .route("/content/documents", get(list_documents).post(create_document))
        .route("/content/documents/upload", axum::routing::post(upload_document))
        .route("/content/documents/{document_id}", get(get_document).delete(delete_document))
        .route("/content/documents/{document_id}/append", axum::routing::post(append_document))
        .route("/content/documents/{document_id}/replace", axum::routing::post(replace_document))
        .route("/content/documents/{document_id}/head", get(get_document_head))
        .route("/content/documents/{document_id}/reprocess", post(reprocess_document))
        .route("/content/documents/{document_id}/revisions", get(list_revisions))
        .route("/content/mutations", get(list_mutations).post(create_mutation))
        .route("/content/mutations/{mutation_id}", get(get_mutation))
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
    let revision_id = head.and_then(|row| row.readable_revision_id.or(row.active_revision_id));
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
                project_id: document.library_id,
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
        return Err(ApiError::BadRequest(
            "document has no stored source to reprocess".to_string(),
        ));
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

fn map_mutation_admission(admission: ContentMutationAdmission) -> ContentMutationDetailResponse {
    ContentMutationDetailResponse {
        mutation: admission.mutation,
        items: admission.items,
        job_id: admission.job_id,
        async_operation_id: admission.async_operation_id,
    }
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
