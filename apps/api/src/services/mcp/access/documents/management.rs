use super::*;

pub(crate) async fn authorize_library_for_mcp(
    auth: &AuthContext,
    state: &AppState,
    library_ref: &str,
) -> Result<crate::infra::repositories::catalog_repository::CatalogLibraryRow, ApiError> {
    load_library_by_catalog_ref(auth, state, library_ref, POLICY_MCP_MEMORY_READ).await
}

pub(crate) async fn list_documents(
    auth: &AuthContext,
    state: &AppState,
    library_id: Uuid,
    limit: usize,
    status_filter: Option<&str>,
) -> Result<serde_json::Value, ApiError> {
    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let _library =
        load_library_and_authorize(auth, state, library_id, POLICY_MCP_MEMORY_READ).await?;

    let summaries = state.application_services.content.list_documents(state, library_id).await?;

    let filtered: Vec<_> = summaries
        .into_iter()
        .filter(|summary| summary.document.document_state != "deleted")
        .filter(|summary| {
            list_documents_matches_status_filter(summary.readiness_summary.as_ref(), status_filter)
        })
        .take(limit)
        .collect();

    let documents: Vec<serde_json::Value> = filtered
        .iter()
        .map(|summary| {
            let readiness_kind = summary
                .readiness_summary
                .as_ref()
                .map_or("unknown", |row| row.readiness_kind.as_str());
            let source_uri =
                summary.active_revision.as_ref().and_then(|row| row.source_uri.as_deref());
            let byte_size = summary.active_revision.as_ref().map(|row| row.byte_size);
            let title = summary
                .active_revision
                .as_ref()
                .and_then(|row| row.title.as_deref())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(&summary.document.external_key);
            json!({
                "documentId": summary.document.id,
                "title": title,
                "readinessKind": readiness_kind,
                "sourceUri": source_uri,
                "byteSize": byte_size,
                "createdAt": summary.document.created_at,
            })
        })
        .collect();

    let current_after =
        crate::infra::repositories::content_fingerprint_repository::list_current_revision_ids(
            &state.persistence.postgres,
            &filtered.iter().map(|summary| summary.document.id).collect::<Vec<_>>(),
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "document visibility lookup failed"))?
        .into_iter()
        .collect::<HashMap<_, _>>();
    if filtered.iter().any(|summary| {
        summary
            .active_revision
            .as_ref()
            .is_none_or(|revision| current_after.get(&summary.document.id) != Some(&revision.id))
    }) {
        return Err(ApiError::knowledge_not_ready("document sources changed; retry the list"));
    }

    Ok(json!({
        "libraryId": library_id,
        "documents": documents,
        "count": documents.len(),
        "limit": limit,
    }))
}

pub(super) fn list_documents_matches_status_filter(
    readiness_summary: Option<&crate::domains::content::DocumentReadinessSummary>,
    status_filter: Option<&str>,
) -> bool {
    let Some(filter) = status_filter.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let Some(readiness_summary) = readiness_summary else {
        return false;
    };

    match filter {
        "readable" => {
            readability_state_from_kind(readiness_summary.readiness_kind)
                == McpReadabilityState::Readable
        }
        "processing" => {
            readability_state_from_kind(readiness_summary.readiness_kind)
                == McpReadabilityState::Processing
        }
        "failed" => {
            readability_state_from_kind(readiness_summary.readiness_kind)
                == McpReadabilityState::Failed
        }
        _ => false,
    }
}

pub(crate) async fn delete_document(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
) -> Result<serde_json::Value, ApiError> {
    let document =
        content_document_repository::get_document_by_id(&state.persistence.postgres, document_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;

    authorize_document_permission(
        auth,
        document.workspace_id,
        document.library_id,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )?;

    let admission = state
        .application_services
        .content
        .admit_mutation(
            state,
            crate::services::content::service::AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "delete".to_string(),
                idempotency_key: None,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "mcp".to_string(),
                source_identity: None,
                revision: None,
                parent_async_operation_id: None,
            },
        )
        .await?;

    Ok(json!({
        "documentId": document_id,
        "libraryId": document.library_id,
        "workspaceId": document.workspace_id,
        "mutationId": admission.mutation.id,
        "status": "accepted",
    }))
}
