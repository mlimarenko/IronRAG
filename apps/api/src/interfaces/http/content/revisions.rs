use super::*;

#[utoipa::path(
    post,
    path = "/v1/content/documents/{documentId}/visibility-intents",
    tag = "content",
    operation_id = "createContentVisibilityIntent",
    params(("documentId" = uuid::Uuid, Path, description = "Document identifier")),
    request_body = ironrag_contracts::documents::CreateVisibilityIntentRequest,
    responses(
        (status = 201, description = "Independent visibility obligation accepted; no ingest job", body = ironrag_contracts::documents::VisibilityIntentReceipt),
        (status = 200, description = "Exact original receipt replayed without recapturing the floor", body = ironrag_contracts::documents::VisibilityIntentReceipt),
        (status = 400, description = "Invalid descriptor or deleted document"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document not found"),
        (status = 409, description = "Complete identity conflicts with the existing key"),
    ),
)]
pub async fn create_visibility_intent(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(intent): Json<ironrag_contracts::documents::CreateVisibilityIntentRequest>,
) -> Result<axum::response::Response, ApiError> {
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let outcome = state
        .application_services
        .content
        .admit_visibility_intent(
            &state,
            crate::infra::repositories::admission_repository::VisibilityAdmissionRequest {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_owned(),
                intent,
            },
        )
        .await?;
    let status = match &outcome {
        crate::infra::repositories::admission_repository::AdmissionOutcome::Created(_) => {
            axum::http::StatusCode::CREATED
        }
        crate::infra::repositories::admission_repository::AdmissionOutcome::Replayed(_) => {
            axum::http::StatusCode::OK
        }
    };
    Ok((status, Json(outcome.into_bundle().receipt)).into_response())
}

/// Creates a new revision of a document. Content-negotiated on
/// `Content-Type`, mirroring `create_document`'s pattern: `application/json`
/// with `{mode:"append"|"replace", appendedText|markdown}` covers text
/// changes; `multipart/form-data` replaces the underlying file bytes. One
/// creation door for revisions, async (202 + Location to the tracked operation).
#[tracing::instrument(level = "info", name = "http.create_revision", skip_all, fields(document_id = %document_id))]
#[utoipa::path(
    post,
    path = "/v1/content/documents/{documentId}/revisions",
    tag = "content",
    operation_id = "createContentRevision",
    params(("documentId" = uuid::Uuid, Path, description = "Document identifier")),
    request_body(
        content(
            (CreateRevisionRequest = "application/json"),
            (CreateRevisionMultipartRequest = "multipart/form-data"),
        ),
        description = "JSON body for a text append/replace, OR multipart/form-data to replace the file",
    ),
    responses(
        (status = 202, description = "Revision admitted for async processing", body = ContentMutationDetailResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document not found"),
        (status = 415, description = "Content-Type is neither application/json nor multipart/form-data"),
    ),
)]
pub async fn create_revision(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ApiError> {
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    let admission = if content_type.starts_with("multipart/form-data") {
        let multipart = Multipart::from_request(request, &state)
            .await
            .map_err(|error| ApiError::BadRequest(format!("invalid multipart body: {error}")))?;
        let payload = parse_replace_multipart(&state, multipart).await?;
        state
            .application_services
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
                    visibility_intent_id: payload.visibility_intent_id,
                    file_name: payload.file_name,
                    title: payload.title,
                    language_code: payload.language_code,
                    source_uri: payload.source_uri,
                    document_hint: payload.document_hint,
                    mime_type: payload.mime_type,
                    file_bytes: payload.file_bytes,
                },
            )
            .await?
    } else if content_type.starts_with("application/json") {
        const MAX_JSON_BODY_BYTES: usize = 25 * 1024 * 1024;
        let bytes = axum::body::to_bytes(request.into_body(), MAX_JSON_BODY_BYTES).await.map_err(
            |error| ApiError::BadRequest(format!("failed to read request body: {error}")),
        )?;
        let body: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::BadRequest(format!("invalid JSON body: {error}")))?;
        if body["mode"] == "append"
            && [
                "markdown",
                "fileName",
                "mimeType",
                "title",
                "languageCode",
                "sourceUri",
                "documentHint",
                "visibilityIntentId",
            ]
            .iter()
            .any(|field| body.get(field).is_some())
        {
            return Err(ApiError::BadRequest(
                "append does not accept replacement metadata or visibility binding".to_owned(),
            ));
        }
        let payload: CreateRevisionRequest = serde_json::from_value(body)
            .map_err(|error| ApiError::BadRequest(format!("invalid JSON body: {error}")))?;
        match payload.mode {
            types::RevisionMode::Append => {
                if payload.visibility_intent_id.is_some() {
                    return Err(ApiError::VisibilityBindingConflict(
                        "visibility binding is only valid for complete replacement".to_owned(),
                    ));
                }
                let appended_text = payload.appended_text.ok_or_else(|| {
                    ApiError::BadRequest("appendedText is required when mode is append".to_string())
                })?;
                state
                    .application_services
                    .content
                    .append_inline_mutation(
                        &state,
                        AppendInlineMutationCommand {
                            base_revision_id: payload.base_revision_id.ok_or_else(|| {
                                ApiError::BadRequest(
                                    "baseRevisionId is required when mode is append".to_owned(),
                                )
                            })?,
                            workspace_id: document.workspace_id,
                            library_id: document.library_id,
                            document_id,
                            idempotency_key: payload.idempotency_key,
                            requested_by_principal_id: Some(auth.principal_id),
                            request_surface: "rest".to_string(),
                            source_identity: None,
                            appended_text,
                        },
                    )
                    .await?
            }
            types::RevisionMode::Replace => {
                let markdown = payload.markdown.ok_or_else(|| {
                    ApiError::BadRequest("markdown is required when mode is replace".to_string())
                })?;
                state
                    .application_services
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
                            visibility_intent_id: payload.visibility_intent_id,
                            file_name: payload.file_name.ok_or_else(|| {
                                ApiError::BadRequest(
                                    "fileName is required for replacement".to_owned(),
                                )
                            })?,
                            mime_type: payload.mime_type,
                            title: payload.title,
                            language_code: payload.language_code,
                            source_uri: payload.source_uri,
                            document_hint: payload.document_hint,
                            file_bytes: markdown.into_bytes(),
                        },
                    )
                    .await?
            }
        }
    } else {
        return Err(ApiError::unsupported_media_type(format!(
            "Content-Type must be application/json or multipart/form-data, got `{content_type}`"
        )));
    };

    let body = map_mutation_admission(admission);
    let async_operation_id = body.async_operation_id;
    let mut response = (axum::http::StatusCode::ACCEPTED, Json(body)).into_response();
    if let Some(operation_id) = async_operation_id {
        let location = format!("/v1/ops/operations/{operation_id}");
        if let Ok(value) = axum::http::HeaderValue::from_str(&location) {
            response.headers_mut().insert(axum::http::header::LOCATION, value);
        }
    }
    Ok(response)
}

#[tracing::instrument(
    level = "info",
    name = "http.list_revisions",
    skip_all,
    fields(document_id = %document_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/content/documents/{documentId}/revisions",
    tag = "content",
    operation_id = "listContentRevisions",
    params(("documentId" = uuid::Uuid, Path, description = "Document identifier")),
    responses(
        (status = 200, description = "Currently published revision for the document; withdrawn history is not exposed", body = [ContentRevision]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
    ),
)]
pub async fn list_revisions(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<Vec<ContentRevision>>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let revision =
        state.application_services.content.get_current_revision(&state, document_id).await?;
    let revisions = match revision {
        Some(revision)
            if crate::infra::repositories::content_fingerprint_repository::revision_is_current(
                &state.persistence.postgres,
                document_id,
                revision.id,
            )
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "revision visibility lookup failed")
            })? =>
        {
            vec![revision]
        }
        _ => Vec::new(),
    };
    span.record("item_count", revisions.len());
    Ok(Json(revisions))
}

// POST/GET /content/mutations (+ /{mutationId}) were removed here — a
// duplicate write/read surface superseded by the typed create/revisions
// endpoints above and by GET/QUERY /v1/audit/events for the admission
// trail. Keeping both would have violated "one write model per resource".

#[tracing::instrument(
    level = "info",
    name = "http.reprocess_document",
    skip_all,
    fields(document_id = %document_id)
)]
#[utoipa::path(
    post,
    path = "/v1/content/documents/{documentId}/reprocess",
    tag = "content",
    operation_id = "reprocessContentDocument",
    params(("documentId" = uuid::Uuid, Path, description = "Document identifier")),
    request_body = ReprocessDocumentRequest,
    responses(
        (status = 200, description = "Mutation describing the reprocess", body = ContentMutationDetailResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document not found"),
    ),
)]
pub async fn reprocess_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<ReprocessDocumentRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
        .await?;
    let admission = self::batch::reprocess_single_document(
        &state,
        None,
        auth.principal_id,
        payload.idempotency_key,
        document_id,
    )
    .await?;
    Ok(Json(map_mutation_admission(admission)))
}
