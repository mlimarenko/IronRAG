use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{
        ContentChunk, ContentDocument, ContentDocumentHead, ContentDocumentPipelineJob,
        ContentDocumentPipelineState, ContentDocumentSummary, ContentMutation,
        ContentMutationItem, ContentRevision, ContentRevisionReadiness,
    },
    domains::{
        ai::AiBindingPurpose, ingest::IngestStageEvent, provider_profiles::ProviderModelSelection,
        runtime_graph::RuntimeNodeType,
    },
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
    },
    infra::repositories::{
        catalog_repository,
        content_repository::{
            self, NewContentDocument, NewContentDocumentHead, NewContentMutation,
            NewContentMutationItem, NewContentRevision,
        },
    },
    integrations::llm::ChatRequest,
    interfaces::http::router_support::ApiError,
    services::{
        billing_service::CaptureIngestAttemptBillingCommand,
        content_storage::ContentStorageService,
        extract_service::{
            MaterializeChunkResultCommand, NewEdgeCandidate, NewNodeCandidate,
            PersistExtractContentCommand,
        },
        graph_extract::parse_graph_extraction_output,
        ingest_service::AdmitIngestJobCommand,
        ingest_service::{
            FinalizeAttemptCommand, IngestJobHandle, LeaseAttemptCommand,
            RecordStageEventCommand,
        },
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
            build_runtime_file_extraction_plan, validate_upload_file_admission,
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
    pub source_identity: Option<String>,
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
pub struct ReconcileFailedIngestMutationCommand {
    pub mutation_id: Uuid,
    pub failure_code: String,
    pub failure_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailedRevisionReadiness {
    pub text_state: String,
    pub vector_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<chrono::DateTime<chrono::Utc>>,
    pub vector_ready_at: Option<chrono::DateTime<chrono::Utc>>,
    pub graph_ready_at: Option<chrono::DateTime<chrono::Utc>>,
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
    pub source_identity: Option<String>,
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
    pub source_identity: Option<String>,
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
    workspace_id: Uuid,
    library_id: Uuid,
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
struct PendingChunkInsert {
    chunk_index: i32,
    start_offset: i32,
    end_offset: i32,
    token_count: Option<i32>,
    normalized_text: String,
    text_checksum: String,
}

#[derive(Clone, Default)]
pub struct ContentService;

impl ContentService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn build_runtime_extraction_plan(
        &self,
        state: &AppState,
        library_id: Uuid,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> Result<FileExtractionPlan, UploadAdmissionError> {
        let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        let vision_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::Vision)
            .await
            .map_err(|_| {
                UploadAdmissionError::from_file_extract_error(
                    file_name,
                    mime_type,
                    file_size_bytes,
                    FileExtractError::ExtractionFailed {
                        file_kind: UploadFileKind::Image,
                        message: "failed to resolve active vision binding".to_string(),
                    },
                )
            })?;
        let vision_provider = vision_binding.as_ref().and_then(|binding| {
            binding.provider_kind.parse().ok().map(|provider_kind| ProviderModelSelection {
                provider_kind,
                model_name: binding.model_name.clone(),
            })
        });
        let plan = build_runtime_file_extraction_plan(
            state.llm_gateway.as_ref(),
            vision_provider.as_ref(),
            vision_binding.as_ref().map(|binding| binding.api_key.as_str()),
            vision_binding.as_ref().and_then(|binding| binding.provider_base_url.as_deref()),
            Some(file_name),
            mime_type,
            file_bytes.to_vec(),
        )
        .await
        .map_err(|error| {
            UploadAdmissionError::from_file_extract_error(
                file_name,
                mime_type,
                file_size_bytes,
                error,
            )
        })?;
        validate_extraction_plan(file_name, mime_type, file_size_bytes, &plan)?;
        Ok(plan)
    }

    fn validate_inline_file_admission(
        &self,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> Result<UploadFileKind, ApiError> {
        let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        validate_upload_file_admission(Some(file_name), mime_type, file_bytes).map_err(|error| {
            ApiError::from_upload_admission(UploadAdmissionError::from_file_extract_error(
                file_name,
                mime_type,
                file_size_bytes,
                error,
            ))
        })
    }

    pub async fn resolve_revision_storage_key(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Option<String>, ApiError> {
        let revision = content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        if let Some(storage_key) = revision
            .storage_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
        {
            return Ok(Some(storage_key));
        }

        let Some(file_name) = storage_backed_revision_file_name(
            &revision.content_source_kind,
            revision.source_uri.as_deref(),
            revision.title.as_deref(),
        ) else {
            return Ok(None);
        };

        let storage_key = ContentStorageService::build_revision_storage_key(
            revision.workspace_id,
            revision.library_id,
            &file_name,
            &revision.checksum,
        );
        let exists = state
            .content_storage
            .has_revision_source(&storage_key)
            .await
            .map_err(|_| ApiError::Internal)?;
        if !exists {
            return Ok(None);
        }

        content_repository::update_revision_storage_key(
            &state.persistence.postgres,
            revision_id,
            Some(&storage_key),
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        let _ = state
            .canonical_services
            .knowledge
            .set_revision_storage_ref(state, revision_id, Some(&storage_key))
            .await?;
        Ok(Some(storage_key))
    }

    pub async fn list_documents(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<ContentDocumentSummary>, ApiError> {
        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        let documents = state
            .arango_document_store
            .list_documents_by_library(library.workspace_id, library_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let document_ids = documents.iter().map(|row| row.document_id).collect::<Vec<_>>();
        let content_heads = content_repository::list_document_heads_by_document_ids(
            &state.persistence.postgres,
            &document_ids,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let latest_mutation_ids = content_heads
            .iter()
            .filter_map(|row| row.latest_mutation_id)
            .collect::<Vec<_>>();
        let mutations_by_id = content_repository::list_mutations_by_ids(
            &state.persistence.postgres,
            &latest_mutation_ids,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<std::collections::HashMap<_, _>>();
        let job_handles_by_mutation_id = state
            .canonical_services
            .ingest
            .list_job_handles_by_mutation_ids(
                state,
                library.workspace_id,
                library_id,
                &latest_mutation_ids,
            )
            .await?
            .into_iter()
            .filter_map(|handle| handle.job.mutation_id.map(|mutation_id| (mutation_id, handle)))
            .collect::<std::collections::HashMap<_, _>>();
        let heads_by_document_id = content_heads
            .into_iter()
            .map(|row| (row.document_id, row))
            .collect::<std::collections::HashMap<_, _>>();
        let mut summaries = Vec::with_capacity(documents.len());
        for row in documents {
            let content_head = heads_by_document_id.get(&row.document_id);
            let latest_mutation = content_head
                .and_then(|head| head.latest_mutation_id)
                .and_then(|mutation_id| mutations_by_id.get(&mutation_id).cloned())
                .map(map_mutation_row);
            let latest_job = content_head
                .and_then(|head| head.latest_mutation_id)
                .and_then(|mutation_id| job_handles_by_mutation_id.get(&mutation_id).cloned())
                .map(map_document_pipeline_job);
            summaries.push(
                self.build_document_summary_from_knowledge(
                    state,
                    row,
                    content_head,
                    latest_mutation,
                    latest_job,
                )
                .await?,
            );
        }
        Ok(summaries)
    }

    pub async fn get_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentDocumentSummary, ApiError> {
        let row = state
            .arango_document_store
            .get_document(document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let content_head =
            content_repository::get_document_head(&state.persistence.postgres, document_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        let latest_mutation = match content_head.as_ref().and_then(|head| head.latest_mutation_id) {
            Some(mutation_id) => content_repository::get_mutation_by_id(
                &state.persistence.postgres,
                mutation_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .map(map_mutation_row),
            None => None,
        };
        let latest_job = match content_head.as_ref().and_then(|head| head.latest_mutation_id) {
            Some(mutation_id) => state
                .canonical_services
                .ingest
                .get_job_handle_by_mutation_id(state, mutation_id)
                .await?
                .map(map_document_pipeline_job),
            None => None,
        };
        self
            .build_document_summary_from_knowledge(
                state,
                row,
                content_head.as_ref(),
                latest_mutation,
                latest_job,
            )
            .await
    }

    pub async fn get_document_head(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Option<ContentDocumentHead>, ApiError> {
        let document = state
            .arango_document_store
            .get_document(document_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let Some(document) = document else {
            return Ok(None);
        };
        let row = content_repository::get_document_head(&state.persistence.postgres, document_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(Some(ContentDocumentHead {
            document_id,
            active_revision_id: document.active_revision_id,
            readable_revision_id: document.readable_revision_id,
            latest_mutation_id: row.as_ref().and_then(|head| head.latest_mutation_id),
            latest_successful_attempt_id: row
                .as_ref()
                .and_then(|head| head.latest_successful_attempt_id),
            head_updated_at: row.map_or(document.updated_at, |head| head.head_updated_at),
        }))
    }

    pub async fn list_revisions(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Vec<ContentRevision>, ApiError> {
        let rows = state
            .arango_document_store
            .list_revisions_by_document(document_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_knowledge_revision_row).collect())
    }

    pub async fn list_chunks(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<ContentChunk>, ApiError> {
        let rows = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_knowledge_chunk_row).collect())
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
        let document = ContentDocument {
            id: row.id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            external_key: row.external_key.clone(),
            document_state: row.document_state.clone(),
            created_at: row.created_at,
        };
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
                    title: None,
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
                    source_identity: command.source_identity.clone(),
                },
            )
            .await?;
        let mutation_lock = content_repository::acquire_content_mutation_lock(
            &state.persistence.postgres,
            mutation.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        let result = async {
            let existing_admission = self.get_mutation_admission(state, mutation.id).await?;
            if let Some(existing_document_id) =
                existing_admission.items.iter().find_map(|item| item.document_id)
            {
                let document = self.get_document(state, existing_document_id).await?;
                return Ok(CreateDocumentAdmission { document, mutation: existing_admission });
            }

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
        .await;
        let release_result =
            content_repository::release_content_mutation_lock(mutation_lock, mutation.id)
                .await
                .map_err(|_| ApiError::Internal);
        match (result, release_result) {
            (Ok(admission), Ok(())) => Ok(admission),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(_), Err(error)) => Err(error),
        }
    }

    pub async fn upload_inline_document(
        &self,
        state: &AppState,
        command: UploadInlineDocumentCommand,
    ) -> Result<CreateDocumentAdmission, ApiError> {
        self.validate_inline_file_admission(
            &command.file_name,
            command.mime_type.as_deref(),
            &command.file_bytes,
        )?;
        let file_checksum = sha256_hex_bytes(&command.file_bytes);
        let file_name = command.file_name.trim().to_string();
        let title = command
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| file_name.clone());
        let storage_key = self
            .persist_inline_file_source(
                state,
                command.workspace_id,
                command.library_id,
                &file_name,
                &format!("sha256:{file_checksum}"),
                &command.file_bytes,
            )
            .await?;
        self
            .admit_document(
                state,
                AdmitDocumentCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    external_key: command.external_key,
                    idempotency_key: command.idempotency_key,
                    created_by_principal_id: command.requested_by_principal_id,
                    request_surface: command.request_surface.clone(),
                    source_identity: command.source_identity.clone(),
                    revision: Some(RevisionAdmissionMetadata {
                        content_source_kind: "upload".to_string(),
                        checksum: format!("sha256:{file_checksum}"),
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
                        storage_key: Some(storage_key),
                    }),
                },
            )
            .await
    }

    pub async fn create_revision(
        &self,
        state: &AppState,
        command: CreateRevisionCommand,
    ) -> Result<ContentRevision, ApiError> {
        let document = state
            .arango_document_store
            .get_document(command.document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        let latest = state
            .arango_document_store
            .list_revisions_by_document(command.document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .max_by_key(|row| row.revision_number);
        let next_revision_number = latest
            .as_ref()
            .and_then(|row| i32::try_from(row.revision_number).ok())
            .map_or(1, |value| value.saturating_add(1));
        let row = content_repository::create_revision(
            &state.persistence.postgres,
            &NewContentRevision {
                document_id: document.document_id,
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                revision_number: next_revision_number,
                parent_revision_id: latest.as_ref().map(|row| row.revision_id),
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
                    source_identity: command.source_identity.clone(),
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
        let source_identity = command.source_identity.clone();
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
                    source_identity: source_identity.clone(),
                    revision: Some(RevisionAdmissionMetadata {
                        content_source_kind: "append".to_string(),
                        checksum: format!("sha256:{}", sha256_hex_bytes(merged_text.as_bytes())),
                        mime_type: appendable.mime_type,
                        byte_size: i64::try_from(merged_text.len()).unwrap_or(i64::MAX),
                        title: appendable.title,
                        language_code: appendable.language_code,
                        source_uri: Some(source_uri_for_inline_payload(
                            "append",
                            source_identity.as_deref(),
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
        self.validate_inline_file_admission(
            &command.file_name,
            command.mime_type.as_deref(),
            &command.file_bytes,
        )?;
        let file_checksum = sha256_hex_bytes(&command.file_bytes);
        let head = self.get_document_head(state, command.document_id).await?;
        let base_revision =
            match head.as_ref().and_then(|row| row.active_revision_id.or(row.readable_revision_id))
            {
                Some(revision_id) => state
                    .arango_document_store
                    .get_revision(revision_id)
                    .await
                    .map_err(|_| ApiError::Internal)?,
                None => None,
            };
        let storage_key = self
            .persist_inline_file_source(
                state,
                command.workspace_id,
                command.library_id,
                &command.file_name,
                &format!("sha256:{file_checksum}"),
                &command.file_bytes,
            )
            .await?;
        self
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
                    source_identity: command.source_identity.clone(),
                    revision: Some(RevisionAdmissionMetadata {
                        content_source_kind: "replace".to_string(),
                        checksum: format!("sha256:{file_checksum}"),
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
                        language_code: None,
                        source_uri: Some(source_uri_for_inline_payload(
                            "replace",
                            command.source_identity.as_deref(),
                            Some(&command.file_name),
                        )),
                        storage_key: Some(storage_key),
                    }),
                },
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

        let head = self.get_document_head(state, document_id).await?;
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

        Ok(ContentDocument {
            id: document.id,
            workspace_id: document.workspace_id,
            library_id: document.library_id,
            external_key: document.external_key,
            document_state: document.document_state,
            created_at: document.created_at,
        })
    }

    pub async fn promote_document_head(
        &self,
        state: &AppState,
        command: PromoteHeadCommand,
    ) -> Result<ContentDocumentHead, ApiError> {
        if let Some(active_revision_id) = command.active_revision_id {
            state
                .arango_document_store
                .get_revision(active_revision_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("revision", active_revision_id))?;
        }
        if let Some(readable_revision_id) = command.readable_revision_id {
            state
                .arango_document_store
                .get_revision(readable_revision_id)
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
        let document = state
            .arango_document_store
            .get_document(command.document_id)
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
        Ok(ContentDocumentHead {
            document_id: row.document_id,
            active_revision_id: row.active_revision_id,
            readable_revision_id: row.readable_revision_id,
            latest_mutation_id: row.latest_mutation_id,
            latest_successful_attempt_id: row.latest_successful_attempt_id,
            head_updated_at: row.head_updated_at,
        })
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
            let request_source_identity =
                command.source_identity.as_deref().map(str::trim).filter(|value| !value.is_empty());
            if let Some(existing) = content_repository::find_mutation_by_idempotency(
                &state.persistence.postgres,
                principal_id,
                &command.request_surface,
                idempotency_key,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            {
                ensure_existing_mutation_matches_request(&existing, request_source_identity)?;
                return Ok(map_mutation_row(existing));
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
                    source_identity: command.source_identity.as_deref(),
                    mutation_state: "accepted",
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await;
            return match row {
                Ok(row) => Ok(map_mutation_row(row)),
                Err(error) if is_content_mutation_idempotency_violation(&error) => {
                    let existing = content_repository::find_mutation_by_idempotency(
                        &state.persistence.postgres,
                        principal_id,
                        &command.request_surface,
                        idempotency_key,
                    )
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .ok_or(ApiError::Internal)?;
                    ensure_existing_mutation_matches_request(&existing, request_source_identity)?;
                    Ok(map_mutation_row(existing))
                }
                Err(_) => Err(ApiError::Internal),
            };
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
                source_identity: command.source_identity.as_deref(),
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

    pub async fn reconcile_failed_ingest_mutation(
        &self,
        state: &AppState,
        command: ReconcileFailedIngestMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let admission = self.get_mutation_admission(state, command.mutation_id).await?;
        let job_handle = state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(state, command.mutation_id)
            .await?;
        let async_operation_id = admission.async_operation_id.or_else(|| {
            job_handle
                .as_ref()
                .and_then(|handle| handle.async_operation.as_ref().map(|operation| operation.id))
                .or_else(|| job_handle.as_ref().and_then(|handle| handle.job.async_operation_id))
        });
        let stage_events = if let Some(attempt) =
            job_handle.as_ref().and_then(|handle| handle.latest_attempt.as_ref())
        {
            state.canonical_services.ingest.list_stage_events(state, attempt.id).await?
        } else {
            Vec::new()
        };

        if let Some(operation_id) = async_operation_id {
            let _ = state
                .canonical_services
                .ops
                .update_async_operation(
                    state,
                    UpdateAsyncOperationCommand {
                        operation_id,
                        status: "failed".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: Some(command.failure_code.clone()),
                    },
                )
                .await?;
        }

        for item in &admission.items {
            if matches!(item.item_state.as_str(), "applied" | "failed") {
                continue;
            }
            let _ = self
                .update_mutation_item(
                    state,
                    UpdateMutationItemCommand {
                        item_id: item.id,
                        document_id: item.document_id,
                        base_revision_id: item.base_revision_id,
                        result_revision_id: item.result_revision_id,
                        item_state: "failed".to_string(),
                        message: Some(command.failure_message.clone()),
                    },
                )
                .await?;
        }

        if matches!(admission.mutation.mutation_state.as_str(), "accepted" | "running") {
            let _ = self
                .update_mutation(
                    state,
                    UpdateMutationCommand {
                        mutation_id: command.mutation_id,
                        mutation_state: "failed".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: Some(command.failure_code.clone()),
                        conflict_code: None,
                    },
                )
                .await?;
        }

        let document_id =
            admission.items.iter().find_map(|item| item.document_id).or_else(|| {
                job_handle.as_ref().and_then(|handle| handle.job.knowledge_document_id)
            });
        let revision_id =
            admission.items.iter().find_map(|item| item.result_revision_id).or_else(|| {
                job_handle.as_ref().and_then(|handle| handle.job.knowledge_revision_id)
            });

        if let Some(document_id) = document_id
            && let Some(document) = state
                .arango_document_store
                .get_document(document_id)
                .await
                .map_err(|_| ApiError::Internal)?
        {
            let head =
                content_repository::get_document_head(&state.persistence.postgres, document_id)
                    .await
                    .map_err(|_| ApiError::Internal)?;
            let _ = self
                .promote_document_head(
                    state,
                    PromoteHeadCommand {
                        document_id,
                        active_revision_id: document.active_revision_id,
                        readable_revision_id: document.readable_revision_id,
                        latest_mutation_id: Some(command.mutation_id),
                        latest_successful_attempt_id: head
                            .as_ref()
                            .and_then(|current_head| current_head.latest_successful_attempt_id),
                    },
                )
                .await?;
        }

        if let Some(revision_id) = revision_id
            && let Some(revision) = state
                .arango_document_store
                .get_revision(revision_id)
                .await
                .map_err(|_| ApiError::Internal)?
        {
            let readiness = derive_failed_revision_readiness(&revision, &stage_events);
            let _ = state
                .arango_document_store
                .update_revision_readiness(
                    revision_id,
                    &readiness.text_state,
                    &readiness.vector_state,
                    &readiness.graph_state,
                    readiness.text_readable_at,
                    readiness.vector_ready_at,
                    readiness.graph_ready_at,
                    revision.superseded_by_revision_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?;
        }

        self.get_mutation_admission(state, command.mutation_id).await
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

    async fn complete_successful_inline_mutation(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        attempt_id: Uuid,
    ) -> Result<ContentMutationAdmission, ApiError> {
        self.run_inline_post_chunk_pipeline(state, context, attempt_id).await?;
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

    async fn run_inline_post_chunk_pipeline(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        attempt_id: Uuid,
    ) -> Result<(), ApiError> {
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id,
                    stage_name: "embed_chunk".to_string(),
                    stage_state: "started".to_string(),
                    message: Some("rebuilding chunk embeddings for inline mutation".to_string()),
                    details_json: serde_json::json!({
                        "libraryId": context.library_id,
                        "revisionId": context.revision_id,
                    }),
                },
            )
            .await?;
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id,
                    stage_name: "embed_chunk".to_string(),
                    stage_state: "completed".to_string(),
                    message: Some(
                        "vector stage deferred to keep inline ingestion non-blocking".to_string(),
                    ),
                    details_json: serde_json::json!({
                        "strategy": "deferred_non_blocking",
                    }),
                },
            )
            .await?;

        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id,
                    stage_name: "extract_graph".to_string(),
                    stage_state: "started".to_string(),
                    message: Some("extracting graph candidates from chunks".to_string()),
                    details_json: serde_json::json!({
                        "libraryId": context.library_id,
                        "revisionId": context.revision_id,
                    }),
                },
            )
            .await?;
        let (chunk_count, extracted_entities, extracted_relations) =
            self.materialize_inline_graph_candidates(state, context, attempt_id).await?;
        let graph_outcome = state
            .canonical_services
            .graph
            .rebuild_arango_library_graph(state, context.library_id)
            .await
            .map_err(|error| {
                ApiError::BadRequest(format!("inline graph stage failed: {error:#}"))
            })?;
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id,
                    stage_name: "extract_graph".to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("graph candidates extracted and reconciled".to_string()),
                    details_json: serde_json::json!({
                        "chunksProcessed": chunk_count,
                        "extractedEntityCandidates": extracted_entities,
                        "extractedRelationCandidates": extracted_relations,
                        "upsertedEntities": graph_outcome.upserted_entities,
                        "upsertedRelations": graph_outcome.upserted_relations,
                    }),
                },
            )
            .await?;

        let revision = state
            .arango_document_store
            .get_revision(context.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_revision", context.revision_id)
            })?;
        let now = Utc::now();
        let _ = state
            .arango_document_store
            .update_revision_readiness(
                revision.revision_id,
                &revision.text_state,
                "ready",
                "ready",
                revision.text_readable_at,
                revision.vector_ready_at.or(Some(now)),
                revision.graph_ready_at.or(Some(now)),
                revision.superseded_by_revision_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?;

        Ok(())
    }

    async fn materialize_inline_graph_candidates(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        attempt_id: Uuid,
    ) -> Result<(usize, usize, usize), ApiError> {
        let graph_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(
                state,
                context.library_id,
                AiBindingPurpose::ExtractGraph,
            )
            .await?
            .ok_or_else(|| {
                ApiError::BadRequest(
                    "active extract_graph binding is required for inline graph extraction"
                        .to_string(),
                )
            })?;
        let chunks = state
            .arango_document_store
            .list_chunks_by_revision(context.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let mut extracted_entities = 0usize;
        let mut extracted_relations = 0usize;

        for chunk in &chunks {
            let prompt = format!(
                "Extract graph entities and relations from the text chunk.\n\
Return STRICT JSON object:\n\
{{\"entities\":[{{\"label\":\"...\",\"node_type\":\"entity|topic|document\",\"aliases\":[...],\"summary\":\"...\"}}],\"relations\":[{{\"source_label\":\"...\",\"target_label\":\"...\",\"relation_type\":\"...\",\"summary\":\"...\"}}]}}\n\
No markdown and no prose.\n\
Chunk:\n{}",
                chunk.content_text
            );
            let response = state
                .llm_gateway
                .generate(ChatRequest {
                    provider_kind: graph_binding.provider_kind.clone(),
                    model_name: graph_binding.model_name.clone(),
                    prompt,
                    api_key_override: Some(graph_binding.api_key.clone()),
                    base_url_override: graph_binding.provider_base_url.clone(),
                    system_prompt: graph_binding.system_prompt.clone(),
                    temperature: None,
                    top_p: None,
                    // Provider adapters in `UnifiedGateway` normalize output token controls.
                    // Some OpenAI-compatible routes reject explicit max_output_tokens.
                    max_output_tokens_override: None,
                    extra_parameters_json: graph_binding.extra_parameters_json.clone(),
                })
                .await
                .map_err(|error| {
                    ApiError::BadRequest(format!(
                        "inline graph provider call failed for chunk {}: {error:#}",
                        chunk.chunk_id
                    ))
                })?;
            let _ = state
                .canonical_services
                .billing
                .capture_ingest_attempt(
                    state,
                    CaptureIngestAttemptBillingCommand {
                        workspace_id: context.workspace_id,
                        library_id: context.library_id,
                        attempt_id,
                        binding_id: Some(graph_binding.binding_id),
                        provider_kind: response.provider_kind.clone(),
                        model_name: response.model_name.clone(),
                        call_kind: "extract_graph".to_string(),
                        usage_json: response.usage_json.clone(),
                    },
                )
                .await?;
            let parsed = parse_graph_extraction_output(&response.output_text).map_err(|error| {
                ApiError::BadRequest(format!(
                    "inline graph parse failed for chunk {}: {error:#}",
                    chunk.chunk_id
                ))
            })?;

            let node_candidates = parsed
                .entities
                .iter()
                .map(|entity| NewNodeCandidate {
                    canonical_key: crate::services::graph_merge::canonical_node_key(
                        entity.node_type.clone(),
                        &entity.label,
                    ),
                    node_kind: inline_runtime_node_type_slug(&entity.node_type).to_string(),
                    display_label: entity.label.clone(),
                    summary: entity.summary.clone(),
                })
                .collect::<Vec<_>>();
            let edge_candidates = parsed
                .relations
                .iter()
                .map(|relation| {
                    let from_key = crate::services::graph_merge::canonical_node_key(
                        RuntimeNodeType::Entity,
                        &relation.source_label,
                    );
                    let to_key = crate::services::graph_merge::canonical_node_key(
                        RuntimeNodeType::Entity,
                        &relation.target_label,
                    );
                    NewEdgeCandidate {
                        canonical_key: crate::services::graph_merge::canonical_edge_key(
                            &from_key,
                            &relation.relation_type,
                            &to_key,
                        ),
                        edge_kind: relation.relation_type.clone(),
                        from_canonical_key: from_key,
                        to_canonical_key: to_key,
                        summary: relation.summary.clone(),
                    }
                })
                .collect::<Vec<_>>();
            extracted_entities += node_candidates.len();
            extracted_relations += edge_candidates.len();

            let _ = state
                .canonical_services
                .extract
                .materialize_chunk_result(
                    state,
                    MaterializeChunkResultCommand {
                        chunk_id: chunk.chunk_id,
                        attempt_id,
                        extract_state: "ready".to_string(),
                        provider_call_id: None,
                        finished_at: Some(Utc::now()),
                        failure_code: None,
                        node_candidates,
                        edge_candidates,
                    },
                )
                .await?;
        }

        Ok((chunks.len(), extracted_entities, extracted_relations))
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

    async fn persist_inline_file_source(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        file_name: &str,
        checksum: &str,
        file_bytes: &[u8],
    ) -> Result<String, ApiError> {
        state
            .content_storage
            .persist_revision_source(workspace_id, library_id, file_name, checksum, file_bytes)
            .await
            .map_err(|_| ApiError::Internal)
    }

    async fn persist_revision_chunks(
        &self,
        state: &AppState,
        revision_id: Uuid,
        text: &str,
    ) -> Result<(), ApiError> {
        let revision = state
            .arango_document_store
            .get_revision(revision_id)
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
        let created_chunks =
            content_repository::create_chunks(&state.persistence.postgres, &postgres_chunks)
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
                text_generation: Some(revision.revision_number),
                vector_generation: None,
            });
        }
        let _ = state.canonical_services.knowledge.write_chunks(state, knowledge_chunks).await?;
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
            workspace_id: admission.mutation.workspace_id,
            library_id: admission.mutation.library_id,
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
        let base_revision = state
            .arango_document_store
            .get_revision(readable_revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("revision", readable_revision_id))?;
        Ok(AppendableDocumentContext {
            current_content,
            mime_type: base_revision.mime_type,
            title: base_revision.title.or_else(|| Some(document_id.to_string())),
            language_code: None,
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
        let latest_mutation_state =
            if matches!(latest_mutation.mutation_state.as_str(), "accepted" | "running") {
                self.reconcile_stale_inflight_mutation_if_terminal(state, &latest_mutation)
                    .await?
                    .unwrap_or(latest_mutation.mutation_state)
            } else {
                latest_mutation.mutation_state
            };
        if matches!(latest_mutation_state.as_str(), "accepted" | "running") {
            return Err(ApiError::ConflictingMutation(
                "document is still processing a previous mutation".to_string(),
            ));
        }
        Ok(())
    }

    async fn build_document_summary_from_knowledge(
        &self,
        state: &AppState,
        document_row: KnowledgeDocumentRow,
        content_head: Option<&content_repository::ContentDocumentHeadRow>,
        latest_mutation: Option<ContentMutation>,
        latest_job: Option<ContentDocumentPipelineJob>,
    ) -> Result<ContentDocumentSummary, ApiError> {
        let active_revision_row = if let Some(revision_id) = document_row.active_revision_id {
            state
                .arango_document_store
                .get_revision(revision_id)
                .await
                .map_err(|_| ApiError::Internal)?
        } else {
            None
        };
        let active_revision = active_revision_row.clone().map(map_knowledge_revision_row);
        let effective_readiness_row = match (
            document_row.readable_revision_id,
            document_row.active_revision_id,
            active_revision_row.as_ref(),
        ) {
            (Some(readable_revision_id), Some(active_revision_id), Some(active_row))
                if readable_revision_id == active_revision_id =>
            {
                Some(active_row.clone())
            }
            (Some(readable_revision_id), _, _) => state
                .arango_document_store
                .get_revision(readable_revision_id)
                .await
                .map_err(|_| ApiError::Internal)?,
            (None, Some(_), Some(active_row)) => Some(active_row.clone()),
            (None, Some(active_revision_id), None) => state
                .arango_document_store
                .get_revision(active_revision_id)
                .await
                .map_err(|_| ApiError::Internal)?,
            (None, None, _) => None,
        };
        let head = Some(ContentDocumentHead {
            document_id: document_row.document_id,
            active_revision_id: document_row.active_revision_id,
            readable_revision_id: document_row.readable_revision_id,
            latest_mutation_id: content_head.and_then(|row| row.latest_mutation_id),
            latest_successful_attempt_id: content_head
                .and_then(|row| row.latest_successful_attempt_id),
            head_updated_at: content_head
                .map_or(document_row.updated_at, |row| row.head_updated_at),
        });

        Ok(ContentDocumentSummary {
            document: map_knowledge_document_row(document_row),
            head,
            active_revision,
            readiness: effective_readiness_row.map(map_knowledge_revision_readiness),
            pipeline: ContentDocumentPipelineState { latest_mutation, latest_job },
        })
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

impl ContentService {
    async fn reconcile_stale_inflight_mutation_if_terminal(
        &self,
        state: &AppState,
        latest_mutation: &content_repository::ContentMutationRow,
    ) -> Result<Option<String>, ApiError> {
        let admission = self.get_mutation_admission(state, latest_mutation.id).await?;
        let job_handle = state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(state, latest_mutation.id)
            .await?;
        let job_failed =
            job_handle.as_ref().is_some_and(|handle| handle.job.queue_state == "failed");
        let attempt_failed = job_handle
            .as_ref()
            .and_then(|handle| handle.latest_attempt.as_ref())
            .is_some_and(|attempt| {
                matches!(attempt.attempt_state.as_str(), "failed" | "abandoned" | "canceled")
            });
        let async_operation_failed = admission.async_operation_id.and_then(|operation_id| {
            job_handle
                .as_ref()
                .and_then(|handle| handle.async_operation.as_ref())
                .filter(|operation| operation.id == operation_id)
                .map(|operation| operation.status == "failed")
        }) == Some(true);

        if !(job_failed || attempt_failed || async_operation_failed) {
            return Ok(None);
        }

        let failure_code = job_handle
            .as_ref()
            .and_then(|handle| handle.latest_attempt.as_ref())
            .and_then(|attempt| attempt.failure_code.clone())
            .or_else(|| {
                job_handle
                    .as_ref()
                    .and_then(|handle| handle.async_operation.as_ref())
                    .and_then(|operation| operation.failure_code.clone())
            })
            .unwrap_or_else(|| "canonical_pipeline_failed".to_string());
        let failure_message = format!(
            "terminal ingest failure left mutation {} in {}",
            latest_mutation.id, latest_mutation.mutation_state
        );
        let reconciled = self
            .reconcile_failed_ingest_mutation(
                state,
                ReconcileFailedIngestMutationCommand {
                    mutation_id: latest_mutation.id,
                    failure_code,
                    failure_message,
                },
            )
            .await?;
        Ok(Some(reconciled.mutation.mutation_state))
    }
}

pub(crate) fn derive_failed_revision_readiness(
    revision: &KnowledgeRevisionRow,
    stage_events: &[IngestStageEvent],
) -> FailedRevisionReadiness {
    let now = Utc::now();
    let extract_completed = has_completed_stage(stage_events, "extract_content");
    let embed_completed = has_completed_stage(stage_events, "embed_chunk");
    let graph_completed = has_completed_stage(stage_events, "extract_graph");

    let text_state = if revision.text_state == "text_readable" || extract_completed {
        "text_readable"
    } else {
        "failed"
    };
    let vector_state =
        if revision.vector_state == "ready" || embed_completed { "ready" } else { "failed" };
    let graph_state =
        if revision.graph_state == "ready" || graph_completed { "ready" } else { "failed" };

    FailedRevisionReadiness {
        text_state: text_state.to_string(),
        vector_state: vector_state.to_string(),
        graph_state: graph_state.to_string(),
        text_readable_at: (text_state == "text_readable")
            .then(|| revision.text_readable_at.unwrap_or(now)),
        vector_ready_at: (vector_state == "ready").then(|| revision.vector_ready_at.unwrap_or(now)),
        graph_ready_at: (graph_state == "ready").then(|| revision.graph_ready_at.unwrap_or(now)),
    }
}

fn has_completed_stage(stage_events: &[IngestStageEvent], stage_name: &str) -> bool {
    stage_events
        .iter()
        .any(|event| event.stage_name == stage_name && event.stage_state == "completed")
}

fn storage_backed_revision_file_name(
    content_source_kind: &str,
    source_uri: Option<&str>,
    title: Option<&str>,
) -> Option<String> {
    if !matches!(content_source_kind, "upload" | "replace") {
        return None;
    }
    source_uri
        .and_then(|value| value.split_once("://").map(|(_, rest)| rest).or(Some(value)))
        .and_then(|value| value.rsplit('/').next())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "inline")
        .map(ToString::to_string)
        .or_else(|| {
            title
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
}

fn map_knowledge_document_row(row: KnowledgeDocumentRow) -> ContentDocument {
    ContentDocument {
        id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key,
        document_state: row.document_state,
        created_at: row.created_at,
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

fn map_knowledge_revision_row(row: KnowledgeRevisionRow) -> ContentRevision {
    ContentRevision {
        id: row.revision_id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        revision_number: i32::try_from(row.revision_number).unwrap_or(i32::MAX),
        parent_revision_id: None,
        content_source_kind: row.revision_kind,
        checksum: row.checksum,
        mime_type: row.mime_type,
        byte_size: row.byte_size,
        title: row.title,
        language_code: None,
        source_uri: row.source_uri,
        storage_key: row.storage_ref,
        created_by_principal_id: None,
        created_at: row.created_at,
    }
}

fn map_knowledge_revision_readiness(row: KnowledgeRevisionRow) -> ContentRevisionReadiness {
    ContentRevisionReadiness {
        revision_id: row.revision_id,
        text_state: row.text_state,
        vector_state: row.vector_state,
        graph_state: row.graph_state,
        text_readable_at: row.text_readable_at,
        vector_ready_at: row.vector_ready_at,
        graph_ready_at: row.graph_ready_at,
    }
}

fn map_knowledge_chunk_row(row: KnowledgeChunkRow) -> ContentChunk {
    let start_offset = row.span_start.unwrap_or(0);
    let end_offset = row.span_end.unwrap_or_else(|| {
        start_offset.saturating_add(i32::try_from(row.normalized_text.len()).unwrap_or(0))
    });
    let checksum = format!("sha256:{:x}", Sha256::digest(row.normalized_text.as_bytes()));
    ContentChunk {
        id: row.chunk_id,
        revision_id: row.revision_id,
        chunk_index: row.chunk_index,
        start_offset,
        end_offset,
        token_count: row.token_count,
        normalized_text: row.normalized_text,
        text_checksum: checksum,
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
        source_identity: row.source_identity,
        failure_code: row.failure_code,
        conflict_code: row.conflict_code,
    }
}

fn map_document_pipeline_job(handle: IngestJobHandle) -> ContentDocumentPipelineJob {
    let latest_attempt = handle.latest_attempt;
    ContentDocumentPipelineJob {
        id: handle.job.id,
        workspace_id: handle.job.workspace_id,
        library_id: handle.job.library_id,
        mutation_id: handle.job.mutation_id,
        async_operation_id: handle.job.async_operation_id,
        job_kind: handle.job.job_kind,
        queue_state: handle.job.queue_state,
        queued_at: handle.job.queued_at,
        available_at: handle.job.available_at,
        completed_at: handle.job.completed_at,
        current_stage: latest_attempt.as_ref().and_then(|attempt| attempt.current_stage.clone()),
        failure_code: latest_attempt.as_ref().and_then(|attempt| attempt.failure_code.clone()),
        retryable: latest_attempt.as_ref().is_some_and(|attempt| attempt.retryable),
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

fn ensure_existing_mutation_matches_request(
    existing: &content_repository::ContentMutationRow,
    request_source_identity: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(request_source_identity) = request_source_identity {
        match existing.source_identity.as_deref() {
            Some(existing_source_identity)
                if existing_source_identity != request_source_identity =>
            {
                return Err(ApiError::idempotency_conflict(
                    "the same idempotency key was already used with a different payload",
                ));
            }
            None => {
                return Err(ApiError::idempotency_conflict(
                    "the same idempotency key was already used before payload identity tracking was available; retry with a new idempotency key",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_content_mutation_idempotency_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            database_error.constraint() == Some("idx_content_mutation_idempotency")
        }
        _ => false,
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
    if let Some(mime_type) = requested_mime_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !value.eq_ignore_ascii_case("application/octet-stream"))
    {
        return mime_type.to_string();
    }

    match file_name.and_then(file_extension) {
        Some(extension) if extension == "pdf" => "application/pdf".to_string(),
        Some(extension) if extension == "docx" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
        }
        Some(extension) if extension == "pptx" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string()
        }
        Some(extension) if extension == "md" => "text/markdown".to_string(),
        Some(extension) if extension == "txt" => "text/plain".to_string(),
        Some(extension) if extension == "json" => "application/json".to_string(),
        Some(extension) if extension == "png" => "image/png".to_string(),
        Some(extension) if extension == "jpg" || extension == "jpeg" => "image/jpeg".to_string(),
        Some(extension) if extension == "gif" => "image/gif".to_string(),
        Some(extension) if extension == "bmp" => "image/bmp".to_string(),
        Some(extension) if extension == "webp" => "image/webp".to_string(),
        Some(extension) if extension == "svg" => "image/svg+xml".to_string(),
        Some(extension) if extension == "tif" || extension == "tiff" => "image/tiff".to_string(),
        _ if fallback_kind == "append" => "text/plain".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn file_extension(file_name: &str) -> Option<String> {
    let (_, extension) = file_name.rsplit_once('.')?;
    Some(extension.trim().to_ascii_lowercase())
}

fn inline_runtime_node_type_slug(node_type: &RuntimeNodeType) -> &'static str {
    match node_type {
        RuntimeNodeType::Document => "document",
        RuntimeNodeType::Entity => "entity",
        RuntimeNodeType::Topic => "topic",
    }
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
