mod batch;
mod library_transfer;
mod multipart;
mod types;
mod web_runs;

use axum::{
    Json, Router,
    extract::{Path, Query, State, multipart::Multipart},
    routing::{get, post},
};
use uuid::Uuid;

use self::{
    batch::{batch_cancel_documents, batch_delete_documents, batch_reprocess_documents},
    library_transfer::{download_document_source, export_library, import_library},
    multipart::{parse_replace_multipart, parse_upload_multipart},
    types::{
        AppendDocumentBodyRequest, ChunkSummary, ChunksQuery, ContentDocumentDetailResponse,
        ContentMutationDetailResponse, CreateDocumentRequest, CreateDocumentResponse,
        CreateMutationRequest, EditDocumentRequest, ListDocumentsQuery, ListMutationsQuery,
        PreparedDataQuery, PreparedSegmentsPageResponse, ReprocessDocumentRequest,
        TechnicalFactsPageResponse, build_reprocess_revision_metadata, build_revision_metadata,
        map_document_summary, map_mutation_admission, normalize_page_window, paginate_items,
    },
    web_runs::{
        cancel_web_ingest_run, create_web_ingest_run, get_web_ingest_run,
        list_web_ingest_run_pages, list_web_ingest_runs,
    },
};
use crate::{
    app::state::AppState,
    domains::content::{ContentDocumentHead, ContentRevision},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_READ, POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ,
            POLICY_LIBRARY_WRITE, load_canonical_content_document_and_authorize,
            load_content_document_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::content::service::{
        AdmitDocumentCommand, AdmitMutationCommand, AppendInlineMutationCommand,
        CreateDocumentAdmission, EditInlineMutationCommand, ReplaceInlineMutationCommand,
        UploadInlineDocumentCommand,
    },
};

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
        .route("/content/documents/{document_id}/source", get(download_document_source))
        .route("/content/documents/{document_id}/append", axum::routing::post(append_document))
        .route("/content/documents/{document_id}/edit", axum::routing::post(edit_document))
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

    let summaries = state
        .canonical_services
        .content
        .list_documents_with_deleted(&state, library.id, include_deleted)
        .await?;
    let items = summaries.into_iter().map(map_document_summary).collect();
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
                file_name: None,
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
                title: payload.title.or(Some(payload.file_name.clone())),
                file_name: payload.file_name,
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
    let lifecycle = crate::services::content::document_accounting::load_document_lifecycle(
        &state,
        summary.document.workspace_id,
        summary.document.library_id,
        summary.document.id,
    )
    .await
    .ok();
    let mut response = map_document_summary(summary);
    response.lifecycle = lifecycle;
    Ok(Json(response))
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
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
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
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
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

async fn edit_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<EditDocumentRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let admission = state
        .canonical_services
        .content
        .edit_inline_mutation(
            &state,
            EditInlineMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                idempotency_key: payload.idempotency_key,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                markdown: payload.markdown,
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
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
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
    let document = load_canonical_content_document_and_authorize(
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

async fn reprocess_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<ReprocessDocumentRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
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

async fn resolve_readable_revision_id(
    state: &AppState,
    document_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    let head = state.canonical_services.content.get_document_head(state, document_id).await?;
    Ok(head.and_then(|row| row.effective_revision_id()))
}

#[cfg(test)]
mod tests {
    use super::{batch, types};
    use crate::domains::content::{
        ContentDocument, ContentDocumentPipelineState, ContentDocumentSummary, ContentRevision,
    };
    use crate::interfaces::http::router_support::ApiError;
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

        let metadata = types::build_reprocess_revision_metadata(&revision, None);

        assert_eq!(metadata.content_source_kind, "upload");
        assert_eq!(metadata.checksum, revision.checksum);
        assert_eq!(metadata.mime_type, revision.mime_type);
        assert_eq!(metadata.byte_size, revision.byte_size);
        assert_eq!(metadata.title, revision.title);
        assert_eq!(metadata.language_code, revision.language_code);
        assert_eq!(metadata.source_uri, revision.source_uri);
        assert_eq!(metadata.storage_key, revision.storage_key);
    }

    #[test]
    fn reprocess_metadata_preserves_edited_source_storage() {
        let revision = ContentRevision {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 3,
            parent_revision_id: Some(Uuid::now_v7()),
            content_source_kind: "edit".to_string(),
            checksum: "sha256:edited".to_string(),
            mime_type: "text/markdown".to_string(),
            byte_size: 128,
            title: Some("Inventory.xlsx".to_string()),
            language_code: None,
            source_uri: Some("edit://Inventory.md".to_string()),
            storage_key: Some("content/demo/Inventory.md".to_string()),
            created_by_principal_id: None,
            created_at: Utc::now(),
        };

        let metadata = types::build_reprocess_revision_metadata(&revision, None);

        assert_eq!(metadata.content_source_kind, "edit");
        assert_eq!(metadata.mime_type, "text/markdown");
        assert_eq!(metadata.source_uri.as_deref(), Some("edit://Inventory.md"));
        assert_eq!(metadata.storage_key.as_deref(), Some("content/demo/Inventory.md"));
    }

    #[test]
    fn batch_limit_allows_documents_surface_capacity() {
        assert_eq!(batch::BATCH_MAX_DOCUMENTS, 1000);
        assert!(batch::ensure_batch_document_limit(batch::BATCH_MAX_DOCUMENTS).is_ok());
    }

    #[test]
    fn batch_limit_rejects_more_than_documents_surface_capacity() {
        let error = batch::ensure_batch_document_limit(batch::BATCH_MAX_DOCUMENTS + 1)
            .expect_err("requests above the canonical documents surface limit must fail");
        let ApiError::BadRequest(message) = error else {
            unreachable!(
                "ensure_batch_document_limit must return bad_request for capacity violations"
            );
        };
        assert!(message.contains("batch size exceeds maximum"));
    }

    #[test]
    fn map_document_summary_uses_canonical_summary_file_name() {
        let document_id = Uuid::now_v7();
        let summary = ContentDocumentSummary {
            document: ContentDocument {
                id: document_id,
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                external_key: "external-key".to_string(),
                document_state: "active".to_string(),
                created_at: Utc::now(),
            },
            file_name: "readable-revision.pdf".to_string(),
            head: None,
            active_revision: Some(ContentRevision {
                id: Uuid::now_v7(),
                document_id,
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                revision_number: 2,
                parent_revision_id: None,
                content_source_kind: "replace".to_string(),
                checksum: "checksum".to_string(),
                mime_type: "application/pdf".to_string(),
                byte_size: 128,
                title: Some("processing-replacement.pdf".to_string()),
                language_code: None,
                source_uri: Some("upload://processing-replacement.pdf".to_string()),
                storage_key: Some("content/demo".to_string()),
                created_by_principal_id: None,
                created_at: Utc::now(),
            }),
            source_access: None,
            readiness: None,
            readiness_summary: None,
            prepared_revision: None,
            web_page_provenance: None,
            pipeline: ContentDocumentPipelineState { latest_mutation: None, latest_job: None },
        };

        let response = types::map_document_summary(summary);

        assert_eq!(response.file_name, "readable-revision.pdf");
    }
}
