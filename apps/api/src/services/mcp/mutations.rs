use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use uuid::Uuid;

use sha2::{Digest as _, Sha256};

use crate::{
    app::state::{AppState, McpMemorySettings},
    domains::ingest::{WebDiscoveredPage, WebIngestRun, WebIngestRunReceipt},
    infra::repositories::catalog_repository::CatalogLibraryRow,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE,
            POLICY_OPERATION_READ, authorize_document_permission, authorize_library_discovery,
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

mod execution;

pub(crate) use execution::{
    DocumentMutationIdentity, process_append_mutation, process_replace_mutation,
    process_upload_mutation,
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

        let mut command = UploadInlineDocumentCommand {
            workspace_id: library.workspace_id,
            library_id: library.id,
            external_key: None,
            idempotency_key: None,
            requested_by_principal_id: Some(auth.principal_id),
            request_surface: "mcp".to_owned(),
            source_identity: None,
            file_name,
            title: document.title,
            document_hint: None,
            mime_type,
            file_bytes,
            parent_external_key: None,
        };
        let payload_identity = hash_upload_payload(&command)?;
        let normalized_idempotency_key = normalize_upload_idempotency_key(
            request.idempotency_key.as_deref(),
            library.id,
            index,
            &payload_identity,
        );

        command.idempotency_key = Some(normalized_idempotency_key);
        let receipt = process_upload_mutation(auth, state, command).await?;
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

    match request.operation_kind {
        McpDocumentMutationKind::Append => {
            let base_revision_id = request
                .base_revision_id
                .ok_or_else(|| ApiError::invalid_mcp_tool_call("append requires baseRevisionId"))?;
            if request.replacement_file_name.is_some()
                || request.replacement_content_base64.is_some()
                || request.replacement_mime_type.is_some()
                || request.title.is_some()
                || request.language_code.is_some()
                || request.source_uri.is_some()
                || request.document_hint.is_some()
                || request.visibility_intent_id.is_some()
            {
                return Err(ApiError::invalid_mcp_tool_call(
                    "append does not accept replacement metadata",
                ));
            }
            let appended_text = request
                .appended_text
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ApiError::invalid_mcp_tool_call("append requires non-empty appendedText")
                })?;
            let payload_identity = hash_append_payload(base_revision_id, appended_text);
            let idempotency_key = normalize_document_idempotency_key(
                request.idempotency_key.as_deref(),
                document.document_id,
                &McpMutationOperationKind::Append,
                &payload_identity,
            );
            process_append_mutation(
                auth,
                state,
                &library,
                document.document_id,
                DocumentMutationIdentity { idempotency_key, payload_identity },
                execution::AppendMutationPayload {
                    base_revision_id,
                    appended_text: request.appended_text.unwrap_or_default(),
                },
            )
            .await
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
            if request.base_revision_id.is_some() || request.appended_text.is_some() {
                return Err(ApiError::invalid_mcp_tool_call(
                    "replace does not accept append input",
                ));
            }
            let mut command = ReplaceInlineMutationCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                document_id: document.document_id,
                idempotency_key: None,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "mcp".to_owned(),
                source_identity: None,
                file_name: file_name.to_owned(),
                title: request.title,
                language_code: request.language_code,
                source_uri: request.source_uri,
                document_hint: request.document_hint,
                mime_type: request.replacement_mime_type,
                file_bytes,
                visibility_intent_id: request.visibility_intent_id,
            };
            let payload_identity = hash_replace_payload(&command)?;
            command.idempotency_key = Some(normalize_document_idempotency_key(
                request.idempotency_key.as_deref(),
                document.document_id,
                &McpMutationOperationKind::Replace,
                &payload_identity,
            ));
            process_replace_mutation(auth, state, command).await
        }
    }
}

/// Backs the `get_operation` MCP tool. Reads the same shared
/// `ops_async_operation` store as `GET /v1/ops/operations/{operationId}`
/// (authorized the same way, via `POLICY_OPERATION_READ`) rather than the
/// content-mutation-specific receipt store the old `get_mutation_status`
/// tool used — see plan §6.3/§6.4 convergence.
pub(crate) async fn get_operation(
    auth: &AuthContext,
    state: &AppState,
    operation_id: Uuid,
) -> Result<McpOperationStatus, ApiError> {
    let _ = load_async_operation_and_authorize(auth, state, operation_id, POLICY_OPERATION_READ)
        .await?;
    let operation = state.application_services.ops.get_async_operation(state, operation_id).await?;
    let progress =
        state.application_services.ops.get_async_operation_progress(state, operation_id).await?;
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
        .application_services
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
    let run = state.application_services.web_ingest.get_run(state, request.run_id).await?;
    authorize_library_permission(auth, run.workspace_id, run.library_id, POLICY_LIBRARY_READ)?;
    Ok(run)
}

pub(crate) async fn list_web_ingest_run_pages(
    auth: &AuthContext,
    state: &AppState,
    request: McpListWebIngestRunPagesRequest,
) -> Result<Vec<WebDiscoveredPage>, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_READ)?;
    let run = state.application_services.web_ingest.get_run(state, request.run_id).await?;
    authorize_library_permission(auth, run.workspace_id, run.library_id, POLICY_LIBRARY_READ)?;
    state.application_services.web_ingest.list_pages(state, request.run_id).await
}

pub(crate) async fn cancel_web_ingest_run(
    auth: &AuthContext,
    state: &AppState,
    request: McpCancelWebIngestRunRequest,
) -> Result<WebIngestRunReceipt, ApiError> {
    auth.require_any_scope(POLICY_LIBRARY_WRITE)?;
    let run = state.application_services.web_ingest.get_run(state, request.run_id).await?;
    authorize_library_permission(auth, run.workspace_id, run.library_id, POLICY_LIBRARY_WRITE)?;
    state.application_services.web_ingest.cancel_run(state, request.run_id).await
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
        Some(base) => format!("mcp:{operation}:{base}"),
        None => format!("mcp:{operation}:{document_id}:{payload_identity}"),
    }
}

fn hash_upload_payload(command: &UploadInlineDocumentCommand) -> Result<String, ApiError> {
    hash_semantic_identity(&serde_json::json!({
        "workspaceId": command.workspace_id,
        "libraryId": command.library_id,
        "replacement": command.replacement_identity()?,
        "externalKey": command.external_key,
        "parentExternalKey": command.parent_external_key,
    }))
}

fn hash_append_payload(base_revision_id: Uuid, appended_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(base_revision_id.as_bytes());
    hasher.update(appended_text.as_bytes());
    hex::encode(hasher.finalize())
}

fn hash_replace_payload(command: &ReplaceInlineMutationCommand) -> Result<String, ApiError> {
    hash_semantic_identity(&serde_json::json!({
        "workspaceId": command.workspace_id,
        "libraryId": command.library_id,
        "documentId": command.document_id,
        "replacement": command.replacement_identity()?,
        "visibilityIntentId": command.visibility_intent_id,
    }))
}

fn hash_semantic_identity(value: &serde_json::Value) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| ApiError::internal_with_log(error, "mutation identity"))?;
    Ok(hex::encode(Sha256::digest(bytes)))
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
        "failed" | "canceled" => McpMutationReceiptStatus::Failed,
        _ => McpMutationReceiptStatus::Accepted,
    }
}

#[cfg(test)]
mod tests;
