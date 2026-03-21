use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::ContentMutation,
    infra::repositories::catalog_repository::CatalogLibraryRow,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_WRITE, authorize_document_permission,
            authorize_library_discovery, authorize_library_permission,
        },
        router_support::ApiError,
    },
    mcp_types::{
        McpDocumentMutationKind, McpGetMutationStatusRequest, McpMutationOperationKind,
        McpMutationReceipt, McpMutationReceiptStatus, McpUpdateDocumentRequest,
        McpUploadDocumentInput, McpUploadDocumentsRequest,
    },
    services::content_service::{
        AppendInlineMutationCommand, ReplaceInlineMutationCommand, UploadInlineDocumentCommand,
    },
    services::mcp_access,
    services::mcp_support::{
        hash_append_payload, hash_replace_payload, hash_upload_payload,
        map_content_mutation_status_to_receipt_status, normalize_document_idempotency_key,
        normalize_upload_idempotency_key, parse_mutation_operation_kind,
        payload_identity_from_source_uri, validate_mcp_upload_batch_size,
        validate_mcp_upload_file_size,
    },
};

pub async fn upload_documents(
    auth: &AuthContext,
    state: &AppState,
    request: McpUploadDocumentsRequest,
) -> Result<Vec<McpMutationReceipt>, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let settings = &state.mcp_memory;
    let library = crate::interfaces::http::authorization::load_library_and_authorize(
        auth,
        state,
        request.library_id,
        POLICY_LIBRARY_WRITE,
    )
    .await?;
    if request.documents.is_empty() {
        return Err(ApiError::invalid_mcp_tool_call("documents must not be empty"));
    }

    let mut receipts = Vec::with_capacity(request.documents.len());
    let mut total_upload_bytes = 0_u64;
    for (index, document) in request.documents.into_iter().enumerate() {
        let file_name = document.file_name.trim();
        if file_name.is_empty() {
            return Err(ApiError::invalid_mcp_tool_call(format!(
                "documents[{index}].fileName must not be empty"
            )));
        }

        let file_bytes = BASE64_STANDARD.decode(document.content_base64.trim()).map_err(|_| {
            ApiError::invalid_mcp_tool_call(format!(
                "documents[{index}].contentBase64 must be valid base64"
            ))
        })?;
        if file_bytes.is_empty() {
            return Err(ApiError::invalid_mcp_tool_call(format!(
                "documents[{index}] upload body must not be empty"
            )));
        }
        validate_mcp_upload_file_size(
            settings,
            file_name,
            document.mime_type.as_deref(),
            &file_bytes,
        )?;
        total_upload_bytes =
            total_upload_bytes.saturating_add(u64::try_from(file_bytes.len()).unwrap_or(u64::MAX));
        validate_mcp_upload_batch_size(settings, total_upload_bytes)?;

        let payload_identity = hash_upload_payload(
            file_name,
            document.mime_type.as_deref(),
            document.title.as_deref(),
            &file_bytes,
        );
        let normalized_idempotency_key = normalize_upload_idempotency_key(
            request.idempotency_key.as_deref(),
            library.id,
            index,
            &payload_identity,
        );

        if let Some(existing) = find_existing_mutation_by_idempotency(
            auth,
            state,
            &normalized_idempotency_key,
            &payload_identity,
        )
        .await?
        {
            receipts.push(resolve_mutation_receipt(state, auth, existing).await?);
            continue;
        }

        let receipt = process_upload_mutation(
            auth,
            state,
            &library,
            normalized_idempotency_key,
            payload_identity,
            document,
            file_bytes,
        )
        .await?;
        receipts.push(receipt);
    }

    Ok(receipts)
}

pub async fn update_document(
    auth: &AuthContext,
    state: &AppState,
    request: McpUpdateDocumentRequest,
) -> Result<McpMutationReceipt, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let settings = &state.mcp_memory;
    let library = crate::interfaces::http::authorization::load_library_and_authorize(
        auth,
        state,
        request.library_id,
        POLICY_LIBRARY_WRITE,
    )
    .await?;
    let document = state
        .arango_document_store
        .get_document(request.document_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("knowledge_document", request.document_id))?;
    authorize_document_permission(
        auth,
        document.workspace_id,
        document.library_id,
        document.document_id,
        POLICY_DOCUMENTS_WRITE,
    )?;
    if document.library_id != library.id {
        return Err(ApiError::inaccessible_memory_scope(
            "document is not visible inside the requested library",
        ));
    }

    let current_state =
        mcp_access::resolve_document_state(auth, state, document.document_id).await?;
    state
        .canonical_services
        .content
        .ensure_document_accepts_new_mutation(state, document.document_id)
        .await?;

    let (operation_kind, payload_identity) = match request.operation_kind {
        McpDocumentMutationKind::Append => {
            let appended_text = request
                .appended_text
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ApiError::invalid_mcp_tool_call("append requires non-empty appendedText")
                })?;
            if current_state.readability_state != crate::mcp_types::McpReadabilityState::Readable {
                return Err(ApiError::unreadable_document(
                    current_state.status_reason.unwrap_or_else(|| {
                        "document is not readable enough for append".to_string()
                    }),
                ));
            }
            (McpMutationOperationKind::Append, hash_append_payload(appended_text))
        }
        McpDocumentMutationKind::Replace => {
            let file_name = request
                .replacement_file_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ApiError::invalid_mcp_tool_call("replace requires replacementFileName")
                })?;
            let file_bytes = BASE64_STANDARD
                .decode(request.replacement_content_base64.as_deref().map(str::trim).ok_or_else(
                    || ApiError::invalid_mcp_tool_call("replace requires replacementContentBase64"),
                )?)
                .map_err(|_| {
                    ApiError::invalid_mcp_tool_call("replacementContentBase64 must be valid base64")
                })?;
            if file_bytes.is_empty() {
                return Err(ApiError::invalid_mcp_tool_call(
                    "replacement upload body must not be empty",
                ));
            }
            validate_mcp_upload_file_size(
                settings,
                file_name,
                request.replacement_mime_type.as_deref(),
                &file_bytes,
            )?;
            (
                McpMutationOperationKind::Replace,
                hash_replace_payload(
                    file_name,
                    request.replacement_mime_type.as_deref(),
                    &file_bytes,
                ),
            )
        }
    };
    let normalized_idempotency_key = normalize_document_idempotency_key(
        request.idempotency_key.as_deref(),
        document.document_id,
        &operation_kind,
        &payload_identity,
    );

    if let Some(existing) = find_existing_mutation_by_idempotency(
        auth,
        state,
        &normalized_idempotency_key,
        &payload_identity,
    )
    .await?
    {
        return resolve_mutation_receipt(state, auth, existing).await;
    }

    match request.operation_kind {
        McpDocumentMutationKind::Append => {
            process_append_mutation(
                auth,
                state,
                &library,
                document.document_id,
                normalized_idempotency_key,
                payload_identity,
                request.appended_text.unwrap_or_default(),
            )
            .await
        }
        McpDocumentMutationKind::Replace => {
            let replacement_content_base64 = request.replacement_content_base64.unwrap_or_default();
            let file_name =
                request.replacement_file_name.unwrap_or_else(|| "replace.bin".to_string());
            let file_bytes =
                BASE64_STANDARD.decode(replacement_content_base64.trim()).map_err(|_| {
                    ApiError::invalid_mcp_tool_call("replacementContentBase64 must be valid base64")
                })?;
            validate_mcp_upload_file_size(
                settings,
                &file_name,
                request.replacement_mime_type.as_deref(),
                &file_bytes,
            )?;
            process_replace_mutation(
                auth,
                state,
                &library,
                document.document_id,
                normalized_idempotency_key,
                payload_identity,
                file_name,
                request.replacement_mime_type,
                file_bytes,
            )
            .await
        }
    }
}

pub async fn get_mutation_status(
    auth: &AuthContext,
    state: &AppState,
    request: McpGetMutationStatusRequest,
) -> Result<McpMutationReceipt, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let mutation =
        state.canonical_services.content.get_mutation(state, request.receipt_id).await.map_err(
            |error| match error {
                ApiError::NotFound(_) => {
                    ApiError::NotFound(format!("mutation receipt {} not found", request.receipt_id))
                }
                other => other,
            },
        )?;

    resolve_mutation_receipt(state, auth, mutation).await
}

pub(crate) async fn find_existing_mutation_by_idempotency(
    auth: &AuthContext,
    state: &AppState,
    idempotency_key: &str,
    payload_identity: &str,
) -> Result<Option<ContentMutation>, ApiError> {
    let existing = state
        .canonical_services
        .content
        .find_mutation_by_idempotency(state, auth.principal_id, "mcp", idempotency_key)
        .await?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    ensure_matching_mutation_payload_identity(state, existing.id, payload_identity).await?;
    Ok(Some(existing))
}

pub(crate) async fn ensure_matching_mutation_payload_identity(
    state: &AppState,
    mutation_id: Uuid,
    payload_identity: &str,
) -> Result<(), ApiError> {
    let items = state.canonical_services.content.list_mutation_items(state, mutation_id).await?;
    let existing_payload_identity = if let Some(revision_id) =
        items.iter().find_map(|item| item.result_revision_id)
    {
        state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .and_then(|revision| payload_identity_from_source_uri(revision.source_uri.as_deref()))
    } else {
        None
    };

    if let Some(existing_payload_identity) = existing_payload_identity
        && existing_payload_identity != payload_identity
    {
        return Err(ApiError::idempotency_conflict(
            "the same idempotency key was already used with a different payload",
        ));
    }

    Ok(())
}

pub(crate) async fn process_upload_mutation(
    auth: &AuthContext,
    state: &AppState,
    library: &CatalogLibraryRow,
    idempotency_key: String,
    payload_identity: String,
    document: McpUploadDocumentInput,
    file_bytes: Vec<u8>,
) -> Result<McpMutationReceipt, ApiError> {
    let admission = state
        .canonical_services
        .content
        .upload_inline_document(
            state,
            UploadInlineDocumentCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                external_key: None,
                idempotency_key: Some(idempotency_key),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "mcp".to_string(),
                source_identity: Some(payload_identity),
                file_name: document.file_name.trim().to_string(),
                title: document.title,
                mime_type: document.mime_type,
                file_bytes,
            },
        )
        .await?;
    resolve_mutation_receipt(state, auth, admission.mutation.mutation).await
}

pub(crate) async fn process_append_mutation(
    auth: &AuthContext,
    state: &AppState,
    library: &CatalogLibraryRow,
    document_id: Uuid,
    idempotency_key: String,
    payload_identity: String,
    appended_text: String,
) -> Result<McpMutationReceipt, ApiError> {
    let admission = state
        .canonical_services
        .content
        .append_inline_mutation(
            state,
            AppendInlineMutationCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                document_id,
                idempotency_key: Some(idempotency_key),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "mcp".to_string(),
                source_identity: Some(payload_identity),
                appended_text,
            },
        )
        .await?;
    resolve_mutation_receipt(state, auth, admission.mutation).await
}

pub(crate) async fn process_replace_mutation(
    auth: &AuthContext,
    state: &AppState,
    library: &CatalogLibraryRow,
    document_id: Uuid,
    idempotency_key: String,
    payload_identity: String,
    file_name: String,
    mime_type: Option<String>,
    file_bytes: Vec<u8>,
) -> Result<McpMutationReceipt, ApiError> {
    let admission = state
        .canonical_services
        .content
        .replace_inline_mutation(
            state,
            ReplaceInlineMutationCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                document_id,
                idempotency_key: Some(idempotency_key),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "mcp".to_string(),
                source_identity: Some(payload_identity),
                file_name,
                mime_type,
                file_bytes,
            },
        )
        .await?;
    resolve_mutation_receipt(state, auth, admission.mutation).await
}

pub(crate) async fn resolve_mutation_receipt(
    state: &AppState,
    auth: &AuthContext,
    row: ContentMutation,
) -> Result<McpMutationReceipt, ApiError> {
    let items = state.canonical_services.content.list_mutation_items(state, row.id).await?;
    let mut document_id = items.iter().find_map(|item| item.document_id);
    if document_id.is_none()
        && let Some(revision_id) = items.iter().find_map(|item| item.result_revision_id)
    {
        document_id = state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .map(|revision| revision.document_id);
    }

    if let Some(document_id) = document_id {
        let document = state
            .arango_document_store
            .get_document(document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_document", document_id))?;
        authorize_library_discovery(auth, document.workspace_id, document.library_id)?;
        authorize_document_permission(
            auth,
            document.workspace_id,
            document.library_id,
            document.document_id,
            POLICY_DOCUMENTS_WRITE,
        )?;
    } else {
        authorize_library_permission(auth, row.workspace_id, row.library_id, POLICY_LIBRARY_WRITE)?;
    }

    let mut status = map_content_mutation_status_to_receipt_status(&row.mutation_state);
    let mut failure_kind = row.failure_code.clone().or(row.conflict_code.clone());
    let last_status_at = row.completed_at.unwrap_or(row.requested_at);

    if matches!(status, McpMutationReceiptStatus::Ready)
        && let Some(document_id) = document_id
        && let Some(result_revision_id) = items.iter().find_map(|item| item.result_revision_id)
    {
        let current_revision_id = state
            .arango_document_store
            .get_document(document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .and_then(|row| row.active_revision_id.or(row.readable_revision_id));
        if current_revision_id != Some(result_revision_id) {
            status = McpMutationReceiptStatus::Superseded;
        }
    }

    if matches!(status, McpMutationReceiptStatus::Failed) && failure_kind.is_none() {
        let jobs = state
            .canonical_services
            .ingest
            .list_jobs(state, Some(row.workspace_id), Some(row.library_id))
            .await?;
        if let Some(job) = jobs.into_iter().find(|job| job.mutation_id == Some(row.id)) {
            let attempts = state.canonical_services.ingest.list_attempts(state, job.id).await?;
            if let Some(attempt) = attempts.into_iter().max_by_key(|attempt| attempt.attempt_number)
            {
                failure_kind = attempt.failure_code.or(attempt.failure_class);
            }
        }
    }

    Ok(McpMutationReceipt {
        receipt_id: row.id,
        token_id: auth.token_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        document_id,
        operation_kind: parse_mutation_operation_kind(&row.operation_kind)?,
        idempotency_key: row.idempotency_key.unwrap_or_default(),
        status,
        runtime_tracking_id: None,
        accepted_at: row.requested_at,
        last_status_at,
        failure_kind,
    })
}
