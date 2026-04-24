use std::collections::HashMap;

use chrono::Utc;
use futures::{StreamExt, TryStreamExt, stream};
use tracing::warn;
use uuid::Uuid;

use crate::{
    agent_runtime::{task::RuntimeTask, tasks::graph_extract::GraphExtractTask},
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::content::{ContentDocument, ContentDocumentHead, ContentRevision},
    domains::ops::ASYNC_OP_STATUS_READY,
    domains::provider_profiles::ProviderModelSelection,
    infra::arangodb::document_store::{
        KnowledgeStructuredBlockRow, KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow,
    },
    infra::repositories::{
        self as repositories, catalog_repository,
        content_repository::{
            self, NewContentDocument, NewContentDocumentHead, NewContentRevision,
        },
    },
    interfaces::http::router_support::ApiError,
    services::{
        content::source_access::derive_storage_backed_content_file_name,
        content::storage::ContentStorageService,
        graph::extract::{
            GraphExtractionSubTypeHintEntry, GraphExtractionSubTypeHintGroup,
            GraphExtractionSubTypeHints, extract_chunk_graph_candidates,
        },
        graph::projection::resolve_projection_scope,
        ingest::runtime::resolve_effective_runtime_task_context,
        ingest::service::{INGEST_STAGE_EXTRACT_CONTENT, LeaseAttemptCommand},
        ingest::structured_preparation::PrepareStructuredRevisionCommand,
        ingest::technical_facts::ExtractTechnicalFactsCommand,
        knowledge::service::{
            CreateKnowledgeChunkCommand, CreateKnowledgeDocumentCommand,
            CreateKnowledgeRevisionCommand, PromoteKnowledgeDocumentCommand,
        },
        ops::billing::CaptureGraphExtractionBillingCommand,
    },
    shared::extraction::file_extract::{
        FileExtractError, FileExtractionPlan, UploadAdmissionError, UploadFileKind,
        build_runtime_file_extraction_plan, validate_upload_file_admission,
    },
    shared::extraction::{
        table_graph::{TableGraphProfile, build_table_graph_profile},
        table_summary::{is_table_summary_text, parse_table_column_summary},
    },
};

use super::pipeline::{
    build_canonical_graph_extraction_request, build_graph_chunk_content, typed_fact_supports_chunk,
};

/// Locally cached copy of the canonical graph extraction prompt version.
/// Diff-aware reuse only fires when the parent revision used the same version.
const GRAPH_EXTRACTION_VERSION_FOR_REUSE: &str = "graph_extract_v6";
use super::{
    ContentMutationAdmission, ContentService, CreateDocumentCommand, CreateRevisionCommand,
    EditableDocumentContext, InlineMutationContext, MaterializeRevisionGraphCandidatesCommand,
    PendingChunkInsert, PreparedRevisionPersistenceSummary, PromoteHeadCommand,
    RevisionAdmissionMetadata, RevisionGraphCandidateMaterialization, locate_chunk_offsets,
    map_revision_row, map_structured_revision_data, sha256_hex_text,
};

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

impl ContentService {
    pub async fn build_runtime_extraction_plan(
        &self,
        state: &AppState,
        library_id: Uuid,
        file_name: &str,
        mime_type: Option<&str>,
        file_bytes: &[u8],
    ) -> Result<FileExtractionPlan, UploadAdmissionError> {
        let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        // Vision binding is only needed for image/PDF files that might
        // contain images. For text/markdown/CSV/code files, the absence
        // of a vision binding should NOT block extraction — the pipeline
        // just skips the image description step.
        let vision_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::Vision)
            .await
            .unwrap_or(None);
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

    pub(crate) fn validate_inline_file_admission(
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !exists {
            return Ok(None);
        }

        content_repository::update_revision_storage_key(
            &state.persistence.postgres,
            revision_id,
            Some(&storage_key),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        if let Err(error) = state
            .canonical_services
            .knowledge
            .set_revision_storage_ref(state, revision_id, Some(&storage_key))
            .await
        {
            warn!(
                %revision_id,
                storage_key = %storage_key,
                ?error,
                "post-storage-key-sync failed after canonical revision storage update"
            );
        }
        Ok(Some(storage_key))
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let document = ContentDocument {
            id: row.id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            external_key: row.external_key.clone(),
            document_state: row.document_state.clone(),
            created_at: row.created_at,
        };
        if let Err(error) = state
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
            .await
        {
            tracing::warn!(
                document_id = %document.id,
                library_id = %document.library_id,
                ?error,
                "post-create knowledge document shell sync failed after canonical document create"
            );
        }
        Ok(document)
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        if document.document_state == "deleted" || document.deleted_at.is_some() {
            return Err(ApiError::BadRequest(
                "deleted documents do not accept new revisions".to_string(),
            ));
        }
        let latest = content_repository::get_latest_revision_for_document(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let next_revision_number = latest
            .as_ref()
            .map(|row| row.revision_number)
            .map_or(1, |value| value.saturating_add(1));
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let revision = map_revision_row(row);
        if let Err(error) = state
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
            .await
        {
            warn!(
                revision_id = %revision.id,
                document_id = %revision.document_id,
                ?error,
                "post-create knowledge revision sync failed after canonical revision create"
            );
        }
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

    pub async fn promote_document_head(
        &self,
        state: &AppState,
        command: PromoteHeadCommand,
    ) -> Result<ContentDocumentHead, ApiError> {
        if let Some(active_revision_id) = command.active_revision_id {
            content_repository::get_revision_by_id(&state.persistence.postgres, active_revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("revision", active_revision_id))?;
        }
        if let Some(readable_revision_id) = command.readable_revision_id {
            content_repository::get_revision_by_id(
                &state.persistence.postgres,
                readable_revision_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let document = content_repository::get_document_by_id(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        let latest_revision_no =
            self.load_document_latest_revision_no(state, command.document_id).await?;
        self.promote_knowledge_document(
            state,
            PromoteKnowledgeDocumentCommand {
                document_id: command.document_id,
                document_state: document.document_state,
                active_revision_id: command.active_revision_id,
                readable_revision_id: command.readable_revision_id,
                latest_revision_no,
                deleted_at: document.deleted_at,
            },
            "knowledge document sync failed after canonical head update; Postgres head is committed and the Arango mirror may be stale until retry",
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

    pub(crate) async fn promote_knowledge_document(
        &self,
        state: &AppState,
        command: PromoteKnowledgeDocumentCommand,
        failure_message: &'static str,
    ) -> Result<(), ApiError> {
        state
            .canonical_services
            .knowledge
            .promote_document(state, command.clone())
            .await
            .map(|_| ())
            .map_err(|error| {
                tracing::error!(
                    document_id = %command.document_id,
                    ?error,
                    "{failure_message}"
                );
                match error {
                    ApiError::Internal => ApiError::InternalMessage(failure_message.to_string()),
                    other => other,
                }
            })
    }

    pub(crate) async fn promote_pending_document_mutation_head(
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

    pub(crate) async fn load_document_latest_revision_no(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Option<i64>, ApiError> {
        Ok(content_repository::get_latest_revision_for_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .map(|revision| i64::from(revision.revision_number)))
    }

    pub(crate) async fn create_revision_from_metadata(
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

    pub(crate) async fn persist_inline_file_source(
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))
    }

    pub(super) async fn lease_inline_attempt(
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

    pub(super) fn inline_mutation_context_from_admission(
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

    pub(super) async fn load_editable_document_context(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<EditableDocumentContext, ApiError> {
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
                    "document is not readable enough for inline text mutations".to_string(),
                )
            })?;
        let base_revision = content_repository::get_revision_by_id(
            &state.persistence.postgres,
            readable_revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("revision", readable_revision_id))?;
        Ok(EditableDocumentContext {
            current_content,
            mime_type: base_revision.mime_type,
            title: base_revision.title.or_else(|| Some(document_id.to_string())),
            language_code: None,
        })
    }

    pub async fn prepare_and_persist_revision_structure(
        &self,
        state: &AppState,
        revision_id: Uuid,
        extraction_plan: &FileExtractionPlan,
    ) -> Result<PreparedRevisionPersistenceSummary, ApiError> {
        let revision =
            content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        let source_text = extraction_plan.source_text.clone().unwrap_or_default();
        let normalized_text =
            extraction_plan.normalized_text.clone().unwrap_or_else(|| source_text.clone());

        let prepare_structure_start = std::time::Instant::now();
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
        let prepare_structure_elapsed_ms = prepare_structure_start.elapsed().as_millis() as i64;

        let extract_technical_facts_start = std::time::Instant::now();
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
        let extract_technical_facts_elapsed_ms =
            extract_technical_facts_start.elapsed().as_millis() as i64;

        let chunk_content_start = std::time::Instant::now();
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let _ =
            content_repository::delete_chunks_by_revision(&state.persistence.postgres, revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let _ =
            state.canonical_services.knowledge.delete_revision_chunks(state, revision_id).await?;
        let mut next_search_char = 0usize;
        let mut pending_chunks = Vec::with_capacity(prepared.chunk_windows.len());
        let mut knowledge_chunks = Vec::with_capacity(prepared.chunk_windows.len());
        for chunk in &prepared.chunk_windows {
            let (start_offset, end_offset) =
                locate_chunk_offsets(&normalized_text, &chunk.content_text, next_search_char);
            next_search_char = end_offset;
            pending_chunks.push(PendingChunkInsert {
                chunk_index: chunk.chunk_index,
                start_offset: i32::try_from(start_offset).unwrap_or(i32::MAX),
                end_offset: i32::try_from(end_offset).unwrap_or(i32::MAX),
                token_count: chunk.token_count,
                chunk_kind: Some(chunk.chunk_kind.as_str().to_string()),
                content_text: chunk.content_text.clone(),
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
                content_text: pending_chunk.content_text.clone(),
                normalized_text: chunk.normalized_text,
                span_start: Some(chunk.start_offset),
                span_end: Some(chunk.end_offset),
                token_count: chunk.token_count,
                support_block_ids: pending_chunk.support_block_ids.clone(),
                section_path: pending_chunk.section_path.clone(),
                heading_trail: pending_chunk.heading_trail.clone(),
                literal_digest: pending_chunk.literal_digest.clone(),
                chunk_state: "ready".to_string(),
                text_generation: Some(i64::from(revision.revision_number)),
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let chunk_content_elapsed_ms = chunk_content_start.elapsed().as_millis() as i64;

        Ok(PreparedRevisionPersistenceSummary {
            prepared_revision: map_structured_revision_data(&prepared.prepared_revision),
            chunk_count: prepared.chunk_windows.len(),
            technical_fact_count: extracted_facts.facts.len(),
            technical_conflict_count: extracted_facts.conflicts.len(),
            normalization_profile: prepared.prepared_revision.normalization_profile.clone(),
            prepare_structure_elapsed_ms,
            chunk_content_elapsed_ms,
            extract_technical_facts_elapsed_ms,
        })
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_revision", command.revision_id)
            })?;
        let document = state
            .arango_document_store
            .get_document(revision.document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_document", revision.document_id)
            })?;
        let all_chunks = state
            .arango_document_store
            .list_chunks_by_revision(command.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let revision_facts = state
            .canonical_services
            .knowledge
            .list_typed_technical_facts(state, command.revision_id)
            .await?;
        let structured_blocks = state
            .arango_document_store
            .list_structured_blocks_by_revision(command.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let table_graph_context = build_revision_table_graph_context(&structured_blocks);
        let library_extraction_prompt =
            catalog_repository::get_library_by_id(&state.persistence.postgres, command.library_id)
                .await
                .ok()
                .flatten()
                .and_then(|row| row.extraction_prompt);
        let sub_type_hints = load_sub_type_hints_for_extraction(state, command.library_id).await;
        let chunk_count = all_chunks.len();
        let graph_extract_parallelism =
            state.settings.ingestion_graph_extract_parallelism_per_doc.max(1);

        let chunks = all_chunks;

        let _ = state
            .arango_graph_store
            .delete_entity_candidates_by_revision(command.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let _ = state
            .arango_graph_store
            .delete_relation_candidates_by_revision(command.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        // ----------------------------------------------------------------
        // Diff-aware ingest: reuse extraction output from a parent revision
        // when a chunk's text checksum is unchanged.
        //
        // The reconcile step downstream (services/graph/rebuild.rs) reads
        // `runtime_graph_extraction` records by document and merges them into
        // the canonical graph. If we synthesize records for the new revision
        // by copying the parent's normalized output (re-keyed to the new
        // chunk_id), the reconcile flow treats them identically and we skip
        // the LLM call for unchanged chunks entirely.
        // ----------------------------------------------------------------
        let reuse_plan = self.build_chunk_reuse_plan(state, &command, chunks.as_slice()).await?;
        let reused_chunk_ids: std::collections::BTreeSet<Uuid> =
            reuse_plan.records_by_new_chunk.keys().copied().collect();
        let mut reused_entity_count = 0usize;
        let mut reused_relation_count = 0usize;

        for (new_chunk_id, old_record) in &reuse_plan.records_by_new_chunk {
            // Count what we are about to reuse — pulled from the old normalized
            // output to surface in the lifecycle summary.
            if let Some(entities) = old_record.normalized_output_json.get("entities")
                && let Some(arr) = entities.as_array()
            {
                reused_entity_count = reused_entity_count.saturating_add(arr.len());
            }
            if let Some(relations) = old_record.normalized_output_json.get("relations")
                && let Some(arr) = relations.as_array()
            {
                reused_relation_count = reused_relation_count.saturating_add(arr.len());
            }

            // The downstream reconcile step filters extraction records by
            // lifecycle.revision_id. Cloning the parent's raw_output_json
            // verbatim would carry the OLD revision_id and cause every reused
            // chunk to be silently dropped during merge. Rewrite lifecycle to
            // point at the current revision before persisting.
            let mut raw_output_json = old_record.raw_output_json.clone();
            if let Some(obj) = raw_output_json.as_object_mut() {
                let lifecycle = obj.entry("lifecycle").or_insert_with(|| serde_json::json!({}));
                if let Some(lifecycle_obj) = lifecycle.as_object_mut() {
                    lifecycle_obj.insert(
                        "revision_id".to_string(),
                        serde_json::Value::String(command.revision_id.to_string()),
                    );
                }
            }

            let synthetic_id = Uuid::now_v7();
            crate::infra::repositories::create_runtime_graph_extraction_record(
                &state.persistence.postgres,
                &crate::infra::repositories::CreateRuntimeGraphExtractionRecordInput {
                    id: synthetic_id,
                    runtime_execution_id: old_record.runtime_execution_id,
                    library_id: command.library_id,
                    document_id: revision.document_id,
                    chunk_id: *new_chunk_id,
                    provider_kind: format!("{}+reuse", old_record.provider_kind),
                    model_name: old_record.model_name.clone(),
                    extraction_version: old_record.extraction_version.clone(),
                    prompt_hash: old_record.prompt_hash.clone(),
                    status: "ready".to_string(),
                    raw_output_json,
                    normalized_output_json: old_record.normalized_output_json.clone(),
                    glean_pass_count: 0,
                    error_message: None,
                },
            )
            .await
            .map_err(|e| {
                ApiError::internal_with_log(
                    e,
                    "create_runtime_graph_extraction_record (diff reuse)",
                )
            })?;
        }

        if !reuse_plan.records_by_new_chunk.is_empty() {
            tracing::info!(
                revision_id = %command.revision_id,
                total_chunks = chunk_count,
                reused = reuse_plan.records_by_new_chunk.len(),
                reused_entities = reused_entity_count,
                reused_relations = reused_relation_count,
                "diff-aware ingest: reusing graph extraction output for unchanged chunks",
            );
        }

        // Filter out reused chunks from the extraction loop — they are already
        // covered by the synthetic records we just inserted.
        let chunks: Vec<_> = chunks
            .into_iter()
            .filter(|chunk| !reused_chunk_ids.contains(&chunk.chunk_id))
            .collect();

        // Per-chunk graph extraction shares immutable state. The previous
        // version `.clone()`-d every captured value once *per chunk* — for
        // a 100-chunk document with thousands of typed facts, hundreds of
        // sub_type hints and a populated table-graph context, that
        // amounted to ~50–200 MB of redundant copies floating in memory
        // alongside the in-flight LLM futures. Wrapping the heavy shared
        // structures in `Arc` once turns each per-chunk capture into a
        // refcount bump (8 bytes), so the hot loop only allocates the
        // small per-chunk content/facts views.
        let document = std::sync::Arc::new(document);
        let revision = std::sync::Arc::new(revision);
        let revision_facts = std::sync::Arc::new(revision_facts);
        let library_extraction_prompt = std::sync::Arc::new(library_extraction_prompt);
        let sub_type_hints = std::sync::Arc::new(sub_type_hints);
        let table_graph_context = std::sync::Arc::new(table_graph_context);

        let per_chunk_stream = stream::iter(chunks.into_iter().map(|chunk| {
            let state = state.clone();
            let graph_runtime_context = graph_runtime_context.clone();
            let document = std::sync::Arc::clone(&document);
            let revision = std::sync::Arc::clone(&revision);
            let command = command.clone();
            let revision_facts = std::sync::Arc::clone(&revision_facts);
            let library_extraction_prompt = std::sync::Arc::clone(&library_extraction_prompt);
            let sub_type_hints = std::sync::Arc::clone(&sub_type_hints);
            let table_graph_context = std::sync::Arc::clone(&table_graph_context);

            async move {
                let table_graph_profile = table_graph_context.profile_for_chunk(&chunk);
                let Some(chunk_content) =
                    build_graph_chunk_content(
                        &chunk,
                        table_graph_profile,
                        table_graph_context.requires_row_only_graph(),
                    )
                else {
                    return Ok::<ChunkExtractAggregate, ApiError>(
                        ChunkExtractAggregate::default(),
                    );
                };

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
                        chunk_content,
                        &chunk_facts,
                        command.attempt_id,
                        (*library_extraction_prompt).clone(),
                        (*sub_type_hints).clone(),
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

                // Pull the few small numeric/string fields we actually need
                // out of the response BEFORE moving it. Everything else
                // (the full normalized graph, the raw output JSON, recovery
                // attempts, etc.) is dropped at the end of this future,
                // not held in a result Vec across the whole library.
                let prompt_tokens = response
                    .usage_json
                    .get("prompt_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let completion_tokens = response
                    .usage_json
                    .get("completion_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let total_tokens = response
                    .usage_json
                    .get("total_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                Ok::<ChunkExtractAggregate, ApiError>(ChunkExtractAggregate {
                    extracted_entities,
                    extracted_relations,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                })
            }
        }));

        // Stream-fold the per-chunk results into a single aggregate. The
        // previous version did `.try_collect::<Vec<_>>()` + a follow-up
        // `for (entities, ...)` loop, which kept *every* chunk's tuple
        // (including its `serde_json::Value` usage payload) resident until
        // the entire document was done. For a 100-chunk document with
        // mid-sized usage payloads that was ~5 MB just on `Value` objects,
        // multiplied by `library_limit=12` parallel docs = 60 MB sitting
        // around for nothing. Fold consumes each result inline and drops it.
        let aggregate = per_chunk_stream
            .buffer_unordered(graph_extract_parallelism)
            .try_fold(ChunkExtractAggregate::default(), |mut acc, item| async move {
                acc.extracted_entities =
                    acc.extracted_entities.saturating_add(item.extracted_entities);
                acc.extracted_relations =
                    acc.extracted_relations.saturating_add(item.extracted_relations);
                acc.prompt_tokens += item.prompt_tokens;
                acc.completion_tokens += item.completion_tokens;
                acc.total_tokens += item.total_tokens;
                Ok(acc)
            })
            .await?;

        let extracted_entities = aggregate.extracted_entities;
        let extracted_relations = aggregate.extracted_relations;
        // The active extract_graph binding is the single source of truth for
        // provider/model on this stage. Per-chunk responses always echo the
        // same binding, so we read it directly from the runtime context
        // instead of carrying it through the per-chunk aggregate.
        let provider_kind =
            graph_runtime_context.provider_profile.indexing.provider_kind.as_str().to_string();
        let model_name = graph_runtime_context.provider_profile.indexing.model_name.clone();
        let agg_prompt = aggregate.prompt_tokens;
        let agg_completion = aggregate.completion_tokens;
        let agg_total = aggregate.total_tokens;

        let usage_json = serde_json::json!({
            "prompt_tokens": agg_prompt,
            "completion_tokens": agg_completion,
            "total_tokens": agg_total,
        });

        Ok(RevisionGraphCandidateMaterialization {
            chunk_count,
            extracted_entities: extracted_entities.saturating_add(reused_entity_count),
            extracted_relations: extracted_relations.saturating_add(reused_relation_count),
            provider_kind: Some(provider_kind),
            model_name: Some(model_name),
            usage_json,
            reused_chunks: reused_chunk_ids.len(),
            reused_entities: reused_entity_count,
            reused_relations: reused_relation_count,
        })
    }

    /// Diff-aware reuse plan: maps each new chunk_id whose text is unchanged
    /// against the parent revision to the latest "ready" extraction record
    /// for the matching old chunk. Skips records whose extraction version no
    /// longer matches the current prompt to avoid reusing stale outputs.
    async fn build_chunk_reuse_plan(
        &self,
        state: &AppState,
        command: &MaterializeRevisionGraphCandidatesCommand,
        new_chunks: &[crate::infra::arangodb::document_store::KnowledgeChunkRow],
    ) -> Result<ChunkReusePlan, ApiError> {
        use std::collections::HashMap;

        // Step 1: load the new revision row to find its parent_revision_id.
        let new_revision_row = content_repository::get_revision_by_id(
            &state.persistence.postgres,
            command.revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "diff_reuse: get_revision_by_id new"))?;
        let Some(new_revision_row) = new_revision_row else {
            return Ok(ChunkReusePlan::default());
        };
        let Some(parent_revision_id) = new_revision_row.parent_revision_id else {
            return Ok(ChunkReusePlan::default());
        };

        // Step 2: load parent chunks (Postgres has the text_checksum column).
        let parent_chunks = content_repository::list_chunks_by_revision(
            &state.persistence.postgres,
            parent_revision_id,
        )
        .await
        .map_err(|e| {
            ApiError::internal_with_log(e, "diff_reuse: list_chunks_by_revision parent")
        })?;
        if parent_chunks.is_empty() {
            return Ok(ChunkReusePlan::default());
        }
        let parent_chunk_ids: std::collections::BTreeSet<Uuid> =
            parent_chunks.iter().map(|c| c.id).collect();

        // Step 3: load all extraction records for this document and pick the
        // latest "ready" record per parent chunk.
        let all_records =
            crate::infra::repositories::list_runtime_graph_extraction_records_by_document(
                &state.persistence.postgres,
                new_revision_row.document_id,
            )
            .await
            .map_err(|e| {
                ApiError::internal_with_log(
                    e,
                    "diff_reuse: list_runtime_graph_extraction_records_by_document",
                )
            })?;
        // Wrap each record in Arc so the subsequent text_checksum and
        // new_chunk HashMaps only clone the refcount instead of the full
        // `raw_output_json` + `normalized_output_json` `serde_json::Value`
        // payloads, which for documents with hundreds of chunks across
        // multiple revisions can be 200+ MB each map level (previously
        // cloned three times through this function).
        let mut latest_records_by_parent_chunk: HashMap<
            Uuid,
            std::sync::Arc<crate::infra::repositories::RuntimeGraphExtractionRecordRow>,
        > = HashMap::new();
        for record in all_records {
            if record.status != ASYNC_OP_STATUS_READY {
                continue;
            }
            if !parent_chunk_ids.contains(&record.chunk_id) {
                continue;
            }
            if record.extraction_version != GRAPH_EXTRACTION_VERSION_FOR_REUSE {
                continue;
            }
            match latest_records_by_parent_chunk.get(&record.chunk_id) {
                Some(existing) if existing.created_at >= record.created_at => {}
                _ => {
                    latest_records_by_parent_chunk
                        .insert(record.chunk_id, std::sync::Arc::new(record));
                }
            }
        }
        if latest_records_by_parent_chunk.is_empty() {
            return Ok(ChunkReusePlan::default());
        }

        let mut record_by_checksum: HashMap<
            String,
            std::sync::Arc<crate::infra::repositories::RuntimeGraphExtractionRecordRow>,
        > = HashMap::new();
        for chunk in &parent_chunks {
            if let Some(record) = latest_records_by_parent_chunk.get(&chunk.id) {
                record_by_checksum
                    .entry(chunk.text_checksum.clone())
                    .or_insert_with(|| std::sync::Arc::clone(record));
            }
        }
        if record_by_checksum.is_empty() {
            return Ok(ChunkReusePlan::default());
        }

        let new_chunks_pg = content_repository::list_chunks_by_revision(
            &state.persistence.postgres,
            command.revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "diff_reuse: list_chunks_by_revision new"))?;
        let new_chunk_id_set: std::collections::BTreeSet<Uuid> =
            new_chunks.iter().map(|c| c.chunk_id).collect();
        let mut records_by_new_chunk: HashMap<
            Uuid,
            std::sync::Arc<crate::infra::repositories::RuntimeGraphExtractionRecordRow>,
        > = HashMap::new();
        for chunk in new_chunks_pg {
            if !new_chunk_id_set.contains(&chunk.id) {
                continue;
            }
            if let Some(record) = record_by_checksum.get(&chunk.text_checksum) {
                records_by_new_chunk.insert(chunk.id, std::sync::Arc::clone(record));
            }
        }
        Ok(ChunkReusePlan { records_by_new_chunk })
    }
}

#[derive(Debug, Default)]
struct ChunkReusePlan {
    records_by_new_chunk: std::collections::HashMap<
        Uuid,
        std::sync::Arc<crate::infra::repositories::RuntimeGraphExtractionRecordRow>,
    >,
}

/// Per-chunk graph extraction outcome reduced to just the small fields the
/// caller actually aggregates. Replaces the old 5-tuple `(usize, usize,
/// Option<String>, Option<String>, serde_json::Value)` so the stream-fold
/// path holds only ~96 bytes per chunk in flight instead of the full
/// `serde_json::Value` usage payload.
#[derive(Debug, Default)]
struct ChunkExtractAggregate {
    extracted_entities: usize,
    extracted_relations: usize,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
}

#[derive(Clone, Default)]
struct RevisionTableGraphContext {
    by_row_block_id: HashMap<Uuid, TableGraphProfile>,
    row_only_table_graph: bool,
}

impl RevisionTableGraphContext {
    fn profile_for_chunk(
        &self,
        chunk: &crate::infra::arangodb::document_store::KnowledgeChunkRow,
    ) -> Option<&TableGraphProfile> {
        chunk.support_block_ids.iter().find_map(|block_id| self.by_row_block_id.get(block_id))
    }

    fn requires_row_only_graph(&self) -> bool {
        self.row_only_table_graph
    }
}

/// Process-local TTL cache for `load_sub_type_hints_for_extraction`.
///
/// The underlying SQL is a library-wide full scan of
/// `runtime_graph_node` with a JSON-path group-by — measured at
/// ~3.5 s on a mid-size prod corpus under merge load, and the function
/// is called once per
/// `extract_graph` stage (so 1× per document ingested). Under a bulk
/// 24-concurrent worker drain this aggregated to 20+ calls per 30 min
/// window and dominated the slow-statement log after I3 bulk upserts
/// removed the previous merge-side contention. The returned hints
/// change slowly — a single ingest adds at most a handful of
/// (node_type, sub_type) pairs out of thousands — so a short TTL is
/// sound; readers see at worst a minute-old hint set.
///
/// Cache is keyed by `(library_id, projection_version)` so a new
/// projection version (published at the end of a full graph rebuild)
/// transparently invalidates. Missing/stale entries fall through to
/// the SQL path; SQL failure still yields empty hints, matching the
/// prior fail-open behaviour.
const SUB_TYPE_HINTS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone)]
struct SubTypeHintsCacheEntry {
    hints: GraphExtractionSubTypeHints,
    fetched_at: std::time::Instant,
}

fn sub_type_hints_cache()
-> &'static std::sync::Mutex<std::collections::HashMap<(Uuid, i64), SubTypeHintsCacheEntry>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<(Uuid, i64), SubTypeHintsCacheEntry>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Loads vocabulary-aware extraction hints: observed `sub_type` values per
/// `node_type` for the current library at the active projection version.
///
/// Returns an empty `GraphExtractionSubTypeHints` on any failure (missing
/// snapshot, SQL error, empty graph). Hints are a soft prompt anchor — never
/// fail the ingest path because of them.
async fn load_sub_type_hints_for_extraction(
    state: &AppState,
    library_id: Uuid,
) -> GraphExtractionSubTypeHints {
    const TOP_PER_NODE_TYPE: usize = 15;

    let projection_scope = match resolve_projection_scope(state, library_id).await {
        Ok(scope) => scope,
        Err(error) => {
            warn!(
                library_id = %library_id,
                error = %error,
                "sub_type hints: failed to resolve projection scope, falling back to empty hints"
            );
            return GraphExtractionSubTypeHints::default();
        }
    };

    // Cache hit: fresh entry for the same (library, projection_version).
    {
        let guard = sub_type_hints_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.get(&(library_id, projection_scope.projection_version)) {
            if entry.fetched_at.elapsed() < SUB_TYPE_HINTS_CACHE_TTL {
                return entry.hints.clone();
            }
        }
    }

    let rows = match repositories::list_observed_sub_type_hints(
        &state.persistence.postgres,
        library_id,
        projection_scope.projection_version,
    )
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(
                library_id = %library_id,
                error = %error,
                "sub_type hints: SQL aggregation failed, falling back to empty hints"
            );
            return GraphExtractionSubTypeHints::default();
        }
    };

    let mut groups: Vec<GraphExtractionSubTypeHintGroup> = Vec::new();
    for row in rows {
        if groups.last().is_none_or(|group| group.node_type != row.node_type) {
            groups.push(GraphExtractionSubTypeHintGroup {
                node_type: row.node_type.clone(),
                entries: Vec::new(),
            });
        }
        if let Some(group) = groups.last_mut() {
            if group.entries.len() >= TOP_PER_NODE_TYPE {
                continue;
            }
            group.entries.push(GraphExtractionSubTypeHintEntry {
                sub_type: row.sub_type,
                occurrences: row.occurrences,
            });
        }
    }

    let hints = GraphExtractionSubTypeHints { by_node_type: groups };
    {
        let mut guard = sub_type_hints_cache().lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(
            (library_id, projection_scope.projection_version),
            SubTypeHintsCacheEntry { hints: hints.clone(), fetched_at: std::time::Instant::now() },
        );
        // Опtimistic housekeeping: drop stale entries when the cache
        // accumulates across many libraries.
        if guard.len() > 64 {
            guard.retain(|_, entry| entry.fetched_at.elapsed() < SUB_TYPE_HINTS_CACHE_TTL);
        }
    }
    hints
}

fn build_revision_table_graph_context(
    blocks: &[KnowledgeStructuredBlockRow],
) -> RevisionTableGraphContext {
    let mut row_parent_table_ids = HashMap::<Uuid, Uuid>::new();
    let mut summaries_by_table = HashMap::<Uuid, Vec<_>>::new();

    for block in blocks {
        if block.block_kind == "table_row" {
            if let Some(parent_block_id) = block.parent_block_id {
                row_parent_table_ids.insert(block.block_id, parent_block_id);
            }
            continue;
        }

        if block.block_kind != "metadata_block" || !is_table_summary_text(&block.normalized_text) {
            continue;
        }
        let Some(parent_block_id) = block.parent_block_id else {
            continue;
        };
        let Some(summary) = parse_table_column_summary(&block.normalized_text) else {
            continue;
        };
        summaries_by_table.entry(parent_block_id).or_default().push(summary);
    }

    let profiles_by_table = summaries_by_table
        .into_iter()
        .filter_map(|(table_block_id, summaries)| {
            let profile = build_table_graph_profile(&summaries);
            (!profile.is_empty()).then_some((table_block_id, profile))
        })
        .collect::<HashMap<_, _>>();

    let by_row_block_id = row_parent_table_ids
        .into_iter()
        .filter_map(|(row_block_id, table_block_id)| {
            profiles_by_table.get(&table_block_id).cloned().map(|profile| (row_block_id, profile))
        })
        .collect();

    RevisionTableGraphContext {
        by_row_block_id,
        row_only_table_graph: revision_requires_row_only_table_graph(blocks),
    }
}

fn revision_requires_row_only_table_graph(blocks: &[KnowledgeStructuredBlockRow]) -> bool {
    let has_table_rows = blocks.iter().any(|block| block.block_kind == "table_row");
    has_table_rows && blocks.iter().all(block_supports_row_only_table_graph)
}

fn block_supports_row_only_table_graph(block: &KnowledgeStructuredBlockRow) -> bool {
    match block.block_kind.as_str() {
        "heading" | "table" | "table_row" => true,
        "metadata_block" => is_table_summary_text(&block.normalized_text),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::build_revision_table_graph_context;
    use crate::{
        infra::arangodb::document_store::{KnowledgeChunkRow, KnowledgeStructuredBlockRow},
        shared::extraction::table_graph::build_graph_table_row_text,
    };

    fn make_chunk(normalized_text: &str) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
            key: Uuid::nil().to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("table_row".to_string()),
            content_text: String::new(),
            normalized_text: normalized_text.to_string(),
            span_start: None,
            span_end: None,
            token_count: None,
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: None,
            vector_generation: None,
            quality_score: None,
        }
    }

    fn make_block(
        block_id: Uuid,
        block_kind: &str,
        normalized_text: &str,
        parent_block_id: Option<Uuid>,
    ) -> KnowledgeStructuredBlockRow {
        KnowledgeStructuredBlockRow {
            key: block_id.to_string(),
            arango_id: None,
            arango_rev: None,
            block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            ordinal: 0,
            block_kind: block_kind.to_string(),
            text: normalized_text.to_string(),
            normalized_text: normalized_text.to_string(),
            heading_trail: vec![],
            section_path: vec![],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id,
            table_coordinates_json: None,
            code_language: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn table_graph_profiles_attach_summary_statistics_to_row_chunks() {
        let table_id = Uuid::now_v7();
        let row_id = Uuid::now_v7();
        let graph_context = build_revision_table_graph_context(&[
            make_block(
                row_id,
                "table_row",
                "Sheet: organizations | Row 1 | Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics | Founded: 1972 | Website: https://price.net",
                Some(table_id),
            ),
            make_block(
                Uuid::now_v7(),
                "metadata_block",
                "Table Summary | Sheet: organizations | Column: Name | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Most Frequent Count: 1 | Most Frequent Tie Count: 3 | Most Frequent Values: Ferrell LLC; Meyer Group; Adams LLC",
                Some(table_id),
            ),
            make_block(
                Uuid::now_v7(),
                "metadata_block",
                "Table Summary | Sheet: organizations | Column: Country | Value Kind: categorical | Value Shape: label | Aggregation Priority: 3 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 2 | Most Frequent Count: 2 | Most Frequent Tie Count: 1 | Most Frequent Values: Papua New Guinea",
                Some(table_id),
            ),
            make_block(
                Uuid::now_v7(),
                "metadata_block",
                "Table Summary | Sheet: organizations | Column: Founded | Value Kind: numeric | Value Shape: identifier | Aggregation Priority: 3 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Average: 1991.67 | Min: 1972 | Max: 2012",
                Some(table_id),
            ),
        ]);

        let mut chunk = make_chunk(
            "Sheet: organizations | Row 1 | Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics | Founded: 1972 | Website: https://price.net",
        );
        chunk.support_block_ids = vec![row_id];

        let profile = graph_context.profile_for_chunk(&chunk).expect("profile");
        let text =
            build_graph_table_row_text(&chunk.normalized_text, Some(profile)).expect("graph text");

        assert_eq!(text, "Name: Ferrell LLC | Country: Papua New Guinea");
    }

    #[test]
    fn row_only_table_graph_mode_activates_for_table_native_revisions() {
        let table_id = Uuid::now_v7();
        let graph_context = build_revision_table_graph_context(&[
            make_block(Uuid::now_v7(), "heading", "test1", None),
            make_block(table_id, "table", "| col_1 |\n| --- |\n| test1 |", None),
            make_block(
                Uuid::now_v7(),
                "table_row",
                "Sheet: test1 | Row 1 | col_1: test1",
                Some(table_id),
            ),
            make_block(
                Uuid::now_v7(),
                "metadata_block",
                "Table Summary | Sheet: test1 | Column: col_1 | Value Kind: categorical | Value Shape: label | Aggregation Priority: 1 | Row Count: 1 | Non-empty Count: 1 | Distinct Count: 1 | Most Frequent Count: 1 | Most Frequent Tie Count: 1 | Most Frequent Values: test1",
                Some(table_id),
            ),
        ]);

        assert!(graph_context.requires_row_only_graph());
    }

    #[test]
    fn row_only_table_graph_mode_stays_disabled_for_mixed_markdown_and_tables() {
        let table_id = Uuid::now_v7();
        let graph_context = build_revision_table_graph_context(&[
            make_block(Uuid::now_v7(), "heading", "Inventory", None),
            make_block(Uuid::now_v7(), "paragraph", "This section summarizes the inventory.", None),
            make_block(table_id, "table", "| name | stock |\n| --- | --- |\n| Widget | 7 |", None),
            make_block(
                Uuid::now_v7(),
                "table_row",
                "Sheet: inventory | Row 1 | Name: Widget | Stock: 7",
                Some(table_id),
            ),
        ]);

        assert!(!graph_context.requires_row_only_graph());
    }
}
