use super::*;

/// Caller-supplied idempotency key and payload identity for one inline
/// mutation. The two are normalized together before the content plane
/// records its admission, so they travel as one value.
#[derive(Debug, Clone)]
pub(crate) struct DocumentMutationIdentity {
    pub(crate) idempotency_key: String,
    pub(crate) payload_identity: String,
}

/// Explicit source and incremental text for one append request.
pub(crate) struct AppendMutationPayload {
    pub(crate) base_revision_id: Uuid,
    pub(crate) appended_text: String,
}

pub(crate) async fn process_upload_mutation(
    auth: &AuthContext,
    state: &AppState,
    command: UploadInlineDocumentCommand,
) -> Result<McpMutationReceipt, ApiError> {
    let admission =
        state.application_services.content.upload_inline_document(state, command).await?;
    resolve_mutation_receipt(state, auth, &admission.mutation).await
}

pub(crate) async fn process_append_mutation(
    auth: &AuthContext,
    state: &AppState,
    library: &CatalogLibraryRow,
    document_id: Uuid,
    identity: DocumentMutationIdentity,
    payload: AppendMutationPayload,
) -> Result<McpMutationReceipt, ApiError> {
    let admission = state
        .application_services
        .content
        .append_inline_mutation(
            state,
            AppendInlineMutationCommand {
                base_revision_id: payload.base_revision_id,
                workspace_id: library.workspace_id,
                library_id: library.id,
                document_id,
                idempotency_key: Some(identity.idempotency_key),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "mcp".to_string(),
                source_identity: Some(identity.payload_identity),
                appended_text: payload.appended_text,
            },
        )
        .await?;
    resolve_mutation_receipt(state, auth, &admission).await
}

pub(crate) async fn process_replace_mutation(
    auth: &AuthContext,
    state: &AppState,
    command: ReplaceInlineMutationCommand,
) -> Result<McpMutationReceipt, ApiError> {
    let admission =
        state.application_services.content.replace_inline_mutation(state, command).await?;
    resolve_mutation_receipt(state, auth, &admission).await
}

/// Builds the MCP-facing receipt from a [`ContentMutationAdmission`].
/// `operation_id` on the resulting [`McpMutationReceipt`] is the
/// shared async-operation id (pollable via the `get_operation` tool
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
            .application_services
            .ingest
            .list_jobs(state, Some(row.workspace_id), Some(row.library_id))
            .await?;
        if let Some(job) = jobs.into_iter().find(|job| job.mutation_id == Some(row.id)) {
            let attempts = state.application_services.ingest.list_attempts(state, job.id).await?;
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
