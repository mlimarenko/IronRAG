use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        content::ContentMutation,
        ingest::{WebDiscoveredPage, WebIngestRun, WebIngestRunReceipt},
    },
    infra::repositories::catalog_repository::CatalogLibraryRow,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE,
            authorize_document_permission, authorize_library_discovery,
            authorize_library_permission,
        },
        router_support::ApiError,
    },
    mcp_types::{
        McpCancelWebIngestRunRequest, McpDocumentMutationKind, McpGetMutationStatusRequest,
        McpGetWebIngestRunRequest, McpListWebIngestRunPagesRequest, McpMutationOperationKind,
        McpMutationReceipt, McpMutationReceiptStatus, McpSubmitWebIngestRunRequest,
        McpUpdateDocumentRequest, McpUploadDocumentInput, McpUploadDocumentsRequest,
    },
    services::content::service::{
        AppendInlineMutationCommand, ReplaceInlineMutationCommand, UploadInlineDocumentCommand,
    },
    services::ingest::web::CreateWebIngestRunCommand,
    services::mcp::support::{
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
        let file_name = resolve_upload_file_name(&document, index)?;
        let mime_type = resolve_upload_mime_type(&document);
        let file_bytes = resolve_upload_file_bytes(&document, index)?;
        if file_bytes.is_empty() {
            return Err(ApiError::invalid_mcp_tool_call(format!(
                "documents[{index}] upload body must not be empty"
            )));
        }
        validate_mcp_upload_file_size(settings, &file_name, mime_type.as_deref(), &file_bytes)?;
        total_upload_bytes =
            total_upload_bytes.saturating_add(u64::try_from(file_bytes.len()).unwrap_or(u64::MAX));
        validate_mcp_upload_batch_size(settings, total_upload_bytes)?;

        let payload_identity = hash_upload_payload(
            &file_name,
            mime_type.as_deref(),
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
            document.title,
            file_name,
            mime_type,
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", request.document_id))?;
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
        crate::services::mcp::access::resolve_document_state(auth, state, document.document_id)
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
    let mutation_kind = match request.operation_kind {
        McpDocumentMutationKind::Append => "append",
        McpDocumentMutationKind::Replace => "replace",
    };
    state
        .canonical_services
        .content
        .ensure_document_accepts_new_mutation(state, document.document_id, mutation_kind)
        .await?;
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

pub async fn submit_web_ingest_run(
    auth: &AuthContext,
    state: &AppState,
    request: McpSubmitWebIngestRunRequest,
) -> Result<WebIngestRunReceipt, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_WRITE)?;
    let library = crate::interfaces::http::authorization::load_library_and_authorize(
        auth,
        state,
        request.library_id,
        POLICY_LIBRARY_WRITE,
    )
    .await?;
    let run = state
        .canonical_services
        .web_ingest
        .create_run(
            state,
            CreateWebIngestRunCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                seed_url: request.seed_url,
                mode: request.mode,
                boundary_policy: request.boundary_policy,
                max_depth: request.max_depth,
                max_pages: request.max_pages,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "mcp".to_string(),
                idempotency_key: request.idempotency_key,
            },
        )
        .await?;
    Ok(WebIngestRunReceipt {
        run_id: run.run_id,
        library_id: run.library_id,
        mode: run.mode,
        run_state: run.run_state,
        async_operation_id: run.async_operation_id,
        counts: run.counts,
        failure_code: run.failure_code,
        cancel_requested_at: run.cancel_requested_at,
    })
}

pub async fn get_web_ingest_run(
    auth: &AuthContext,
    state: &AppState,
    request: McpGetWebIngestRunRequest,
) -> Result<WebIngestRun, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_READ)?;
    let run = state.canonical_services.web_ingest.get_run(state, request.run_id).await?;
    authorize_library_permission(auth, run.workspace_id, run.library_id, POLICY_LIBRARY_READ)?;
    Ok(run)
}

pub async fn list_web_ingest_run_pages(
    auth: &AuthContext,
    state: &AppState,
    request: McpListWebIngestRunPagesRequest,
) -> Result<Vec<WebDiscoveredPage>, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_READ)?;
    let run = state.canonical_services.web_ingest.get_run(state, request.run_id).await?;
    authorize_library_permission(auth, run.workspace_id, run.library_id, POLICY_LIBRARY_READ)?;
    state.canonical_services.web_ingest.list_pages(state, request.run_id).await
}

pub async fn cancel_web_ingest_run(
    auth: &AuthContext,
    state: &AppState,
    request: McpCancelWebIngestRunRequest,
) -> Result<WebIngestRunReceipt, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_WRITE)?;
    let run = state.canonical_services.web_ingest.get_run(state, request.run_id).await?;
    authorize_library_permission(auth, run.workspace_id, run.library_id, POLICY_LIBRARY_WRITE)?;
    state.canonical_services.web_ingest.cancel_run(state, request.run_id).await
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
    ensure_matching_mutation_payload_identity(
        state,
        existing.id,
        existing.source_identity.as_deref(),
        payload_identity,
    )
    .await?;
    Ok(Some(existing))
}

pub(crate) async fn ensure_matching_mutation_payload_identity(
    state: &AppState,
    mutation_id: Uuid,
    existing_source_identity: Option<&str>,
    payload_identity: &str,
) -> Result<(), ApiError> {
    let existing_payload_identity = if let Some(existing_source_identity) = existing_source_identity
    {
        Some(existing_source_identity.to_string())
    } else {
        let items =
            state.canonical_services.content.list_mutation_items(state, mutation_id).await?;
        if let Some(revision_id) = items.iter().find_map(|item| item.result_revision_id) {
            state
                .arango_document_store
                .get_revision(revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .and_then(|revision| {
                    payload_identity_from_source_uri(revision.source_uri.as_deref())
                })
        } else {
            return Err(ApiError::idempotency_conflict(
                "the same idempotency key was already used before payload identity tracking was available; retry with a new idempotency key",
            ));
        }
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
    title: Option<String>,
    file_name: String,
    mime_type: Option<String>,
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
                file_name,
                title,
                mime_type,
                file_bytes,
            },
        )
        .await?;
    resolve_mutation_receipt(state, auth, admission.mutation.mutation).await
}

fn resolve_upload_file_name(
    document: &McpUploadDocumentInput,
    index: usize,
) -> Result<String, ApiError> {
    if let Some(file_name) =
        document.file_name.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        return Ok(file_name.to_string());
    }

    if document.body.as_deref().map(str::trim).filter(|value| !value.is_empty()).is_some() {
        return Ok(default_inline_file_name(
            document.title.as_deref(),
            document.source_uri.as_deref(),
            index,
        ));
    }

    Err(ApiError::invalid_mcp_tool_call(format!("documents[{index}].fileName must not be empty")))
}

fn resolve_upload_mime_type(document: &McpUploadDocumentInput) -> Option<String> {
    document
        .mime_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            document
                .body
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|_| "text/plain".to_string())
        })
}

fn resolve_upload_file_bytes(
    document: &McpUploadDocumentInput,
    index: usize,
) -> Result<Vec<u8>, ApiError> {
    let source_type = document
        .source_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let has_base64 = document
        .content_base64
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let has_body =
        document.body.as_deref().map(str::trim).filter(|value| !value.is_empty()).is_some();

    if has_base64 && has_body {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "documents[{index}] must provide either contentBase64 or body, not both"
        )));
    }
    if matches!(source_type.as_deref(), Some("inline")) && !has_body {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "documents[{index}].sourceType=inline requires body"
        )));
    }
    if matches!(source_type.as_deref(), Some("file" | "binary")) && !has_base64 {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "documents[{index}].sourceType={} requires contentBase64",
            source_type.as_deref().unwrap_or("file")
        )));
    }

    if let Some(content_base64) =
        document.content_base64.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        return BASE64_STANDARD.decode(content_base64).map_err(|_| {
            ApiError::invalid_mcp_tool_call(format!(
                "documents[{index}].contentBase64 must be valid base64"
            ))
        });
    }

    if let Some(body) = document.body.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(body.as_bytes().to_vec());
    }

    Err(ApiError::invalid_mcp_tool_call(format!(
        "documents[{index}] requires either contentBase64 or body"
    )))
}

fn default_inline_file_name(title: Option<&str>, source_uri: Option<&str>, index: usize) -> String {
    if let Some(candidate) = source_uri
        .and_then(|value| value.rsplit('/').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return candidate.to_string();
    }

    if let Some(candidate) = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_inline_file_stem)
        .filter(|value| !value.is_empty())
    {
        return format!("{candidate}.txt");
    }

    format!("inline-{}.txt", index + 1)
}

fn normalize_inline_file_stem(title: &str) -> String {
    let mut normalized = String::with_capacity(title.len());
    let mut last_was_separator = false;
    for ch in title.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            last_was_separator = false;
            Some(ch.to_ascii_lowercase())
        } else if matches!(ch, ' ' | '-' | '_' | '.') {
            if last_was_separator {
                None
            } else {
                last_was_separator = true;
                Some('-')
            }
        } else {
            None
        };
        if let Some(ch) = mapped {
            normalized.push(ch);
        }
    }
    normalized.trim_matches('-').to_string()
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .map(|revision| revision.document_id);
    }

    if let Some(document_id) = document_id {
        let document = state
            .arango_document_store
            .get_document(document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .and_then(|row| row.readable_revision_id);
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
        accepted_at: row.requested_at,
        last_status_at,
        failure_kind,
    })
}
