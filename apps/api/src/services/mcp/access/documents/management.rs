use super::*;

pub(crate) async fn authorize_library_for_mcp(
    auth: &AuthContext,
    state: &AppState,
    library_ref: &str,
) -> Result<crate::infra::repositories::catalog_repository::CatalogLibraryRow, ApiError> {
    load_library_by_catalog_ref(auth, state, library_ref, POLICY_MCP_MEMORY_READ).await
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentInventoryCursor {
    principal_id: Uuid,
    library_id: Uuid,
    status_filter: Option<ironrag_contracts::documents::DocumentStatus>,
    readable_only: Option<bool>,
    scope_fingerprint: String,
    created_at: chrono::DateTime<chrono::Utc>,
    document_id: Uuid,
    snapshot_at: chrono::DateTime<chrono::Utc>,
}

impl DocumentInventoryCursor {
    fn decode(
        token: &str,
        principal_id: Uuid,
        library_id: Uuid,
        request: &crate::mcp_types::McpListDocumentsRequest,
    ) -> Result<Self, ApiError> {
        use base64::Engine as _;
        if token.len() > 4096 {
            return Err(ApiError::invalid_continuation_token("invalid document list continuation"));
        }
        let bytes =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token).map_err(|_| {
                ApiError::invalid_continuation_token("invalid document list continuation")
            })?;
        let cursor: Self = serde_json::from_slice(&bytes).map_err(|_| {
            ApiError::invalid_continuation_token("invalid document list continuation")
        })?;
        if cursor.principal_id != principal_id
            || cursor.library_id != library_id
            || cursor.status_filter != request.status_filter
            || cursor.readable_only != request.readable_only
        {
            return Err(ApiError::invalid_continuation_token(
                "document list continuation does not match this principal, library or filter",
            ));
        }
        Ok(cursor)
    }

    fn encode(&self) -> Result<String, ApiError> {
        use base64::Engine as _;
        let bytes = serde_json::to_vec(self).map_err(|error| {
            ApiError::internal_with_log(error, "document list continuation failed")
        })?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }
}

pub(crate) async fn list_documents(
    auth: &AuthContext,
    state: &AppState,
    library_id: Uuid,
    request: &crate::mcp_types::McpListDocumentsRequest,
) -> Result<serde_json::Value, ApiError> {
    use crate::{
        infra::repositories::content_document_list_repository::{
            DocumentListFilters, DocumentListPageCursor, DocumentListSearchRanges,
            DocumentListSortColumn,
        },
        services::content::service::ListDocumentsPageCommand,
    };

    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let library = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "library lookup failed"))?
        .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
    if !auth.has_document_or_library_read_scope_for_library(
        library.workspace_id,
        library.id,
        POLICY_MCP_MEMORY_READ,
    ) {
        return Err(ApiError::Unauthorized);
    }
    let id_filter =
        super::super::catalog::document_inventory_scope(auth, library.workspace_id, library.id);
    let cursor = request
        .continuation_token
        .as_deref()
        .map(|token| DocumentInventoryCursor::decode(token, auth.principal_id, library_id, request))
        .transpose()?;
    let snapshot_at = cursor.as_ref().map_or_else(chrono::Utc::now, |value| value.snapshot_at);
    let limit = u32::try_from(request.limit.unwrap_or(50).clamp(1, 200))
        .map_err(|_| ApiError::invalid_mcp_tool_call("invalid document list limit"))?;
    let page = state
        .application_services
        .content
        .list_documents_page(
            state,
            ListDocumentsPageCommand {
                library_id,
                include_deleted: false,
                cursor: cursor.as_ref().map(|value| DocumentListPageCursor {
                    created_at: value.created_at,
                    document_id: value.document_id,
                    text_value: None,
                    number_value: None,
                    finished_value: None,
                    value_is_null: false,
                }),
                limit,
                search: None,
                normalized_search: None,
                search_statuses: Vec::new(),
                filters: DocumentListFilters {
                    readable: request.readable_only,
                    ..DocumentListFilters::default()
                },
                search_ranges: DocumentListSearchRanges::default(),
                snapshot_at,
                sort: DocumentListSortColumn::CreatedAt,
                sort_desc: true,
                status_filter: request.status_filter.into_iter().collect(),
                id_filter,
                include_status_counts: false,
            },
        )
        .await?;
    if cursor.as_ref().is_some_and(|value| value.scope_fingerprint != page.scope_fingerprint) {
        return Err(ApiError::knowledge_not_ready(
            "document inventory changed; discard continuation and restart the list once",
        ));
    }

    let documents = page
        .items
        .iter()
        .map(|entry| {
            json!({
                "documentId": entry.id,
                "title": entry.file_name.as_deref().unwrap_or(&entry.external_key),
                "status": entry.status,
                "readinessKind": entry.readiness,
                "readable": entry.readable_revision_id.is_some(),
                "sourceUri": entry.source_uri,
                "byteSize": entry.file_size,
                "createdAt": entry.uploaded_at,
            })
        })
        .collect::<Vec<_>>();
    let continuation_token = page
        .next_cursor
        .map(|next| {
            DocumentInventoryCursor {
                principal_id: auth.principal_id,
                library_id,
                status_filter: request.status_filter,
                readable_only: request.readable_only,
                scope_fingerprint: page.scope_fingerprint,
                created_at: next.created_at,
                document_id: next.document_id,
                snapshot_at,
            }
            .encode()
        })
        .transpose()?;

    Ok(json!({
        "libraryId": library_id,
        "documents": documents,
        "count": documents.len(),
        "totalCount": page.total_count,
        "limit": limit,
        "hasMore": page.has_more,
        "continuationToken": continuation_token,
    }))
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
                enrichment_scope: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_types::McpListDocumentsRequest;
    use ironrag_contracts::documents::DocumentStatus;

    #[test]
    fn inventory_continuation_rejects_changed_principal_library_or_filter() {
        let principal_id = Uuid::from_u128(1);
        let library_id = Uuid::from_u128(2);
        let mut request = McpListDocumentsRequest {
            library: None,
            limit: Some(1),
            status_filter: Some(DocumentStatus::Failed),
            readable_only: None,
            continuation_token: None,
        };
        let cursor = DocumentInventoryCursor {
            principal_id,
            library_id,
            status_filter: request.status_filter,
            readable_only: request.readable_only,
            scope_fingerprint: "same-snapshot".to_owned(),
            created_at: chrono::Utc::now(),
            document_id: Uuid::from_u128(3),
            snapshot_at: chrono::Utc::now(),
        };
        let token = cursor.encode().expect("encode inventory continuation");
        let decoded = DocumentInventoryCursor::decode(&token, principal_id, library_id, &request)
            .expect("same scope resumes");
        assert_eq!(decoded.document_id, cursor.document_id);
        assert_eq!(decoded.scope_fingerprint, cursor.scope_fingerprint);
        assert!(
            DocumentInventoryCursor::decode(&token, Uuid::from_u128(4), library_id, &request)
                .is_err()
        );
        assert!(
            DocumentInventoryCursor::decode(&token, principal_id, Uuid::from_u128(5), &request)
                .is_err()
        );
        request.status_filter = Some(DocumentStatus::Ready);
        assert!(
            DocumentInventoryCursor::decode(&token, principal_id, library_id, &request).is_err()
        );
        request.status_filter = cursor.status_filter;
        request.readable_only = Some(true);
        assert!(
            DocumentInventoryCursor::decode(&token, principal_id, library_id, &request).is_err()
        );
    }

    #[test]
    fn inventory_request_separates_operational_status_from_readability() {
        let request: McpListDocumentsRequest = serde_json::from_value(json!({
            "statusFilter": "failed",
            "readableOnly": true,
        }))
        .expect("failed enrichment can preserve readable content");
        assert_eq!(request.status_filter, Some(DocumentStatus::Failed));
        assert_eq!(request.readable_only, Some(true));
        assert!(
            serde_json::from_value::<McpListDocumentsRequest>(json!({
                "statusFilter": "readable",
            }))
            .is_err()
        );
    }
}
