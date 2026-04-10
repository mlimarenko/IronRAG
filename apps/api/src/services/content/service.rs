use std::collections::{HashMap, HashSet};

use chrono::Utc;
use futures::{StreamExt, TryStreamExt, stream};
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

use crate::{
    agent_runtime::{task::RuntimeTask, tasks::graph_extract::GraphExtractTask},
    app::state::AppState,
    domains::content::{
        ContentChunk, ContentDocument, ContentDocumentHead, ContentDocumentPipelineJob,
        ContentDocumentPipelineState, ContentDocumentSummary, ContentMutation, ContentMutationItem,
        ContentRevision, ContentRevisionReadiness, WebPageProvenance,
    },
    domains::knowledge::{
        PreparedSegmentDetail, PreparedSegmentListItem, StructuredDocumentRevision,
        TypedTechnicalFact,
    },
    domains::{
        ai::AiBindingPurpose, ingest::IngestStageEvent, provider_profiles::ProviderModelSelection,
    },
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow, KnowledgeStructuredBlockRow,
        KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow,
    },
    infra::repositories::{
        self, catalog_repository,
        content_repository::{
            self, NewContentDocument, NewContentDocumentHead, NewContentMutation,
            NewContentMutationItem, NewContentRevision,
        },
        ingest_repository,
    },
    interfaces::http::router_support::ApiError,
    services::{
        content::source_access::{
            derive_content_source_file_name, derive_storage_backed_content_file_name,
            describe_content_source,
        },
        content::storage::ContentStorageService,
        graph::extract::{
            GraphExtractionRequest, GraphExtractionStructuredChunkContext,
            GraphExtractionTechnicalFact, extract_chunk_graph_candidates,
        },
        ingest::runtime::resolve_effective_runtime_task_context,
        ingest::service::{
            AdmitIngestJobCommand, FinalizeAttemptCommand, INGEST_STAGE_CHUNK_CONTENT,
            INGEST_STAGE_EMBED_CHUNK, INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH,
            INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_FINALIZING,
            INGEST_STAGE_PREPARE_STRUCTURE, IngestJobHandle, LeaseAttemptCommand,
            RecordStageEventCommand,
        },
        ingest::structured_preparation::PrepareStructuredRevisionCommand,
        ingest::technical_facts::ExtractTechnicalFactsCommand,
        knowledge::service::{
            CreateKnowledgeChunkCommand, CreateKnowledgeDocumentCommand,
            CreateKnowledgeRevisionCommand, PromoteKnowledgeDocumentCommand,
        },
        ops::billing::{CaptureGraphExtractionBillingCommand, CaptureIngestAttemptBillingCommand},
        ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    },
    shared::extraction::file_extract::{
        FileExtractError, FileExtractionPlan, UploadAdmissionError, UploadFileKind,
        build_inline_text_extraction_plan, build_runtime_file_extraction_plan,
        validate_upload_file_admission,
    },
};

#[derive(Debug, Clone)]
pub struct CreateDocumentCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: Option<String>,
    pub file_name: Option<String>,
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
    pub file_name: Option<String>,
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
pub struct MaterializeWebCaptureCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Uuid,
    pub requested_by_principal_id: Option<Uuid>,
    pub final_url: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub storage_key: String,
}

#[derive(Debug, Clone)]
pub struct ContentMutationAdmission {
    pub mutation: ContentMutation,
    pub items: Vec<ContentMutationItem>,
    pub job_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct MaterializedWebCapture {
    pub document: ContentDocument,
    pub revision: ContentRevision,
    pub mutation_item: ContentMutationItem,
    pub job_id: Uuid,
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
pub struct MaterializeRevisionGraphCandidatesCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub attempt_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionGraphCandidateMaterialization {
    pub chunk_count: usize,
    pub extracted_entities: usize,
    pub extracted_relations: usize,
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
    chunk_kind: Option<String>,
    normalized_text: String,
    text_checksum: String,
    support_block_ids: Vec<Uuid>,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    literal_digest: Option<String>,
    quality_score: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct PreparedRevisionPersistenceSummary {
    pub prepared_revision: StructuredDocumentRevision,
    pub chunk_count: usize,
    pub technical_fact_count: usize,
    pub technical_conflict_count: usize,
    pub normalization_profile: String,
}

#[derive(Clone, Default)]
pub struct ContentService;

struct PrefetchedDocumentSummaryData {
    revisions_by_id: HashMap<Uuid, KnowledgeRevisionRow>,
    structured_revisions_by_revision_id: HashMap<Uuid, KnowledgeStructuredRevisionRow>,
    web_pages_by_result_revision_id: HashMap<Uuid, ingest_repository::WebDiscoveredPageRow>,
}

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
                    &FileExtractError::ExtractionFailed {
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
            vision_binding.as_ref().and_then(|binding| binding.api_key.as_deref()),
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
                &error,
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
                &error,
            ))
        })
    }

    pub async fn resolve_revision_storage_key(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Option<String>, ApiError> {
        let revision =
            content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
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

        let Some(file_name) = derive_storage_backed_content_file_name(
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
        self.list_documents_with_deleted(state, library_id, false).await
    }

    pub async fn list_documents_with_deleted(
        &self,
        state: &AppState,
        library_id: Uuid,
        include_deleted: bool,
    ) -> Result<Vec<ContentDocumentSummary>, ApiError> {
        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        let documents = state
            .arango_document_store
            .list_documents_by_library(library.workspace_id, library_id, include_deleted)
            .await
            .map_err(|_| ApiError::Internal)?;
        let prefetched_summary_data =
            self.prefetch_document_summary_data(state, &documents).await?;
        let document_ids = documents.iter().map(|row| row.document_id).collect::<Vec<_>>();
        let content_heads = content_repository::list_document_heads_by_document_ids(
            &state.persistence.postgres,
            &document_ids,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let latest_mutation_ids =
            content_heads.iter().filter_map(|row| row.latest_mutation_id).collect::<Vec<_>>();
        let mutations_by_id = content_repository::list_mutations_by_ids(
            &state.persistence.postgres,
            &latest_mutation_ids,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
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
            .collect::<HashMap<_, _>>();
        let heads_by_document_id =
            content_heads.into_iter().map(|row| (row.document_id, row)).collect::<HashMap<_, _>>();
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
            summaries.push(self.build_document_summary_from_prefetched(
                state,
                row,
                content_head,
                latest_mutation,
                latest_job,
                &prefetched_summary_data,
            ));
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
            Some(mutation_id) => {
                content_repository::get_mutation_by_id(&state.persistence.postgres, mutation_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .map(map_mutation_row)
            }
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
        self.build_document_summary_from_knowledge(
            state,
            row,
            content_head.as_ref(),
            latest_mutation,
            latest_job,
        )
        .await
    }

    pub async fn get_document_by_external_key(
        &self,
        state: &AppState,
        library_id: Uuid,
        external_key: &str,
    ) -> Result<Option<ContentDocument>, ApiError> {
        let row = content_repository::get_document_by_external_key(
            &state.persistence.postgres,
            library_id,
            external_key,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(row.map(map_document_row))
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
            head_updated_at: row.as_ref().map_or(document.updated_at, |head| head.head_updated_at),
            document_summary: row.and_then(|head| head.document_summary),
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

    pub async fn list_prepared_segments(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<PreparedSegmentDetail>, ApiError> {
        let blocks =
            state.canonical_services.knowledge.list_structured_blocks(state, revision_id).await?;
        let chunk_rows = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let mut support_chunk_ids_by_block = std::collections::BTreeMap::<Uuid, Vec<Uuid>>::new();
        for chunk in chunk_rows {
            for block_id in chunk.support_block_ids {
                support_chunk_ids_by_block.entry(block_id).or_default().push(chunk.chunk_id);
            }
        }
        Ok(blocks
            .into_iter()
            .map(|block| PreparedSegmentDetail {
                segment: PreparedSegmentListItem {
                    segment_id: block.block_id,
                    revision_id: block.revision_id,
                    ordinal: block.ordinal,
                    block_kind: block.block_kind.clone(),
                    heading_trail: block.heading_trail.clone(),
                    section_path: block.section_path.clone(),
                    page_number: block.page_number,
                    excerpt: segment_excerpt(&block.normalized_text),
                },
                text: block.text,
                normalized_text: block.normalized_text,
                source_span: block.source_span,
                parent_block_id: block.parent_block_id,
                table_coordinates: block.table_coordinates,
                code_language: block.code_language,
                support_chunk_ids: support_chunk_ids_by_block
                    .remove(&block.block_id)
                    .unwrap_or_default(),
            })
            .collect())
    }

    pub async fn list_technical_facts(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<TypedTechnicalFact>, ApiError> {
        state.canonical_services.knowledge.list_typed_technical_facts(state, revision_id).await
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
                    file_name: command.file_name,
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
                        file_name: command.file_name,
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
                let job = match state
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
                    .await
                {
                    Ok(job) => job,
                    Err(error) => {
                        let _ = self
                            .reconcile_failed_ingest_mutation(
                                state,
                                ReconcileFailedIngestMutationCommand {
                                    mutation_id: mutation.id,
                                    failure_code: "ingest_job_admission_failed".to_string(),
                                    failure_message:
                                        "failed to admit ingest job for uploaded document"
                                            .to_string(),
                                },
                            )
                            .await;
                        return Err(error);
                    }
                };
                let _ = self
                    .promote_pending_document_mutation_head(state, document.id, mutation.id)
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
        self.admit_document(
            state,
            AdmitDocumentCommand {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                external_key: command.external_key,
                file_name: Some(file_name.clone()),
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

    pub async fn materialize_web_capture(
        &self,
        state: &AppState,
        command: MaterializeWebCaptureCommand,
    ) -> Result<MaterializedWebCapture, ApiError> {
        let document = match self
            .get_document_by_external_key(state, command.library_id, &command.final_url)
            .await?
        {
            Some(document) => document,
            None => {
                self.create_document(
                    state,
                    CreateDocumentCommand {
                        workspace_id: command.workspace_id,
                        library_id: command.library_id,
                        external_key: Some(command.final_url.clone()),
                        file_name: None,
                        created_by_principal_id: command.requested_by_principal_id,
                    },
                )
                .await?
            }
        };

        let current_head = self.get_document_head(state, document.id).await?;
        let base_revision_id = current_head.as_ref().and_then(|head| head.latest_revision_id());
        let revision = self
            .create_revision(
                state,
                CreateRevisionCommand {
                    document_id: document.id,
                    content_source_kind: "web_page".to_string(),
                    checksum: command.checksum,
                    mime_type: command.mime_type,
                    byte_size: command.byte_size,
                    title: command.title,
                    language_code: None,
                    source_uri: Some(command.final_url.clone()),
                    storage_key: Some(command.storage_key),
                    created_by_principal_id: command.requested_by_principal_id,
                },
            )
            .await?;
        let mutation_item = self
            .create_mutation_item(
                state,
                CreateMutationItemCommand {
                    mutation_id: command.mutation_id,
                    document_id: Some(document.id),
                    base_revision_id,
                    result_revision_id: Some(revision.id),
                    item_state: "pending".to_string(),
                    message: Some("web page accepted and queued for ingest".to_string()),
                },
            )
            .await?;
        let job = match state
            .canonical_services
            .ingest
            .admit_job(
                state,
                AdmitIngestJobCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    mutation_id: Some(command.mutation_id),
                    connector_id: None,
                    async_operation_id: None,
                    knowledge_document_id: Some(document.id),
                    knowledge_revision_id: Some(revision.id),
                    job_kind: "content_mutation".to_string(),
                    priority: 100,
                    dedupe_key: None,
                    available_at: None,
                },
            )
            .await
        {
            Ok(job) => job,
            Err(error) => {
                let _ = self
                    .reconcile_failed_ingest_mutation(
                        state,
                        ReconcileFailedIngestMutationCommand {
                            mutation_id: command.mutation_id,
                            failure_code: "ingest_job_admission_failed".to_string(),
                            failure_message: "failed to admit ingest job for materialized web page"
                                .to_string(),
                        },
                    )
                    .await;
                return Err(error);
            }
        };
        let _ = self
            .promote_pending_document_mutation_head(state, document.id, command.mutation_id)
            .await?;

        Ok(MaterializedWebCapture { document, revision, mutation_item, job_id: job.id })
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
        if document.document_state == "deleted" || document.deleted_at.is_some() {
            return Err(ApiError::BadRequest(
                "deleted documents do not accept new revisions".to_string(),
            ));
        }
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
                document_id: document.id,
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
        let accept_command = Self::accept_mutation_command_from_admit(&command);

        if command.operation_kind == "delete" {
            let document_lock = content_repository::acquire_content_document_lock(
                &state.persistence.postgres,
                command.document_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?;
            let result = self.admit_delete_mutation(state, &command, &accept_command).await;
            let release_result = content_repository::release_content_document_lock(
                document_lock,
                command.document_id,
            )
            .await
            .map_err(|_| ApiError::Internal);
            return match (result, release_result) {
                (Ok(admission), Ok(())) => Ok(admission),
                (Err(error), Ok(())) => Err(error),
                (Ok(_), Err(error)) => Err(error),
                (Err(_), Err(error)) => Err(error),
            };
        }

        if let Some(existing_admission) =
            self.get_existing_mutation_admission_for_request(state, &accept_command).await?
        {
            return Ok(existing_admission);
        }

        self.ensure_document_accepts_new_mutation(
            state,
            command.document_id,
            &command.operation_kind,
        )
        .await?;
        let current_head = self.get_document_head(state, command.document_id).await?;
        let base_revision_id = current_head.as_ref().and_then(|row| row.latest_revision_id());

        let mutation = self.accept_mutation(state, accept_command).await?;
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
                    status: "accepted".to_string(),
                    subject_kind: "content_mutation".to_string(),
                    subject_id: Some(mutation.id),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;

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
        let job = match state
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
            .await
        {
            Ok(job) => job,
            Err(error) => {
                let _ = self
                    .reconcile_failed_ingest_mutation(
                        state,
                        ReconcileFailedIngestMutationCommand {
                            mutation_id: mutation.id,
                            failure_code: "ingest_job_admission_failed".to_string(),
                            failure_message: "failed to admit ingest job for mutation".to_string(),
                        },
                    )
                    .await;
                return Err(error);
            }
        };
        let _ = self
            .promote_pending_document_mutation_head(state, command.document_id, mutation.id)
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
        let source_identity = command.source_identity.clone().or_else(|| {
            Some(format!("replace-inline:{file_checksum}:{}", command.file_name.trim()))
        });
        let accept_command = AcceptMutationCommand {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            operation_kind: "replace".to_string(),
            requested_by_principal_id: command.requested_by_principal_id,
            request_surface: command.request_surface.clone(),
            idempotency_key: command.idempotency_key.clone(),
            source_identity: source_identity.clone(),
        };
        if let Some(existing_admission) =
            self.get_existing_mutation_admission_for_request(state, &accept_command).await?
        {
            return Ok(existing_admission);
        }
        self.ensure_document_accepts_new_mutation(state, command.document_id, "replace").await?;
        let head = self.get_document_head(state, command.document_id).await?;
        let base_revision = match head.as_ref().and_then(|row| row.latest_revision_id()) {
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
        self.admit_mutation(
            state,
            AdmitMutationCommand {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                document_id: command.document_id,
                operation_kind: "replace".to_string(),
                idempotency_key: command.idempotency_key,
                requested_by_principal_id: command.requested_by_principal_id,
                request_surface: command.request_surface,
                source_identity,
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
        self.delete_document_with_context(state, document_id, None).await
    }

    async fn delete_document_with_context(
        &self,
        state: &AppState,
        document_id: Uuid,
        latest_mutation_id: Option<Uuid>,
    ) -> Result<ContentDocument, ApiError> {
        let current_document =
            content_repository::get_document_by_id(&state.persistence.postgres, document_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let head = self.get_document_head(state, document_id).await?;
        let readable_revision_id = head.as_ref().and_then(|row| row.readable_revision_id);
        let resolved_latest_mutation_id =
            latest_mutation_id.or_else(|| head.as_ref().and_then(|row| row.latest_mutation_id));
        let latest_successful_attempt_id =
            head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let latest_revision_no = self.load_document_latest_revision_no(state, document_id).await?;
        let deleted_at = current_document.deleted_at.or_else(|| Some(Utc::now()));

        let mut transaction =
            state.persistence.postgres.begin().await.map_err(|_| ApiError::Internal)?;
        let _ = ingest_repository::cancel_queued_jobs_for_document_with_executor(
            &mut *transaction,
            document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let document = content_repository::update_document_state_with_executor(
            &mut *transaction,
            document_id,
            "deleted",
            deleted_at,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let _ = content_repository::upsert_document_head_with_executor(
            &mut *transaction,
            &NewContentDocumentHead {
                document_id,
                active_revision_id: None,
                readable_revision_id,
                latest_mutation_id: resolved_latest_mutation_id,
                latest_successful_attempt_id,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        transaction.commit().await.map_err(|_| ApiError::Internal)?;
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
                    latest_revision_no,
                    deleted_at: document.deleted_at,
                },
            )
            .await?;
        self.converge_document_technical_facts(state, document_id, None).await?;
        self.refresh_deleted_document_graph_state(state, document.library_id, document_id).await?;

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
        let document = content_repository::get_document_by_id(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        let latest_revision_no =
            self.load_document_latest_revision_no(state, command.document_id).await?;
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
                    latest_revision_no,
                    deleted_at: document.deleted_at,
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
            document_summary: row.document_summary,
        })
    }

    async fn promote_pending_document_mutation_head(
        &self,
        state: &AppState,
        document_id: Uuid,
        mutation_id: Uuid,
    ) -> Result<ContentDocumentHead, ApiError> {
        let head = self.get_document_head(state, document_id).await?;
        self.promote_document_head(
            state,
            PromoteHeadCommand {
                document_id,
                active_revision_id: head.as_ref().and_then(|current| current.active_revision_id),
                readable_revision_id: head
                    .as_ref()
                    .and_then(|current| current.readable_revision_id),
                latest_mutation_id: Some(mutation_id),
                latest_successful_attempt_id: head
                    .as_ref()
                    .and_then(|current| current.latest_successful_attempt_id),
            },
        )
        .await
    }

    async fn load_document_latest_revision_no(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Option<i64>, ApiError> {
        Ok(state
            .arango_document_store
            .list_revisions_by_document(document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .map(|revision| revision.revision_number)
            .max())
    }

    pub async fn settle_deleted_document_mutation(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<(), ApiError> {
        let admission = self.get_mutation_admission(state, mutation_id).await?;
        if !matches!(admission.mutation.mutation_state.as_str(), "accepted" | "running") {
            return Ok(());
        }
        if let Some(operation_id) = admission.async_operation_id {
            let _ = state
                .canonical_services
                .ops
                .update_async_operation(
                    state,
                    UpdateAsyncOperationCommand {
                        operation_id,
                        status: "failed".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: Some("document_deleted".to_string()),
                    },
                )
                .await?;
        }
        for item in &admission.items {
            if matches!(item.item_state.as_str(), "applied" | "failed" | "skipped") {
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
                        item_state: "skipped".to_string(),
                        message: Some("mutation skipped because document was deleted".to_string()),
                    },
                )
                .await?;
        }
        let _ = self
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id,
                    mutation_state: "canceled".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: Some("document_deleted".to_string()),
                    conflict_code: None,
                },
            )
            .await?;
        Ok(())
    }

    pub async fn accept_mutation(
        &self,
        state: &AppState,
        command: AcceptMutationCommand,
    ) -> Result<ContentMutation, ApiError> {
        self.create_mutation_record(state, &command, "accepted").await
    }

    fn accept_mutation_command_from_admit(command: &AdmitMutationCommand) -> AcceptMutationCommand {
        AcceptMutationCommand {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            operation_kind: command.operation_kind.clone(),
            requested_by_principal_id: command.requested_by_principal_id,
            request_surface: command.request_surface.clone(),
            idempotency_key: command.idempotency_key.clone(),
            source_identity: command.source_identity.clone(),
        }
    }

    async fn admit_delete_mutation(
        &self,
        state: &AppState,
        command: &AdmitMutationCommand,
        accept_command: &AcceptMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let current_document = content_repository::get_document_by_id(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        let current_head = self.get_document_head(state, command.document_id).await?;
        let base_revision_id = current_head.as_ref().and_then(|row| row.latest_revision_id());
        let superseded_mutation_id = current_head.as_ref().and_then(|head| head.latest_mutation_id);

        if let Some(existing_mutation) =
            self.find_existing_mutation_for_request(state, accept_command).await?
        {
            let existing_mutation_id = existing_mutation.id;
            return self
                .finalize_delete_mutation_admission(
                    state,
                    command,
                    existing_mutation,
                    base_revision_id,
                    superseded_mutation_id
                        .filter(|mutation_id| *mutation_id != existing_mutation_id),
                )
                .await;
        }

        if current_document.document_state == "deleted" || current_document.deleted_at.is_some() {
            let canonical_delete_mutation = match superseded_mutation_id {
                Some(latest_mutation_id) => match content_repository::get_mutation_by_id(
                    &state.persistence.postgres,
                    latest_mutation_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?
                {
                    Some(existing_row) if existing_row.operation_kind == "delete" => {
                        map_mutation_row(existing_row)
                    }
                    _ => self.accept_mutation(state, accept_command.clone()).await?,
                },
                None => self.accept_mutation(state, accept_command.clone()).await?,
            };
            return self
                .finalize_delete_mutation_admission(
                    state,
                    command,
                    canonical_delete_mutation.clone(),
                    base_revision_id,
                    superseded_mutation_id
                        .filter(|mutation_id| *mutation_id != canonical_delete_mutation.id),
                )
                .await;
        }

        self.ensure_document_accepts_new_mutation(
            state,
            command.document_id,
            &command.operation_kind,
        )
        .await?;
        let mutation = self.accept_mutation(state, accept_command.clone()).await?;
        self.finalize_delete_mutation_admission(
            state,
            command,
            mutation,
            base_revision_id,
            superseded_mutation_id,
        )
        .await
    }

    async fn find_existing_mutation_for_request(
        &self,
        state: &AppState,
        command: &AcceptMutationCommand,
    ) -> Result<Option<ContentMutation>, ApiError> {
        let (Some(principal_id), Some(idempotency_key)) = (
            command.requested_by_principal_id,
            command.idempotency_key.as_deref().map(str::trim).filter(|value| !value.is_empty()),
        ) else {
            return Ok(None);
        };
        let request_source_identity =
            command.source_identity.as_deref().map(str::trim).filter(|value| !value.is_empty());
        let existing = content_repository::find_mutation_by_idempotency(
            &state.persistence.postgres,
            principal_id,
            &command.request_surface,
            idempotency_key,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        if let Some(existing) = existing {
            ensure_existing_mutation_matches_request(
                &existing,
                command.workspace_id,
                command.library_id,
                &command.operation_kind,
                request_source_identity,
            )?;
            return Ok(Some(map_mutation_row(existing)));
        }
        Ok(None)
    }

    async fn get_existing_mutation_admission_for_request(
        &self,
        state: &AppState,
        command: &AcceptMutationCommand,
    ) -> Result<Option<ContentMutationAdmission>, ApiError> {
        let Some(existing) = self.find_existing_mutation_for_request(state, command).await? else {
            return Ok(None);
        };
        let admission = self.get_mutation_admission(state, existing.id).await?;
        Ok(Some(admission))
    }

    async fn create_mutation_record(
        &self,
        state: &AppState,
        command: &AcceptMutationCommand,
        mutation_state: &str,
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
                ensure_existing_mutation_matches_request(
                    &existing,
                    command.workspace_id,
                    command.library_id,
                    &command.operation_kind,
                    request_source_identity,
                )?;
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
                    mutation_state,
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
                    ensure_existing_mutation_matches_request(
                        &existing,
                        command.workspace_id,
                        command.library_id,
                        &command.operation_kind,
                        request_source_identity,
                    )?;
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
                mutation_state,
                failure_code: None,
                conflict_code: None,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_mutation_row(row))
    }

    async fn finalize_delete_mutation_admission(
        &self,
        state: &AppState,
        command: &AdmitMutationCommand,
        mutation: ContentMutation,
        base_revision_id: Option<Uuid>,
        superseded_mutation_id: Option<Uuid>,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let pending_item = self
            .ensure_delete_mutation_item(
                state,
                mutation.id,
                command.document_id,
                base_revision_id,
                "pending",
                "document delete admitted",
            )
            .await?;
        let async_operation =
            self.ensure_delete_async_operation(state, command, mutation.id).await?;
        let mutation_id = mutation.id;
        let completed_at = mutation.completed_at.unwrap_or_else(Utc::now);

        let _ = self
            .delete_document_with_context(state, command.document_id, Some(mutation.id))
            .await?;

        if let Some(superseded_mutation_id) =
            superseded_mutation_id.filter(|mutation_id| *mutation_id != mutation.id)
        {
            self.settle_deleted_document_mutation(state, superseded_mutation_id).await?;
        }

        let _ = if pending_item.item_state == "applied"
            && pending_item.base_revision_id == base_revision_id
            && pending_item.result_revision_id.is_none()
        {
            pending_item
        } else {
            self.update_mutation_item(
                state,
                UpdateMutationItemCommand {
                    item_id: pending_item.id,
                    document_id: Some(command.document_id),
                    base_revision_id,
                    result_revision_id: None,
                    item_state: "applied".to_string(),
                    message: Some("document deleted".to_string()),
                },
            )
            .await?
        };

        let _ = if mutation.mutation_state == "applied" && mutation.completed_at.is_some() {
            mutation
        } else {
            self.update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id: mutation.id,
                    mutation_state: "applied".to_string(),
                    completed_at: Some(completed_at),
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await?
        };

        if async_operation.status != "ready"
            || async_operation.completed_at.is_none()
            || async_operation.failure_code.is_some()
        {
            let _ = state
                .canonical_services
                .ops
                .update_async_operation(
                    state,
                    UpdateAsyncOperationCommand {
                        operation_id: async_operation.id,
                        status: "ready".to_string(),
                        completed_at: Some(completed_at),
                        failure_code: None,
                    },
                )
                .await?;
        }

        self.get_mutation_admission(state, mutation_id).await
    }

    async fn ensure_delete_mutation_item(
        &self,
        state: &AppState,
        mutation_id: Uuid,
        document_id: Uuid,
        base_revision_id: Option<Uuid>,
        item_state: &str,
        message: &str,
    ) -> Result<ContentMutationItem, ApiError> {
        let existing_items = self.list_mutation_items(state, mutation_id).await?;
        let existing_item = existing_items
            .iter()
            .find(|item| item.document_id == Some(document_id))
            .cloned()
            .or_else(|| existing_items.into_iter().next());

        if let Some(existing_item) = existing_item {
            if existing_item.item_state == "applied" && item_state == "pending" {
                return Ok(existing_item);
            }
            if existing_item.base_revision_id == base_revision_id
                && existing_item.result_revision_id.is_none()
                && existing_item.item_state == item_state
                && existing_item.message.as_deref() == Some(message)
            {
                return Ok(existing_item);
            }
            return self
                .update_mutation_item(
                    state,
                    UpdateMutationItemCommand {
                        item_id: existing_item.id,
                        document_id: Some(document_id),
                        base_revision_id,
                        result_revision_id: None,
                        item_state: item_state.to_string(),
                        message: Some(message.to_string()),
                    },
                )
                .await;
        }

        self.create_mutation_item(
            state,
            CreateMutationItemCommand {
                mutation_id,
                document_id: Some(document_id),
                base_revision_id,
                result_revision_id: None,
                item_state: item_state.to_string(),
                message: Some(message.to_string()),
            },
        )
        .await
    }

    async fn ensure_delete_async_operation(
        &self,
        state: &AppState,
        command: &AdmitMutationCommand,
        mutation_id: Uuid,
    ) -> Result<crate::domains::ops::OpsAsyncOperation, ApiError> {
        if let Some(existing) = state
            .canonical_services
            .ops
            .get_latest_async_operation_by_subject(state, "content_mutation", mutation_id)
            .await?
        {
            return Ok(existing);
        }

        state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: "content_mutation".to_string(),
                    surface_kind: command.request_surface.clone(),
                    requested_by_principal_id: command.requested_by_principal_id,
                    status: "accepted".to_string(),
                    subject_kind: "content_mutation".to_string(),
                    subject_id: Some(mutation_id),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await
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
                    stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
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
            .knowledge
            .set_revision_extract_state(
                state,
                context.revision_id,
                "ready",
                Some(&text),
                Some(&sha256_hex_text(&text)),
            )
            .await?;
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id: attempt.id,
                    stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("appended text materialized".to_string()),
                    details_json: serde_json::json!({ "contentLength": text.chars().count() }),
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
                    stage_name: INGEST_STAGE_PREPARE_STRUCTURE.to_string(),
                    stage_state: "started".to_string(),
                    message: Some("building structured revision from normalized text".to_string()),
                    details_json: serde_json::json!({ "revisionId": context.revision_id }),
                },
            )
            .await?;
        let extraction_plan = build_inline_text_extraction_plan(&text);
        let preparation = self
            .prepare_and_persist_revision_structure(state, context.revision_id, &extraction_plan)
            .await?;
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id: attempt.id,
                    stage_name: INGEST_STAGE_PREPARE_STRUCTURE.to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("structured revision prepared".to_string()),
                    details_json: serde_json::json!({
                        "revisionId": context.revision_id,
                        "normalizationProfile": preparation.normalization_profile,
                        "blockCount": preparation.prepared_revision.block_count,
                        "chunkCount": preparation.chunk_count,
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
                    attempt_id: attempt.id,
                    stage_name: INGEST_STAGE_CHUNK_CONTENT.to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("content chunks persisted".to_string()),
                    details_json: serde_json::json!({
                        "revisionId": context.revision_id,
                        "chunkCount": preparation.chunk_count,
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
                    attempt_id: attempt.id,
                    stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS.to_string(),
                    stage_state: "completed".to_string(),
                    message: Some("technical facts extracted from structured revision".to_string()),
                    details_json: serde_json::json!({
                        "revisionId": context.revision_id,
                        "technicalFactCount": preparation.technical_fact_count,
                        "technicalConflictCount": preparation.technical_conflict_count,
                    }),
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
        self.converge_document_technical_facts(
            state,
            context.document_id,
            Some(context.revision_id),
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
                    current_stage: Some(INGEST_STAGE_FINALIZING.to_string()),
                    failure_class: None,
                    failure_code: None,
                    retryable: false,
                },
            )
            .await?;
        self.get_mutation_admission(state, context.mutation_id).await
    }

    pub async fn converge_document_technical_facts(
        &self,
        state: &AppState,
        document_id: Uuid,
        retained_revision_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let revisions = state
            .arango_document_store
            .list_revisions_by_document(document_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        for revision in revisions {
            if Some(revision.revision_id) == retained_revision_id {
                continue;
            }
            if let Err(error) = state
                .arango_document_store
                .delete_technical_facts_by_revision(revision.revision_id)
                .await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?error,
                    "failed to delete ArangoDB technical facts for document revision"
                );
            }
        }
        Ok(())
    }

    async fn refresh_deleted_document_graph_state(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        repositories::delete_runtime_document_contribution_summary_by_document_id(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        repositories::delete_query_execution_references_by_document(
            &state.persistence.postgres,
            library_id,
            document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        repositories::deactivate_runtime_graph_evidence_by_document(
            &state.persistence.postgres,
            library_id,
            document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        // Clean up ArangoDB artifacts for all revisions of the deleted document.
        let revisions = state
            .arango_document_store
            .list_revisions_by_document(document_id)
            .await
            .unwrap_or_default();
        for revision in &revisions {
            if let Err(e) =
                state.arango_document_store.delete_chunks_by_revision(revision.revision_id).await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?e,
                    "failed to delete ArangoDB chunks for deleted document"
                );
            }
            if let Err(e) = state
                .arango_document_store
                .delete_structured_blocks_by_revision(revision.revision_id)
                .await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?e,
                    "failed to delete ArangoDB blocks for deleted document"
                );
            }
            if let Err(e) = state
                .arango_graph_store
                .delete_entity_candidates_by_revision(revision.revision_id)
                .await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?e,
                    "failed to delete ArangoDB entity candidates for deleted document"
                );
            }
            if let Err(e) = state
                .arango_graph_store
                .delete_relation_candidates_by_revision(revision.revision_id)
                .await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?e,
                    "failed to delete ArangoDB relation candidates for deleted document"
                );
            }
        }

        let projection_scope = crate::services::graph::projection::resolve_projection_scope(
            state, library_id,
        )
        .await
        .map_err(|error| {
            ApiError::SettlementRefreshFailed(format!(
                "settlement refresh failed: document delete graph projection scope failed: {error}"
            ))
        })?;
        state
            .canonical_services
            .graph
            .project_canonical_graph(state, &projection_scope)
            .await
            .map_err(|error| {
                ApiError::SettlementRefreshFailed(format!(
                    "settlement refresh failed: document delete graph projection refresh failed: {error}"
                ))
            })?;
        Ok(())
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
                    stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
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
                    stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
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
                    stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
                    stage_state: "started".to_string(),
                    message: Some("extracting graph candidates from chunks".to_string()),
                    details_json: serde_json::json!({
                        "libraryId": context.library_id,
                        "revisionId": context.revision_id,
                    }),
                },
            )
            .await?;
        let graph_materialization = self
            .materialize_revision_graph_candidates(
                state,
                MaterializeRevisionGraphCandidatesCommand {
                    workspace_id: context.workspace_id,
                    library_id: context.library_id,
                    revision_id: context.revision_id,
                    attempt_id: Some(attempt_id),
                },
            )
            .await;
        let mut graph_ready = false;

        match graph_materialization {
            Ok(graph_materialization) => {
                let graph_outcome = state
                    .canonical_services
                    .graph
                    .reconcile_revision_graph(
                        state,
                        context.library_id,
                        context.document_id,
                        context.revision_id,
                        Some(attempt_id),
                    )
                    .await;
                graph_ready = graph_outcome.as_ref().is_ok_and(|outcome| outcome.graph_ready);

                match graph_outcome {
                    Ok(graph_outcome) => {
                        if let Some(embedding_usage) = graph_outcome.embedding_usage {
                            if let Err(error) = state
                                .canonical_services
                                .billing
                                .capture_ingest_attempt(
                                    &state,
                                    CaptureIngestAttemptBillingCommand {
                                        workspace_id: context.workspace_id,
                                        library_id: context.library_id,
                                        attempt_id,
                                        binding_id: None,
                                        provider_kind: embedding_usage
                                            .provider_kind
                                            .clone()
                                            .unwrap_or_default(),
                                        model_name: embedding_usage
                                            .model_name
                                            .clone()
                                            .unwrap_or_default(),
                                        call_kind: "embed_graph".to_string(),
                                        usage_json: embedding_usage.into_usage_json(),
                                    },
                                )
                                .await
                            {
                                warn!(
                                    attempt_id = %attempt_id,
                                    ?error,
                                    "embedding billing capture failed; continuing ingest",
                                );
                            }
                        }
                        state
                            .canonical_services
                            .ingest
                            .record_stage_event(
                                state,
                                RecordStageEventCommand {
                                    attempt_id,
                                    stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
                                    stage_state: "completed".to_string(),
                                    message: Some("graph candidates extracted and reconciled".to_string()),
                                    details_json: serde_json::json!({
                                        "chunksProcessed": graph_materialization.chunk_count,
                                        "extractedEntityCandidates": graph_materialization.extracted_entities,
                                        "extractedRelationCandidates": graph_materialization.extracted_relations,
                                        "projectedNodes": graph_outcome.projection.node_count,
                                        "projectedEdges": graph_outcome.projection.edge_count,
                                        "projectionVersion": graph_outcome.projection.projection_version,
                                        "graphStatus": graph_outcome.projection.graph_status,
                                        "graphContributionCount": graph_outcome.graph_contribution_count,
                                        "graphReady": graph_ready,
                                    }),
                                },
                            )
                            .await?;
                    }
                    Err(error) => {
                        state
                            .canonical_services
                            .ingest
                            .record_stage_event(
                                state,
                                RecordStageEventCommand {
                                    attempt_id,
                                    stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
                                    stage_state: "failed".to_string(),
                                    message: Some(
                                        "inline graph rebuild failed; readable revision preserved"
                                            .to_string(),
                                    ),
                                    details_json: serde_json::json!({
                                        "chunksProcessed": graph_materialization.chunk_count,
                                        "extractedEntityCandidates": graph_materialization.extracted_entities,
                                        "extractedRelationCandidates": graph_materialization.extracted_relations,
                                        "graphReady": false,
                                        "degradedToReadable": true,
                                        "error": format!("{error:#}"),
                                    }),
                                },
                            )
                            .await?;
                    }
                }
            }
            Err(error) => {
                state
                    .canonical_services
                    .ingest
                    .record_stage_event(
                        state,
                        RecordStageEventCommand {
                            attempt_id,
                            stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
                            stage_state: "failed".to_string(),
                            message: Some(
                                "inline graph candidate extraction failed; readable revision preserved"
                                    .to_string(),
                            ),
                            details_json: serde_json::json!({
                                "graphReady": false,
                                "degradedToReadable": true,
                                "error": error.to_string(),
                            }),
                        },
                    )
                    .await?;
            }
        }

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
                if graph_ready { "ready" } else { "processing" },
                revision.text_readable_at,
                revision.vector_ready_at.or(Some(now)),
                revision.graph_ready_at.or(graph_ready.then_some(now)),
                revision.superseded_by_revision_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?;

        Ok(())
    }

    pub async fn materialize_revision_graph_candidates(
        &self,
        state: &AppState,
        command: MaterializeRevisionGraphCandidatesCommand,
    ) -> Result<RevisionGraphCandidateMaterialization, ApiError> {
        let graph_runtime_context = resolve_effective_runtime_task_context(
            state,
            command.library_id,
            &GraphExtractTask::spec(),
        )
        .await
        .map_err(|error| {
            ApiError::BadRequest(format!(
                "active extract_graph binding is required for graph extraction: {error:#}"
            ))
        })?;
        let revision = state
            .arango_document_store
            .get_revision(command.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_revision", command.revision_id)
            })?;
        let document = state
            .arango_document_store
            .get_document(revision.document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_document", revision.document_id)
            })?;
        let all_chunks = state
            .arango_document_store
            .list_chunks_by_revision(command.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let revision_facts = state
            .canonical_services
            .knowledge
            .list_typed_technical_facts(state, command.revision_id)
            .await?;
        let library_extraction_prompt = repositories::catalog_repository::get_library_by_id(
            &state.persistence.postgres,
            command.library_id,
        )
        .await
        .ok()
        .flatten()
        .and_then(|row| row.extraction_prompt);
        let chunk_count = all_chunks.len();
        let graph_extract_parallelism = state.settings.ingestion_worker_concurrency.clamp(1, 8);

        // Incremental graph update: skip chunks whose content has not changed
        // compared to the parent revision.
        let unchanged_chunk_ids =
            find_unchanged_chunks_for_revision(state, command.revision_id).await;
        let chunks: Vec<_> = if unchanged_chunk_ids.is_empty() {
            all_chunks
        } else {
            let to_process: Vec<_> = all_chunks
                .into_iter()
                .filter(|c| !unchanged_chunk_ids.contains(&c.chunk_id))
                .collect();
            tracing::info!(
                revision_id = %command.revision_id,
                total_chunks = chunk_count,
                unchanged = unchanged_chunk_ids.len(),
                to_process = to_process.len(),
                "incremental graph update: skipping unchanged chunks"
            );
            to_process
        };

        let _ = state
            .arango_graph_store
            .delete_entity_candidates_by_revision(command.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let _ = state
            .arango_graph_store
            .delete_relation_candidates_by_revision(command.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;

        let per_chunk_totals = stream::iter(chunks.into_iter().map(|chunk| {
            let state = state.clone();
            let graph_runtime_context = graph_runtime_context.clone();
            let document = document.clone();
            let revision = revision.clone();
            let command = command.clone();
            let revision_facts = revision_facts.clone();
            let library_extraction_prompt = library_extraction_prompt.clone();

            async move {
                let chunk_facts = revision_facts
                    .iter()
                    .filter(|fact| typed_fact_supports_chunk(fact, &chunk))
                    .cloned()
                    .collect::<Vec<_>>();
                let response = extract_chunk_graph_candidates(
                    &state,
                    &graph_runtime_context,
                    &build_canonical_graph_extraction_request(
                        &document,
                        &revision,
                        &chunk,
                        &chunk_facts,
                        command.attempt_id,
                        library_extraction_prompt,
                    ),
                )
                .await
                .map_err(|error| {
                    ApiError::BadRequest(format!(
                        "graph extraction failed for chunk {}: {}",
                        chunk.chunk_id, error.message
                    ))
                })?;

                let graph_extraction_id = response.graph_extraction_id.ok_or_else(|| {
                    ApiError::Conflict(
                        "graph extraction response is missing canonical graph_extraction_id"
                            .to_string(),
                    )
                })?;
                let runtime_execution_id = response.runtime_execution_id.ok_or_else(|| {
                    ApiError::Conflict(
                        "graph extraction response is missing canonical runtime_execution_id"
                            .to_string(),
                    )
                })?;
                if let Err(error) = state
                    .canonical_services
                    .billing
                    .capture_graph_extraction(
                        &state,
                        CaptureGraphExtractionBillingCommand {
                            workspace_id: command.workspace_id,
                            library_id: command.library_id,
                            graph_extraction_id,
                            runtime_execution_id,
                            binding_id: None,
                            provider_kind: response.provider_kind.clone(),
                            model_name: response.model_name.clone(),
                            usage_json: response.usage_json.clone(),
                        },
                    )
                    .await
                {
                    warn!(
                        revision_id = %command.revision_id,
                        chunk_id = %chunk.chunk_id,
                        graph_extraction_id = %graph_extraction_id,
                        runtime_execution_id = %runtime_execution_id,
                        ?error,
                        "graph extraction billing capture failed; continuing canonical graph admission",
                    );
                }

                let extracted_entities = response.normalized.entities.len();
                let extracted_relations = response.normalized.relations.len();

                Ok::<(usize, usize), ApiError>((extracted_entities, extracted_relations))
            }
        }))
        .buffer_unordered(graph_extract_parallelism)
        .try_collect::<Vec<_>>()
        .await?;

        let (extracted_entities, extracted_relations) = per_chunk_totals.into_iter().fold(
            (0usize, 0usize),
            |(entities_total, relations_total), (entities, relations)| {
                (entities_total.saturating_add(entities), relations_total.saturating_add(relations))
            },
        );

        Ok(RevisionGraphCandidateMaterialization {
            chunk_count,
            extracted_entities,
            extracted_relations,
        })
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
                    current_stage: Some(INGEST_STAGE_EXTRACT_CONTENT.to_string()),
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

    pub async fn prepare_and_persist_revision_structure(
        &self,
        state: &AppState,
        revision_id: Uuid,
        extraction_plan: &FileExtractionPlan,
    ) -> Result<PreparedRevisionPersistenceSummary, ApiError> {
        let revision = state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        let source_text = extraction_plan.source_text.clone().unwrap_or_default();
        let normalized_text =
            extraction_plan.normalized_text.clone().unwrap_or_else(|| source_text.clone());
        let mut prepared = state
            .canonical_services
            .structured_preparation
            .prepare_revision(PrepareStructuredRevisionCommand {
                revision_id,
                document_id: revision.document_id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                preparation_state: "prepared".to_string(),
                normalization_profile: extraction_plan.normalization_profile.clone(),
                source_format: extraction_plan.source_format_metadata.source_format.clone(),
                language_code: None,
                source_text,
                normalized_text: normalized_text.clone(),
                structure_hints: extraction_plan.structure_hints.clone(),
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            })
            .map_err(|error| {
                ApiError::BadRequest(format!(
                    "structured preparation failed for {revision_id}: {error}"
                ))
            })?;
        let extracted_facts = state.canonical_services.technical_facts.extract_from_blocks(
            ExtractTechnicalFactsCommand {
                revision_id,
                document_id: revision.document_id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                blocks: prepared.ordered_blocks.clone(),
            },
        );
        prepared.prepared_revision.typed_fact_count =
            i32::try_from(extracted_facts.facts.len()).unwrap_or(i32::MAX);

        let now = Utc::now();
        let _ = state
            .arango_document_store
            .upsert_structured_revision(&KnowledgeStructuredRevisionRow {
                key: revision_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                document_id: revision.document_id,
                preparation_state: prepared.prepared_revision.preparation_state.clone(),
                normalization_profile: prepared.prepared_revision.normalization_profile.clone(),
                source_format: prepared.prepared_revision.source_format.clone(),
                language_code: prepared.prepared_revision.language_code.clone(),
                block_count: prepared.prepared_revision.block_count,
                chunk_count: prepared.prepared_revision.chunk_count,
                typed_fact_count: prepared.prepared_revision.typed_fact_count,
                outline_json: serde_json::to_value(&prepared.prepared_revision.outline)
                    .unwrap_or_else(|_| serde_json::json!([])),
                prepared_at: prepared.prepared_revision.prepared_at,
                updated_at: now,
            })
            .await
            .map_err(|_| ApiError::Internal)?;
        let structured_block_rows = prepared
            .ordered_blocks
            .iter()
            .map(|block| KnowledgeStructuredBlockRow {
                key: block.block_id.to_string(),
                arango_id: None,
                arango_rev: None,
                block_id: block.block_id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                document_id: revision.document_id,
                revision_id,
                ordinal: block.ordinal,
                block_kind: block.block_kind.as_str().to_string(),
                text: block.text.clone(),
                normalized_text: block.normalized_text.clone(),
                heading_trail: block.heading_trail.clone(),
                section_path: block.section_path.clone(),
                page_number: block.page_number,
                span_start: block.source_span.as_ref().map(|span| span.start_offset),
                span_end: block.source_span.as_ref().map(|span| span.end_offset),
                parent_block_id: block.parent_block_id,
                table_coordinates_json: block.table_coordinates.as_ref().map(|coordinates| {
                    serde_json::to_value(coordinates).unwrap_or(serde_json::Value::Null)
                }),
                code_language: block.code_language.clone(),
                created_at: now,
                updated_at: now,
            })
            .collect::<Vec<_>>();
        let _ = state
            .arango_document_store
            .replace_structured_blocks(revision_id, &structured_block_rows)
            .await
            .map_err(|_| ApiError::Internal)?;

        let _ =
            content_repository::delete_chunks_by_revision(&state.persistence.postgres, revision_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        let _ =
            state.canonical_services.knowledge.delete_revision_chunks(state, revision_id).await?;
        let mut next_search_char = 0usize;
        let mut pending_chunks = Vec::with_capacity(prepared.chunk_windows.len());
        let mut knowledge_chunks = Vec::with_capacity(prepared.chunk_windows.len());
        for chunk in &prepared.chunk_windows {
            let (start_offset, end_offset) =
                locate_chunk_offsets(&normalized_text, &chunk.normalized_text, next_search_char);
            next_search_char = end_offset;
            pending_chunks.push(PendingChunkInsert {
                chunk_index: chunk.chunk_index,
                start_offset: i32::try_from(start_offset).unwrap_or(i32::MAX),
                end_offset: i32::try_from(end_offset).unwrap_or(i32::MAX),
                token_count: chunk.token_count,
                chunk_kind: Some(chunk.chunk_kind.as_str().to_string()),
                normalized_text: chunk.normalized_text.clone(),
                text_checksum: sha256_hex_text(&chunk.normalized_text),
                support_block_ids: chunk.support_block_ids.clone(),
                section_path: chunk.section_path.clone(),
                heading_trail: chunk.heading_trail.clone(),
                literal_digest: chunk.literal_digest.clone(),
                quality_score: Some(chunk.quality_score),
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
        let mut block_to_chunk_ids = std::collections::BTreeMap::<Uuid, Vec<Uuid>>::new();
        for (chunk, pending_chunk) in created_chunks.into_iter().zip(pending_chunks.iter()) {
            for block_id in &pending_chunk.support_block_ids {
                block_to_chunk_ids.entry(*block_id).or_default().push(chunk.id);
            }
            knowledge_chunks.push(CreateKnowledgeChunkCommand {
                chunk_id: chunk.id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                document_id: revision.document_id,
                revision_id,
                chunk_index: chunk.chunk_index,
                chunk_kind: pending_chunk.chunk_kind.clone(),
                content_text: pending_chunk.normalized_text.clone(),
                normalized_text: chunk.normalized_text,
                span_start: Some(chunk.start_offset),
                span_end: Some(chunk.end_offset),
                token_count: chunk.token_count,
                support_block_ids: pending_chunk.support_block_ids.clone(),
                section_path: pending_chunk.section_path.clone(),
                heading_trail: pending_chunk.heading_trail.clone(),
                literal_digest: pending_chunk.literal_digest.clone(),
                chunk_state: "ready".to_string(),
                text_generation: Some(revision.revision_number),
                vector_generation: None,
                quality_score: pending_chunk.quality_score,
            });
        }
        let _ = state.canonical_services.knowledge.write_chunks(state, knowledge_chunks).await?;

        let technical_fact_rows = extracted_facts
            .facts
            .iter()
            .map(|fact| {
                let support_chunk_ids = fact
                    .support_block_ids
                    .iter()
                    .filter_map(|block_id| block_to_chunk_ids.get(block_id))
                    .flatten()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                KnowledgeTechnicalFactRow {
                    key: fact.fact_id.to_string(),
                    arango_id: None,
                    arango_rev: None,
                    fact_id: fact.fact_id,
                    workspace_id: fact.workspace_id,
                    library_id: fact.library_id,
                    document_id: fact.document_id,
                    revision_id: fact.revision_id,
                    fact_kind: fact.fact_kind.as_str().to_string(),
                    canonical_value_text: fact.canonical_value.canonical_string(),
                    canonical_value_exact: fact
                        .canonical_value
                        .canonical_string()
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .collect(),
                    canonical_value_json: serde_json::to_value(&fact.canonical_value)
                        .unwrap_or(serde_json::Value::Null),
                    display_value: fact.display_value.clone(),
                    qualifiers_json: serde_json::to_value(&fact.qualifiers)
                        .unwrap_or_else(|_| serde_json::json!([])),
                    support_block_ids: fact.support_block_ids.clone(),
                    support_chunk_ids,
                    confidence: fact.confidence,
                    extraction_kind: fact.extraction_kind.clone(),
                    conflict_group_id: fact.conflict_group_id.clone(),
                    created_at: fact.created_at,
                    updated_at: now,
                }
            })
            .collect::<Vec<_>>();
        let _ = state
            .arango_document_store
            .replace_technical_facts(revision_id, &technical_fact_rows)
            .await
            .map_err(|_| ApiError::Internal)?;

        Ok(PreparedRevisionPersistenceSummary {
            prepared_revision: map_structured_revision_data(&prepared.prepared_revision),
            chunk_count: prepared.chunk_windows.len(),
            technical_fact_count: extracted_facts.facts.len(),
            technical_conflict_count: extracted_facts.conflicts.len(),
            normalization_profile: prepared.prepared_revision.normalization_profile.clone(),
        })
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
        operation_kind: &str,
    ) -> Result<(), ApiError> {
        let document =
            content_repository::get_document_by_id(&state.persistence.postgres, document_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        if document.document_state == "deleted" || document.deleted_at.is_some() {
            return Err(ApiError::BadRequest(if operation_kind == "delete" {
                "document is already deleted".to_string()
            } else {
                "deleted documents do not accept new mutations".to_string()
            }));
        }
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
        if operation_kind != "delete"
            && matches!(latest_mutation_state.as_str(), "accepted" | "running")
        {
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
        let readable_revision_row =
            match (document_row.readable_revision_id, active_revision_row.as_ref()) {
                (Some(readable_revision_id), Some(active_row))
                    if readable_revision_id == active_row.revision_id =>
                {
                    Some(active_row.clone())
                }
                (Some(readable_revision_id), _) => state
                    .arango_document_store
                    .get_revision(readable_revision_id)
                    .await
                    .map_err(|_| ApiError::Internal)?,
                (None, _) => None,
            };
        let effective_readiness_row =
            readable_revision_row.clone().or_else(|| active_revision_row.clone());
        let prepared_revision_row =
            match effective_readiness_row.as_ref().map(|row| row.revision_id) {
                Some(revision_id) => state
                    .arango_document_store
                    .get_structured_revision(revision_id)
                    .await
                    .map_err(|_| ApiError::Internal)?,
                None => None,
            };
        let web_page_row = match active_revision_row
            .as_ref()
            .filter(|revision| revision.revision_kind == "web_page")
            .map(|revision| revision.revision_id)
        {
            Some(revision_id) => ingest_repository::get_web_discovered_page_by_result_revision_id(
                &state.persistence.postgres,
                revision_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?,
            None => None,
        };

        let mut revisions_by_id = HashMap::new();
        if let Some(revision) = active_revision_row.clone() {
            revisions_by_id.insert(revision.revision_id, revision);
        }
        if let Some(revision) = readable_revision_row {
            revisions_by_id.entry(revision.revision_id).or_insert(revision);
        }
        if let Some(revision) = effective_readiness_row.clone() {
            revisions_by_id.entry(revision.revision_id).or_insert(revision);
        }

        let mut structured_revisions_by_revision_id = HashMap::new();
        if let Some(revision) = prepared_revision_row {
            structured_revisions_by_revision_id.insert(revision.revision_id, revision);
        }

        let mut web_pages_by_result_revision_id = HashMap::new();
        if let Some(page) = web_page_row
            .and_then(|row| row.result_revision_id.map(|revision_id| (revision_id, row)))
        {
            web_pages_by_result_revision_id.insert(page.0, page.1);
        }

        Ok(self.build_document_summary_from_prefetched(
            state,
            document_row,
            content_head,
            latest_mutation,
            latest_job,
            &PrefetchedDocumentSummaryData {
                revisions_by_id,
                structured_revisions_by_revision_id,
                web_pages_by_result_revision_id,
            },
        ))
    }

    async fn prefetch_document_summary_data(
        &self,
        state: &AppState,
        documents: &[KnowledgeDocumentRow],
    ) -> Result<PrefetchedDocumentSummaryData, ApiError> {
        let revision_ids = documents
            .iter()
            .flat_map(|document| [document.active_revision_id, document.readable_revision_id])
            .flatten()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let revisions_by_id = state
            .arango_document_store
            .list_revisions_by_ids(&revision_ids)
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .map(|row| (row.revision_id, row))
            .collect::<HashMap<_, _>>();

        let effective_revision_ids = documents
            .iter()
            .filter_map(|document| {
                self.resolve_effective_readiness_row(document, None, &revisions_by_id)
            })
            .map(|revision| revision.revision_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let structured_revisions_by_revision_id = state
            .arango_document_store
            .list_structured_revisions_by_revision_ids(&effective_revision_ids)
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .map(|row| (row.revision_id, row))
            .collect::<HashMap<_, _>>();

        let web_page_revision_ids = documents
            .iter()
            .filter_map(|document| document.active_revision_id)
            .filter(|revision_id| {
                revisions_by_id
                    .get(revision_id)
                    .is_some_and(|revision| revision.revision_kind == "web_page")
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let web_pages_by_result_revision_id =
            ingest_repository::list_web_discovered_pages_by_result_revision_ids(
                &state.persistence.postgres,
                &web_page_revision_ids,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .filter_map(|row| row.result_revision_id.map(|revision_id| (revision_id, row)))
            .collect::<HashMap<_, _>>();

        Ok(PrefetchedDocumentSummaryData {
            revisions_by_id,
            structured_revisions_by_revision_id,
            web_pages_by_result_revision_id,
        })
    }

    fn build_document_summary_from_prefetched(
        &self,
        state: &AppState,
        document_row: KnowledgeDocumentRow,
        content_head: Option<&content_repository::ContentDocumentHeadRow>,
        latest_mutation: Option<ContentMutation>,
        latest_job: Option<ContentDocumentPipelineJob>,
        prefetched: &PrefetchedDocumentSummaryData,
    ) -> ContentDocumentSummary {
        let document_external_key = document_row.external_key.clone();
        let deleted_document =
            document_row.document_state == "deleted" || document_row.deleted_at.is_some();
        let active_revision_row = document_row
            .active_revision_id
            .and_then(|revision_id| prefetched.revisions_by_id.get(&revision_id).cloned());
        let active_revision = active_revision_row.clone().map(map_knowledge_revision_row);
        let display_revision_row = self.resolve_effective_readiness_row(
            &document_row,
            active_revision_row.as_ref(),
            &prefetched.revisions_by_id,
        );
        let effective_readiness_row =
            (!deleted_document).then_some(display_revision_row.clone()).flatten();
        let prepared_revision = effective_readiness_row
            .as_ref()
            .and_then(|revision| {
                prefetched.structured_revisions_by_revision_id.get(&revision.revision_id)
            })
            .cloned()
            .map(map_structured_revision_row);
        let head = Some(ContentDocumentHead {
            document_id: document_row.document_id,
            active_revision_id: document_row.active_revision_id,
            readable_revision_id: document_row.readable_revision_id,
            latest_mutation_id: content_head.and_then(|row| row.latest_mutation_id),
            latest_successful_attempt_id: content_head
                .and_then(|row| row.latest_successful_attempt_id),
            head_updated_at: content_head
                .map_or(document_row.updated_at, |row| row.head_updated_at),
            document_summary: content_head.and_then(|row| row.document_summary.clone()),
        });
        let readiness_summary = (!deleted_document).then(|| {
            state.canonical_services.ops.derive_document_readiness_summary(
                state,
                document_row.document_id,
                document_row.active_revision_id,
                effective_readiness_row.as_ref(),
                prepared_revision.as_ref(),
                latest_mutation.as_ref(),
                latest_job.as_ref(),
                document_row.created_at,
            )
        });
        let source_descriptor = effective_readiness_row.as_ref().map(|revision| {
            describe_content_source(
                revision.document_id,
                Some(revision.revision_id),
                &revision.revision_kind,
                revision.source_uri.as_deref(),
                revision.storage_ref.as_deref(),
                revision.title.as_deref(),
                &document_external_key,
            )
        });
        let fallback_file_name = display_revision_row.as_ref().map(|revision| {
            derive_content_source_file_name(
                revision.source_uri.as_deref(),
                revision.title.as_deref(),
                &document_external_key,
            )
        });
        let web_page_provenance = effective_readiness_row
            .as_ref()
            .filter(|revision| revision.revision_kind == "web_page")
            .and_then(|revision| {
                prefetched
                    .web_pages_by_result_revision_id
                    .get(&revision.revision_id)
                    .map(map_web_page_provenance_row)
                    .or_else(|| {
                        Some(WebPageProvenance {
                            run_id: None,
                            candidate_id: None,
                            source_uri: revision.source_uri.clone(),
                            canonical_url: revision.source_uri.clone(),
                        })
                    })
            });

        ContentDocumentSummary {
            document: map_knowledge_document_row(&document_row),
            file_name: document_row
                .file_name
                .clone()
                .or_else(|| {
                    source_descriptor.as_ref().map(|descriptor| descriptor.file_name.clone())
                })
                .or(fallback_file_name)
                .or_else(|| active_revision.as_ref().and_then(|r| r.title.clone()))
                .unwrap_or_else(|| document_external_key.clone()),
            head,
            active_revision,
            source_access: source_descriptor.and_then(|descriptor| descriptor.access),
            readiness: effective_readiness_row.map(map_knowledge_revision_readiness),
            readiness_summary,
            prepared_revision,
            web_page_provenance,
            pipeline: ContentDocumentPipelineState { latest_mutation, latest_job },
        }
    }

    fn resolve_effective_readiness_row(
        &self,
        document_row: &KnowledgeDocumentRow,
        active_revision_row: Option<&KnowledgeRevisionRow>,
        revisions_by_id: &HashMap<Uuid, KnowledgeRevisionRow>,
    ) -> Option<KnowledgeRevisionRow> {
        match (
            document_row.readable_revision_id,
            document_row.active_revision_id,
            active_revision_row,
        ) {
            (Some(readable_revision_id), Some(active_revision_id), Some(active_row))
                if readable_revision_id == active_revision_id =>
            {
                Some(active_row.clone())
            }
            (Some(readable_revision_id), _, _) => revisions_by_id
                .get(&readable_revision_id)
                .cloned()
                .or_else(|| active_revision_row.cloned()),
            (None, Some(_), Some(active_row)) => Some(active_row.clone()),
            (None, Some(active_revision_id), None) => {
                revisions_by_id.get(&active_revision_id).cloned()
            }
            (None, None, _) => None,
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

fn segment_excerpt(text: &str) -> String {
    const EXCERPT_LIMIT: usize = 180;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= EXCERPT_LIMIT {
        compact
    } else {
        let prefix = compact.chars().take(EXCERPT_LIMIT).collect::<String>();
        format!("{prefix}...")
    }
}

fn map_knowledge_document_row(row: &KnowledgeDocumentRow) -> ContentDocument {
    ContentDocument {
        id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key.clone(),
        document_state: row.document_state.clone(),
        created_at: row.created_at,
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

fn map_structured_revision_row(row: KnowledgeStructuredRevisionRow) -> StructuredDocumentRevision {
    let outline = serde_json::from_value(row.outline_json).unwrap_or_default();
    StructuredDocumentRevision {
        revision_id: row.revision_id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        preparation_state: row.preparation_state,
        normalization_profile: row.normalization_profile,
        source_format: row.source_format,
        language_code: row.language_code,
        block_count: row.block_count,
        chunk_count: row.chunk_count,
        typed_fact_count: row.typed_fact_count,
        outline,
        prepared_at: row.prepared_at,
    }
}

fn map_web_page_provenance_row(row: &ingest_repository::WebDiscoveredPageRow) -> WebPageProvenance {
    WebPageProvenance {
        run_id: Some(row.run_id),
        candidate_id: Some(row.id),
        source_uri: row.final_url.clone().or(row.discovered_url.clone()),
        canonical_url: row.canonical_url.clone().or(row.final_url.clone()),
    }
}

fn map_structured_revision_data(
    data: &crate::shared::extraction::structured_document::StructuredDocumentRevisionData,
) -> StructuredDocumentRevision {
    StructuredDocumentRevision {
        revision_id: data.revision_id,
        document_id: data.document_id,
        workspace_id: data.workspace_id,
        library_id: data.library_id,
        preparation_state: data.preparation_state.clone(),
        normalization_profile: data.normalization_profile.clone(),
        source_format: data.source_format.clone(),
        language_code: data.language_code.clone(),
        block_count: data.block_count,
        chunk_count: data.chunk_count,
        typed_fact_count: data.typed_fact_count,
        outline: data.outline.clone(),
        prepared_at: data.prepared_at,
    }
}

fn map_knowledge_chunk_row(row: KnowledgeChunkRow) -> ContentChunk {
    let start_offset = row.span_start.unwrap_or(0);
    let end_offset = row.span_end.unwrap_or_else(|| {
        start_offset.saturating_add(i32::try_from(row.normalized_text.len()).unwrap_or(0))
    });
    let checksum =
        format!("sha256:{}", hex::encode(Sha256::digest(row.normalized_text.as_bytes())));
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
        claimed_at: latest_attempt.as_ref().map(|attempt| attempt.started_at),
        last_activity_at: latest_attempt
            .as_ref()
            .and_then(|attempt| {
                attempt.heartbeat_at.or(attempt.finished_at).or(Some(attempt.started_at))
            })
            .or(handle.job.completed_at),
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
    request_workspace_id: Uuid,
    request_library_id: Uuid,
    request_operation_kind: &str,
    request_source_identity: Option<&str>,
) -> Result<(), ApiError> {
    if existing.workspace_id != request_workspace_id
        || existing.library_id != request_library_id
        || existing.operation_kind != request_operation_kind
    {
        return Err(ApiError::idempotency_conflict(
            "the same idempotency key was already used for a different mutation request",
        ));
    }
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

fn build_canonical_graph_extraction_request(
    document: &KnowledgeDocumentRow,
    revision: &KnowledgeRevisionRow,
    chunk: &KnowledgeChunkRow,
    technical_facts: &[TypedTechnicalFact],
    attempt_id: Option<Uuid>,
    library_extraction_prompt: Option<String>,
) -> GraphExtractionRequest {
    GraphExtractionRequest {
        library_id: revision.library_id,
        document: repositories::DocumentRow {
            id: document.document_id,
            library_id: document.library_id,
            source_id: None,
            external_key: document.external_key.clone(),
            title: document.title.clone(),
            mime_type: Some(revision.mime_type.clone()),
            checksum: Some(revision.checksum.clone()),
            active_revision_id: Some(revision.revision_id),
            document_state: document.document_state.clone(),
            mutation_kind: None,
            mutation_status: None,
            deleted_at: document.deleted_at,
            created_at: document.created_at,
            updated_at: document.updated_at,
        },
        chunk: repositories::ChunkRow {
            id: chunk.chunk_id,
            document_id: chunk.document_id,
            library_id: chunk.library_id,
            ordinal: chunk.chunk_index,
            content: chunk.content_text.clone(),
            token_count: chunk.token_count,
            metadata_json: serde_json::json!({
                "chunk_kind": chunk.chunk_kind,
                "support_block_ids": chunk.support_block_ids,
                "section_path": chunk.section_path,
                "heading_trail": chunk.heading_trail,
                "literal_digest": chunk.literal_digest,
                "chunk_state": chunk.chunk_state,
                "text_generation": chunk.text_generation,
                "vector_generation": chunk.vector_generation,
            }),
            created_at: revision.created_at,
        },
        structured_chunk: GraphExtractionStructuredChunkContext {
            chunk_kind: chunk.chunk_kind.clone(),
            section_path: chunk.section_path.clone(),
            heading_trail: chunk.heading_trail.clone(),
            support_block_ids: chunk.support_block_ids.clone(),
            literal_digest: chunk.literal_digest.clone(),
        },
        technical_facts: technical_facts
            .iter()
            .map(|fact| GraphExtractionTechnicalFact {
                fact_kind: fact.fact_kind.as_str().to_string(),
                canonical_value: fact.canonical_value.canonical_string(),
                display_value: fact.display_value.clone(),
                qualifiers: fact.qualifiers.clone(),
            })
            .collect(),
        revision_id: Some(revision.revision_id),
        activated_by_attempt_id: attempt_id,
        resume_hint: None,
        library_extraction_prompt,
    }
}

/// For document updates, find chunks that haven't changed by comparing `literal_digest`.
/// Returns the set of `chunk_id`s that can skip graph extraction.
async fn find_unchanged_chunks_for_revision(
    state: &AppState,
    current_revision_id: Uuid,
) -> HashSet<Uuid> {
    // Look up the content revision to find a parent revision.
    let content_revision = match repositories::content_repository::get_revision_by_id(
        &state.persistence.postgres,
        current_revision_id,
    )
    .await
    {
        Ok(Some(rev)) => rev,
        _ => return HashSet::new(),
    };

    let parent_revision_id = match content_revision.parent_revision_id {
        Some(id) => id,
        None => return HashSet::new(),
    };

    // Load chunks from both revisions.
    let current_chunks =
        match state.arango_document_store.list_chunks_by_revision(current_revision_id).await {
            Ok(chunks) => chunks,
            Err(_) => return HashSet::new(),
        };

    let parent_chunks =
        match state.arango_document_store.list_chunks_by_revision(parent_revision_id).await {
            Ok(chunks) => chunks,
            Err(_) => return HashSet::new(),
        };

    // Build digest set for parent chunks.
    let parent_digests: HashSet<String> =
        parent_chunks.iter().filter_map(|c| c.literal_digest.clone()).collect();

    // Find current chunks whose digest matches a parent chunk.
    current_chunks
        .iter()
        .filter_map(|c| {
            c.literal_digest
                .as_ref()
                .filter(|d| parent_digests.contains(d.as_str()))
                .map(|_| c.chunk_id)
        })
        .collect()
}

fn typed_fact_supports_chunk(fact: &TypedTechnicalFact, chunk: &KnowledgeChunkRow) -> bool {
    fact.support_chunk_ids.contains(&chunk.chunk_id)
        || fact.support_block_ids.iter().any(|block_id| chunk.support_block_ids.contains(block_id))
}

fn validate_extraction_plan(
    file_name: &str,
    mime_type: Option<&str>,
    file_size_bytes: u64,
    extraction_plan: &FileExtractionPlan,
) -> Result<(), UploadAdmissionError> {
    if extraction_plan.file_kind == UploadFileKind::TextLike
        && extraction_plan.normalized_text.as_deref().is_some_and(|text| text.trim().is_empty())
    {
        return Err(UploadAdmissionError::from_file_extract_error(
            file_name,
            mime_type,
            file_size_bytes,
            &FileExtractError::ExtractionFailed {
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
