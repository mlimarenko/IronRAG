use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{
        ContentChunk, ContentDocument, ContentDocumentHead, ContentDocumentSummary,
        ContentMutation, ContentMutationItem, ContentRevision,
    },
    infra::repositories::content_repository::{
        self, NewContentDocument, NewContentDocumentHead, NewContentMutation,
        NewContentMutationItem, NewContentRevision,
    },
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct CreateDocumentCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct CreateRevisionCommand {
    pub document_id: Uuid,
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub storage_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct PromoteHeadCommand {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_mutation_id: Option<Uuid>,
    pub latest_successful_attempt_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct AcceptMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateMutationItemCommand {
    pub mutation_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateMutationCommand {
    pub mutation_id: Uuid,
    pub mutation_state: String,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failure_code: Option<String>,
    pub conflict_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateMutationItemCommand {
    pub item_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: String,
    pub message: Option<String>,
}

#[derive(Clone, Default)]
pub struct ContentService;

impl ContentService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn list_documents(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<ContentDocumentSummary>, ApiError> {
        let documents =
            content_repository::list_documents_by_library(&state.persistence.postgres, library_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        let mut summaries = Vec::with_capacity(documents.len());
        for row in documents {
            summaries.push(self.build_document_summary(state, row).await?);
        }
        Ok(summaries)
    }

    pub async fn get_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentDocumentSummary, ApiError> {
        let row = content_repository::get_document_by_id(&state.persistence.postgres, document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        self.build_document_summary(state, row).await
    }

    pub async fn get_document_head(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Option<ContentDocumentHead>, ApiError> {
        let row = content_repository::get_document_head(&state.persistence.postgres, document_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(row.map(map_document_head_row))
    }

    pub async fn list_revisions(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Vec<ContentRevision>, ApiError> {
        let rows = content_repository::list_revisions_by_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_revision_row).collect())
    }

    pub async fn list_chunks(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<ContentChunk>, ApiError> {
        let rows =
            content_repository::list_chunks_by_revision(&state.persistence.postgres, revision_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_chunk_row).collect())
    }

    pub async fn create_document(
        &self,
        state: &AppState,
        command: CreateDocumentCommand,
    ) -> Result<ContentDocument, ApiError> {
        let external_key = command
            .external_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| Uuid::now_v7().to_string());
        let row = content_repository::create_document(
            &state.persistence.postgres,
            &NewContentDocument {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                external_key: &external_key,
                document_state: "active",
                created_by_principal_id: command.created_by_principal_id,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let _ = content_repository::upsert_document_head(
            &state.persistence.postgres,
            &NewContentDocumentHead {
                document_id: row.id,
                active_revision_id: None,
                readable_revision_id: None,
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_document_row(row))
    }

    pub async fn create_revision(
        &self,
        state: &AppState,
        command: CreateRevisionCommand,
    ) -> Result<ContentRevision, ApiError> {
        let document = content_repository::get_document_by_id(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        let latest = content_repository::get_latest_revision_for_document(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let next_revision_number = latest.as_ref().map_or(1, |row| row.revision_number + 1);
        let row = content_repository::create_revision(
            &state.persistence.postgres,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                revision_number: next_revision_number,
                parent_revision_id: latest.as_ref().map(|row| row.id),
                content_source_kind: &command.content_source_kind,
                checksum: &command.checksum,
                mime_type: &command.mime_type,
                byte_size: command.byte_size,
                title: command.title.as_deref(),
                language_code: command.language_code.as_deref(),
                source_uri: command.source_uri.as_deref(),
                storage_key: command.storage_key.as_deref(),
                created_by_principal_id: command.created_by_principal_id,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_revision_row(row))
    }

    pub async fn append_revision(
        &self,
        state: &AppState,
        command: CreateRevisionCommand,
    ) -> Result<ContentRevision, ApiError> {
        self.create_revision(state, command).await
    }

    pub async fn replace_revision(
        &self,
        state: &AppState,
        command: CreateRevisionCommand,
    ) -> Result<ContentRevision, ApiError> {
        self.create_revision(state, command).await
    }

    pub async fn delete_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentDocument, ApiError> {
        let document = content_repository::update_document_state(
            &state.persistence.postgres,
            document_id,
            "deleted",
            Some(Utc::now()),
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;

        let head = content_repository::get_document_head(&state.persistence.postgres, document_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let readable_revision_id = head.as_ref().and_then(|row| row.readable_revision_id);
        let latest_mutation_id = head.as_ref().and_then(|row| row.latest_mutation_id);
        let latest_successful_attempt_id =
            head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let _ = content_repository::upsert_document_head(
            &state.persistence.postgres,
            &NewContentDocumentHead {
                document_id,
                active_revision_id: None,
                readable_revision_id,
                latest_mutation_id,
                latest_successful_attempt_id,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        Ok(map_document_row(document))
    }

    pub async fn promote_document_head(
        &self,
        state: &AppState,
        command: PromoteHeadCommand,
    ) -> Result<ContentDocumentHead, ApiError> {
        if let Some(active_revision_id) = command.active_revision_id {
            content_repository::get_revision_by_id(&state.persistence.postgres, active_revision_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("revision", active_revision_id))?;
        }
        if let Some(readable_revision_id) = command.readable_revision_id {
            content_repository::get_revision_by_id(
                &state.persistence.postgres,
                readable_revision_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("revision", readable_revision_id))?;
        }

        let row = content_repository::upsert_document_head(
            &state.persistence.postgres,
            &NewContentDocumentHead {
                document_id: command.document_id,
                active_revision_id: command.active_revision_id,
                readable_revision_id: command.readable_revision_id,
                latest_mutation_id: command.latest_mutation_id,
                latest_successful_attempt_id: command.latest_successful_attempt_id,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_document_head_row(row))
    }

    pub async fn accept_mutation(
        &self,
        state: &AppState,
        command: AcceptMutationCommand,
    ) -> Result<ContentMutation, ApiError> {
        if let (Some(principal_id), Some(idempotency_key)) = (
            command.requested_by_principal_id,
            command.idempotency_key.as_deref().map(str::trim).filter(|value| !value.is_empty()),
        ) {
            if let Some(existing) = content_repository::find_mutation_by_idempotency(
                &state.persistence.postgres,
                principal_id,
                &command.request_surface,
                idempotency_key,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            {
                return Ok(map_mutation_row(existing));
            }
        }

        let row = content_repository::create_mutation(
            &state.persistence.postgres,
            &NewContentMutation {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                operation_kind: &command.operation_kind,
                requested_by_principal_id: command.requested_by_principal_id,
                request_surface: &command.request_surface,
                idempotency_key: command.idempotency_key.as_deref(),
                mutation_state: "accepted",
                failure_code: None,
                conflict_code: None,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_mutation_row(row))
    }

    pub async fn list_mutations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<ContentMutation>, ApiError> {
        let rows =
            content_repository::list_mutations_by_library(&state.persistence.postgres, library_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_mutation_row).collect())
    }

    pub async fn get_mutation(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<ContentMutation, ApiError> {
        let row = content_repository::get_mutation_by_id(&state.persistence.postgres, mutation_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("mutation", mutation_id))?;
        Ok(map_mutation_row(row))
    }

    pub async fn list_mutation_items(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<Vec<ContentMutationItem>, ApiError> {
        let rows =
            content_repository::list_mutation_items(&state.persistence.postgres, mutation_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_mutation_item_row).collect())
    }

    pub async fn create_mutation_item(
        &self,
        state: &AppState,
        command: CreateMutationItemCommand,
    ) -> Result<ContentMutationItem, ApiError> {
        let row = content_repository::create_mutation_item(
            &state.persistence.postgres,
            &NewContentMutationItem {
                mutation_id: command.mutation_id,
                document_id: command.document_id,
                base_revision_id: command.base_revision_id,
                result_revision_id: command.result_revision_id,
                item_state: &command.item_state,
                message: command.message.as_deref(),
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_mutation_item_row(row))
    }

    pub async fn update_mutation(
        &self,
        state: &AppState,
        command: UpdateMutationCommand,
    ) -> Result<ContentMutation, ApiError> {
        let row = content_repository::update_mutation_status(
            &state.persistence.postgres,
            command.mutation_id,
            &command.mutation_state,
            command.completed_at,
            command.failure_code.as_deref(),
            command.conflict_code.as_deref(),
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("mutation", command.mutation_id))?;
        Ok(map_mutation_row(row))
    }

    pub async fn update_mutation_item(
        &self,
        state: &AppState,
        command: UpdateMutationItemCommand,
    ) -> Result<ContentMutationItem, ApiError> {
        let row = content_repository::update_mutation_item(
            &state.persistence.postgres,
            command.item_id,
            command.document_id,
            command.base_revision_id,
            command.result_revision_id,
            &command.item_state,
            command.message.as_deref(),
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("mutation_item", command.item_id))?;
        Ok(map_mutation_item_row(row))
    }

    async fn build_document_summary(
        &self,
        state: &AppState,
        document_row: content_repository::ContentDocumentRow,
    ) -> Result<ContentDocumentSummary, ApiError> {
        let head =
            content_repository::get_document_head(&state.persistence.postgres, document_row.id)
                .await
                .map_err(|_| ApiError::Internal)?;
        let active_revision =
            if let Some(revision_id) = head.as_ref().and_then(|row| row.active_revision_id) {
                content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .map(map_revision_row)
            } else {
                None
            };

        Ok(ContentDocumentSummary {
            document: map_document_row(document_row),
            head: head.map(map_document_head_row),
            active_revision,
        })
    }
}

fn map_document_row(row: content_repository::ContentDocumentRow) -> ContentDocument {
    ContentDocument {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key,
        document_state: row.document_state,
        created_at: row.created_at,
    }
}

fn map_document_head_row(row: content_repository::ContentDocumentHeadRow) -> ContentDocumentHead {
    ContentDocumentHead {
        document_id: row.document_id,
        active_revision_id: row.active_revision_id,
        readable_revision_id: row.readable_revision_id,
        latest_mutation_id: row.latest_mutation_id,
        latest_successful_attempt_id: row.latest_successful_attempt_id,
        head_updated_at: row.head_updated_at,
    }
}

fn map_revision_row(row: content_repository::ContentRevisionRow) -> ContentRevision {
    ContentRevision {
        id: row.id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        revision_number: row.revision_number,
        parent_revision_id: row.parent_revision_id,
        content_source_kind: row.content_source_kind,
        checksum: row.checksum,
        mime_type: row.mime_type,
        byte_size: row.byte_size,
        title: row.title,
        language_code: row.language_code,
        source_uri: row.source_uri,
        storage_key: row.storage_key,
        created_by_principal_id: row.created_by_principal_id,
        created_at: row.created_at,
    }
}

fn map_chunk_row(row: content_repository::ContentChunkRow) -> ContentChunk {
    ContentChunk {
        id: row.id,
        revision_id: row.revision_id,
        chunk_index: row.chunk_index,
        start_offset: row.start_offset,
        end_offset: row.end_offset,
        token_count: row.token_count,
        normalized_text: row.normalized_text,
        text_checksum: row.text_checksum,
    }
}

fn map_mutation_row(row: content_repository::ContentMutationRow) -> ContentMutation {
    ContentMutation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        operation_kind: row.operation_kind,
        mutation_state: row.mutation_state,
        requested_at: row.requested_at,
        completed_at: row.completed_at,
        requested_by_principal_id: row.requested_by_principal_id,
        request_surface: row.request_surface,
        idempotency_key: row.idempotency_key,
        failure_code: row.failure_code,
        conflict_code: row.conflict_code,
    }
}

fn map_mutation_item_row(row: content_repository::ContentMutationItemRow) -> ContentMutationItem {
    ContentMutationItem {
        id: row.id,
        mutation_id: row.mutation_id,
        document_id: row.document_id,
        base_revision_id: row.base_revision_id,
        result_revision_id: row.result_revision_id,
        item_state: row.item_state,
        message: row.message,
    }
}
