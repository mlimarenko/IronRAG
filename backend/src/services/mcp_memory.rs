use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::{AppState, McpMemorySettings},
    domains::mcp_memory::{
        McpAuditActionKind, McpAuditEvent, McpAuditStatus, McpCapabilitySnapshot, McpDocumentHit,
        McpLibraryDescriptor, McpMutationOperationKind, McpMutationReceipt,
        McpMutationReceiptStatus, McpReadDocumentResponse, McpReadMode, McpReadabilityState,
        McpSearchDocumentsResponse, McpWorkspaceDescriptor,
    },
    infra::repositories::{
        self,
        catalog_repository::{self, CatalogLibraryRow, CatalogWorkspaceRow},
        content_repository, extract_repository,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_WRITE, POLICY_MCP_AUDIT_REVIEW,
            POLICY_MCP_MEMORY_READ, POLICY_WORKSPACE_ADMIN, authorize_document_permission,
            authorize_library_discovery, authorize_library_permission, authorize_mcp_audit_review,
            authorize_workspace_discovery, authorize_workspace_permission,
            load_content_document_and_authorize, sanitize_mcp_audit_scope,
        },
        router_support::{ApiError, map_library_create_error, map_workspace_create_error},
    },
    services::{
        content_service::{
            AcceptMutationCommand, CreateDocumentCommand, CreateMutationItemCommand,
            CreateRevisionCommand, PromoteHeadCommand, UpdateMutationCommand,
            UpdateMutationItemCommand,
        },
        extract_service::PersistExtractContentCommand,
        ingest_service::{
            AdmitIngestJobCommand, FinalizeAttemptCommand, LeaseAttemptCommand,
            RecordStageEventCommand,
        },
    },
    shared::{
        chunking::{ChunkingProfile, split_text_into_chunks_with_profile},
        file_extract::{
            FileExtractError, FileExtractionPlan, UploadAdmissionError, UploadFileKind,
            build_runtime_file_extraction_plan,
        },
        slugs::slugify,
    },
};

const TOOL_CREATE_WORKSPACE: &str = "create_workspace";
const TOOL_CREATE_LIBRARY: &str = "create_library";
const TOOL_LIST_WORKSPACES: &str = "list_workspaces";
const TOOL_LIST_LIBRARIES: &str = "list_libraries";
const TOOL_SEARCH_DOCUMENTS: &str = "search_documents";
const TOOL_READ_DOCUMENT: &str = "read_document";
const TOOL_UPLOAD_DOCUMENTS: &str = "upload_documents";
const TOOL_UPDATE_DOCUMENT: &str = "update_document";
const TOOL_GET_MUTATION_STATUS: &str = "get_mutation_status";

#[derive(Debug, Clone)]
pub struct McpMemoryService {
    settings: McpMemorySettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpListLibrariesRequest {
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSearchDocumentsRequest {
    pub query: String,
    pub library_ids: Option<Vec<Uuid>>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCreateWorkspaceRequest {
    pub slug: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCreateLibraryRequest {
    pub workspace_id: Uuid,
    pub slug: Option<String>,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpReadDocumentRequest {
    pub document_id: Option<Uuid>,
    pub mode: Option<McpReadMode>,
    pub start_offset: Option<usize>,
    pub length: Option<usize>,
    pub continuation_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUploadDocumentInput {
    pub file_name: String,
    pub content_base64: String,
    pub mime_type: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUploadDocumentsRequest {
    pub library_id: Uuid,
    pub idempotency_key: Option<String>,
    pub documents: Vec<McpUploadDocumentInput>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDocumentMutationKind {
    Append,
    Replace,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUpdateDocumentRequest {
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub operation_kind: McpDocumentMutationKind,
    pub idempotency_key: Option<String>,
    pub appended_text: Option<String>,
    pub replacement_file_name: Option<String>,
    pub replacement_content_base64: Option<String>,
    pub replacement_mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGetMutationStatusRequest {
    pub receipt_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpListAuditEventsRequest {
    pub workspace_id: Option<Uuid>,
    pub token_id: Option<Uuid>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
struct VisibleLibraryContext {
    library: CatalogLibraryRow,
    descriptor: McpLibraryDescriptor,
}

#[derive(Debug, Clone)]
struct ResolvedDocumentState {
    document: content_repository::ContentDocumentRow,
    library: CatalogLibraryRow,
    latest_revision_id: Option<Uuid>,
    readability_state: McpReadabilityState,
    status_reason: Option<String>,
    content: Option<String>,
}

#[derive(Debug, Clone)]
struct CanonicalMutationFailure {
    failure_kind: String,
    message: String,
}

#[derive(Debug, Clone)]
enum CanonicalExtractOutcome {
    Ready(FileExtractionPlan),
    Failed(CanonicalMutationFailure),
}

#[derive(Debug, Clone, Default)]
pub struct McpAuditScope {
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpContinuationPayload {
    document_id: Uuid,
    run_id: Uuid,
    latest_revision_id: Option<Uuid>,
    next_offset: usize,
    window_chars: usize,
    read_mode: McpReadMode,
    proof: String,
}

impl Default for McpMemoryService {
    fn default() -> Self {
        Self {
            settings: McpMemorySettings {
                default_read_window_chars: 12_000,
                max_read_window_chars: 50_000,
                default_search_limit: 10,
                max_search_limit: 25,
                idempotency_retention_hours: 72,
                audit_enabled: true,
                upload_max_size_mb: 50,
            },
        }
    }
}

fn resolve_mcp_slug(requested_slug: Option<&str>, name: &str) -> String {
    requested_slug
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(slugify)
        .unwrap_or_else(|| slugify(name))
}

impl McpMemoryService {
    #[must_use]
    pub fn new(settings: McpMemorySettings) -> Self {
        Self { settings }
    }

    #[must_use]
    pub fn visible_tool_names(&self, auth: &AuthContext) -> Vec<String> {
        let mut tools = vec![TOOL_LIST_WORKSPACES.to_string(), TOOL_LIST_LIBRARIES.to_string()];
        if auth.is_system_admin {
            tools.push(TOOL_CREATE_WORKSPACE.to_string());
        }
        if auth.can_admin_any_workspace(POLICY_WORKSPACE_ADMIN) {
            tools.push(TOOL_CREATE_LIBRARY.to_string());
        }
        if auth.can_read_any_library_memory(POLICY_MCP_MEMORY_READ) {
            tools.push(TOOL_SEARCH_DOCUMENTS.to_string());
        }
        if auth.can_read_any_document_memory(POLICY_MCP_MEMORY_READ) {
            tools.push(TOOL_READ_DOCUMENT.to_string());
        }
        if auth.can_write_any_library_memory(POLICY_LIBRARY_WRITE) {
            tools.push(TOOL_UPLOAD_DOCUMENTS.to_string());
        }
        if auth.can_write_any_document_memory(POLICY_DOCUMENTS_WRITE) {
            tools.push(TOOL_UPDATE_DOCUMENT.to_string());
            tools.push(TOOL_GET_MUTATION_STATUS.to_string());
        }
        tools
    }

    pub async fn capability_snapshot(
        &self,
        auth: &AuthContext,
        state: &AppState,
    ) -> Result<McpCapabilitySnapshot, ApiError> {
        let workspaces = self.visible_workspaces(auth, state).await?;
        let libraries = self.visible_libraries(auth, state, None).await?;
        Ok(McpCapabilitySnapshot {
            token_id: auth.token_id,
            token_kind: auth.token_kind.clone(),
            workspace_scope: auth.workspace_id,
            visible_workspace_count: workspaces.len(),
            visible_library_count: libraries.len(),
            tools: self.visible_tool_names(auth),
            generated_at: Utc::now(),
        })
    }

    pub async fn visible_workspaces(
        &self,
        auth: &AuthContext,
        state: &AppState,
    ) -> Result<Vec<McpWorkspaceDescriptor>, ApiError> {
        let rows = self.load_visible_workspace_rows(auth, state).await?;
        let mut items = Vec::with_capacity(rows.len());
        for workspace in rows {
            let libraries = self.visible_libraries(auth, state, Some(workspace.id)).await?;
            let can_write_any_library = libraries.iter().any(|item| item.supports_write);
            items.push(McpWorkspaceDescriptor {
                workspace_id: workspace.id,
                slug: workspace.slug,
                name: workspace.display_name,
                status: workspace.lifecycle_state,
                visible_library_count: libraries.len(),
                can_write_any_library,
            });
        }
        Ok(items)
    }

    pub async fn visible_libraries(
        &self,
        auth: &AuthContext,
        state: &AppState,
        workspace_filter: Option<Uuid>,
    ) -> Result<Vec<McpLibraryDescriptor>, ApiError> {
        let libraries = self.load_visible_library_contexts(auth, state, workspace_filter).await?;
        Ok(libraries.into_iter().map(|item| item.descriptor).collect())
    }

    pub async fn search_documents(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request: McpSearchDocumentsRequest,
    ) -> Result<McpSearchDocumentsResponse, ApiError> {
        auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
        let query = request.query.trim();
        if query.is_empty() {
            return Err(ApiError::BadRequest("query must not be empty".into()));
        }

        let limit = request
            .limit
            .unwrap_or(self.settings.default_search_limit)
            .clamp(1, self.settings.max_search_limit);
        let libraries =
            self.resolve_search_libraries(auth, state, request.library_ids.as_deref()).await?;
        let library_ids = libraries.iter().map(|item| item.library.id).collect::<Vec<_>>();
        let query_lower = query.to_ascii_lowercase();
        let mut hits = Vec::new();
        for library in libraries {
            let documents = content_repository::list_documents_by_library(
                &state.persistence.postgres,
                library.library.id,
            )
            .await
            .map_err(|_| ApiError::Internal)?;
            for document in documents {
                if document.document_state == "deleted" {
                    continue;
                }
                let Some(head) =
                    content_repository::get_document_head(&state.persistence.postgres, document.id)
                        .await
                        .map_err(|_| ApiError::Internal)?
                else {
                    continue;
                };
                let Some(readable_revision_id) = head.readable_revision_id else {
                    continue;
                };
                let extract = extract_repository::get_extract_content_by_revision_id(
                    &state.persistence.postgres,
                    readable_revision_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?;
                let Some(extract) = extract else {
                    continue;
                };
                let Some(normalized_text) = extract.normalized_text.as_deref() else {
                    continue;
                };
                let Some((excerpt, excerpt_start_offset, excerpt_end_offset, score)) =
                    preview_hit(normalized_text, &query_lower)
                else {
                    continue;
                };
                let revision = content_repository::get_revision_by_id(
                    &state.persistence.postgres,
                    readable_revision_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?;
                let document_title = revision
                    .and_then(|row| row.title.filter(|value| !value.trim().is_empty()))
                    .unwrap_or_else(|| document.external_key.clone());
                hits.push(McpDocumentHit {
                    document_id: document.id,
                    logical_document_id: document.id,
                    library_id: document.library_id,
                    workspace_id: document.workspace_id,
                    document_title,
                    latest_revision_id: Some(readable_revision_id),
                    score,
                    excerpt: Some(excerpt),
                    excerpt_start_offset: Some(excerpt_start_offset),
                    excerpt_end_offset: Some(excerpt_end_offset),
                    readability_state: McpReadabilityState::Readable,
                    status_reason: None,
                });
            }
        }
        hits.sort_by(|left, right| right.score.total_cmp(&left.score));
        hits.truncate(limit);

        Ok(McpSearchDocumentsResponse { query: query.to_string(), limit, library_ids, hits })
    }

    pub async fn create_workspace(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request: McpCreateWorkspaceRequest,
    ) -> Result<McpWorkspaceDescriptor, ApiError> {
        if !auth.is_system_admin {
            return Err(ApiError::Unauthorized);
        }
        let name = request.name.trim();
        if name.is_empty() {
            return Err(ApiError::BadRequest("workspace name must not be empty".into()));
        }
        let slug = resolve_mcp_slug(request.slug.as_deref(), name);

        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                state,
                crate::services::catalog_service::CreateWorkspaceCommand {
                    slug: Some(slug.clone()),
                    display_name: name.to_string(),
                    created_by_principal_id: Some(auth.principal_id),
                },
            )
            .await
            .map_err(|error| match error {
                ApiError::Conflict(_) => error,
                _ => map_workspace_create_error(sqlx::Error::Protocol(error.to_string()), &slug),
            })?;

        Ok(McpWorkspaceDescriptor {
            workspace_id: workspace.id,
            slug: workspace.slug,
            name: workspace.display_name,
            status: "active".to_string(),
            visible_library_count: 0,
            can_write_any_library: auth.is_system_admin,
        })
    }

    pub async fn create_library(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request: McpCreateLibraryRequest,
    ) -> Result<McpLibraryDescriptor, ApiError> {
        authorize_workspace_permission(auth, request.workspace_id, POLICY_WORKSPACE_ADMIN)?;

        let name = request.name.trim();
        if name.is_empty() {
            return Err(ApiError::BadRequest("library name must not be empty".into()));
        }
        let slug = resolve_mcp_slug(request.slug.as_deref(), name);

        let library = state
            .canonical_services
            .catalog
            .create_library(
                state,
                crate::services::catalog_service::CreateLibraryCommand {
                    workspace_id: request.workspace_id,
                    slug: Some(slug.clone()),
                    display_name: name.to_string(),
                    description: request.description,
                    created_by_principal_id: Some(auth.principal_id),
                },
            )
            .await
            .map_err(|error| match error {
                ApiError::Conflict(_) => error,
                _ => map_library_create_error(
                    sqlx::Error::Protocol(error.to_string()),
                    request.workspace_id,
                    &slug,
                ),
            })?;

        let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library.id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("library", library.id))?;
        let context = self.describe_library(auth, state, row).await?;
        Ok(context.descriptor)
    }

    pub async fn read_document(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request: McpReadDocumentRequest,
    ) -> Result<McpReadDocumentResponse, ApiError> {
        auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
        let normalized = self.normalize_read_request(auth, request)?;
        let state_view = self.resolve_document_state(auth, state, normalized.document_id).await?;
        let latest_revision_id = state_view.latest_revision_id;

        if state_view.readability_state != McpReadabilityState::Readable {
            return Ok(McpReadDocumentResponse {
                document_id: state_view.document.id,
                library_id: state_view.library.id,
                workspace_id: state_view.library.workspace_id,
                latest_revision_id,
                read_mode: normalized.read_mode,
                readability_state: state_view.readability_state,
                status_reason: state_view.status_reason,
                content: None,
                slice_start_offset: normalized.start_offset,
                slice_end_offset: normalized.start_offset,
                total_content_length: None,
                continuation_token: None,
                has_more: false,
            });
        }

        let content = state_view.content.clone().unwrap_or_default();
        let total_content_length = content.chars().count();
        let slice = char_slice(&content, normalized.start_offset, normalized.window_chars);
        let slice_len = slice.chars().count();
        let slice_end_offset = normalized.start_offset.saturating_add(slice_len);
        let has_more = slice_end_offset < total_content_length;
        let continuation_token = has_more.then(|| {
            self.encode_continuation_token(
                auth,
                normalized.document_id,
                latest_revision_id.unwrap_or(normalized.document_id),
                latest_revision_id,
                slice_end_offset,
                normalized.window_chars,
                normalized.read_mode.clone(),
            )
        });

        Ok(McpReadDocumentResponse {
            document_id: state_view.document.id,
            library_id: state_view.library.id,
            workspace_id: state_view.library.workspace_id,
            latest_revision_id,
            read_mode: normalized.read_mode,
            readability_state: McpReadabilityState::Readable,
            status_reason: None,
            content: Some(slice),
            slice_start_offset: normalized.start_offset.min(total_content_length),
            slice_end_offset,
            total_content_length: Some(total_content_length),
            continuation_token,
            has_more,
        })
    }

    pub async fn upload_documents(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request: McpUploadDocumentsRequest,
    ) -> Result<Vec<McpMutationReceipt>, ApiError> {
        auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
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

        let upload_batch_id = Uuid::now_v7();
        let mut receipts = Vec::with_capacity(request.documents.len());
        let mut total_upload_bytes = 0_u64;
        for (index, document) in request.documents.into_iter().enumerate() {
            let file_name = document.file_name.trim();
            if file_name.is_empty() {
                return Err(ApiError::invalid_mcp_tool_call(format!(
                    "documents[{index}].fileName must not be empty"
                )));
            }

            let file_bytes =
                BASE64_STANDARD.decode(document.content_base64.trim()).map_err(|_| {
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
                &self.settings,
                file_name,
                document.mime_type.as_deref(),
                &file_bytes,
            )?;
            total_upload_bytes = total_upload_bytes
                .saturating_add(u64::try_from(file_bytes.len()).unwrap_or(u64::MAX));
            validate_mcp_upload_batch_size(&self.settings, total_upload_bytes)?;

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

            if let Some(existing) = self
                .find_existing_mutation_by_idempotency(
                    auth,
                    state,
                    &normalized_idempotency_key,
                    &payload_identity,
                )
                .await?
            {
                receipts.push(self.resolve_mutation_receipt(state, auth, existing).await?);
                continue;
            }

            let receipt = self
                .process_upload_mutation(
                    auth,
                    state,
                    &library,
                    normalized_idempotency_key,
                    payload_identity,
                    document,
                    file_bytes,
                    upload_batch_id,
                )
                .await?;
            receipts.push(receipt);
        }

        Ok(receipts)
    }

    pub async fn update_document(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request: McpUpdateDocumentRequest,
    ) -> Result<McpMutationReceipt, ApiError> {
        auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
        let library = crate::interfaces::http::authorization::load_library_and_authorize(
            auth,
            state,
            request.library_id,
            POLICY_LIBRARY_WRITE,
        )
        .await?;
        let document = load_content_document_and_authorize(
            auth,
            state,
            request.document_id,
            POLICY_DOCUMENTS_WRITE,
        )
        .await?;
        if document.library_id != library.id {
            return Err(ApiError::inaccessible_memory_scope(
                "document is not visible inside the requested library",
            ));
        }

        let current_state = self.resolve_document_state(auth, state, document.id).await?;
        self.ensure_document_accepts_new_mutation(state, document.id).await?;

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
                if current_state.readability_state != McpReadabilityState::Readable {
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
                    .decode(
                        request.replacement_content_base64.as_deref().map(str::trim).ok_or_else(
                            || {
                                ApiError::invalid_mcp_tool_call(
                                    "replace requires replacementContentBase64",
                                )
                            },
                        )?,
                    )
                    .map_err(|_| {
                        ApiError::invalid_mcp_tool_call(
                            "replacementContentBase64 must be valid base64",
                        )
                    })?;
                if file_bytes.is_empty() {
                    return Err(ApiError::invalid_mcp_tool_call(
                        "replacement upload body must not be empty",
                    ));
                }
                validate_mcp_upload_file_size(
                    &self.settings,
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
            document.id,
            &operation_kind,
            &payload_identity,
        );

        if let Some(existing) = self
            .find_existing_mutation_by_idempotency(
                auth,
                state,
                &normalized_idempotency_key,
                &payload_identity,
            )
            .await?
        {
            return self.resolve_mutation_receipt(state, auth, existing).await;
        }

        match request.operation_kind {
            McpDocumentMutationKind::Append => {
                self.process_append_mutation(
                    auth,
                    state,
                    &library,
                    &document,
                    normalized_idempotency_key,
                    payload_identity,
                    request.appended_text.unwrap_or_default(),
                    current_state.content.unwrap_or_default(),
                )
                .await
            }
            McpDocumentMutationKind::Replace => {
                let replacement_content_base64 =
                    request.replacement_content_base64.unwrap_or_default();
                let file_name =
                    request.replacement_file_name.unwrap_or_else(|| "replace.bin".to_string());
                let file_bytes =
                    BASE64_STANDARD.decode(replacement_content_base64.trim()).map_err(|_| {
                        ApiError::invalid_mcp_tool_call(
                            "replacementContentBase64 must be valid base64",
                        )
                    })?;
                validate_mcp_upload_file_size(
                    &self.settings,
                    &file_name,
                    request.replacement_mime_type.as_deref(),
                    &file_bytes,
                )?;
                self.process_replace_mutation(
                    auth,
                    state,
                    &library,
                    &document,
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
        &self,
        auth: &AuthContext,
        state: &AppState,
        request: McpGetMutationStatusRequest,
    ) -> Result<McpMutationReceipt, ApiError> {
        auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
        let mutation =
            content_repository::get_mutation_by_id(&state.persistence.postgres, request.receipt_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| {
                    ApiError::NotFound(format!("mutation receipt {} not found", request.receipt_id))
                })?;

        self.resolve_mutation_receipt(state, auth, mutation).await
    }

    pub async fn list_audit_events(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request: McpListAuditEventsRequest,
    ) -> Result<Vec<McpAuditEvent>, ApiError> {
        auth.require_any_scope(POLICY_MCP_AUDIT_REVIEW)?;
        let workspace_filter = authorize_mcp_audit_review(auth, request.workspace_id)?;
        let limit = i64::try_from(request.limit.unwrap_or(50).clamp(1, 200)).unwrap_or(200);

        repositories::list_mcp_audit_events(
            &state.persistence.postgres,
            workspace_filter,
            request.token_id,
            limit,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| self.map_audit_row(row))
        .collect()
    }

    pub async fn record_success_audit(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request_id: &str,
        action_kind: McpAuditActionKind,
        scope: McpAuditScope,
        metadata_json: serde_json::Value,
    ) {
        self.record_audit_event(
            auth,
            state,
            request_id,
            action_kind,
            scope,
            McpAuditStatus::Succeeded,
            None,
            metadata_json,
        )
        .await;
    }

    pub async fn record_error_audit(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request_id: &str,
        action_kind: McpAuditActionKind,
        scope: McpAuditScope,
        error: &ApiError,
        metadata_json: serde_json::Value,
    ) {
        let sanitized = sanitize_mcp_audit_scope(
            error,
            scope.workspace_id,
            scope.library_id,
            scope.document_id,
        );
        let status = match error {
            ApiError::Internal => McpAuditStatus::Failed,
            _ => McpAuditStatus::Rejected,
        };
        self.record_audit_event(
            auth,
            state,
            request_id,
            action_kind,
            McpAuditScope {
                workspace_id: sanitized.workspace_id,
                library_id: sanitized.library_id,
                document_id: sanitized.document_id,
            },
            status,
            Some(error.kind().to_string()),
            metadata_json,
        )
        .await;
    }

    async fn record_audit_event(
        &self,
        auth: &AuthContext,
        state: &AppState,
        request_id: &str,
        action_kind: McpAuditActionKind,
        scope: McpAuditScope,
        status: McpAuditStatus,
        error_kind: Option<String>,
        metadata_json: serde_json::Value,
    ) {
        if !self.settings.audit_enabled {
            return;
        }

        if let Err(error) = repositories::create_mcp_audit_event(
            &state.persistence.postgres,
            &repositories::NewMcpAuditEvent {
                request_id: request_id.to_string(),
                token_id: auth.token_id,
                token_kind: auth.token_kind.clone(),
                action_kind: audit_action_kind_key(&action_kind).to_string(),
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                document_id: scope.document_id,
                status: audit_status_key(&status).to_string(),
                error_kind,
                metadata_json,
            },
        )
        .await
        {
            warn!(
                request_id,
                token_id = %auth.token_id,
                action_kind = audit_action_kind_key(&action_kind),
                ?error,
                "failed to persist mcp audit event",
            );
        }
    }

    async fn find_existing_mutation_by_idempotency(
        &self,
        auth: &AuthContext,
        state: &AppState,
        idempotency_key: &str,
        payload_identity: &str,
    ) -> Result<Option<content_repository::ContentMutationRow>, ApiError> {
        let existing = content_repository::find_mutation_by_idempotency(
            &state.persistence.postgres,
            auth.principal_id,
            "mcp",
            idempotency_key,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let Some(existing) = existing else {
            return Ok(None);
        };
        self.ensure_matching_mutation_payload_identity(state, existing.id, payload_identity)
            .await?;
        Ok(Some(existing))
    }

    async fn ensure_matching_mutation_payload_identity(
        &self,
        state: &AppState,
        mutation_id: Uuid,
        payload_identity: &str,
    ) -> Result<(), ApiError> {
        let items =
            content_repository::list_mutation_items(&state.persistence.postgres, mutation_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        let existing_payload_identity =
            if let Some(revision_id) = items.iter().find_map(|item| item.result_revision_id) {
                content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .and_then(|revision| {
                        payload_identity_from_source_uri(revision.source_uri.as_deref())
                    })
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

    async fn ensure_document_accepts_new_mutation(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        let Some(head) =
            content_repository::get_document_head(&state.persistence.postgres, document_id)
                .await
                .map_err(|_| ApiError::Internal)?
        else {
            return Ok(());
        };
        let Some(latest_mutation_id) = head.latest_mutation_id else {
            return Ok(());
        };
        let Some(latest_mutation) =
            content_repository::get_mutation_by_id(&state.persistence.postgres, latest_mutation_id)
                .await
                .map_err(|_| ApiError::Internal)?
        else {
            return Ok(());
        };
        if matches!(latest_mutation.mutation_state.as_str(), "accepted" | "running") {
            return Err(ApiError::ConflictingMutation(
                "document is still processing a previous mutation".to_string(),
            ));
        }
        Ok(())
    }

    async fn process_upload_mutation(
        &self,
        auth: &AuthContext,
        state: &AppState,
        library: &CatalogLibraryRow,
        idempotency_key: String,
        payload_identity: String,
        document: McpUploadDocumentInput,
        file_bytes: Vec<u8>,
        _upload_batch_id: Uuid,
    ) -> Result<McpMutationReceipt, ApiError> {
        let content_service = &state.canonical_services.content;
        let document_row = content_service
            .create_document(
                state,
                CreateDocumentCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    external_key: None,
                    created_by_principal_id: Some(auth.principal_id),
                },
            )
            .await?;
        let head = content_service.get_document_head(state, document_row.id).await?;
        let readable_revision_id = head.as_ref().and_then(|row| row.readable_revision_id);
        let latest_successful_attempt_id =
            head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let mutation = content_service
            .accept_mutation(
                state,
                AcceptMutationCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    operation_kind: "upload".to_string(),
                    requested_by_principal_id: Some(auth.principal_id),
                    request_surface: "mcp".to_string(),
                    idempotency_key: Some(idempotency_key),
                },
            )
            .await?;
        let file_name = document.file_name.trim().to_string();
        let title = document
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| file_name.clone());
        let mime_type =
            infer_revision_mime_type(document.mime_type.as_deref(), Some(&file_name), "upload");
        let revision = content_service
            .create_revision(
                state,
                CreateRevisionCommand {
                    document_id: document_row.id,
                    content_source_kind: "upload".to_string(),
                    checksum: payload_identity.clone(),
                    mime_type,
                    byte_size: i64::try_from(file_bytes.len()).unwrap_or(i64::MAX),
                    title: Some(title),
                    language_code: None,
                    source_uri: Some(payload_source_uri(&payload_identity)),
                    storage_key: None,
                    created_by_principal_id: Some(auth.principal_id),
                },
            )
            .await?;
        let item = content_service
            .create_mutation_item(
                state,
                CreateMutationItemCommand {
                    mutation_id: mutation.id,
                    document_id: Some(document_row.id),
                    base_revision_id: None,
                    result_revision_id: Some(revision.id),
                    item_state: "pending".to_string(),
                    message: Some("document upload accepted".to_string()),
                },
            )
            .await?;
        let _ = content_service
            .promote_document_head(
                state,
                PromoteHeadCommand {
                    document_id: document_row.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id,
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id,
                },
            )
            .await?;

        self.run_file_mutation_pipeline(
            auth,
            state,
            library,
            mutation.id,
            item.id,
            document_row.id,
            revision.id,
            &file_name,
            document.mime_type.as_deref(),
            &file_bytes,
        )
        .await
    }

    async fn process_append_mutation(
        &self,
        auth: &AuthContext,
        state: &AppState,
        library: &CatalogLibraryRow,
        document: &content_repository::ContentDocumentRow,
        idempotency_key: String,
        payload_identity: String,
        appended_text: String,
        current_content: String,
    ) -> Result<McpMutationReceipt, ApiError> {
        let content_service = &state.canonical_services.content;
        let head = content_service.get_document_head(state, document.id).await?;
        let base_revision_id =
            head.as_ref().and_then(|row| row.active_revision_id.or(row.readable_revision_id));
        let latest_successful_attempt_id =
            head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let readable_revision_id = head.as_ref().and_then(|row| row.readable_revision_id);
        let base_revision = match base_revision_id {
            Some(revision_id) => {
                content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
            }
            None => None,
        };
        let merged_text = merge_appended_text(&current_content, &appended_text);
        let mutation = content_service
            .accept_mutation(
                state,
                AcceptMutationCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    operation_kind: "append".to_string(),
                    requested_by_principal_id: Some(auth.principal_id),
                    request_surface: "mcp".to_string(),
                    idempotency_key: Some(idempotency_key),
                },
            )
            .await?;
        let revision = content_service
            .append_revision(
                state,
                CreateRevisionCommand {
                    document_id: document.id,
                    content_source_kind: "append".to_string(),
                    checksum: sha256_hex(&merged_text),
                    mime_type: base_revision
                        .as_ref()
                        .map(|row| row.mime_type.clone())
                        .unwrap_or_else(|| "text/plain".to_string()),
                    byte_size: i64::try_from(merged_text.len()).unwrap_or(i64::MAX),
                    title: base_revision
                        .as_ref()
                        .and_then(|row| row.title.clone())
                        .or_else(|| Some(document.external_key.clone())),
                    language_code: base_revision.as_ref().and_then(|row| row.language_code.clone()),
                    source_uri: Some(payload_source_uri(&payload_identity)),
                    storage_key: None,
                    created_by_principal_id: Some(auth.principal_id),
                },
            )
            .await?;
        let item = content_service
            .create_mutation_item(
                state,
                CreateMutationItemCommand {
                    mutation_id: mutation.id,
                    document_id: Some(document.id),
                    base_revision_id,
                    result_revision_id: Some(revision.id),
                    item_state: "pending".to_string(),
                    message: Some("append mutation accepted".to_string()),
                },
            )
            .await?;
        let _ = content_service
            .promote_document_head(
                state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id,
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id,
                },
            )
            .await?;

        self.run_text_mutation_pipeline(
            auth,
            state,
            library,
            mutation.id,
            item.id,
            document.id,
            revision.id,
            merged_text,
        )
        .await
    }

    async fn process_replace_mutation(
        &self,
        auth: &AuthContext,
        state: &AppState,
        library: &CatalogLibraryRow,
        document: &content_repository::ContentDocumentRow,
        idempotency_key: String,
        payload_identity: String,
        file_name: String,
        mime_type: Option<String>,
        file_bytes: Vec<u8>,
    ) -> Result<McpMutationReceipt, ApiError> {
        let content_service = &state.canonical_services.content;
        let head = content_service.get_document_head(state, document.id).await?;
        let base_revision_id =
            head.as_ref().and_then(|row| row.active_revision_id.or(row.readable_revision_id));
        let latest_successful_attempt_id =
            head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let readable_revision_id = head.as_ref().and_then(|row| row.readable_revision_id);
        let base_revision = match base_revision_id {
            Some(revision_id) => {
                content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
            }
            None => None,
        };
        let mutation = content_service
            .accept_mutation(
                state,
                AcceptMutationCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    operation_kind: "replace".to_string(),
                    requested_by_principal_id: Some(auth.principal_id),
                    request_surface: "mcp".to_string(),
                    idempotency_key: Some(idempotency_key),
                },
            )
            .await?;
        let revision = content_service
            .replace_revision(
                state,
                CreateRevisionCommand {
                    document_id: document.id,
                    content_source_kind: "replace".to_string(),
                    checksum: payload_identity.clone(),
                    mime_type: infer_revision_mime_type(
                        mime_type.as_deref(),
                        Some(&file_name),
                        "replace",
                    ),
                    byte_size: i64::try_from(file_bytes.len()).unwrap_or(i64::MAX),
                    title: Some(
                        base_revision
                            .as_ref()
                            .and_then(|row| row.title.clone())
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or(file_name.clone()),
                    ),
                    language_code: base_revision.as_ref().and_then(|row| row.language_code.clone()),
                    source_uri: Some(payload_source_uri(&payload_identity)),
                    storage_key: None,
                    created_by_principal_id: Some(auth.principal_id),
                },
            )
            .await?;
        let item = content_service
            .create_mutation_item(
                state,
                CreateMutationItemCommand {
                    mutation_id: mutation.id,
                    document_id: Some(document.id),
                    base_revision_id,
                    result_revision_id: Some(revision.id),
                    item_state: "pending".to_string(),
                    message: Some("replace mutation accepted".to_string()),
                },
            )
            .await?;
        let _ = content_service
            .promote_document_head(
                state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id,
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id,
                },
            )
            .await?;

        self.run_file_mutation_pipeline(
            auth,
            state,
            library,
            mutation.id,
            item.id,
            document.id,
            revision.id,
            &file_name,
            mime_type.as_deref(),
            &file_bytes,
        )
        .await
    }

    async fn run_text_mutation_pipeline(
        &self,
        auth: &AuthContext,
        state: &AppState,
        library: &CatalogLibraryRow,
        mutation_id: Uuid,
        item_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        text: String,
    ) -> Result<McpMutationReceipt, ApiError> {
        let attempt =
            self.create_inline_attempt(state, library, mutation_id, "content_mutation").await?;
        state
            .canonical_services
            .content
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id,
                    mutation_state: "running".to_string(),
                    completed_at: None,
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await?;
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id: attempt.id,
                    stage_name: "extract_content".to_string(),
                    stage_state: "started".to_string(),
                    message: Some("materializing appended text".to_string()),
                    details_json: serde_json::json!({ "documentId": document_id, "revisionId": revision_id }),
                },
            )
            .await?;
        state
            .canonical_services
            .extract
            .persist_extract_content(
                state,
                PersistExtractContentCommand {
                    revision_id,
                    attempt_id: Some(attempt.id),
                    extract_state: "ready".to_string(),
                    normalized_text: Some(text.clone()),
                    text_checksum: Some(sha256_hex(&text)),
                    warning_count: 0,
                },
            )
            .await?;
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id: attempt.id,
                    stage_name: "extract_content".to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("appended text materialized".to_string()),
                    details_json: serde_json::json!({ "contentLength": text.chars().count() }),
                },
            )
            .await?;

        self.persist_revision_chunks(state, revision_id, &text).await?;
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id: attempt.id,
                    stage_name: "chunk_content".to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("content chunks persisted".to_string()),
                    details_json: serde_json::json!({ "revisionId": revision_id }),
                },
            )
            .await?;

        self.complete_successful_mutation(
            auth,
            state,
            mutation_id,
            item_id,
            document_id,
            revision_id,
            attempt.id,
        )
        .await
    }

    async fn run_file_mutation_pipeline(
        &self,
        auth: &AuthContext,
        state: &AppState,
        library: &CatalogLibraryRow,
        mutation_id: Uuid,
        item_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> Result<McpMutationReceipt, ApiError> {
        let attempt =
            self.create_inline_attempt(state, library, mutation_id, "content_mutation").await?;
        state
            .canonical_services
            .content
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id,
                    mutation_state: "running".to_string(),
                    completed_at: None,
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await?;
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id: attempt.id,
                    stage_name: "extract_content".to_string(),
                    stage_state: "started".to_string(),
                    message: Some("extracting readable content".to_string()),
                    details_json: serde_json::json!({ "fileName": file_name, "documentId": document_id }),
                },
            )
            .await?;

        match self.build_extract_outcome(state, file_name, mime_type, file_bytes).await {
            CanonicalExtractOutcome::Ready(plan) => {
                let normalized_text = plan.extracted_text.clone().unwrap_or_default();
                state
                    .canonical_services
                    .extract
                    .persist_extract_content(
                        state,
                        PersistExtractContentCommand {
                            revision_id,
                            attempt_id: Some(attempt.id),
                            extract_state: "ready".to_string(),
                            normalized_text: Some(normalized_text.clone()),
                            text_checksum: Some(sha256_hex(&normalized_text)),
                            warning_count: i32::try_from(plan.extraction_warnings.len())
                                .unwrap_or(i32::MAX),
                        },
                    )
                    .await?;
                state
                    .canonical_services
                    .ingest
                    .record_stage_event(
                        state,
                        RecordStageEventCommand {
                            attempt_id: attempt.id,
                            stage_name: "extract_content".to_string(),
                            stage_state: "completed".to_string(),
                            message: Some("readable content extracted".to_string()),
                            details_json: serde_json::json!({
                                "fileKind": plan.file_kind.as_str(),
                                "warningCount": plan.extraction_warnings.len(),
                            }),
                        },
                    )
                    .await?;
                self.persist_revision_chunks(state, revision_id, &normalized_text).await?;
                state
                    .canonical_services
                    .ingest
                    .record_stage_event(
                        state,
                        RecordStageEventCommand {
                            attempt_id: attempt.id,
                            stage_name: "chunk_content".to_string(),
                            stage_state: "completed".to_string(),
                            message: Some("content chunks persisted".to_string()),
                            details_json: serde_json::json!({ "revisionId": revision_id }),
                        },
                    )
                    .await?;
                self.complete_successful_mutation(
                    auth,
                    state,
                    mutation_id,
                    item_id,
                    document_id,
                    revision_id,
                    attempt.id,
                )
                .await
            }
            CanonicalExtractOutcome::Failed(failure) => {
                state
                    .canonical_services
                    .extract
                    .persist_extract_content(
                        state,
                        PersistExtractContentCommand {
                            revision_id,
                            attempt_id: Some(attempt.id),
                            extract_state: "failed".to_string(),
                            normalized_text: None,
                            text_checksum: None,
                            warning_count: 0,
                        },
                    )
                    .await?;
                state
                    .canonical_services
                    .ingest
                    .record_stage_event(
                        state,
                        RecordStageEventCommand {
                            attempt_id: attempt.id,
                            stage_name: "extract_content".to_string(),
                            stage_state: "failed".to_string(),
                            message: Some(failure.message.clone()),
                            details_json: serde_json::json!({ "failureKind": failure.failure_kind }),
                        },
                    )
                    .await?;
                self.complete_failed_mutation(
                    auth,
                    state,
                    mutation_id,
                    item_id,
                    document_id,
                    revision_id,
                    attempt.id,
                    failure,
                )
                .await
            }
        }
    }

    async fn create_inline_attempt(
        &self,
        state: &AppState,
        library: &CatalogLibraryRow,
        mutation_id: Uuid,
        job_kind: &str,
    ) -> Result<crate::domains::ingest::IngestAttempt, ApiError> {
        let job = state
            .canonical_services
            .ingest
            .admit_job(
                state,
                AdmitIngestJobCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    mutation_id: Some(mutation_id),
                    connector_id: None,
                    job_kind: job_kind.to_string(),
                    priority: 100,
                    dedupe_key: None,
                    available_at: None,
                },
            )
            .await?;
        state
            .canonical_services
            .ingest
            .lease_attempt(
                state,
                LeaseAttemptCommand {
                    job_id: job.id,
                    worker_principal_id: None,
                    lease_token: Some(format!("mcp-inline-{}", Uuid::now_v7())),
                    current_stage: Some("extract_content".to_string()),
                },
            )
            .await
    }

    async fn complete_successful_mutation(
        &self,
        auth: &AuthContext,
        state: &AppState,
        mutation_id: Uuid,
        item_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<McpMutationReceipt, ApiError> {
        let content_service = &state.canonical_services.content;
        let _ = content_service
            .promote_document_head(
                state,
                PromoteHeadCommand {
                    document_id,
                    active_revision_id: Some(revision_id),
                    readable_revision_id: Some(revision_id),
                    latest_mutation_id: Some(mutation_id),
                    latest_successful_attempt_id: Some(attempt_id),
                },
            )
            .await?;
        let _ = content_service
            .update_mutation_item(
                state,
                UpdateMutationItemCommand {
                    item_id,
                    document_id: Some(document_id),
                    base_revision_id: None,
                    result_revision_id: Some(revision_id),
                    item_state: "applied".to_string(),
                    message: Some("mutation applied".to_string()),
                },
            )
            .await?;
        let _ = content_service
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id,
                    mutation_state: "applied".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await?;
        let _ = state
            .canonical_services
            .ingest
            .finalize_attempt(
                state,
                FinalizeAttemptCommand {
                    attempt_id,
                    attempt_state: "succeeded".to_string(),
                    current_stage: Some("chunk_content".to_string()),
                    failure_class: None,
                    failure_code: None,
                    retryable: false,
                },
            )
            .await?;
        let mutation =
            content_repository::get_mutation_by_id(&state.persistence.postgres, mutation_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("mutation", mutation_id))?;
        self.resolve_mutation_receipt(state, auth, mutation).await
    }

    async fn complete_failed_mutation(
        &self,
        auth: &AuthContext,
        state: &AppState,
        mutation_id: Uuid,
        item_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        attempt_id: Uuid,
        failure: CanonicalMutationFailure,
    ) -> Result<McpMutationReceipt, ApiError> {
        let content_service = &state.canonical_services.content;
        let current_head = content_service.get_document_head(state, document_id).await?;
        let latest_successful_attempt_id =
            current_head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let _ = content_service
            .promote_document_head(
                state,
                PromoteHeadCommand {
                    document_id,
                    active_revision_id: Some(revision_id),
                    readable_revision_id: current_head.and_then(|row| row.readable_revision_id),
                    latest_mutation_id: Some(mutation_id),
                    latest_successful_attempt_id,
                },
            )
            .await?;
        let _ = content_service
            .update_mutation_item(
                state,
                UpdateMutationItemCommand {
                    item_id,
                    document_id: Some(document_id),
                    base_revision_id: None,
                    result_revision_id: Some(revision_id),
                    item_state: "failed".to_string(),
                    message: Some(failure.message.clone()),
                },
            )
            .await?;
        let _ = content_service
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id,
                    mutation_state: "failed".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: Some(failure.failure_kind.clone()),
                    conflict_code: None,
                },
            )
            .await?;
        let _ = state
            .canonical_services
            .ingest
            .finalize_attempt(
                state,
                FinalizeAttemptCommand {
                    attempt_id,
                    attempt_state: "failed".to_string(),
                    current_stage: Some("extract_content".to_string()),
                    failure_class: Some("content_extract".to_string()),
                    failure_code: Some(failure.failure_kind),
                    retryable: false,
                },
            )
            .await?;
        let mutation =
            content_repository::get_mutation_by_id(&state.persistence.postgres, mutation_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("mutation", mutation_id))?;
        self.resolve_mutation_receipt(state, auth, mutation).await
    }

    async fn build_extract_outcome(
        &self,
        state: &AppState,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> CanonicalExtractOutcome {
        let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        let profile = state.effective_provider_profile();
        match build_runtime_file_extraction_plan(
            state.llm_gateway.as_ref(),
            &profile.vision,
            Some(file_name),
            mime_type,
            file_bytes.to_vec(),
        )
        .await
        {
            Ok(plan) => {
                match validate_extraction_plan(file_name, mime_type, file_size_bytes, &plan) {
                    Ok(()) => CanonicalExtractOutcome::Ready(plan),
                    Err(error) => CanonicalExtractOutcome::Failed(CanonicalMutationFailure {
                        failure_kind: error.error_kind().to_string(),
                        message: error.message().to_string(),
                    }),
                }
            }
            Err(error) => {
                let rejection = UploadAdmissionError::from_file_extract_error(
                    file_name,
                    mime_type,
                    file_size_bytes,
                    error,
                );
                CanonicalExtractOutcome::Failed(CanonicalMutationFailure {
                    failure_kind: rejection.error_kind().to_string(),
                    message: rejection.message().to_string(),
                })
            }
        }
    }

    async fn persist_revision_chunks(
        &self,
        state: &AppState,
        revision_id: Uuid,
        text: &str,
    ) -> Result<(), ApiError> {
        let chunks = split_text_into_chunks_with_profile(text, ChunkingProfile::default());
        let mut next_search_char = 0usize;
        for (chunk_index, chunk_text) in chunks.iter().enumerate() {
            let (start_offset, end_offset) =
                locate_chunk_offsets(text, chunk_text, next_search_char);
            next_search_char = end_offset;
            let text_checksum = sha256_hex(chunk_text);
            let _ = content_repository::create_chunk(
                &state.persistence.postgres,
                &content_repository::NewContentChunk {
                    revision_id,
                    chunk_index: i32::try_from(chunk_index).unwrap_or(i32::MAX),
                    start_offset: i32::try_from(start_offset).unwrap_or(i32::MAX),
                    end_offset: i32::try_from(end_offset).unwrap_or(i32::MAX),
                    token_count: Some(
                        i32::try_from(chunk_text.split_whitespace().count()).unwrap_or(i32::MAX),
                    ),
                    normalized_text: chunk_text,
                    text_checksum: &text_checksum,
                },
            )
            .await
            .map_err(|_| ApiError::Internal)?;
        }
        Ok(())
    }

    async fn resolve_document_state(
        &self,
        auth: &AuthContext,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ResolvedDocumentState, ApiError> {
        let document =
            content_repository::get_document_by_id(&state.persistence.postgres, document_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, document.library_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("library", document.library_id))?;
        authorize_library_discovery(auth, library.workspace_id, library.id)?;
        let head = content_repository::get_document_head(&state.persistence.postgres, document.id)
            .await
            .map_err(|_| ApiError::Internal)?;

        let Some(head) = head else {
            return Ok(ResolvedDocumentState {
                document,
                library,
                latest_revision_id: None,
                readability_state: McpReadabilityState::Unavailable,
                status_reason: Some("document has no readable revision yet".into()),
                content: None,
            });
        };

        let latest_revision_id = head.readable_revision_id.or(head.active_revision_id);
        let extract_content = if let Some(readable_revision_id) = head.readable_revision_id {
            extract_repository::get_extract_content_by_revision_id(
                &state.persistence.postgres,
                readable_revision_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
        } else {
            None
        };
        let (readability_state, status_reason, content) = match extract_content {
            Some(extract)
                if extract.extract_state == "ready"
                    && extract
                        .normalized_text
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty()) =>
            {
                (McpReadabilityState::Readable, None, extract.normalized_text)
            }
            Some(extract) if extract.extract_state == "failed" => (
                McpReadabilityState::Failed,
                Some("latest readable revision extraction failed".to_string()),
                None,
            ),
            _ if head.active_revision_id.is_some() => (
                McpReadabilityState::Processing,
                Some("latest revision is still being extracted".to_string()),
                None,
            ),
            _ => (
                McpReadabilityState::Unavailable,
                Some("document has no readable revision yet".to_string()),
                None,
            ),
        };
        Ok(ResolvedDocumentState {
            document,
            library,
            latest_revision_id,
            readability_state,
            status_reason,
            content,
        })
    }

    async fn resolve_search_libraries(
        &self,
        auth: &AuthContext,
        state: &AppState,
        requested_library_ids: Option<&[Uuid]>,
    ) -> Result<Vec<VisibleLibraryContext>, ApiError> {
        if let Some(library_ids) = requested_library_ids {
            if library_ids.is_empty() {
                return Err(ApiError::invalid_mcp_tool_call(
                    "libraryIds must not be empty when provided",
                ));
            }
            let mut items = Vec::with_capacity(library_ids.len());
            for library_id in library_ids {
                let library = crate::interfaces::http::authorization::load_library_and_authorize(
                    auth,
                    state,
                    *library_id,
                    POLICY_MCP_MEMORY_READ,
                )
                .await?;
                items.push(self.describe_library(auth, state, library).await?);
            }
            return Ok(items);
        }

        let libraries = self.load_visible_library_contexts(auth, state, None).await?;
        Ok(libraries
            .into_iter()
            .filter(|item| {
                auth.has_library_permission(
                    item.library.workspace_id,
                    item.library.id,
                    POLICY_MCP_MEMORY_READ,
                )
            })
            .collect())
    }

    async fn load_visible_workspace_rows(
        &self,
        auth: &AuthContext,
        state: &AppState,
    ) -> Result<Vec<CatalogWorkspaceRow>, ApiError> {
        let rows = catalog_repository::list_workspaces(&state.persistence.postgres)
            .await
            .map_err(|_| ApiError::Internal)?;

        Ok(rows
            .into_iter()
            .filter(|row| authorize_workspace_discovery(auth, row.id).is_ok())
            .collect())
    }

    async fn load_visible_library_contexts(
        &self,
        auth: &AuthContext,
        state: &AppState,
        workspace_filter: Option<Uuid>,
    ) -> Result<Vec<VisibleLibraryContext>, ApiError> {
        let workspace_ids = if let Some(workspace_id) = workspace_filter {
            authorize_workspace_discovery(auth, workspace_id)?;
            vec![workspace_id]
        } else {
            self.load_visible_workspace_rows(auth, state)
                .await?
                .into_iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>()
        };

        let mut libraries = Vec::new();
        for workspace_id in workspace_ids {
            let rows =
                catalog_repository::list_libraries(&state.persistence.postgres, Some(workspace_id))
                    .await
                    .map_err(|_| ApiError::Internal)?;
            for library in rows {
                if authorize_library_discovery(auth, workspace_id, library.id).is_ok() {
                    libraries.push(self.describe_library(auth, state, library).await?);
                }
            }
        }
        Ok(libraries)
    }

    async fn describe_library(
        &self,
        auth: &AuthContext,
        state: &AppState,
        library: CatalogLibraryRow,
    ) -> Result<VisibleLibraryContext, ApiError> {
        let supports_search =
            auth.has_library_permission(library.workspace_id, library.id, POLICY_MCP_MEMORY_READ);
        let supports_write =
            auth.has_library_permission(library.workspace_id, library.id, POLICY_LIBRARY_WRITE);
        let documents =
            content_repository::list_documents_by_library(&state.persistence.postgres, library.id)
                .await
                .map_err(|_| ApiError::Internal)?;
        let mut document_count = 0usize;
        let mut readable_document_count = 0usize;
        let mut processing_document_count = 0usize;
        for document in documents {
            if document.document_state == "deleted" {
                continue;
            }
            document_count += 1;
            let Some(head) =
                content_repository::get_document_head(&state.persistence.postgres, document.id)
                    .await
                    .map_err(|_| ApiError::Internal)?
            else {
                continue;
            };
            if let Some(readable_revision_id) = head.readable_revision_id {
                let extract = extract_repository::get_extract_content_by_revision_id(
                    &state.persistence.postgres,
                    readable_revision_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?;
                if extract.as_ref().and_then(|row| row.normalized_text.as_deref()).is_some() {
                    readable_document_count += 1;
                    continue;
                }
            }
            if head.active_revision_id.is_some() {
                processing_document_count += 1;
            }
        }
        let descriptor = McpLibraryDescriptor {
            library_id: library.id,
            workspace_id: library.workspace_id,
            slug: library.slug.clone(),
            name: library.display_name.trim().to_string(),
            description: library.description.clone(),
            document_count,
            readable_document_count,
            processing_document_count,
            failed_document_count: document_count
                .saturating_sub(readable_document_count)
                .saturating_sub(processing_document_count),
            supports_search,
            supports_read: auth.has_document_or_library_read_scope_for_library(
                library.workspace_id,
                library.id,
                POLICY_MCP_MEMORY_READ,
            ),
            supports_write,
        };
        Ok(VisibleLibraryContext { library, descriptor })
    }

    async fn resolve_mutation_receipt(
        &self,
        state: &AppState,
        auth: &AuthContext,
        row: content_repository::ContentMutationRow,
    ) -> Result<McpMutationReceipt, ApiError> {
        let items = content_repository::list_mutation_items(&state.persistence.postgres, row.id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let mut document_id = items.iter().find_map(|item| item.document_id);
        if document_id.is_none()
            && let Some(revision_id) = items.iter().find_map(|item| item.result_revision_id)
        {
            document_id =
                content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .map(|revision| revision.document_id);
        }

        if let Some(document_id) = document_id {
            let document =
                content_repository::get_document_by_id(&state.persistence.postgres, document_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
            authorize_library_discovery(auth, document.workspace_id, document.library_id)?;
            authorize_document_permission(
                auth,
                document.workspace_id,
                document.library_id,
                document.id,
                POLICY_DOCUMENTS_WRITE,
            )?;
        } else {
            authorize_library_permission(
                auth,
                row.workspace_id,
                row.library_id,
                POLICY_LIBRARY_WRITE,
            )?;
        }

        let mut status = map_content_mutation_status_to_receipt_status(&row.mutation_state);
        let mut failure_kind = row.failure_code.clone().or(row.conflict_code.clone());
        let last_status_at = row.completed_at.unwrap_or(row.requested_at);

        if matches!(status, McpMutationReceiptStatus::Ready)
            && let Some(document_id) = document_id
            && let Some(result_revision_id) = items.iter().find_map(|item| item.result_revision_id)
        {
            let head =
                content_repository::get_document_head(&state.persistence.postgres, document_id)
                    .await
                    .map_err(|_| ApiError::Internal)?;
            let current_revision_id =
                head.as_ref().and_then(|row| row.active_revision_id.or(row.readable_revision_id));
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
                if let Some(attempt) =
                    attempts.into_iter().max_by_key(|attempt| attempt.attempt_number)
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

    fn normalize_read_request(
        &self,
        auth: &AuthContext,
        request: McpReadDocumentRequest,
    ) -> Result<NormalizedReadRequest, ApiError> {
        if let Some(token) = request.continuation_token.as_deref() {
            let payload = self.decode_continuation_token(auth, token)?;
            return Ok(NormalizedReadRequest {
                document_id: payload.document_id,
                read_mode: payload.read_mode,
                start_offset: payload.next_offset,
                window_chars: payload.window_chars,
            });
        }

        let document_id = request
            .document_id
            .ok_or_else(|| ApiError::invalid_mcp_tool_call("documentId is required"))?;
        let read_mode = request.mode.unwrap_or(McpReadMode::Full);
        let window_chars = request
            .length
            .unwrap_or(self.settings.default_read_window_chars)
            .clamp(1, self.settings.max_read_window_chars);

        Ok(NormalizedReadRequest {
            document_id,
            read_mode,
            start_offset: request.start_offset.unwrap_or(0),
            window_chars,
        })
    }

    fn encode_continuation_token(
        &self,
        auth: &AuthContext,
        document_id: Uuid,
        run_id: Uuid,
        latest_revision_id: Option<Uuid>,
        next_offset: usize,
        window_chars: usize,
        read_mode: McpReadMode,
    ) -> String {
        let proof =
            continuation_proof(auth.token_id, document_id, run_id, next_offset, window_chars);
        let payload = McpContinuationPayload {
            document_id,
            run_id,
            latest_revision_id,
            next_offset,
            window_chars,
            read_mode,
            proof,
        };
        let json = serde_json::to_vec(&payload).unwrap_or_default();
        URL_SAFE_NO_PAD.encode(json)
    }

    fn decode_continuation_token(
        &self,
        auth: &AuthContext,
        token: &str,
    ) -> Result<McpContinuationPayload, ApiError> {
        let decoded = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| ApiError::invalid_continuation_token("invalid continuation token"))?;
        let payload: McpContinuationPayload = serde_json::from_slice(&decoded)
            .map_err(|_| ApiError::invalid_continuation_token("invalid continuation token"))?;
        let expected = continuation_proof(
            auth.token_id,
            payload.document_id,
            payload.run_id,
            payload.next_offset,
            payload.window_chars,
        );
        if payload.proof != expected {
            return Err(ApiError::invalid_continuation_token("invalid continuation token"));
        }
        Ok(payload)
    }

    fn map_audit_row(
        &self,
        row: repositories::McpAuditEventRow,
    ) -> Result<McpAuditEvent, ApiError> {
        Ok(McpAuditEvent {
            audit_event_id: row.id,
            request_id: row.request_id,
            token_id: row.token_id,
            token_kind: row.token_kind,
            action_kind: parse_audit_action_kind(&row.action_kind)?,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            document_id: row.document_id,
            status: parse_audit_status(&row.status)?,
            error_kind: row.error_kind,
            created_at: row.created_at,
        })
    }
}

#[derive(Debug, Clone)]
struct NormalizedReadRequest {
    document_id: Uuid,
    read_mode: McpReadMode,
    start_offset: usize,
    window_chars: usize,
}

fn continuation_proof(
    token_id: Uuid,
    document_id: Uuid,
    run_id: Uuid,
    next_offset: usize,
    window_chars: usize,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token_id.as_bytes());
    hasher.update(document_id.as_bytes());
    hasher.update(run_id.as_bytes());
    hasher.update(next_offset.to_string().as_bytes());
    hasher.update(window_chars.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

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

fn operation_kind_key(operation_kind: &McpMutationOperationKind) -> &'static str {
    match operation_kind {
        McpMutationOperationKind::Upload => "upload",
        McpMutationOperationKind::Append => "append",
        McpMutationOperationKind::Replace => "replace",
    }
}

fn audit_action_kind_key(action_kind: &McpAuditActionKind) -> &'static str {
    match action_kind {
        McpAuditActionKind::CapabilitySnapshot => "capability_snapshot",
        McpAuditActionKind::ListWorkspaces => "list_workspaces",
        McpAuditActionKind::ListLibraries => "list_libraries",
        McpAuditActionKind::SearchDocuments => "search_documents",
        McpAuditActionKind::ReadDocument => "read_document",
        McpAuditActionKind::CreateWorkspace => "create_workspace",
        McpAuditActionKind::CreateLibrary => "create_library",
        McpAuditActionKind::UploadDocuments => "upload_documents",
        McpAuditActionKind::UpdateDocument => "update_document",
        McpAuditActionKind::GetMutationStatus => "get_mutation_status",
        McpAuditActionKind::ReviewAudit => "review_audit",
    }
}

fn audit_status_key(status: &McpAuditStatus) -> &'static str {
    match status {
        McpAuditStatus::Pending => "pending",
        McpAuditStatus::Succeeded => "succeeded",
        McpAuditStatus::Rejected => "rejected",
        McpAuditStatus::Failed => "failed",
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

fn parse_audit_action_kind(value: &str) -> Result<McpAuditActionKind, ApiError> {
    match value {
        "capability_snapshot" => Ok(McpAuditActionKind::CapabilitySnapshot),
        "list_workspaces" => Ok(McpAuditActionKind::ListWorkspaces),
        "list_libraries" => Ok(McpAuditActionKind::ListLibraries),
        "search_documents" => Ok(McpAuditActionKind::SearchDocuments),
        "read_document" => Ok(McpAuditActionKind::ReadDocument),
        "create_workspace" => Ok(McpAuditActionKind::CreateWorkspace),
        "create_library" => Ok(McpAuditActionKind::CreateLibrary),
        "upload_documents" => Ok(McpAuditActionKind::UploadDocuments),
        "update_document" => Ok(McpAuditActionKind::UpdateDocument),
        "get_mutation_status" => Ok(McpAuditActionKind::GetMutationStatus),
        "review_audit" => Ok(McpAuditActionKind::ReviewAudit),
        _ => Err(ApiError::Internal),
    }
}

fn parse_audit_status(value: &str) -> Result<McpAuditStatus, ApiError> {
    match value {
        "pending" => Ok(McpAuditStatus::Pending),
        "succeeded" => Ok(McpAuditStatus::Succeeded),
        "rejected" => Ok(McpAuditStatus::Rejected),
        "failed" => Ok(McpAuditStatus::Failed),
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

fn char_slice(text: &str, start_offset: usize, window_chars: usize) -> String {
    text.chars().skip(start_offset).take(window_chars).collect()
}

fn validate_extraction_plan(
    file_name: &str,
    mime_type: Option<&str>,
    file_size_bytes: u64,
    extraction_plan: &FileExtractionPlan,
) -> Result<(), UploadAdmissionError> {
    if extraction_plan.file_kind == UploadFileKind::TextLike
        && extraction_plan.extracted_text.as_deref().is_some_and(|text| text.trim().is_empty())
    {
        return Err(UploadAdmissionError::from_file_extract_error(
            file_name,
            mime_type,
            file_size_bytes,
            FileExtractError::ExtractionFailed {
                file_kind: UploadFileKind::TextLike,
                message: format!("uploaded file {file_name} is empty"),
            },
        ));
    }

    Ok(())
}

fn payload_source_uri(payload_identity: &str) -> String {
    format!("mcp://payload/{payload_identity}")
}

fn payload_identity_from_source_uri(source_uri: Option<&str>) -> Option<String> {
    source_uri.and_then(|value| value.strip_prefix("mcp://payload/")).map(ToString::to_string)
}

fn infer_revision_mime_type(
    requested_mime_type: Option<&str>,
    file_name: Option<&str>,
    fallback_kind: &str,
) -> String {
    if let Some(mime_type) = requested_mime_type.map(str::trim).filter(|value| !value.is_empty()) {
        return mime_type.to_string();
    }
    match file_name.and_then(file_extension) {
        Some(extension) if extension == "pdf" => "application/pdf".to_string(),
        Some(extension) if extension == "docx" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
        }
        Some(extension) if extension == "md" => "text/markdown".to_string(),
        Some(extension) if extension == "txt" => "text/plain".to_string(),
        Some(extension) if extension == "json" => "application/json".to_string(),
        _ if fallback_kind == "append" => "text/plain".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn file_extension(file_name: &str) -> Option<String> {
    let (_, extension) = file_name.rsplit_once('.')?;
    Some(extension.trim().to_ascii_lowercase())
}

fn merge_appended_text(current_content: &str, appended_text: &str) -> String {
    let current = current_content.trim_end();
    let append = appended_text.trim();
    if current.is_empty() {
        append.to_string()
    } else if append.is_empty() {
        current.to_string()
    } else {
        format!("{current}\n\n{append}")
    }
}

fn locate_chunk_offsets(text: &str, chunk_text: &str, next_search_char: usize) -> (usize, usize) {
    let start_byte = char_offset_to_byte_index(text, next_search_char);
    if let Some(relative_start) = text[start_byte..].find(chunk_text) {
        let chunk_start_byte = start_byte + relative_start;
        let chunk_end_byte = chunk_start_byte + chunk_text.len();
        let chunk_start = text[..chunk_start_byte].chars().count();
        let chunk_end = text[..chunk_end_byte].chars().count();
        return (chunk_start, chunk_end);
    }

    let chunk_start = next_search_char;
    let chunk_end = chunk_start.saturating_add(chunk_text.chars().count());
    (chunk_start, chunk_end)
}

fn char_offset_to_byte_index(text: &str, char_offset: usize) -> usize {
    text.char_indices().nth(char_offset).map_or(text.len(), |(byte_index, _)| byte_index)
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn preview_hit(text: &str, query_lower: &str) -> Option<(String, usize, usize, f64)> {
    let text_lower = text.to_ascii_lowercase();
    let start = text_lower.find(query_lower)?;
    let end = start.saturating_add(query_lower.len());
    let excerpt_start = start.saturating_sub(80);
    let excerpt_end = (end + 160).min(text.len());
    let excerpt = text[excerpt_start..excerpt_end].trim().to_string();
    let score = 1.0f64 / (1.0 + start as f64);
    Some((excerpt, excerpt_start, excerpt_end, score))
}
