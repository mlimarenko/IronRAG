use axum::{
    Json, Router,
    extract::{Multipart, Path, Query, State},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{
        ContentDocument, ContentDocumentHead, ContentDocumentSummary, ContentMutation,
        ContentMutationItem, ContentRevision,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_READ, POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ,
            POLICY_LIBRARY_WRITE, load_content_document_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::{
        content_service::{
            AcceptMutationCommand, CreateDocumentCommand, CreateMutationItemCommand,
            CreateRevisionCommand, PromoteHeadCommand, UpdateMutationCommand,
            UpdateMutationItemCommand,
        },
        ingest_service::AdmitIngestJobCommand,
        mcp_memory::{
            McpDocumentMutationKind, McpUpdateDocumentRequest, McpUploadDocumentInput,
            McpUploadDocumentsRequest,
        },
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDocumentDetailResponse {
    pub document: ContentDocument,
    pub head: Option<ContentDocumentHead>,
    pub active_revision: Option<ContentRevision>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMutationDetailResponse {
    pub mutation: ContentMutation,
    pub items: Vec<ContentMutationItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentResponse {
    pub document: ContentDocumentDetailResponse,
    pub mutation: ContentMutationDetailResponse,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/content/documents", get(list_documents).post(create_document))
        .route("/content/documents/upload", axum::routing::post(upload_document))
        .route("/content/documents/{document_id}", get(get_document).delete(delete_document))
        .route("/content/documents/{document_id}/append", axum::routing::post(append_document))
        .route("/content/documents/{document_id}/replace", axum::routing::post(replace_document))
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

    let content_service = &state.canonical_services.content;
    let ingest_service = &state.canonical_services.ingest;
    let document = content_service
        .create_document(
            &state,
            CreateDocumentCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                external_key: payload.external_key.clone(),
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;

    let mutation = content_service
        .accept_mutation(
            &state,
            AcceptMutationCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                operation_kind: "upload".to_string(),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                idempotency_key: payload.idempotency_key.clone(),
            },
        )
        .await?;

    let mut items = Vec::new();
    let mut job_id = None;
    if let Some(revision_command) =
        build_revision_command(document.id, Some(auth.principal_id), &payload)?
    {
        let revision = content_service.create_revision(&state, revision_command).await?;
        let item = content_service
            .create_mutation_item(
                &state,
                CreateMutationItemCommand {
                    mutation_id: mutation.id,
                    document_id: Some(document.id),
                    base_revision_id: None,
                    result_revision_id: Some(revision.id),
                    item_state: "pending".to_string(),
                    message: Some("document revision accepted and queued for ingest".to_string()),
                },
            )
            .await?;
        items.push(item);
        let head = content_service.get_document_head(&state, document.id).await?;
        let _ = content_service
            .promote_document_head(
                &state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: head.and_then(|row| row.readable_revision_id),
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: None,
                },
            )
            .await?;
        let job = ingest_service
            .admit_job(
                &state,
                AdmitIngestJobCommand {
                    workspace_id: payload.workspace_id,
                    library_id: payload.library_id,
                    mutation_id: Some(mutation.id),
                    connector_id: None,
                    job_kind: "content_mutation".to_string(),
                    priority: 100,
                    dedupe_key: payload.idempotency_key.clone(),
                    available_at: None,
                },
            )
            .await?;
        job_id = Some(job.id);
    } else {
        let _ = content_service
            .promote_document_head(
                &state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: None,
                    readable_revision_id: None,
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: None,
                },
            )
            .await?;
        let mutation = content_service
            .update_mutation(
                &state,
                UpdateMutationCommand {
                    mutation_id: mutation.id,
                    mutation_state: "applied".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await?;
        let summary = content_service.get_document(&state, document.id).await?;
        return Ok(Json(CreateDocumentResponse {
            document: map_document_summary(summary),
            mutation: ContentMutationDetailResponse { mutation, items, job_id },
        }));
    }

    let summary = content_service.get_document(&state, document.id).await?;
    Ok(Json(CreateDocumentResponse {
        document: map_document_summary(summary),
        mutation: ContentMutationDetailResponse { mutation, items, job_id },
    }))
}

async fn upload_document(
    auth: AuthContext,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<CreateDocumentResponse>, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let payload = parse_upload_multipart(&state, multipart).await?;
    let receipts = state
        .mcp_memory_services
        .memory
        .upload_documents(
            &auth,
            &state,
            McpUploadDocumentsRequest {
                library_id: payload.library_id,
                idempotency_key: payload.idempotency_key.clone(),
                documents: vec![McpUploadDocumentInput {
                    file_name: payload.file_name,
                    content_base64: BASE64_STANDARD.encode(payload.file_bytes),
                    mime_type: payload.mime_type,
                    title: payload.title,
                }],
            },
        )
        .await?;
    let receipt = receipts.into_iter().next().ok_or(ApiError::Internal)?;
    let document_id = receipt.document_id.ok_or(ApiError::Internal)?;
    let document = state.canonical_services.content.get_document(&state, document_id).await?;
    let mutation = load_mutation_detail_response(&state, receipt.receipt_id).await?;
    Ok(Json(CreateDocumentResponse { document: map_document_summary(document), mutation }))
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

async fn delete_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let content_service = &state.canonical_services.content;
    let current_head = content_service.get_document_head(&state, document_id).await?;
    let mutation = content_service
        .accept_mutation(
            &state,
            AcceptMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                operation_kind: "delete".to_string(),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                idempotency_key: None,
            },
        )
        .await?;
    let mut item = content_service
        .create_mutation_item(
            &state,
            CreateMutationItemCommand {
                mutation_id: mutation.id,
                document_id: Some(document_id),
                base_revision_id: current_head.as_ref().and_then(|row| row.active_revision_id),
                result_revision_id: None,
                item_state: "pending".to_string(),
                message: Some("document deletion accepted".to_string()),
            },
        )
        .await?;
    let _ = content_service.delete_document(&state, document_id).await?;
    item = content_service
        .update_mutation_item(
            &state,
            UpdateMutationItemCommand {
                item_id: item.id,
                document_id: Some(document_id),
                base_revision_id: current_head.as_ref().and_then(|row| row.active_revision_id),
                result_revision_id: None,
                item_state: "applied".to_string(),
                message: Some("document deleted".to_string()),
            },
        )
        .await?;
    let mutation = content_service
        .update_mutation(
            &state,
            UpdateMutationCommand {
                mutation_id: mutation.id,
                mutation_state: "applied".to_string(),
                completed_at: Some(Utc::now()),
                failure_code: None,
                conflict_code: None,
            },
        )
        .await?;
    Ok(Json(ContentMutationDetailResponse { mutation, items: vec![item], job_id: None }))
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
    let receipt = state
        .mcp_memory_services
        .memory
        .update_document(
            &auth,
            &state,
            McpUpdateDocumentRequest {
                library_id: document.library_id,
                document_id,
                operation_kind: McpDocumentMutationKind::Append,
                idempotency_key: payload.idempotency_key,
                appended_text: Some(payload.appended_text),
                replacement_file_name: None,
                replacement_content_base64: None,
                replacement_mime_type: None,
            },
        )
        .await?;
    Ok(Json(load_mutation_detail_response(&state, receipt.receipt_id).await?))
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
    let receipt = state
        .mcp_memory_services
        .memory
        .update_document(
            &auth,
            &state,
            McpUpdateDocumentRequest {
                library_id: document.library_id,
                document_id,
                operation_kind: McpDocumentMutationKind::Replace,
                idempotency_key: payload.idempotency_key,
                appended_text: None,
                replacement_file_name: Some(payload.file_name),
                replacement_content_base64: Some(BASE64_STANDARD.encode(payload.file_bytes)),
                replacement_mime_type: payload.mime_type,
            },
        )
        .await?;
    Ok(Json(load_mutation_detail_response(&state, receipt.receipt_id).await?))
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

    let content_service = &state.canonical_services.content;
    let ingest_service = &state.canonical_services.ingest;
    let current_head = content_service.get_document_head(&state, document.id).await?;
    let base_revision_id =
        current_head.as_ref().and_then(|row| row.active_revision_id.or(row.readable_revision_id));

    let mutation = content_service
        .accept_mutation(
            &state,
            AcceptMutationCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                operation_kind: payload.operation_kind.clone(),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                idempotency_key: payload.idempotency_key.clone(),
            },
        )
        .await?;

    if payload.operation_kind == "delete" {
        let item = content_service
            .create_mutation_item(
                &state,
                CreateMutationItemCommand {
                    mutation_id: mutation.id,
                    document_id: Some(document.id),
                    base_revision_id,
                    result_revision_id: None,
                    item_state: "pending".to_string(),
                    message: Some("document deletion accepted".to_string()),
                },
            )
            .await?;
        let _ = content_service.delete_document(&state, document.id).await?;
        let item = content_service
            .update_mutation_item(
                &state,
                UpdateMutationItemCommand {
                    item_id: item.id,
                    document_id: Some(document.id),
                    base_revision_id,
                    result_revision_id: None,
                    item_state: "applied".to_string(),
                    message: Some("document deleted".to_string()),
                },
            )
            .await?;
        let mutation = content_service
            .update_mutation(
                &state,
                UpdateMutationCommand {
                    mutation_id: mutation.id,
                    mutation_state: "applied".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await?;
        return Ok(Json(ContentMutationDetailResponse {
            mutation,
            items: vec![item],
            job_id: None,
        }));
    }

    let revision_command = build_revision_command(
        document.id,
        Some(auth.principal_id),
        &CreateDocumentRequest {
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
        },
    )?
    .ok_or_else(|| {
        ApiError::BadRequest(
            "revision metadata is required for non-delete document mutations".to_string(),
        )
    })?;

    let revision = match payload.operation_kind.as_str() {
        "append" => content_service.append_revision(&state, revision_command).await?,
        "replace" => content_service.replace_revision(&state, revision_command).await?,
        _ => {
            return Err(ApiError::BadRequest(format!(
                "unsupported mutation operationKind '{}'",
                payload.operation_kind
            )));
        }
    };

    let item = content_service
        .create_mutation_item(
            &state,
            CreateMutationItemCommand {
                mutation_id: mutation.id,
                document_id: Some(document.id),
                base_revision_id,
                result_revision_id: Some(revision.id),
                item_state: "pending".to_string(),
                message: Some("revision accepted and queued for ingest".to_string()),
            },
        )
        .await?;
    let _ = content_service
        .promote_document_head(
            &state,
            PromoteHeadCommand {
                document_id: document.id,
                active_revision_id: Some(revision.id),
                readable_revision_id: current_head.and_then(|row| row.readable_revision_id),
                latest_mutation_id: Some(mutation.id),
                latest_successful_attempt_id: None,
            },
        )
        .await?;
    let job = ingest_service
        .admit_job(
            &state,
            AdmitIngestJobCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                mutation_id: Some(mutation.id),
                connector_id: None,
                job_kind: "content_mutation".to_string(),
                priority: 100,
                dedupe_key: payload.idempotency_key.clone(),
                available_at: None,
            },
        )
        .await?;

    Ok(Json(ContentMutationDetailResponse { mutation, items: vec![item], job_id: Some(job.id) }))
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
    let mutations = state.canonical_services.content.list_mutations(&state, library.id).await?;
    let jobs = state
        .canonical_services
        .ingest
        .list_jobs(&state, Some(library.workspace_id), Some(library.id))
        .await?;

    let mut responses = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        let items =
            state.canonical_services.content.list_mutation_items(&state, mutation.id).await?;
        let job_id = jobs.iter().find(|job| job.mutation_id == Some(mutation.id)).map(|job| job.id);
        responses.push(ContentMutationDetailResponse { mutation, items, job_id });
    }

    Ok(Json(responses))
}

async fn get_mutation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(mutation_id): Path<Uuid>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let mutation = state.canonical_services.content.get_mutation(&state, mutation_id).await?;
    let library =
        load_library_and_authorize(&auth, &state, mutation.library_id, POLICY_LIBRARY_READ).await?;
    if library.workspace_id != mutation.workspace_id {
        return Err(ApiError::Unauthorized);
    }
    let items = state.canonical_services.content.list_mutation_items(&state, mutation_id).await?;
    let jobs = state
        .canonical_services
        .ingest
        .list_jobs(&state, Some(mutation.workspace_id), Some(mutation.library_id))
        .await?;
    let job_id =
        jobs.into_iter().find(|job| job.mutation_id == Some(mutation_id)).map(|job| job.id);
    Ok(Json(ContentMutationDetailResponse { mutation, items, job_id }))
}

fn map_document_summary(summary: ContentDocumentSummary) -> ContentDocumentDetailResponse {
    ContentDocumentDetailResponse {
        document: summary.document,
        head: summary.head,
        active_revision: summary.active_revision,
    }
}

fn build_revision_command(
    document_id: Uuid,
    created_by_principal_id: Option<Uuid>,
    payload: &CreateDocumentRequest,
) -> Result<Option<CreateRevisionCommand>, ApiError> {
    let checksum = payload.checksum.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let mime_type = payload.mime_type.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let byte_size = payload.byte_size;

    match (checksum, mime_type, byte_size) {
        (None, None, None) => Ok(None),
        (Some(checksum), Some(mime_type), Some(byte_size)) => Ok(Some(CreateRevisionCommand {
            document_id,
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
            created_by_principal_id,
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
                file_name = field.file_name().map(ToString::to_string);
                mime_type = field.content_type().map(ToString::to_string);
                file_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|error| {
                            map_content_multipart_file_body_error(
                                state,
                                file_name.as_deref(),
                                mime_type.as_deref(),
                                &error,
                            )
                        })?
                        .to_vec(),
                );
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
                file_name = field.file_name().map(ToString::to_string);
                mime_type = field.content_type().map(ToString::to_string);
                file_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|error| {
                            map_content_multipart_file_body_error(
                                state,
                                file_name.as_deref(),
                                mime_type.as_deref(),
                                &error,
                            )
                        })?
                        .to_vec(),
                );
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

async fn load_mutation_detail_response(
    state: &AppState,
    mutation_id: Uuid,
) -> Result<ContentMutationDetailResponse, ApiError> {
    let mutation = state.canonical_services.content.get_mutation(state, mutation_id).await?;
    let items = state.canonical_services.content.list_mutation_items(state, mutation_id).await?;
    let jobs = state
        .canonical_services
        .ingest
        .list_jobs(state, Some(mutation.workspace_id), Some(mutation.library_id))
        .await?;
    let job_id =
        jobs.into_iter().find(|job| job.mutation_id == Some(mutation_id)).map(|job| job.id);
    Ok(ContentMutationDetailResponse { mutation, items, job_id })
}
