use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use uuid::Uuid;

use sha2::{Digest as _, Sha256};

use crate::{
    app::state::{AppState, McpMemorySettings},
    domains::{
        content::ContentMutation,
        ingest::{WebDiscoveredPage, WebIngestRun, WebIngestRunReceipt},
    },
    infra::repositories::catalog_repository::CatalogLibraryRow,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE, POLICY_USAGE_READ,
            authorize_document_permission, authorize_library_discovery,
            authorize_library_permission, load_async_operation_and_authorize,
        },
        router_support::ApiError,
    },
    mcp_types::{
        McpCancelWebIngestRunRequest, McpDocumentMutationKind, McpGetWebIngestRunRequest,
        McpListWebIngestRunPagesRequest, McpMutationOperationKind, McpMutationReceipt,
        McpMutationReceiptStatus, McpOperationStatus, McpSubmitWebIngestRunRequest,
        McpUpdateDocumentRequest, McpUploadDocumentInput, McpUploadDocumentsRequest,
    },
    services::content::service::{
        AppendInlineMutationCommand, ContentMutationAdmission, ReplaceInlineMutationCommand,
        UploadInlineDocumentCommand,
    },
    services::ingest::web::CreateWebIngestRunCommand,
    shared::extraction::file_extract::UploadAdmissionError,
    shared::outbound_http::{get_public_http_following_redirects, read_response_bytes_with_limit},
};

pub(crate) async fn upload_documents(
    auth: &AuthContext,
    state: &AppState,
    request: McpUploadDocumentsRequest,
) -> Result<Vec<McpMutationReceipt>, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let settings = &state.mcp_memory;
    let library = crate::services::mcp::access::load_library_by_catalog_ref(
        auth,
        state,
        &request.library,
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
        let file_bytes =
            resolve_upload_file_bytes(&document, index, settings.max_upload_file_bytes()).await?;
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
            let admission =
                state.canonical_services.content.get_mutation_admission(state, existing.id).await?;
            receipts.push(resolve_mutation_receipt(state, auth, &admission).await?);
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

pub(crate) async fn update_document(
    auth: &AuthContext,
    state: &AppState,
    request: McpUpdateDocumentRequest,
) -> Result<McpMutationReceipt, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let settings = &state.mcp_memory;
    let library = crate::services::mcp::access::load_library_by_catalog_ref(
        auth,
        state,
        &request.library,
        POLICY_LIBRARY_WRITE,
    )
    .await?;
    let document = state
        .document_store
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
        let admission =
            state.canonical_services.content.get_mutation_admission(state, existing.id).await?;
        return resolve_mutation_receipt(state, auth, &admission).await;
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

/// Backs the `get_operation` MCP tool. Reads the same canonical
/// `ops_async_operation` store as `GET /v1/ops/operations/{operationId}`
/// (authorized the same way, via `POLICY_USAGE_READ`) rather than the
/// content-mutation-specific receipt store the old `get_mutation_status`
/// tool used — see plan §6.3/§6.4 convergence.
pub(crate) async fn get_operation(
    auth: &AuthContext,
    state: &AppState,
    operation_id: Uuid,
) -> Result<McpOperationStatus, ApiError> {
    let _ =
        load_async_operation_and_authorize(auth, state, operation_id, POLICY_USAGE_READ).await?;
    let operation = state.canonical_services.ops.get_async_operation(state, operation_id).await?;
    let progress =
        state.canonical_services.ops.get_async_operation_progress(state, operation_id).await?;
    Ok(McpOperationStatus { operation, progress })
}

pub(crate) async fn submit_web_ingest_run(
    auth: &AuthContext,
    state: &AppState,
    request: McpSubmitWebIngestRunRequest,
) -> Result<WebIngestRunReceipt, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_WRITE)?;
    let library = crate::services::mcp::access::load_library_by_catalog_ref(
        auth,
        state,
        &request.library,
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
                crawl_filter: request.crawl_filter,
                materialization_filter: request.materialization_filter,
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

pub(crate) async fn get_web_ingest_run(
    auth: &AuthContext,
    state: &AppState,
    request: McpGetWebIngestRunRequest,
) -> Result<WebIngestRun, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_READ)?;
    let run = state.canonical_services.web_ingest.get_run(state, request.run_id).await?;
    authorize_library_permission(auth, run.workspace_id, run.library_id, POLICY_LIBRARY_READ)?;
    Ok(run)
}

pub(crate) async fn list_web_ingest_run_pages(
    auth: &AuthContext,
    state: &AppState,
    request: McpListWebIngestRunPagesRequest,
) -> Result<Vec<WebDiscoveredPage>, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_READ)?;
    let run = state.canonical_services.web_ingest.get_run(state, request.run_id).await?;
    authorize_library_permission(auth, run.workspace_id, run.library_id, POLICY_LIBRARY_READ)?;
    state.canonical_services.web_ingest.list_pages(state, request.run_id).await
}

pub(crate) async fn cancel_web_ingest_run(
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
                .document_store
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
                document_hint: None,
                mime_type,
                file_bytes,
                parent_external_key: None,
            },
        )
        .await?;
    resolve_mutation_receipt(state, auth, &admission.mutation).await
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

    // When the caller provided `fetchUrl` and no explicit `fileName`,
    // derive one from the URL path. This is the normal path for the
    // "download this link and ingest it" flow — expecting the LLM to
    // also generate a filename would just add an extra failure mode
    // when the URL and desired name are already implied by each
    // other. `reqwest::Url::parse` does the path split; the last
    // non-empty path segment becomes the filename.
    if let Some(fetch_url) =
        document.fetch_url.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        if let Ok(parsed) = reqwest::Url::parse(fetch_url)
            && let Some(candidate) = parsed
                .path_segments()
                .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        {
            return Ok(candidate.to_string());
        }
        return Ok(default_inline_file_name(document.title.as_deref(), Some(fetch_url), index));
    }

    if document.body.as_deref().map(str::trim).as_ref().is_some_and(|value| !value.is_empty()) {
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

async fn resolve_upload_file_bytes(
    document: &McpUploadDocumentInput,
    index: usize,
    max_file_bytes: u64,
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
        .as_ref()
        .is_some_and(|value| !value.is_empty());
    let has_body =
        document.body.as_deref().map(str::trim).as_ref().is_some_and(|value| !value.is_empty());
    let fetch_url = document.fetch_url.as_deref().map(str::trim).filter(|value| !value.is_empty());

    let source_count = [has_base64, has_body, fetch_url.is_some()].iter().filter(|v| **v).count();
    if source_count > 1 {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "documents[{index}] must provide exactly one of contentBase64, body, or fetchUrl"
        )));
    }
    if matches!(source_type.as_deref(), Some("inline")) && !has_body {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "documents[{index}].sourceType=inline requires body"
        )));
    }
    if matches!(source_type.as_deref(), Some("file" | "binary"))
        && !has_base64
        && fetch_url.is_none()
    {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "documents[{index}].sourceType={} requires contentBase64 or fetchUrl",
            source_type.as_deref().unwrap_or("file")
        )));
    }

    if let Some(url) = fetch_url {
        return fetch_upload_bytes_from_url(url, index, max_file_bytes).await;
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
        "documents[{index}] requires one of contentBase64, body, or fetchUrl"
    )))
}

/// Fetch an MCP-supplied upload URL into memory, with SSRF guards and
/// a hard size cap. The MCP tool is exposed to LLM-generated inputs,
/// so we have to defend against:
///   * Internal hosts (localhost, link-local, RFC 1918, metadata
///     endpoints) — blocked by resolving the URL host and rejecting
///     any address that is loopback, private, or link-local.
///   * Unbounded responses — `Content-Length` checked against
///     `max_file_bytes` before any body read, and the body stream
///     itself is capped to the same value so chunked responses can't
///     silently blow past the limit.
///   * Non-HTTP schemes — only `http` and `https` are permitted.
async fn fetch_upload_bytes_from_url(
    raw_url: &str,
    index: usize,
    max_file_bytes: u64,
) -> Result<Vec<u8>, ApiError> {
    let response = get_public_http_following_redirects(
        raw_url,
        true,
        5,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(10),
        None,
    )
    .await
    .map_err(|error| {
        ApiError::invalid_mcp_tool_call(format!("documents[{index}].fetchUrl {error}"))
    })?;
    if !response.status().is_success() {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "documents[{index}].fetchUrl returned HTTP {}",
            response.status().as_u16()
        )));
    }
    read_response_bytes_with_limit(response, max_file_bytes).await.map_err(|error| {
        ApiError::invalid_mcp_tool_call(format!("documents[{index}].fetchUrl {error}"))
    })
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
    resolve_mutation_receipt(state, auth, &admission).await
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
                document_hint: None,
                mime_type,
                file_bytes,
            },
        )
        .await?;
    resolve_mutation_receipt(state, auth, &admission).await
}

/// Builds the MCP-facing receipt from a [`ContentMutationAdmission`].
/// `operation_id` on the resulting [`McpMutationReceipt`] is the
/// canonical async-operation id (pollable via the `get_operation` tool
/// and `GET /v1/ops/operations/{operationId}`), falling back to the
/// content-mutation id only for the rare case a mutation settled
/// synchronously without ever getting an async operation row.
pub(crate) async fn resolve_mutation_receipt(
    state: &AppState,
    auth: &AuthContext,
    admission: &ContentMutationAdmission,
) -> Result<McpMutationReceipt, ApiError> {
    let row = &admission.mutation;
    let items = &admission.items;
    let mut document_id = items.iter().find_map(|item| item.document_id);
    if document_id.is_none()
        && let Some(revision_id) = items.iter().find_map(|item| item.result_revision_id)
    {
        document_id = state
            .document_store
            .get_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .map(|revision| revision.document_id);
    }

    if let Some(document_id) = document_id {
        let document = state
            .document_store
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
    let mut failure_kind = row.failure_code.clone().or_else(|| row.conflict_code.clone());
    let last_status_at = row.completed_at.unwrap_or(row.requested_at);

    if matches!(status, McpMutationReceiptStatus::Ready)
        && let Some(document_id) = document_id
        && let Some(result_revision_id) = items.iter().find_map(|item| item.result_revision_id)
    {
        let current_revision_id = state
            .document_store
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
        operation_id: admission.async_operation_id.unwrap_or(row.id),
        token_id: auth.token_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        document_id,
        operation_kind: parse_mutation_operation_kind(&row.operation_kind)?,
        idempotency_key: row.idempotency_key.clone().unwrap_or_default(),
        status,
        accepted_at: row.requested_at,
        last_status_at,
        failure_kind,
    })
}

// --- Idempotency-key / payload-hash / admission helpers ---------------
//
// Split out of the former `services/mcp/support.rs` god-file (plan
// §6.4): every one of these was already only ever called from this
// module, so the move also tightens visibility from `pub(crate)` to
// module-private.

fn normalize_upload_idempotency_key(
    idempotency_key: Option<&str>,
    library_id: Uuid,
    document_index: usize,
    payload_identity: &str,
) -> String {
    match idempotency_key.map(str::trim).filter(|value| !value.is_empty()) {
        Some(base) => format!("mcp:upload:{library_id}:{document_index}:{base}"),
        None => format!("mcp:upload:{library_id}:{payload_identity}"),
    }
}

fn normalize_document_idempotency_key(
    idempotency_key: Option<&str>,
    document_id: Uuid,
    operation_kind: &McpMutationOperationKind,
    payload_identity: &str,
) -> String {
    let operation = operation_kind_key(operation_kind);
    match idempotency_key.map(str::trim).filter(|value| !value.is_empty()) {
        Some(base) => format!("mcp:{operation}:{document_id}:{base}"),
        None => format!("mcp:{operation}:{document_id}:{payload_identity}"),
    }
}

fn hash_upload_payload(
    file_name: &str,
    mime_type: Option<&str>,
    title: Option<&str>,
    file_bytes: &[u8],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(file_name.as_bytes());
    hasher.update(mime_type.unwrap_or_default().as_bytes());
    hasher.update(title.unwrap_or_default().as_bytes());
    hasher.update(file_bytes);
    hex::encode(hasher.finalize())
}

fn hash_append_payload(appended_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(appended_text.as_bytes());
    hex::encode(hasher.finalize())
}

fn hash_replace_payload(file_name: &str, mime_type: Option<&str>, file_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(file_name.as_bytes());
    hasher.update(mime_type.unwrap_or_default().as_bytes());
    hasher.update(file_bytes);
    hex::encode(hasher.finalize())
}

fn validate_mcp_upload_file_size(
    settings: &McpMemorySettings,
    file_name: &str,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> Result<(), ApiError> {
    let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
    if file_size_bytes > settings.max_upload_file_bytes() {
        return Err(ApiError::from_upload_admission(UploadAdmissionError::file_too_large(
            file_name,
            mime_type,
            file_size_bytes,
            settings.upload_max_size_mb,
        )));
    }
    Ok(())
}

fn validate_mcp_upload_batch_size(
    settings: &McpMemorySettings,
    total_upload_bytes: u64,
) -> Result<(), ApiError> {
    if total_upload_bytes > settings.max_upload_batch_bytes() {
        return Err(ApiError::from_upload_admission(UploadAdmissionError::upload_batch_too_large(
            total_upload_bytes,
            settings.upload_max_size_mb,
        )));
    }
    Ok(())
}

const fn operation_kind_key(operation_kind: &McpMutationOperationKind) -> &'static str {
    match operation_kind {
        McpMutationOperationKind::Upload => "upload",
        McpMutationOperationKind::Append => "append",
        McpMutationOperationKind::Replace => "replace",
    }
}

fn parse_mutation_operation_kind(value: &str) -> Result<McpMutationOperationKind, ApiError> {
    match value {
        "upload" => Ok(McpMutationOperationKind::Upload),
        "append" => Ok(McpMutationOperationKind::Append),
        "replace" => Ok(McpMutationOperationKind::Replace),
        _ => Err(ApiError::Internal),
    }
}

fn map_content_mutation_status_to_receipt_status(mutation_state: &str) -> McpMutationReceiptStatus {
    match mutation_state {
        "accepted" => McpMutationReceiptStatus::Accepted,
        "running" => McpMutationReceiptStatus::Processing,
        "applied" => McpMutationReceiptStatus::Ready,
        "failed" | "conflicted" | "canceled" => McpMutationReceiptStatus::Failed,
        _ => McpMutationReceiptStatus::Accepted,
    }
}

fn payload_identity_from_source_uri(source_uri: Option<&str>) -> Option<String> {
    source_uri
        .and_then(|value| {
            value.strip_prefix("mcp://payload/").or_else(|| value.strip_prefix("inline://payload/"))
        })
        .map(ToString::to_string)
}
