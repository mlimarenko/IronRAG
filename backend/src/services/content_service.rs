use chrono::Utc;
use sha2::{Digest, Sha256};
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
    services::{
        extract_service::PersistExtractContentCommand,
        ingest_service::AdmitIngestJobCommand,
        ingest_service::{FinalizeAttemptCommand, LeaseAttemptCommand, RecordStageEventCommand},
        knowledge_service::{
            CreateKnowledgeChunkCommand, CreateKnowledgeDocumentCommand,
            CreateKnowledgeRevisionCommand, PromoteKnowledgeDocumentCommand,
        },
        ops_service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    },
    shared::{
        chunking::{ChunkingProfile, split_text_into_chunks_with_profile},
        file_extract::{
            FileExtractError, FileExtractionPlan, UploadAdmissionError, UploadFileKind,
            build_runtime_file_extraction_plan,
        },
    },
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

#[derive(Debug, Clone)]
pub struct RevisionAdmissionMetadata {
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub storage_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AdmitDocumentCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub idempotency_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub revision: Option<RevisionAdmissionMetadata>,
}

#[derive(Debug, Clone)]
pub struct AdmitMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub operation_kind: String,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub revision: Option<RevisionAdmissionMetadata>,
}

#[derive(Debug, Clone)]
pub struct UploadInlineDocumentCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub file_name: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct AppendInlineMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub appended_text: String,
}

#[derive(Debug, Clone)]
pub struct ReplaceInlineMutationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub idempotency_key: Option<String>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub source_identity: Option<String>,
    pub file_name: String,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ContentMutationAdmission {
    pub mutation: ContentMutation,
    pub items: Vec<ContentMutationItem>,
    pub job_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct CreateDocumentAdmission {
    pub document: ContentDocumentSummary,
    pub mutation: ContentMutationAdmission,
}

#[derive(Debug, Clone)]
struct InlineMutationContext {
    mutation_id: Uuid,
    job_id: Uuid,
    item_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
}

#[derive(Debug, Clone)]
struct AppendableDocumentContext {
    current_content: String,
    mime_type: String,
    title: Option<String>,
    language_code: Option<String>,
}

#[derive(Debug, Clone)]
struct InlineMutationFailure {
    failure_kind: String,
    message: String,
}

#[derive(Debug, Clone)]
struct PendingChunkInsert {
    chunk_index: i32,
    start_offset: i32,
    end_offset: i32,
    token_count: Option<i32>,
    normalized_text: String,
    text_checksum: String,
}

#[derive(Debug, Clone)]
enum InlineExtractOutcome {
    Ready(FileExtractionPlan),
    Failed(InlineMutationFailure),
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
        let document_ids = documents.iter().map(|row| row.id).collect::<Vec<_>>();
        let heads =
            content_repository::list_document_heads_by_document_ids(
                &state.persistence.postgres,
                &document_ids,
            )
            .await
            .map_err(|_| ApiError::Internal)?;
        let head_map = heads
            .into_iter()
            .map(|row| (row.document_id, row))
            .collect::<std::collections::HashMap<_, _>>();
        let active_revision_ids = head_map
            .values()
            .filter_map(|row| row.active_revision_id)
            .collect::<Vec<_>>();
        let active_revision_map = content_repository::list_revisions_by_ids(
            &state.persistence.postgres,
            &active_revision_ids,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<std::collections::HashMap<_, _>>();

        Ok(documents
            .into_iter()
            .map(|row| self.build_document_summary_from_parts(row, &head_map, &active_revision_map))
            .collect())
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
        let document = map_document_row(row);
        let _ = state
            .canonical_services
            .knowledge
            .create_document_shell(
                state,
                CreateKnowledgeDocumentCommand {
                    document_id: document.id,
                    workspace_id: document.workspace_id,
                    library_id: document.library_id,
                    external_key: document.external_key.clone(),
                    document_state: document.document_state.clone(),
                },
            )
            .await?;
        Ok(document)
    }

    pub async fn admit_document(
        &self,
        state: &AppState,
        command: AdmitDocumentCommand,
    ) -> Result<CreateDocumentAdmission, ApiError> {
        let document = self
            .create_document(
                state,
                CreateDocumentCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    external_key: command.external_key,
                    created_by_principal_id: command.created_by_principal_id,
                },
            )
            .await?;

        let mutation = self
            .accept_mutation(
                state,
                AcceptMutationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: "upload".to_string(),
                    requested_by_principal_id: command.created_by_principal_id,
                    request_surface: command.request_surface.clone(),
                    idempotency_key: command.idempotency_key.clone(),
                },
            )
            .await?;

        let async_operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: "content_mutation".to_string(),
                    surface_kind: "rest".to_string(),
                    requested_by_principal_id: command.created_by_principal_id,
                    status: "accepted".to_string(),
                    subject_kind: "content_mutation".to_string(),
                    subject_id: Some(mutation.id),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;

        let (items, job_id, async_operation_id) = if let Some(revision) = command.revision {
            let revision = self
                .create_revision_from_metadata(
                    state,
                    document.id,
                    command.created_by_principal_id,
                    revision,
                )
                .await?;
            let item = self
                .create_mutation_item(
                    state,
                    CreateMutationItemCommand {
                        mutation_id: mutation.id,
                        document_id: Some(document.id),
                        base_revision_id: None,
                        result_revision_id: Some(revision.id),
                        item_state: "pending".to_string(),
                        message: Some(
                            "document revision accepted and queued for ingest".to_string(),
                        ),
                    },
                )
                .await?;
            let head = self.get_document_head(state, document.id).await?;
            let _ = self
                .promote_document_head(
                    state,
                    PromoteHeadCommand {
                        document_id: document.id,
                        active_revision_id: Some(revision.id),
                        readable_revision_id: head.and_then(|row| row.readable_revision_id),
                        latest_mutation_id: Some(mutation.id),
                        latest_successful_attempt_id: None,
                    },
                )
                .await?;
            let job = state
                .canonical_services
                .ingest
                .admit_job(
                    state,
                    AdmitIngestJobCommand {
                        workspace_id: command.workspace_id,
                        library_id: command.library_id,
                        mutation_id: Some(mutation.id),
                        connector_id: None,
                        async_operation_id: Some(async_operation.id),
                        knowledge_document_id: Some(document.id),
                        knowledge_revision_id: Some(revision.id),
                        job_kind: "content_mutation".to_string(),
                        priority: 100,
                        dedupe_key: command.idempotency_key,
                        available_at: None,
                    },
                )
                .await?;
            (vec![item], Some(job.id), Some(async_operation.id))
        } else {
            let _ = self
                .promote_document_head(
                    state,
                    PromoteHeadCommand {
                        document_id: document.id,
                        active_revision_id: None,
                        readable_revision_id: None,
                        latest_mutation_id: Some(mutation.id),
                        latest_successful_attempt_id: None,
                    },
                )
                .await?;
            let _ = self
                .update_mutation(
                    state,
                    UpdateMutationCommand {
                        mutation_id: mutation.id,
                        mutation_state: "applied".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: None,
                        conflict_code: None,
                    },
                )
                .await?;
            let ready_operation = state
                .canonical_services
                .ops
                .update_async_operation(
                    state,
                    UpdateAsyncOperationCommand {
                        operation_id: async_operation.id,
                        status: "ready".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: None,
                    },
                )
                .await?;
            (Vec::new(), None, Some(ready_operation.id))
        };

        let document = self.get_document(state, document.id).await?;
        let mutation = self.get_mutation(state, mutation.id).await?;
        Ok(CreateDocumentAdmission {
            document,
            mutation: ContentMutationAdmission { mutation, items, job_id, async_operation_id },
        })
    }

    pub async fn upload_inline_document(
        &self,
        state: &AppState,
        command: UploadInlineDocumentCommand,
    ) -> Result<CreateDocumentAdmission, ApiError> {
        let file_name = command.file_name.trim().to_string();
        let title = command
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| file_name.clone());
        let admission = self
            .admit_document(
                state,
                AdmitDocumentCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    external_key: command.external_key,
                    idempotency_key: command.idempotency_key,
                    created_by_principal_id: command.requested_by_principal_id,
                    request_surface: command.request_surface.clone(),
                    revision: Some(RevisionAdmissionMetadata {
                        content_source_kind: "upload".to_string(),
                        checksum: format!("sha256:{}", sha256_hex_bytes(&command.file_bytes)),
                        mime_type: infer_inline_mime_type(
                            command.mime_type.as_deref(),
                            Some(&file_name),
                            "upload",
                        ),
                        byte_size: i64::try_from(command.file_bytes.len()).unwrap_or(i64::MAX),
                        title: Some(title),
                        language_code: None,
                        source_uri: Some(source_uri_for_inline_payload(
                            "upload",
                            command.source_identity.as_deref(),
                            Some(&file_name),
                        )),
                        storage_key: None,
                    }),
                },
            )
            .await?;
        let mutation = self
            .materialize_inline_file_mutation(
                state,
                &admission.mutation,
                &file_name,
                command.mime_type.as_deref(),
                &command.file_bytes,
            )
            .await?;
        let document = self.get_document(state, admission.document.document.id).await?;
        Ok(CreateDocumentAdmission { document, mutation })
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
        let revision = map_revision_row(row);
        let _ = state
            .canonical_services
            .knowledge
            .write_revision(
                state,
                CreateKnowledgeRevisionCommand {
                    revision_id: revision.id,
                    workspace_id: revision.workspace_id,
                    library_id: revision.library_id,
                    document_id: revision.document_id,
                    revision_number: i64::from(revision.revision_number),
                    revision_state: "accepted".to_string(),
                    revision_kind: revision.content_source_kind.clone(),
                    storage_ref: revision.storage_key.clone(),
                    source_uri: revision.source_uri.clone(),
                    mime_type: revision.mime_type.clone(),
                    checksum: revision.checksum.clone(),
                    byte_size: revision.byte_size,
                    title: revision.title.clone(),
                    normalized_text: None,
                    text_checksum: None,
                    text_state: "accepted".to_string(),
                    vector_state: "accepted".to_string(),
                    graph_state: "accepted".to_string(),
                    text_readable_at: None,
                    vector_ready_at: None,
                    graph_ready_at: None,
                    superseded_by_revision_id: None,
                },
            )
            .await?;
        Ok(revision)
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

    pub async fn admit_mutation(
        &self,
        state: &AppState,
        command: AdmitMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        self.ensure_document_accepts_new_mutation(state, command.document_id).await?;
        let current_head = self.get_document_head(state, command.document_id).await?;
        let base_revision_id = current_head
            .as_ref()
            .and_then(|row| row.active_revision_id.or(row.readable_revision_id));

        let mutation = self
            .accept_mutation(
                state,
                AcceptMutationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: command.operation_kind.clone(),
                    requested_by_principal_id: command.requested_by_principal_id,
                    request_surface: command.request_surface.clone(),
                    idempotency_key: command.idempotency_key.clone(),
                },
            )
            .await?;
        let async_operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: "content_mutation".to_string(),
                    surface_kind: "rest".to_string(),
                    requested_by_principal_id: command.requested_by_principal_id,
                    status: if command.operation_kind == "delete" {
                        "ready".to_string()
                    } else {
                        "accepted".to_string()
                    },
                    subject_kind: "content_mutation".to_string(),
                    subject_id: Some(mutation.id),
                    completed_at: (command.operation_kind == "delete").then(Utc::now),
                    failure_code: None,
                },
            )
            .await?;

        if command.operation_kind == "delete" {
            let item = self
                .create_mutation_item(
                    state,
                    CreateMutationItemCommand {
                        mutation_id: mutation.id,
                        document_id: Some(command.document_id),
                        base_revision_id,
                        result_revision_id: None,
                        item_state: "pending".to_string(),
                        message: Some("document deletion accepted".to_string()),
                    },
                )
                .await?;
            let _ = self.delete_document(state, command.document_id).await?;
            let item = self
                .update_mutation_item(
                    state,
                    UpdateMutationItemCommand {
                        item_id: item.id,
                        document_id: Some(command.document_id),
                        base_revision_id,
                        result_revision_id: None,
                        item_state: "applied".to_string(),
                        message: Some("document deleted".to_string()),
                    },
                )
                .await?;
            let mutation = self
                .update_mutation(
                    state,
                    UpdateMutationCommand {
                        mutation_id: mutation.id,
                        mutation_state: "applied".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: None,
                        conflict_code: None,
                    },
                )
                .await?;
            return Ok(ContentMutationAdmission {
                mutation,
                items: vec![item],
                job_id: None,
                async_operation_id: Some(async_operation.id),
            });
        }

        let revision = self
            .create_revision_from_metadata(
                state,
                command.document_id,
                command.requested_by_principal_id,
                command.revision.ok_or_else(|| {
                    ApiError::BadRequest(
                        "revision metadata is required for non-delete document mutations"
                            .to_string(),
                    )
                })?,
            )
            .await?;

        let item = self
            .create_mutation_item(
                state,
                CreateMutationItemCommand {
                    mutation_id: mutation.id,
                    document_id: Some(command.document_id),
                    base_revision_id,
                    result_revision_id: Some(revision.id),
                    item_state: "pending".to_string(),
                    message: Some("revision accepted and queued for ingest".to_string()),
                },
            )
            .await?;
        let _ = self
            .promote_document_head(
                state,
                PromoteHeadCommand {
                    document_id: command.document_id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: current_head.and_then(|row| row.readable_revision_id),
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: None,
                },
            )
            .await?;
        let job = state
            .canonical_services
            .ingest
            .admit_job(
                state,
                AdmitIngestJobCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    mutation_id: Some(mutation.id),
                    connector_id: None,
                    async_operation_id: Some(async_operation.id),
                    knowledge_document_id: Some(command.document_id),
                    knowledge_revision_id: Some(revision.id),
                    job_kind: "content_mutation".to_string(),
                    priority: 100,
                    dedupe_key: command.idempotency_key,
                    available_at: None,
                },
            )
            .await?;
        Ok(ContentMutationAdmission {
            mutation,
            items: vec![item],
            job_id: Some(job.id),
            async_operation_id: Some(async_operation.id),
        })
    }

    pub async fn append_inline_mutation(
        &self,
        state: &AppState,
        command: AppendInlineMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let appendable = self.load_appendable_document_context(state, command.document_id).await?;
        let merged_text = merge_appended_text(&appendable.current_content, &command.appended_text);
        let admission = self
            .admit_mutation(
                state,
                AdmitMutationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    document_id: command.document_id,
                    operation_kind: "append".to_string(),
                    idempotency_key: command.idempotency_key,
                    requested_by_principal_id: command.requested_by_principal_id,
                    request_surface: command.request_surface,
                    revision: Some(RevisionAdmissionMetadata {
                        content_source_kind: "append".to_string(),
                        checksum: format!("sha256:{}", sha256_hex_bytes(merged_text.as_bytes())),
                        mime_type: appendable.mime_type,
                        byte_size: i64::try_from(merged_text.len()).unwrap_or(i64::MAX),
                        title: appendable.title,
                        language_code: appendable.language_code,
                        source_uri: Some(source_uri_for_inline_payload(
                            "append",
                            command.source_identity.as_deref(),
                            None,
                        )),
                        storage_key: None,
                    }),
                },
            )
            .await?;
        self.materialize_inline_text_mutation(state, &admission, merged_text).await
    }

    pub async fn replace_inline_mutation(
        &self,
        state: &AppState,
        command: ReplaceInlineMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let head = self.get_document_head(state, command.document_id).await?;
        let base_revision =
            match head.as_ref().and_then(|row| row.active_revision_id.or(row.readable_revision_id))
            {
                Some(revision_id) => {
                    content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                        .await
                        .map_err(|_| ApiError::Internal)?
                }
                None => None,
            };
        let admission = self
            .admit_mutation(
                state,
                AdmitMutationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    document_id: command.document_id,
                    operation_kind: "replace".to_string(),
                    idempotency_key: command.idempotency_key,
                    requested_by_principal_id: command.requested_by_principal_id,
                    request_surface: command.request_surface,
                    revision: Some(RevisionAdmissionMetadata {
                        content_source_kind: "replace".to_string(),
                        checksum: format!("sha256:{}", sha256_hex_bytes(&command.file_bytes)),
                        mime_type: infer_inline_mime_type(
                            command.mime_type.as_deref(),
                            Some(&command.file_name),
                            "replace",
                        ),
                        byte_size: i64::try_from(command.file_bytes.len()).unwrap_or(i64::MAX),
                        title: Some(
                            base_revision
                                .as_ref()
                                .and_then(|row| row.title.clone())
                                .filter(|value| !value.trim().is_empty())
                                .unwrap_or_else(|| command.file_name.clone()),
                        ),
                        language_code: base_revision.and_then(|row| row.language_code),
                        source_uri: Some(source_uri_for_inline_payload(
                            "replace",
                            command.source_identity.as_deref(),
                            Some(&command.file_name),
                        )),
                        storage_key: None,
                    }),
                },
            )
            .await?;
        self.materialize_inline_file_mutation(
            state,
            &admission,
            &command.file_name,
            command.mime_type.as_deref(),
            &command.file_bytes,
        )
        .await
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
        let _ = state
            .canonical_services
            .knowledge
            .promote_document(
                state,
                PromoteKnowledgeDocumentCommand {
                    document_id,
                    document_state: document.document_state.clone(),
                    active_revision_id: None,
                    readable_revision_id,
                    latest_revision_no: None,
                    deleted_at: None,
                },
            )
            .await?;

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
        let document = content_repository::get_document_by_id(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        let _ = state
            .canonical_services
            .knowledge
            .promote_document(
                state,
                PromoteKnowledgeDocumentCommand {
                    document_id: command.document_id,
                    document_state: document.document_state,
                    active_revision_id: command.active_revision_id,
                    readable_revision_id: command.readable_revision_id,
                    latest_revision_no: None,
                    deleted_at: None,
                },
            )
            .await?;
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

    pub async fn list_mutation_admissions(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> Result<Vec<ContentMutationAdmission>, ApiError> {
        let mutations = self.list_mutations(state, library_id).await?;
        let mutation_ids = mutations.iter().map(|mutation| mutation.id).collect::<Vec<_>>();
        let job_handles = state
            .canonical_services
            .ingest
            .list_job_handles_by_mutation_ids(state, workspace_id, library_id, &mutation_ids)
            .await?;

        let mut admissions = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            let items = self.list_mutation_items(state, mutation.id).await?;
            let job_handle =
                job_handles.iter().find(|handle| handle.job.mutation_id == Some(mutation.id));
            let async_operation_id = job_handle
                .and_then(|handle| handle.async_operation.as_ref().map(|operation| operation.id))
                .or_else(|| job_handle.and_then(|handle| handle.job.async_operation_id));
            admissions.push(ContentMutationAdmission {
                mutation,
                items,
                job_id: job_handle.map(|handle| handle.job.id),
                async_operation_id,
            });
        }
        Ok(admissions)
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

    pub async fn find_mutation_by_idempotency(
        &self,
        state: &AppState,
        principal_id: Uuid,
        request_surface: &str,
        idempotency_key: &str,
    ) -> Result<Option<ContentMutation>, ApiError> {
        let row = content_repository::find_mutation_by_idempotency(
            &state.persistence.postgres,
            principal_id,
            request_surface,
            idempotency_key,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(row.map(map_mutation_row))
    }

    pub async fn get_mutation_admission(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let mutation = self.get_mutation(state, mutation_id).await?;
        let items = self.list_mutation_items(state, mutation_id).await?;
        let job_handle = state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(state, mutation_id)
            .await?;
        let mut async_operation_id = job_handle
            .as_ref()
            .and_then(|handle| handle.async_operation.as_ref().map(|operation| operation.id))
            .or_else(|| job_handle.as_ref().and_then(|handle| handle.job.async_operation_id));
        if async_operation_id.is_none()
            && let Some(operation) = state
                .canonical_services
                .ops
                .get_latest_async_operation_by_subject(state, "content_mutation", mutation_id)
                .await?
        {
            async_operation_id = Some(operation.id);
        }
        Ok(ContentMutationAdmission {
            mutation,
            items,
            job_id: job_handle.as_ref().map(|handle| handle.job.id),
            async_operation_id,
        })
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

    async fn materialize_inline_text_mutation(
        &self,
        state: &AppState,
        admission: &ContentMutationAdmission,
        text: String,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let context = self.inline_mutation_context_from_admission(admission)?;
        let attempt = self.lease_inline_attempt(state, &context).await?;
        self.update_mutation(
            state,
            UpdateMutationCommand {
                mutation_id: context.mutation_id,
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
                    details_json: serde_json::json!({
                        "documentId": context.document_id,
                        "revisionId": context.revision_id,
                    }),
                },
            )
            .await?;
        state
            .canonical_services
            .extract
            .persist_extract_content(
                state,
                PersistExtractContentCommand {
                    revision_id: context.revision_id,
                    attempt_id: Some(attempt.id),
                    extract_state: "ready".to_string(),
                    normalized_text: Some(text.clone()),
                    text_checksum: Some(sha256_hex_text(&text)),
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
        self.persist_revision_chunks(state, context.revision_id, &text).await?;
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
                    details_json: serde_json::json!({ "revisionId": context.revision_id }),
                },
            )
            .await?;
        self.complete_successful_inline_mutation(state, &context, attempt.id).await
    }

    async fn materialize_inline_file_mutation(
        &self,
        state: &AppState,
        admission: &ContentMutationAdmission,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> Result<ContentMutationAdmission, ApiError> {
        let context = self.inline_mutation_context_from_admission(admission)?;
        let attempt = self.lease_inline_attempt(state, &context).await?;
        self.update_mutation(
            state,
            UpdateMutationCommand {
                mutation_id: context.mutation_id,
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
                    details_json: serde_json::json!({
                        "fileName": file_name,
                        "documentId": context.document_id,
                    }),
                },
            )
            .await?;

        match self.build_extract_outcome(state, file_name, mime_type, file_bytes).await {
            InlineExtractOutcome::Ready(plan) => {
                let normalized_text = plan.extracted_text.clone().unwrap_or_default();
                state
                    .canonical_services
                    .extract
                    .persist_extract_content(
                        state,
                        PersistExtractContentCommand {
                            revision_id: context.revision_id,
                            attempt_id: Some(attempt.id),
                            extract_state: "ready".to_string(),
                            normalized_text: Some(normalized_text.clone()),
                            text_checksum: Some(sha256_hex_text(&normalized_text)),
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
                self.persist_revision_chunks(state, context.revision_id, &normalized_text).await?;
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
                            details_json: serde_json::json!({ "revisionId": context.revision_id }),
                        },
                    )
                    .await?;
                self.complete_successful_inline_mutation(state, &context, attempt.id).await
            }
            InlineExtractOutcome::Failed(failure) => {
                state
                    .canonical_services
                    .extract
                    .persist_extract_content(
                        state,
                        PersistExtractContentCommand {
                            revision_id: context.revision_id,
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
                            details_json: serde_json::json!({
                                "failureKind": failure.failure_kind,
                            }),
                        },
                    )
                    .await?;
                self.complete_failed_inline_mutation(state, &context, attempt.id, failure).await
            }
        }
    }

    async fn complete_successful_inline_mutation(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        attempt_id: Uuid,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let _ = self
            .promote_document_head(
                state,
                PromoteHeadCommand {
                    document_id: context.document_id,
                    active_revision_id: Some(context.revision_id),
                    readable_revision_id: Some(context.revision_id),
                    latest_mutation_id: Some(context.mutation_id),
                    latest_successful_attempt_id: Some(attempt_id),
                },
            )
            .await?;
        let _ = self
            .update_mutation_item(
                state,
                UpdateMutationItemCommand {
                    item_id: context.item_id,
                    document_id: Some(context.document_id),
                    base_revision_id: None,
                    result_revision_id: Some(context.revision_id),
                    item_state: "applied".to_string(),
                    message: Some("mutation applied".to_string()),
                },
            )
            .await?;
        let _ = self
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id: context.mutation_id,
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
                    knowledge_generation_id: None,
                    attempt_state: "succeeded".to_string(),
                    current_stage: Some("chunk_content".to_string()),
                    failure_class: None,
                    failure_code: None,
                    retryable: false,
                },
            )
            .await?;
        self.get_mutation_admission(state, context.mutation_id).await
    }

    async fn complete_failed_inline_mutation(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        attempt_id: Uuid,
        failure: InlineMutationFailure,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let current_head = self.get_document_head(state, context.document_id).await?;
        let latest_successful_attempt_id =
            current_head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let _ = self
            .promote_document_head(
                state,
                PromoteHeadCommand {
                    document_id: context.document_id,
                    active_revision_id: Some(context.revision_id),
                    readable_revision_id: current_head.and_then(|row| row.readable_revision_id),
                    latest_mutation_id: Some(context.mutation_id),
                    latest_successful_attempt_id,
                },
            )
            .await?;
        let _ = self
            .update_mutation_item(
                state,
                UpdateMutationItemCommand {
                    item_id: context.item_id,
                    document_id: Some(context.document_id),
                    base_revision_id: None,
                    result_revision_id: Some(context.revision_id),
                    item_state: "failed".to_string(),
                    message: Some(failure.message.clone()),
                },
            )
            .await?;
        let _ = self
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id: context.mutation_id,
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
                    knowledge_generation_id: None,
                    attempt_state: "failed".to_string(),
                    current_stage: Some("extract_content".to_string()),
                    failure_class: Some("content_extract".to_string()),
                    failure_code: Some(failure.failure_kind),
                    retryable: false,
                },
            )
            .await?;
        self.get_mutation_admission(state, context.mutation_id).await
    }

    async fn lease_inline_attempt(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
    ) -> Result<crate::domains::ingest::IngestAttempt, ApiError> {
        state
            .canonical_services
            .ingest
            .lease_attempt(
                state,
                LeaseAttemptCommand {
                    job_id: context.job_id,
                    worker_principal_id: None,
                    lease_token: Some(format!("inline-{}", Uuid::now_v7())),
                    knowledge_generation_id: None,
                    current_stage: Some("extract_content".to_string()),
                },
            )
            .await
    }

    async fn build_extract_outcome(
        &self,
        state: &AppState,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> InlineExtractOutcome {
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
                    Ok(()) => InlineExtractOutcome::Ready(plan),
                    Err(error) => InlineExtractOutcome::Failed(InlineMutationFailure {
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
                InlineExtractOutcome::Failed(InlineMutationFailure {
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
        let revision =
            content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        let _ =
            content_repository::delete_chunks_by_revision(&state.persistence.postgres, revision_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        let _ =
            state.canonical_services.knowledge.delete_revision_chunks(state, revision_id).await?;
        let chunks = split_text_into_chunks_with_profile(text, ChunkingProfile::default());
        let mut next_search_char = 0usize;
        let mut pending_chunks = Vec::with_capacity(chunks.len());
        let mut knowledge_chunks = Vec::with_capacity(chunks.len());
        for (chunk_index, chunk_text) in chunks.iter().enumerate() {
            let (start_offset, end_offset) =
                locate_chunk_offsets(text, chunk_text, next_search_char);
            next_search_char = end_offset;
            pending_chunks.push(PendingChunkInsert {
                chunk_index: i32::try_from(chunk_index).unwrap_or(i32::MAX),
                start_offset: i32::try_from(start_offset).unwrap_or(i32::MAX),
                end_offset: i32::try_from(end_offset).unwrap_or(i32::MAX),
                token_count: Some(
                    i32::try_from(chunk_text.split_whitespace().count()).unwrap_or(i32::MAX),
                ),
                normalized_text: chunk_text.to_string(),
                text_checksum: sha256_hex_text(chunk_text),
            });
        }
        let postgres_chunks = pending_chunks
            .iter()
            .map(|chunk| content_repository::NewContentChunk {
                revision_id,
                chunk_index: chunk.chunk_index,
                start_offset: chunk.start_offset,
                end_offset: chunk.end_offset,
                token_count: chunk.token_count,
                normalized_text: &chunk.normalized_text,
                text_checksum: &chunk.text_checksum,
            })
            .collect::<Vec<_>>();
        let created_chunks = content_repository::create_chunks(
            &state.persistence.postgres,
            &postgres_chunks,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        for (chunk, pending_chunk) in created_chunks.into_iter().zip(pending_chunks.iter()) {
            knowledge_chunks.push(CreateKnowledgeChunkCommand {
                chunk_id: chunk.id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                document_id: revision.document_id,
                revision_id,
                chunk_index: chunk.chunk_index,
                content_text: pending_chunk.normalized_text.clone(),
                normalized_text: chunk.normalized_text,
                span_start: Some(chunk.start_offset),
                span_end: Some(chunk.end_offset),
                token_count: chunk.token_count,
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                chunk_state: "ready".to_string(),
                text_generation: Some(i64::from(revision.revision_number)),
                vector_generation: None,
            });
        }
        let _ = state
            .canonical_services
            .knowledge
            .write_chunks(state, knowledge_chunks)
            .await?;
        Ok(())
    }

    fn inline_mutation_context_from_admission(
        &self,
        admission: &ContentMutationAdmission,
    ) -> Result<InlineMutationContext, ApiError> {
        let item = admission.items.first().ok_or_else(|| ApiError::Internal)?;
        Ok(InlineMutationContext {
            mutation_id: admission.mutation.id,
            job_id: admission.job_id.ok_or_else(|| ApiError::Internal)?,
            item_id: item.id,
            document_id: item.document_id.ok_or_else(|| ApiError::Internal)?,
            revision_id: item.result_revision_id.ok_or_else(|| ApiError::Internal)?,
        })
    }

    async fn load_appendable_document_context(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<AppendableDocumentContext, ApiError> {
        let head = self.get_document_head(state, document_id).await?;
        let readable_revision_id =
            head.as_ref().and_then(|row| row.readable_revision_id).ok_or_else(|| {
                ApiError::unreadable_document("document has no readable revision".to_string())
            })?;
        let extract = state
            .canonical_services
            .extract
            .get_extract_content(state, readable_revision_id)
            .await?;
        let current_content = extract
            .normalized_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| {
                ApiError::unreadable_document(
                    "document is not readable enough for append".to_string(),
                )
            })?;
        let base_revision = content_repository::get_revision_by_id(
            &state.persistence.postgres,
            readable_revision_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("revision", readable_revision_id))?;
        Ok(AppendableDocumentContext {
            current_content,
            mime_type: base_revision.mime_type,
            title: base_revision.title.or_else(|| Some(document_id.to_string())),
            language_code: base_revision.language_code,
        })
    }

    pub async fn ensure_document_accepts_new_mutation(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        let Some(head) = self.get_document_head(state, document_id).await? else {
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

    fn build_document_summary_from_parts(
        &self,
        document_row: content_repository::ContentDocumentRow,
        head_map: &std::collections::HashMap<Uuid, content_repository::ContentDocumentHeadRow>,
        active_revision_map: &std::collections::HashMap<Uuid, content_repository::ContentRevisionRow>,
    ) -> ContentDocumentSummary {
        let head = head_map.get(&document_row.id).cloned();
        let active_revision = head
            .as_ref()
            .and_then(|row| row.active_revision_id)
            .and_then(|revision_id| active_revision_map.get(&revision_id).cloned())
            .map(map_revision_row);

        ContentDocumentSummary {
            document: map_document_row(document_row),
            head: head.map(map_document_head_row),
            active_revision,
        }
    }

    async fn create_revision_from_metadata(
        &self,
        state: &AppState,
        document_id: Uuid,
        created_by_principal_id: Option<Uuid>,
        metadata: RevisionAdmissionMetadata,
    ) -> Result<ContentRevision, ApiError> {
        self.create_revision(
            state,
            CreateRevisionCommand {
                document_id,
                content_source_kind: metadata.content_source_kind,
                checksum: metadata.checksum,
                mime_type: metadata.mime_type,
                byte_size: metadata.byte_size,
                title: metadata.title,
                language_code: metadata.language_code,
                source_uri: metadata.source_uri,
                storage_key: metadata.storage_key,
                created_by_principal_id,
            },
        )
        .await
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

fn source_uri_for_inline_payload(
    operation_kind: &str,
    source_identity: Option<&str>,
    file_name: Option<&str>,
) -> String {
    if let Some(source_identity) = source_identity {
        return format!("mcp://payload/{source_identity}");
    }

    match file_name {
        Some(file_name) => format!("{operation_kind}://{file_name}"),
        None => format!("{operation_kind}://inline"),
    }
}

fn infer_inline_mime_type(
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

fn sha256_hex_text(value: &str) -> String {
    sha256_hex_bytes(value.as_bytes())
}

fn sha256_hex_bytes(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    hex::encode(hasher.finalize())
}
